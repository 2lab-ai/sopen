//! The engine's part of 04 §8 (K-C1–K-C5, K-M1, K-M2, K-N1, K-E1), each on this machine's file
//! layer and through this machine's own OpenSSH `sftp-server` (a real server over pipes; skipped
//! with a note where it is not installed, a failure under `SOPEN_REQUIRE_SFTP_SERVER`). Faults
//! and races come from [`Faulty`], a file layer with hooks — never from sleeps.

use super::*;
use crate::remote::ops::{
    taken, FileMeta, NoRename, Opened, Publish, Renamed, StageFailure, Staged, COPY_STEP,
};
use crate::remote::{CreateError, Entry, Like, LocalFs, RemoteFs, SftpRemote, Stat, Uploaded};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn s(path: &Path) -> String {
    path.to_str().unwrap().to_string()
}

/// A new folder with its path resolved (macOS: `/var` → `/private/var`), as a server's
/// `realpath` gives it, holding `src` and `dst`.
fn scratch() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let (src, dst) = (root.join("src"), root.join("dst"));
    std::fs::create_dir(&src).unwrap();
    std::fs::create_dir(&dst).unwrap();
    (tmp, src, dst)
}

fn sha(path: &Path) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(std::fs::read(path).unwrap()).into()
}

fn ino(path: &Path) -> u64 {
    std::fs::symlink_metadata(path).unwrap().ino()
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn mtime_secs(path: &Path) -> i64 {
    std::fs::symlink_metadata(path).unwrap().mtime()
}

fn set_mtime(path: &Path, secs: u64) {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(t).set_accessed(t))
        .unwrap();
}

/// Names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    out.sort();
    out
}

/// sopen's staged temps (`.<name>.sopen-<random>`) anywhere below `dir`.
fn temps(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().into_string().unwrap();
        if name.contains(".sopen-") {
            out.push(s(&e.path()));
        }
        if e.file_type().unwrap().is_dir() {
            out.extend(temps(&e.path()));
        }
    }
    out
}

fn job(op: Op, src: &Path, items: &[&str], dst: &Path) -> Job {
    Job {
        op,
        src_dir: s(src),
        names: items.iter().map(|n| n.to_string()).collect(),
        dst_dir: s(dst),
    }
}

fn item(name: &str, outcome: ItemOutcome) -> Item {
    Item {
        name: name.to_string(),
        outcome,
    }
}

/// The reason of a refused operation: every item not reached, the reason its first note.
fn refusal(r: &Report) -> &str {
    assert!(
        r.items.iter().all(|i| i.outcome == ItemOutcome::NotReached),
        "not refused: {r:?}"
    );
    r.notes
        .first()
        .unwrap_or_else(|| panic!("no reason: {r:?}"))
}

/// The reason of an item Esc stopped inside.
fn stop_reason(i: &Item) -> &str {
    match &i.outcome {
        ItemOutcome::Failed(why) if why.starts_with(STOPPED) => why,
        other => panic!("not stopped: {other:?}"),
    }
}

fn reply(answer: Answer) -> Reply {
    Reply {
        answer,
        same_for_rest: false,
    }
}

/// The dialog side, scripted: answers in order (a question nobody scripted fails the test),
/// every progress line kept, and Esc once a file's progress reaches `stop_at` bytes, at the
/// first progress line of the verb `stop_when`, or (`stop_on_ask`) while a question is open.
#[derive(Default)]
struct Script {
    cancel: Arc<AtomicBool>,
    replies: VecDeque<Reply>,
    asked: Vec<Clash>,
    seen: Vec<Progress>,
    stop_at: Option<u64>,
    stop_when: Option<&'static str>,
    stop_on_ask: bool,
    /// Runs when a question is asked, before it is answered.
    on_ask: Option<Box<dyn FnMut()>>,
}

impl Script {
    fn answers(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self {
            replies: replies.into_iter().collect(),
            ..Self::default()
        }
    }
}

impl Control for Script {
    fn cancel(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    fn progress(&mut self, p: &Progress) {
        self.seen.push(p.clone());
        let at = self.stop_at.is_some_and(|at| p.done >= at && p.done > 0);
        if at || self.stop_when == Some(p.verb) {
            self.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn ask(&mut self, clash: &Clash) -> Reply {
        self.asked.push(clash.clone());
        if let Some(on_ask) = self.on_ask.as_mut() {
            on_ask();
        }
        if self.stop_on_ask {
            self.cancel.store(true, Ordering::Relaxed);
        }
        self.replies
            .pop_front()
            .unwrap_or_else(|| panic!("an unscripted question: {clash:?}"))
    }
}

/// Answers a rename before it reaches the disk (`None`: let it through).
type RenameHook<'a> = Box<dyn FnMut(&str, &str) -> Option<anyhow::Result<Renamed>> + Send + 'a>;
/// Runs right after a rename that went through.
type RenamedHook<'a> = Box<dyn FnMut(&str, &str) + Send + 'a>;
/// Fails a file's copy, by its source path (`None`: let it through).
type CopyHook<'a> = Box<dyn FnMut(&str) -> Option<anyhow::Error> + Send + 'a>;
/// Edits a folder's listing.
type ListHook<'a> = Box<dyn FnMut(&str, &mut Vec<String>) + Send + 'a>;
/// Runs right after a same-host copy into its temp succeeded, with the temp's path.
type CopiedHook<'a> = Box<dyn FnMut(&str) + Send + 'a>;
/// Fails a call before it reaches the disk, by the call's name and its first path (`None`: let
/// it through) — also where a test does something at that exact step.
type FaultHook<'a> = Box<dyn FnMut(&str, &str) -> Option<anyhow::Error> + Send + 'a>;
/// Runs right after a call went through, by the call's name and its first path.
type AfterHook<'a> = Box<dyn FnMut(&str, &str) + Send + 'a>;
/// A file's inode and SHA-256: what proves it untouched.
type Ident = (u64, [u8; 32]);
/// A change made to a file at a test's step.
type Change = fn(&Path);
/// One file layer, owned.
type Layer = Box<dyn FileOps + Send>;
/// Opens a file layer, or `None` where it is missing (no `sftp-server`).
type Opener = Box<dyn FnMut() -> Option<Layer>>;

/// A file layer that hands every call to `inner`, with faults and hooks (04 §8's "fault-injecting
/// file layer"): a rename answered or failed before it reaches the disk, something done right
/// after a rename (a publish, a move), a file's copy failed, a listing edited, a `realpath` that
/// resolves nothing, a server without `hardlink@openssh.com`.
struct Faulty {
    inner: Layer,
    rename: RenameHook<'static>,
    renamed: RenamedHook<'static>,
    copy: CopyHook<'static>,
    copied: CopiedHook<'static>,
    list: ListHook<'static>,
    /// `identity`, `lstat`, `remove`, `remove_dir`, `list_entries`.
    fault: FaultHook<'static>,
    /// `make_symlink` (its link), `set_meta`, `read_to`, `open_read`.
    after: AfterHook<'static>,
    plain_realpath: bool,
    no_link: bool,
    /// A lower [`RemoteFs::hash_check_limit`].
    hash_limit: Option<u64>,
    /// A same-host copy is reported as made by the host itself (no digest of its bytes), as
    /// `copy-data` and `copy_file_range` are: a move then reads it back.
    host_made: bool,
    /// Barriers asked of this layer ([`FileOps::barrier`]).
    barriers: Arc<AtomicUsize>,
    /// One ordered record of what reached the disk through this layer: `write:<path>` (a copy
    /// staged), `rename:<from>-><to>`, `mkdir:`, `symlink:`, `remove:`, `rmdir:`, `list:`, and
    /// `barrier:<dirs; deepest first>` once a barrier completed (`barrier-failed:` when not).
    trace: Arc<Mutex<Vec<String>>>,
    /// A stream read from this layer fails after this many bytes.
    fail_read_after: Option<usize>,
    /// No filesystem id ([`FileOps::fs_id`]): two connections cannot tell they share one.
    no_fs_id: bool,
    /// The stream a copy from another host writes is corrupted (its first byte flipped) on its
    /// way into the temp, while the reader's digest is of the bytes read.
    corrupt: bool,
    /// No source can be pinned ([`FileOps::pin`]): as when descriptors run out.
    no_pins: bool,
    /// How many sources were pinned.
    pinned: Arc<AtomicUsize>,
    /// The Trash this layer has instead of its inner one's.
    trash: Option<Box<dyn crate::remote::ops::Trash + Send>>,
}

impl Faulty {
    /// The layer back, for the next step of a test.
    fn into_inner(self) -> Layer {
        self.inner
    }

    fn new(inner: Layer) -> Self {
        Self {
            inner,
            rename: Box::new(|_, _| None),
            renamed: Box::new(|_, _| {}),
            copy: Box::new(|_| None),
            copied: Box::new(|_| {}),
            list: Box::new(|_, _| {}),
            fault: Box::new(|_, _| None),
            after: Box::new(|_, _| {}),
            plain_realpath: false,
            no_link: false,
            hash_limit: None,
            host_made: false,
            barriers: Arc::default(),
            trace: Arc::default(),
            fail_read_after: None,
            no_fs_id: false,
            corrupt: false,
            no_pins: false,
            pinned: Arc::default(),
            trash: None,
        }
    }
}

impl Faulty {
    fn log(&self, event: String) {
        self.trace.lock().unwrap().push(event);
    }
}

impl RemoteFs for Faulty {
    fn stat_opt(&mut self, path: &str) -> anyhow::Result<Option<Stat>> {
        self.inner.stat_opt(path)
    }
    fn lstat_opt(&mut self, path: &str) -> anyhow::Result<Option<Stat>> {
        if let Some(e) = (self.fault)("lstat", path) {
            return Err(e);
        }
        self.inner.lstat_opt(path)
    }
    fn realpath(&mut self, path: &str) -> anyhow::Result<String> {
        if self.plain_realpath {
            return Ok(path.to_string());
        }
        self.inner.realpath(path)
    }
    fn read_head(&mut self, path: &str, max: usize) -> anyhow::Result<Vec<u8>> {
        self.inner.read_head(path, max)
    }
    fn list_dir(&mut self, path: &str) -> anyhow::Result<Vec<String>> {
        let mut names = self.inner.list_dir(path)?;
        (self.list)(path, &mut names);
        Ok(names)
    }
    fn list_entries(&mut self, path: &str) -> anyhow::Result<Vec<Entry>> {
        if let Some(e) = (self.fault)("list_entries", path) {
            return Err(e);
        }
        self.log(format!("list:{path}"));
        self.inner.list_entries(path)
    }
    fn free_space(&mut self, path: &str) -> Option<u64> {
        self.inner.free_space(path)
    }
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> anyhow::Result<u64> {
        let read = self.inner.read_to(path, out, progress)?;
        self.log(format!("read:{path}"));
        (self.after)("read_to", path);
        Ok(read)
    }
    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<Like>,
    ) -> Result<Uploaded, CreateError> {
        self.inner.create_from(local, path, like)
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> anyhow::Result<()> {
        self.inner.set_mode(path, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        self.inner.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        self.inner.link_new(from, to)
    }
    fn can_link(&self) -> bool {
        !self.no_link && self.inner.can_link()
    }
    fn can_replace(&self) -> bool {
        self.inner.can_replace()
    }
    fn remove(&mut self, path: &str) -> anyhow::Result<()> {
        if let Some(e) = (self.fault)("remove", path) {
            return Err(e);
        }
        self.inner.remove(path)?;
        self.log(format!("remove:{path}"));
        Ok(())
    }
    fn hash_check_limit(&self) -> u64 {
        self.hash_limit
            .unwrap_or_else(|| self.inner.hash_check_limit())
    }
}

impl FileOps for Faulty {
    fn identity(
        &mut self,
        path: &str,
        follow: bool,
    ) -> anyhow::Result<Option<crate::remote::ops::Ident>> {
        if let Some(e) = (self.fault)("identity", path) {
            return Err(e);
        }
        self.inner.identity(path, follow)
    }
    fn pin(
        &mut self,
        path: &str,
        id: &crate::remote::ops::Ident,
    ) -> Option<crate::remote::ops::Pin> {
        if self.no_pins {
            return None;
        }
        let pin = self.inner.pin(path, id)?;
        self.pinned.fetch_add(1, Ordering::Relaxed);
        Some(pin)
    }
    fn meta(&mut self, path: &str) -> anyhow::Result<FileMeta> {
        self.inner.meta(path)
    }
    fn rename_new(&mut self, from: &str, to: &str) -> anyhow::Result<Renamed> {
        if let Some(answer) = (self.rename)(from, to) {
            return answer;
        }
        let done = self.inner.rename_new(from, to)?;
        self.log(format!("rename:{from}->{to}"));
        (self.renamed)(from, to);
        Ok(done)
    }
    fn can_rename_dir(&self) -> bool {
        self.inner.can_rename_dir()
    }
    fn can_publish(&self) -> bool {
        !self.no_link && self.inner.can_publish()
    }
    fn publish_probe(&mut self, dir: &str) -> anyhow::Result<Vec<String>> {
        self.inner.publish_probe(dir)
    }
    /// The same hooks and record as [`Faulty::rename_new`]: a publish is a rename to them.
    fn publish_new(&mut self, from: &str, to: &str) -> anyhow::Result<Renamed> {
        if let Some(answer) = (self.rename)(from, to) {
            return answer;
        }
        let done = self.inner.publish_new(from, to)?;
        self.log(format!("rename:{from}->{to}"));
        (self.renamed)(from, to);
        Ok(done)
    }
    /// The file layer's own walk (on this machine through folder descriptors): its steps are
    /// seen through [`crate::remote::ops::seam_tree_looked`], not this layer's hooks.
    fn remove_tree(
        &mut self,
        path: &str,
        watch: &mut dyn crate::remote::ops::TreeWatch,
    ) -> crate::remote::ops::TreeEnd {
        self.inner.remove_tree(path, watch)
    }
    fn make_folder(&mut self, path: &str) -> anyhow::Result<()> {
        self.inner.make_folder(path)?;
        self.log(format!("mkdir:{path}"));
        Ok(())
    }
    fn trash(&self) -> Option<&dyn crate::remote::ops::Trash> {
        match &self.trash {
            Some(trash) => Some(trash.as_ref()),
            None => self.inner.trash(),
        }
    }
    fn publish_mode(&mut self, dir: &str) -> anyhow::Result<(Publish, Vec<String>)> {
        self.inner.publish_mode(dir)
    }
    fn barrier(&mut self, dirs: &[String]) -> anyhow::Result<()> {
        self.barriers.fetch_add(1, Ordering::Relaxed);
        let shown = dirs.join(";");
        let done = match (self.fault)("barrier", &shown) {
            Some(e) => Err(e),
            None => self.inner.barrier(dirs),
        };
        match &done {
            Ok(()) => self.log(format!("barrier:{shown}")),
            Err(_) => self.log(format!("barrier-failed:{shown}")),
        }
        done
    }
    fn fs_id(&mut self, path: &str) -> Option<u64> {
        if self.no_fs_id {
            return None;
        }
        self.inner.fs_id(path)
    }
    fn make_dir(&mut self, path: &str) -> anyhow::Result<()> {
        self.inner.make_dir(path)?;
        self.log(format!("mkdir:{path}"));
        Ok(())
    }
    fn remove_dir(&mut self, path: &str) -> anyhow::Result<()> {
        if let Some(e) = (self.fault)("remove_dir", path) {
            return Err(e);
        }
        self.inner.remove_dir(path)?;
        self.log(format!("rmdir:{path}"));
        Ok(())
    }
    fn read_link(&mut self, path: &str) -> anyhow::Result<String> {
        self.inner.read_link(path)
    }
    fn make_symlink(&mut self, target: &str, link: &str) -> anyhow::Result<()> {
        self.inner.make_symlink(target, link)?;
        self.log(format!("symlink:{link}"));
        (self.after)("make_symlink", link);
        Ok(())
    }
    fn set_meta(&mut self, path: &str, meta: &FileMeta) -> Vec<String> {
        let notes = self.inner.set_meta(path, meta);
        (self.after)("set_meta", path);
        notes
    }
    fn copy_within(
        &mut self,
        from: &str,
        temp: &str,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<Staged, StageFailure> {
        if let Some(e) = (self.copy)(from) {
            return Err(StageFailure::not_created(e));
        }
        let mut staged = self.inner.copy_within(from, temp, cancel, progress)?;
        if self.host_made {
            staged.sha256 = None;
        }
        self.log(format!("write:{temp}"));
        (self.copied)(temp);
        Ok(staged)
    }
    fn open_read(&mut self, path: &str) -> anyhow::Result<Opened<'_>> {
        if let Some(e) = (self.copy)(path) {
            return Err(e);
        }
        let fail_after = self.fail_read_after;
        let mut opened = self.inner.open_read(path)?;
        (self.after)("open_read", path);
        if let Some(n) = fail_after {
            opened.read = Box::new(FailAfter {
                inner: opened.read,
                left: n,
            });
        }
        Ok(opened)
    }
    fn stage_from(
        &mut self,
        src: &mut dyn Read,
        meta: &FileMeta,
        temp: &str,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<Staged, StageFailure> {
        let staged = if self.corrupt {
            let mut flipped = Flip {
                inner: src,
                first: true,
            };
            self.inner
                .stage_from(&mut flipped, meta, temp, cancel, progress)?
        } else {
            self.inner.stage_from(src, meta, temp, cancel, progress)?
        };
        self.log(format!("write:{temp}"));
        (self.copied)(temp);
        Ok(staged)
    }
}

/// A reader that fails once it has passed `left` bytes on.
struct FailAfter<'r> {
    inner: Box<dyn Read + 'r>,
    left: usize,
}

impl Read for FailAfter<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Err(std::io::Error::other(
                "injected: the source stopped answering",
            ));
        }
        let want = buf.len().min(self.left);
        let n = self.inner.read(&mut buf[..want])?;
        self.left -= n;
        Ok(n)
    }
}

/// A reader that flips the first byte it passes on.
struct Flip<'r> {
    inner: &'r mut dyn Read,
    first: bool,
}

impl Read for Flip<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if self.first && n > 0 {
            buf[0] ^= 0xff;
            self.first = false;
        }
        Ok(n)
    }
}

/// Puts a new link to `target` at `path`, with the old link's times: a lookalike (another inode;
/// the same size when the two targets are as long, the same mtime).
fn swap_link(path: &Path, target: &str) {
    let old = std::fs::symlink_metadata(path).unwrap();
    let temp = path.with_file_name(format!(".swap-{}", crate::remote::random_hex()));
    std::os::unix::fs::symlink(target, &temp).unwrap();
    let at = libc::timespec {
        tv_sec: old.mtime() as _,
        tv_nsec: old.mtime_nsec() as _,
    };
    let c = std::ffi::CString::new(s(&temp)).unwrap();
    // SAFETY: a NUL-terminated path and two valid times.
    let r = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            [at, at].as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    assert_eq!(r, 0, "{}", std::io::Error::last_os_error());
    std::fs::rename(&temp, path).unwrap();
}

/// A rename that is refused for these sources (another filesystem): their move is a copy, a
/// check and a removal.
fn no_rename_from(froms: &[&Path]) -> RenameHook<'static> {
    let froms: Vec<String> = froms.iter().map(|p| s(p)).collect();
    Box::new(move |f, _| {
        froms
            .iter()
            .any(|x| x == f)
            .then(|| Err(anyhow::anyhow!(NoRename("another filesystem".into()))))
    })
}

/// The SHA-256 of each named file in `dir`.
fn payloads(dir: &Path, files: &[&str]) -> Vec<[u8; 32]> {
    files.iter().map(|f| sha(&dir.join(f))).collect()
}

/// Runs `test` on this machine's file layer, then through a local `sftp-server`.
fn both(test: impl Fn(Layer, &str)) {
    test(Box::new(LocalFs::new()), "local");
    if let Some(sftp) = SftpRemote::local_server() {
        test(Box::new(sftp), "sftp");
    }
}

/// A move that may not rename (another filesystem, 04 §5.2): `to` refuses with [`NoRename`].
fn no_rename_to(to: &Path) -> RenameHook<'static> {
    let to = s(to);
    Box::new(move |_, t| {
        (t == to).then(|| Err(anyhow::anyhow!(NoRename("another filesystem".into()))))
    })
}

/// Does the filesystem holding `dir` treat `a` and `b` as one name?
fn one_name(dir: &Path, a: &str, b: &str) -> bool {
    let probe = dir.join(a);
    std::fs::write(&probe, "").unwrap();
    let same = dir.join(b).exists();
    std::fs::remove_file(&probe).unwrap();
    same
}

#[test]
fn valid_names() {
    for bad in ["", "a/b", ".", "..", &"x".repeat(256)] {
        assert!(valid_name(bad).is_err(), "{bad:?}");
    }
    for good in ["a", ".hidden", "a b", "..x", &"x".repeat(255)] {
        assert!(valid_name(good).is_ok(), "{good:?}");
    }
}

/// K-C1: the copy has the same bytes, mode and mtime; the source is unchanged; no temp is left.
#[test]
fn k_c1_a_copied_file_keeps_bytes_mode_and_mtime() {
    both(|mut fs, how| {
        let (_t, src, dst) = scratch();
        let a = src.join("a.txt");
        std::fs::write(&a, b"hello copy").unwrap();
        set_mode(&a, 0o640);
        set_mtime(&a, 1_600_000_000);
        let before = (sha(&a), ino(&a), mtime_secs(&a));
        let mut ctl = Script::default();
        let report = run(
            &job(Op::Copy, &src, &["a.txt"], &dst),
            Hosts::Same(&mut *fs),
            &mut ctl,
        );
        assert_eq!(
            report.items,
            vec![item("a.txt", ItemOutcome::Done)],
            "{how}"
        );
        assert!(report.all_done(), "{how}: {report:?}");
        let b = dst.join("a.txt");
        assert_eq!(std::fs::read(&b).unwrap(), b"hello copy", "{how}");
        assert_eq!(mode(&b), 0o640, "{how}");
        assert_eq!(mtime_secs(&b), 1_600_000_000, "{how}");
        assert_eq!((sha(&a), ino(&a), mtime_secs(&a)), before, "{how}");
        assert_eq!(temps(&dst), Vec::<String>::new(), "{how}");
    });
}

/// K-C2: Skip leaves both, Rename makes `<stem>-1<ext>`, Cancel stops the rest; the existing
/// file's inode and content never change. "The same for the rest" answers every later clash.
#[test]
fn k_c2_a_name_in_the_way_is_never_overwritten() {
    both(|mut fs, how| {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join("a.txt"), "new a").unwrap();
        std::fs::write(src.join("b.txt"), "new b").unwrap();
        std::fs::write(dst.join("a.txt"), "old a").unwrap();
        let old = dst.join("a.txt");
        let kept = (ino(&old), sha(&old));
        let copy_a = job(Op::Copy, &src, &["a.txt"], &dst);

        let mut ctl = Script::answers([reply(Answer::Skip)]);
        let r = run(&copy_a, Hosts::Same(&mut *fs), &mut ctl);
        let asked = Clash {
            name: "a.txt".into(),
            free: "a-1.txt".into(),
        };
        assert_eq!(ctl.asked, vec![asked], "{how}");
        assert_eq!(r.items, vec![item("a.txt", ItemOutcome::Skipped)], "{how}");
        assert_eq!(names(&dst), ["a.txt"], "{how}");
        assert_eq!((ino(&old), sha(&old)), kept, "{how}");

        let mut ctl = Script::answers([reply(Answer::Rename("a-1.txt".into()))]);
        let r = run(&copy_a, Hosts::Same(&mut *fs), &mut ctl);
        let renamed = ItemOutcome::Renamed("a-1.txt".into());
        assert_eq!(r.items, vec![item("a.txt", renamed)], "{how}");
        assert_eq!(std::fs::read(dst.join("a-1.txt")).unwrap(), b"new a");
        assert_eq!((ino(&old), sha(&old)), kept, "{how}");

        // The next free name now skips the one just made; Cancel leaves the rest undone.
        let mut ctl = Script::answers([reply(Answer::Cancel)]);
        let both_items = job(Op::Copy, &src, &["a.txt", "b.txt"], &dst);
        let r = run(&both_items, Hosts::Same(&mut *fs), &mut ctl);
        assert_eq!(ctl.asked[0].free, "a-2.txt", "{how}");
        assert_eq!(
            r.items,
            vec![
                item(
                    "a.txt",
                    ItemOutcome::Failed("cancelled: nothing of it was copied".to_string())
                ),
                item("b.txt", ItemOutcome::NotReached)
            ],
            "{how}"
        );
        assert_eq!(names(&dst), ["a-1.txt", "a.txt"], "{how}");

        // Skip, the same for the rest: asked once for two names in the way.
        std::fs::write(dst.join("b.txt"), "old b").unwrap();
        let mut ctl = Script::answers([Reply {
            answer: Answer::Skip,
            same_for_rest: true,
        }]);
        let r = run(&both_items, Hosts::Same(&mut *fs), &mut ctl);
        assert_eq!(ctl.asked.len(), 1, "{how}");
        assert_eq!(
            r.items,
            vec![
                item("a.txt", ItemOutcome::Skipped),
                item("b.txt", ItemOutcome::Skipped)
            ],
            "{how}"
        );
        assert_eq!((ino(&old), sha(&old)), kept, "{how}");
        assert_eq!(std::fs::read(dst.join("b.txt")).unwrap(), b"old b");

        // A folder in the way: Rename gives the copy the free name; the folder there is
        // untouched.
        std::fs::create_dir(src.join("d")).unwrap();
        std::fs::write(src.join("d/x"), "x").unwrap();
        std::fs::create_dir(dst.join("d")).unwrap();
        let mut ctl = Script::answers([reply(Answer::Rename("d-1".into()))]);
        let r = run(
            &job(Op::Copy, &src, &["d"], &dst),
            Hosts::Same(&mut *fs),
            &mut ctl,
        );
        assert_eq!(ctl.asked[0].free, "d-1", "{how}");
        assert_eq!(r.items, vec![item("d", ItemOutcome::Renamed("d-1".into()))]);
        assert_eq!(names(&dst.join("d")), Vec::<String>::new(), "{how}");
        assert_eq!(names(&dst.join("d-1")), ["x"], "{how}");
        assert_eq!(temps(&dst), Vec::<String>::new(), "{how}");
    });
}

/// K-C3: a folder's files, subfolder and link (as a link, the same target) are copied; the fifo
/// is skipped and listed under its path.
#[test]
fn k_c3_a_folder_copies_files_subfolders_and_links_and_skips_a_fifo() {
    both(|mut fs, how| {
        let (_t, src, dst) = scratch();
        let d = src.join("d");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("f1"), "one").unwrap();
        std::fs::write(d.join("sub/f2"), "two").unwrap();
        std::os::unix::fs::symlink("f1", d.join("lnk")).unwrap();
        let fifo = std::ffi::CString::new(s(&d.join("fifo"))).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
        let mut ctl = Script::default();
        let r = run(
            &job(Op::Copy, &src, &["d"], &dst),
            Hosts::Same(&mut *fs),
            &mut ctl,
        );
        assert_eq!(
            r.items,
            vec![
                item("d", ItemOutcome::Done),
                item("d/fifo", ItemOutcome::Skipped)
            ],
            "{how}: {r:?}"
        );
        let c = dst.join("d");
        assert_eq!(names(&c), ["f1", "lnk", "sub"], "{how}");
        assert_eq!(std::fs::read(c.join("sub/f2")).unwrap(), b"two");
        let link = std::fs::symlink_metadata(c.join("lnk")).unwrap();
        assert!(link.file_type().is_symlink(), "{how}: copied as a link");
        assert_eq!(
            std::fs::read_link(c.join("lnk")).unwrap(),
            PathBuf::from("f1")
        );
        assert_eq!(temps(&dst), Vec::<String>::new(), "{how}");
    });
}

/// K-C3 through `sftp-server`: the link is made at the link's path with the target's text —
/// the library writes `SSH_FXP_SYMLINK` in OpenSSH's order, so sopen passes `(target, link)`.
#[test]
fn k_c3_sftp_symlink_puts_the_link_at_its_name_with_its_target() {
    let Some(mut fs) = SftpRemote::local_server() else {
        return;
    };
    let (_t, _src, dst) = scratch();
    let link = dst.join("l");
    fs.make_symlink("../some/target", &s(&link)).unwrap();
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        PathBuf::from("../some/target")
    );
    assert_eq!(fs.read_link(&s(&link)).unwrap(), "../some/target");
    let again = fs.make_symlink("x", &s(&link)).unwrap_err();
    assert!(crate::remote::ops::is_taken(&again), "{again:#}");
}

/// K-C4: a same-host remote copy runs `copy-data` in 8 MiB steps; Esc stops it between steps,
/// leaving no file under the target name and no temp.
#[test]
fn k_c4_a_remote_copy_moves_in_8_mib_steps_and_stops_between_them() {
    let Some(mut fs) = SftpRemote::local_server() else {
        return;
    };
    if !fs.offers_copy_data() {
        eprintln!("skipping: this sftp-server does not offer copy-data (OpenSSH before 9.0)");
        return;
    }
    let (_t, src, dst) = scratch();
    let size = 2 * COPY_STEP + 12_345;
    let body: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    std::fs::write(src.join("big"), &body).unwrap();

    // 04 §5.1's contract, as a literal: steps of 8 MiB.
    const MIB8: u64 = 8 * 1024 * 1024;
    assert_eq!(COPY_STEP, MIB8);
    let mut steps = Vec::new();
    let temp = dst.join(".big.sopen-test");
    let staged = fs
        .copy_within(
            &s(&src.join("big")),
            &s(&temp),
            &AtomicBool::new(false),
            &mut |d| steps.push(d),
        )
        .unwrap();
    assert_eq!(steps, vec![MIB8, 2 * MIB8, size]);
    assert_eq!(staged.size, size);
    assert_eq!(sha(&temp), sha(&src.join("big")));
    std::fs::remove_file(&temp).unwrap();

    let mut ctl = Script {
        stop_at: Some(COPY_STEP),
        ..Script::default()
    };
    let r = run(
        &job(Op::Copy, &src, &["big"], &dst),
        Hosts::Same(&mut fs),
        &mut ctl,
    );
    assert!(stop_reason(&r.items[0]).contains("nothing of it"), "{r:?}");
    assert_eq!(r.items.len(), 1);
    assert_eq!(names(&dst), Vec::<String>::new(), "no file, no temp");
    let dones: Vec<u64> = ctl.seen.iter().map(|p| p.done).collect();
    assert!(dones.contains(&COPY_STEP), "{dones:?}");
    assert!(
        !dones.contains(&size),
        "stopped before the last step: {dones:?}"
    );
    let first = &ctl.seen[0];
    assert_eq!(
        (first.verb, first.item, first.items, first.total),
        ("copying", 1, 1, size)
    );
}

/// K-C4 on this machine: Esc during a copy leaves no file under the target name and no temp.
#[test]
fn k_c4_a_local_copy_stopped_leaves_no_file_and_no_temp() {
    let (_t, src, dst) = scratch();
    let size = 3 * COPY_STEP;
    std::fs::write(src.join("big"), vec![7u8; size as usize]).unwrap();
    let mut ctl = Script {
        stop_at: Some(1),
        ..Script::default()
    };
    let r = run(
        &job(Op::Copy, &src, &["big"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut ctl,
    );
    assert!(stop_reason(&r.items[0]).contains("nothing of it"), "{r:?}");
    assert_eq!(names(&dst), Vec::<String>::new());
    assert_eq!(names(&src), ["big"]);
}

/// K-C4: without `hardlink@openssh.com` nothing can be published without replacing, so copy
/// and move are refused before anything happens, with the reason.
#[test]
fn k_c4_without_hardlink_copy_and_move_are_refused() {
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    for op in [Op::Copy, Op::Move] {
        let mut fs = Faulty::new(Box::new(LocalFs::new()));
        fs.no_link = true;
        let r = run(
            &job(op, &src, &["a"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        let why = refusal(&r);
        assert!(why.contains("hardlink@openssh.com"), "{why}");
        assert_eq!(r.items, vec![item("a", ItemOutcome::NotReached)]);
    }
    assert_eq!(names(&dst), Vec::<String>::new());
    assert_eq!(names(&src), ["a"]);
}

/// K-C5: between this machine and a remote, both ways, and between two remotes (two sessions):
/// the bytes arrive (SHA-256 equal), a folder with its link too.
#[test]
fn k_c5_copies_between_hosts_stream_through_this_machine() {
    let (Some(mut one), Some(mut two)) = (SftpRemote::local_server(), SftpRemote::local_server())
    else {
        return;
    };
    let (_t, src, dst) = scratch();
    let body: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(src.join("f.bin"), &body).unwrap();
    std::fs::create_dir(src.join("d")).unwrap();
    std::fs::write(src.join("d/x"), "x").unwrap();
    std::os::unix::fs::symlink("x", src.join("d/l")).unwrap();
    let mut local = LocalFs::new();
    let leg = |leg: &str, hosts: Hosts| {
        let to = dst.join(leg);
        std::fs::create_dir(&to).unwrap();
        let r = run(
            &job(Op::Copy, &src, &["f.bin", "d"], &to),
            hosts,
            &mut Script::default(),
        );
        assert!(r.all_done(), "{leg}: {r:?}");
        assert_eq!(sha(&to.join("f.bin")), sha(&src.join("f.bin")), "{leg}");
        assert_eq!(std::fs::read(to.join("d/x")).unwrap(), b"x", "{leg}");
        assert_eq!(
            std::fs::read_link(to.join("d/l")).unwrap(),
            PathBuf::from("x")
        );
        assert_eq!(temps(&to), Vec::<String>::new(), "{leg}");
    };
    let here_to_remote = Hosts::Two {
        src: &mut local,
        dst: &mut one,
    };
    leg("here-to-remote", here_to_remote);
    let remote_to_remote = Hosts::Two {
        src: &mut two,
        dst: &mut one,
    };
    leg("remote-to-remote", remote_to_remote);
    let remote_to_here = Hosts::Two {
        src: &mut two,
        dst: &mut local,
    };
    leg("remote-to-here", remote_to_here);
}

/// Zero bytes on every copy path: on one host (local; `copy-data`; streamed when the server
/// refuses `copy-data`) and between hosts.
#[test]
fn an_empty_file_copies_on_every_path() {
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("empty"), "").unwrap();
    std::fs::write(src.join("small"), "small but not empty").unwrap();
    let mut paths: Vec<(&str, Opener)> = vec![
        ("local", Box::new(|| Some(Box::new(LocalFs::new()) as _))),
        (
            "copy-data",
            Box::new(|| SftpRemote::local_server().map(|f| Box::new(f) as _)),
        ),
        (
            "streamed",
            Box::new(|| {
                SftpRemote::local_server_args(&["-P", "copy-data"], None).map(|f| Box::new(f) as _)
            }),
        ),
    ];
    for (how, open) in paths.iter_mut() {
        let Some(mut fs) = open() else { continue };
        let to = dst.join(*how);
        std::fs::create_dir(&to).unwrap();
        let r = run(
            &job(Op::Copy, &src, &["empty", "small"], &to),
            Hosts::Same(fs.as_mut()),
            &mut Script::default(),
        );
        assert!(r.all_done(), "{how}: {r:?}");
        assert_eq!(std::fs::read(to.join("empty")).unwrap(), b"", "{how}");
        assert_eq!(sha(&to.join("small")), sha(&src.join("small")), "{how}");
    }
    if let Some(mut remote) = SftpRemote::local_server() {
        let to = dst.join("between");
        std::fs::create_dir(&to).unwrap();
        let r = run(
            &job(Op::Copy, &src, &["empty"], &to),
            Hosts::Two {
                src: &mut LocalFs::new(),
                dst: &mut remote,
            },
            &mut Script::default(),
        );
        assert!(r.all_done(), "{r:?}");
        assert_eq!(std::fs::read(to.join("empty")).unwrap(), b"");
    }
}

/// K-M1 on this machine: a file and a folder are renamed in place (the inode is kept).
#[test]
fn k_m1_a_local_move_within_a_filesystem_keeps_the_inode() {
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    std::fs::create_dir(src.join("d")).unwrap();
    std::fs::write(src.join("d/x"), "x").unwrap();
    let inodes = (ino(&src.join("a")), ino(&src.join("d")));
    let r = run(
        &job(Op::Move, &src, &["a", "d"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!((ino(&dst.join("a")), ino(&dst.join("d"))), inodes);
    assert_eq!(names(&src), Vec::<String>::new());
    assert_eq!(names(&dst.join("d")), ["x"]);
}

/// The request log of a local `sftp-server` (`-e -l INFO`).
fn logged_server(log: &Path, args: &[&str]) -> Option<SftpRemote> {
    SftpRemote::local_server_args(args, Some(log))
}

/// K-M1 through `sftp-server`: a regular file moves by `hardlink@openssh.com` then `REMOVE` (the
/// inode is kept), a link by being made again and the old one removed; never a
/// `posix-rename@openssh.com` request.
#[test]
fn k_m1_a_remote_file_moves_by_hardlink_then_remove_never_posix_rename() {
    let (_t, src, dst) = scratch();
    let log = src.parent().unwrap().join("server.log");
    let Some(mut fs) = logged_server(&log, &[]) else {
        return;
    };
    std::fs::write(src.join("a"), "a").unwrap();
    std::os::unix::fs::symlink("a", src.join("l")).unwrap();
    let before = ino(&src.join("a"));
    let r = run(
        &job(Op::Move, &src, &["a", "l"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(ino(&dst.join("a")), before);
    assert_eq!(
        std::fs::read_link(dst.join("l")).unwrap(),
        PathBuf::from("a")
    );
    assert_eq!(names(&src), Vec::<String>::new());
    drop(fs);
    let text = std::fs::read_to_string(&log).unwrap();
    eprintln!("sftp-server request log:\n{text}");
    assert!(text.contains("hardlink old"), "{text}");
    assert!(text.contains("remove name"), "{text}");
    assert!(text.contains("symlink old"), "{text}");
    assert!(!text.contains("posix-rename"), "{text}");
}

/// K-M1 before S6: a remote folder moves by copy, check and remove — no rename request of
/// either kind reaches the server.
#[test]
fn k_m1_a_remote_folder_moves_by_copy_check_and_remove_before_s6() {
    let (_t, src, dst) = scratch();
    let log = src.parent().unwrap().join("server.log");
    let Some(mut fs) = logged_server(&log, &[]) else {
        return;
    };
    let d = src.join("d");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    for (name, body) in [("a", "1"), ("b", "22"), ("sub/c", "333")] {
        std::fs::write(d.join(name), body).unwrap();
    }
    let r = run(
        &job(Op::Move, &src, &["d"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(names(&dst.join("d")), ["a", "b", "sub"]);
    assert_eq!(std::fs::read(dst.join("d/sub/c")).unwrap(), b"333");
    assert_eq!(names(&src), Vec::<String>::new());
    drop(fs);
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("hardlink old"), "{text}");
    assert!(text.contains("rmdir name"), "{text}");
    assert!(!text.contains("rename old"), "{text}");
    assert!(!text.contains("posix-rename"), "{text}");
}

/// The three-file folder of K-M1 / K-M2.
fn three_files(src: &Path) -> PathBuf {
    let d = src.join("d");
    std::fs::create_dir(&d).unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(d.join(name), name).unwrap();
    }
    d
}

/// K-M1 / K-M2: a folder move by copy stopped after its first file was published — the target
/// folder holds that file and no temp, and no source file was removed. Locally through a
/// filesystem boundary (the rename refused), remotely as every remote folder before S6.
#[test]
fn k_m2_a_folder_move_stopped_after_one_file_removes_no_source() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        let d = three_files(&src);
        let files = ["a.txt", "b.txt", "c.txt"];
        let before = payloads(&d, &files);
        let mut ctl = Script::default();
        let stop = ctl.cancel.clone();
        let into = format!("{}/", s(&dst.join("d")));
        let mut fs = Faulty::new(inner);
        fs.rename = no_rename_to(&dst.join("d"));
        fs.renamed = Box::new(move |_, to| {
            if to.starts_with(&into) {
                stop.store(true, Ordering::Relaxed);
            }
        });
        let r = run(
            &job(Op::Move, &src, &["d"], &dst),
            Hosts::Same(&mut fs),
            &mut ctl,
        );
        assert_eq!(r.items.len(), 1, "{how}: {r:?}");
        let why = stop_reason(&r.items[0]);
        assert!(why.contains("no source was removed"), "{how}: {why}");
        assert!(
            r.notes.iter().any(|n| n.contains("partial")),
            "{how}: {r:?}"
        );
        assert_eq!(names(&dst.join("d")), ["a.txt"], "{how}");
        assert_eq!(
            sha(&dst.join("d/a.txt")),
            before[0],
            "{how}: the committed file"
        );
        assert_eq!(temps(&dst), Vec::<String>::new(), "{how}");
        assert_eq!(names(&d), files, "{how}");
        assert_eq!(
            payloads(&d, &files),
            before,
            "{how}: every source as it was"
        );
    });
}

/// K-M2: the same with a failure on the second file: that item fails with the reason, the
/// target holds the first file, no temp, no source removed.
#[test]
fn k_m2_a_folder_move_failing_on_its_second_file_removes_no_source() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        let d = three_files(&src);
        let files = ["a.txt", "b.txt", "c.txt"];
        let before = payloads(&d, &files);
        let mut fs = Faulty::new(inner);
        fs.rename = no_rename_to(&dst.join("d"));
        fs.copy = Box::new(|from| {
            from.ends_with("b.txt")
                .then(|| anyhow::anyhow!("injected read error"))
        });
        let r = run(
            &job(Op::Move, &src, &["d"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        let [Item {
            outcome: ItemOutcome::Failed(why),
            ..
        }] = &r.items[..]
        else {
            panic!("{how}: {r:?}")
        };
        assert!(why.contains("injected read error"), "{how}: {why}");
        assert!(
            r.notes.iter().any(|n| n.contains("partial")),
            "{how}: the report says the target is partial: {r:?}"
        );
        assert_eq!(names(&dst.join("d")), ["a.txt"], "{how}");
        assert_eq!(
            sha(&dst.join("d/a.txt")),
            before[0],
            "{how}: the committed file"
        );
        assert_eq!(temps(&dst), Vec::<String>::new(), "{how}");
        assert_eq!(names(&d), files, "{how}");
        assert_eq!(
            payloads(&d, &files),
            before,
            "{how}: every source as it was"
        );
    });
}

/// K-M2: a source rewritten between its copy and its removal is kept, and the report says both
/// copies exist — a single file, and a file inside a folder (the rest of the folder moves). The
/// copies are streamed (another host): a host-made copy's read-back catches the rewrite earlier
/// (r14).
#[test]
fn k_m2_a_source_rewritten_before_its_removal_is_kept() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join("f"), "first").unwrap();
        three_files(&src);
        let rewrite = |path: PathBuf| {
            move |_: &str, to: &str| {
                if to.ends_with(&format!("/{}", path.file_name().unwrap().to_str().unwrap())) {
                    std::fs::write(&path, "rewritten, longer").unwrap();
                }
            }
        };
        let mut fs = Faulty::new(inner);
        let on_f = rewrite(src.join("f"));
        let on_b = rewrite(src.join("d/b.txt"));
        fs.renamed = Box::new(move |from, to| {
            on_f(from, to);
            on_b(from, to);
        });
        let r = run(
            &job(Op::Move, &src, &["f", "d"], &dst),
            Hosts::Two {
                src: &mut LocalFs::new(),
                dst: &mut fs,
            },
            &mut Script::default(),
        );
        let kept =
            |i: &Item| matches!(&i.outcome, ItemOutcome::Failed(why) if why.contains("both"));
        assert!(kept(&r.items[0]) && kept(&r.items[1]), "{how}: {r:?}");
        assert_eq!(std::fs::read(src.join("f")).unwrap(), b"rewritten, longer");
        assert_eq!(std::fs::read(dst.join("f")).unwrap(), b"first", "{how}");
        assert_eq!(names(&src.join("d")), ["b.txt"], "{how}");
        assert_eq!(names(&dst.join("d")), ["a.txt", "b.txt", "c.txt"], "{how}");
        assert_eq!(std::fs::read(dst.join("d/b.txt")).unwrap(), b"b.txt");
    });
}

/// K-M2: a move to another host is a copy, a check and a removal; a source that cannot be
/// removed stays, and the report says both copies exist.
#[test]
fn k_m2_a_move_to_another_host_copies_checks_and_removes() {
    let Some(mut remote) = SftpRemote::local_server() else {
        return;
    };
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    three_files(&src);
    let r = run(
        &job(Op::Move, &src, &["a", "d"], &dst),
        Hosts::Two {
            src: &mut LocalFs::new(),
            dst: &mut remote,
        },
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(names(&src), Vec::<String>::new());
    assert_eq!(names(&dst.join("d")), ["a.txt", "b.txt", "c.txt"]);

    // The source made unremovable: both copies exist, and the report says so.
    std::fs::write(src.join("b"), "b").unwrap();
    set_mode(&src, 0o555);
    let r = run(
        &job(Op::Move, &src, &["b"], &dst),
        Hosts::Two {
            src: &mut LocalFs::new(),
            dst: &mut remote,
        },
        &mut Script::default(),
    );
    set_mode(&src, 0o755);
    let [Item {
        outcome: ItemOutcome::Failed(why),
        ..
    }] = &r.items[..]
    else {
        panic!("{r:?}")
    };
    assert!(why.contains("both copies"), "{why}");
    assert_eq!(std::fs::read(src.join("b")).unwrap(), b"b");
    assert_eq!(std::fs::read(dst.join("b")).unwrap(), b"b");
}

/// K-N1 on this machine: a name that exists is an error and nothing changes; a free name
/// renames; an invalid one is refused.
#[test]
fn k_n1_a_rename_never_takes_an_existing_name() {
    let (_t, src, _dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    std::fs::write(src.join("b"), "b").unwrap();
    let rename = |to: &str| job(Op::Rename { to: to.into() }, &src, &["a"], &src);
    let mut fs = LocalFs::new();
    let r = run(&rename("b"), Hosts::Same(&mut fs), &mut Script::default());
    let [Item {
        outcome: ItemOutcome::Failed(why),
        ..
    }] = &r.items[..]
    else {
        panic!("{r:?}")
    };
    assert!(why.contains("exists"), "{why}");
    assert_eq!(std::fs::read(src.join("b")).unwrap(), b"b");
    assert_eq!(names(&src), ["a", "b"]);

    let r = run(&rename("c"), Hosts::Same(&mut fs), &mut Script::default());
    assert!(r.all_done(), "{r:?}");
    assert_eq!(names(&src), ["b", "c"]);

    let bad = job(Op::Rename { to: "x/y".into() }, &src, &["c"], &src);
    let r = run(&bad, Hosts::Same(&mut fs), &mut Script::default());
    refusal(&r);
    assert_eq!(names(&src), ["b", "c"]);
}

/// K-N1 on a case-insensitive filesystem (macOS by default): `readme` → `README` renames; and
/// where the filesystem answers "exists" for the source's other spelling (as through `link`),
/// the two steps through a free temp name run — with the second step failing, the file is back
/// under `readme`, or the report names the temp it has.
#[test]
fn k_n1_a_case_only_rename_on_a_case_insensitive_filesystem() {
    let (_t, src, _dst) = scratch();
    if !one_name(&src, "probe", "PROBE") {
        eprintln!("skipping: {} is case-sensitive", src.display());
        return;
    }
    let readme = src.join("readme");
    std::fs::write(&readme, "r").unwrap();
    let to_upper = job(
        Op::Rename {
            to: "README".into(),
        },
        &src,
        &["readme"],
        &src,
    );
    let r = run(
        &to_upper,
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(names(&src), ["README"]);
    std::fs::rename(src.join("README"), &readme).unwrap();

    // "Exists" for the other spelling, then the second step fails.
    let readme_s = s(&readme);
    let calls = Arc::new(AtomicUsize::new(0));
    let fail_back = Arc::new(AtomicBool::new(false));
    let run_with_faults = |back_fails: bool| {
        calls.store(0, Ordering::Relaxed);
        fail_back.store(back_fails, Ordering::Relaxed);
        let mut fs = Faulty::new(Box::new(LocalFs::new()));
        let (calls, fail_back, readme) = (calls.clone(), fail_back.clone(), readme_s.clone());
        fs.rename = Box::new(
            move |from, to| match calls.fetch_add(1, Ordering::Relaxed) {
                0 => Some(Err(taken(to))),
                1 => None,
                2 => Some(Err(anyhow::anyhow!("injected: second step"))),
                _ if fail_back.load(Ordering::Relaxed) && to == readme => {
                    let _ = from;
                    Some(Err(anyhow::anyhow!("injected: back")))
                }
                _ => None,
            },
        );
        run(&to_upper, Hosts::Same(&mut fs), &mut Script::default())
    };
    let r = run_with_faults(false);
    let [Item {
        outcome: ItemOutcome::Failed(why),
        ..
    }] = &r.items[..]
    else {
        panic!("{r:?}")
    };
    assert!(why.contains("back under readme"), "{why}");
    assert_eq!(names(&src), ["readme"]);

    let r = run_with_faults(true);
    let [Item {
        outcome: ItemOutcome::Failed(why),
        ..
    }] = &r.items[..]
    else {
        panic!("{r:?}")
    };
    let now = names(&src);
    assert_eq!(now.len(), 1);
    assert!(now[0].contains(".sopen-renaming-"), "{now:?}");
    assert!(why.contains(&s(&src.join(&now[0]))), "{why}");
}

/// K-N1 through `sftp-server`: a rename is `hardlink@` + `REMOVE`, never posix-rename; a name
/// that exists is an error; a case-only rename where the listing shows only the source's
/// spelling goes through a temp name, and one where the listing also holds the new spelling is
/// an error; a remote folder is refused before S6 with the reason, nothing changed.
#[test]
fn k_n1_a_remote_rename() {
    let (_t, src, _dst) = scratch();
    let log = src.parent().unwrap().join("server.log");
    let Some(mut sftp) = logged_server(&log, &[]) else {
        return;
    };
    std::fs::write(src.join("a"), "a").unwrap();
    std::fs::write(src.join("b"), "b").unwrap();
    std::fs::create_dir(src.join("d")).unwrap();
    let rename = |from: &str, to: &str| job(Op::Rename { to: to.into() }, &src, &[from], &src);
    let failed = |r: &Report| match &r.items[..] {
        [Item {
            outcome: ItemOutcome::Failed(why),
            ..
        }] => why.clone(),
        _ => panic!("{r:?}"),
    };

    let r = run(
        &rename("a", "b"),
        Hosts::Same(&mut sftp),
        &mut Script::default(),
    );
    assert!(failed(&r).contains("exists"), "{r:?}");
    assert_eq!(std::fs::read(src.join("b")).unwrap(), b"b");

    let r = run(
        &rename("a", "c"),
        Hosts::Same(&mut sftp),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(names(&src), ["b", "c", "d"]);

    let r = run(
        &rename("d", "e"),
        Hosts::Same(&mut sftp),
        &mut Script::default(),
    );
    assert_eq!(failed(&r), crate::remote::ops::REMOTE_FOLDER_RENAME);
    assert_eq!(names(&src), ["b", "c", "d"]);

    if one_name(&src, "probe", "PROBE") {
        std::fs::write(src.join("readme"), "r").unwrap();
        let r = run(
            &rename("readme", "README"),
            Hosts::Same(&mut sftp),
            &mut Script::default(),
        );
        assert!(r.all_done(), "{r:?}");
        assert!(names(&src).contains(&"README".to_string()), "{r:?}");
        std::fs::rename(src.join("README"), src.join("readme")).unwrap();
        // A listing that also holds the new spelling: another entry, so an error.
        let mut fs = Faulty::new(Box::new(sftp));
        fs.list = Box::new(|_, names| names.push("README".into()));
        let r = run(
            &rename("readme", "README"),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        assert!(failed(&r).contains("exists"), "{r:?}");
        assert!(names(&src).contains(&"readme".to_string()));
        // The server ends with its session: its log is complete.
        drop(fs);
    } else {
        // A case-sensitive server: README beside readme is another entry.
        std::fs::write(src.join("readme"), "r").unwrap();
        std::fs::write(src.join("README"), "R").unwrap();
        let r = run(
            &rename("readme", "README"),
            Hosts::Same(&mut sftp),
            &mut Script::default(),
        );
        assert!(failed(&r).contains("exists"), "{r:?}");
        assert_eq!(std::fs::read(src.join("README")).unwrap(), b"R");
        drop(sftp);
    }
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("hardlink old"), "{text}");
    assert!(text.contains("remove name"), "{text}");
    assert!(!text.contains("posix-rename"), "{text}");
    assert!(!text.contains("rename old"), "{text}");
}

/// K-E1: a folder copied or moved into itself or below itself — directly, through a link to
/// it, or (locally) where only the device and inode tell — is refused before anything happens.
#[test]
fn k_e1_a_folder_into_itself_is_refused() {
    both(|mut inner, how| {
        let (_t, src, dst) = scratch();
        let d = src.join("d");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let alias = dst.join("alias");
        std::os::unix::fs::symlink(&d, &alias).unwrap();
        for (op, to) in [
            (Op::Copy, d.join("sub")),
            (Op::Copy, d.clone()),
            (Op::Move, d.join("sub")),
            (Op::Copy, alias.join("sub")),
            (Op::Move, alias.clone()),
        ] {
            let r = run(
                &job(op.clone(), &src, &["d"], &to),
                Hosts::Same(&mut *inner),
                &mut Script::default(),
            );
            let why = refusal(&r);
            assert!(
                why.contains("into itself"),
                "{how} {op:?} {}: {why}",
                to.display()
            );
            assert_eq!(names(&d), ["sub"], "{how}");
            assert_eq!(names(&d.join("sub")), Vec::<String>::new(), "{how}");
        }
    });
    // A realpath that resolves nothing: the device and inode of the target's ancestors still
    // find the folder.
    let (_t, src, dst) = scratch();
    let d = src.join("d");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::os::unix::fs::symlink(&d, dst.join("alias")).unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.plain_realpath = true;
    let r = run(
        &job(Op::Copy, &src, &["d"], &dst.join("alias/sub")),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(refusal(&r).contains("into itself"), "{r:?}");
}

/// K-E1: two names equal only after Unicode normalisation, on a normalising filesystem: the
/// second is "exists" (the filesystem decides); on one that keeps both, it is a new entry.
#[test]
fn k_e1_unicode_equivalent_names_are_decided_by_the_filesystem() {
    let (nfc, nfd) = ("\u{e9}.txt", "e\u{301}.txt");
    both(|mut fs, how| {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join(nfc), "nfc").unwrap();
        std::fs::write(dst.join(nfd), "nfd").unwrap();
        let normalising = dst.join(nfc).exists();
        let mut ctl = Script::answers([reply(Answer::Skip)]);
        let r = run(
            &job(Op::Copy, &src, &[nfc], &dst),
            Hosts::Same(&mut *fs),
            &mut ctl,
        );
        if normalising {
            assert_eq!(ctl.asked.len(), 1, "{how}");
            assert_eq!(r.items, vec![item(nfc, ItemOutcome::Skipped)], "{how}");
            assert_eq!(std::fs::read(dst.join(nfd)).unwrap(), b"nfd", "{how}");
            assert_eq!(names(&dst).len(), 1, "{how}");
        } else {
            assert!(ctl.asked.is_empty(), "{how}");
            assert!(r.all_done(), "{how}: {r:?}");
            assert_eq!(names(&dst).len(), 2, "{how}");
        }
    });
}

/// K-E1: a read-only source folder is copied and its bits are set on the copy after its
/// contents; a read-only target folder fails the item with the server's reason.
#[test]
fn k_e1_read_only_folders() {
    both(|mut fs, how| {
        let (_t, src, dst) = scratch();
        let ro = src.join("ro");
        std::fs::create_dir(&ro).unwrap();
        std::fs::write(ro.join("f"), "f").unwrap();
        set_mode(&ro, 0o555);
        let r = run(
            &job(Op::Copy, &src, &["ro"], &dst),
            Hosts::Same(&mut *fs),
            &mut Script::default(),
        );
        let copied = (mode(&dst.join("ro")), names(&dst.join("ro")));
        set_mode(&ro, 0o755);
        set_mode(&dst.join("ro"), 0o755);
        assert!(r.all_done(), "{how}: {r:?}");
        assert_eq!(copied, (0o555, vec!["f".to_string()]), "{how}");

        std::fs::write(src.join("a"), "a").unwrap();
        let shut = dst.join("shut");
        std::fs::create_dir(&shut).unwrap();
        set_mode(&shut, 0o555);
        let r = run(
            &job(Op::Copy, &src, &["a"], &shut),
            Hosts::Same(&mut *fs),
            &mut Script::default(),
        );
        set_mode(&shut, 0o755);
        let [Item {
            outcome: ItemOutcome::Failed(why),
            ..
        }] = &r.items[..]
        else {
            panic!("{how}: {r:?}")
        };
        assert!(why.to_lowercase().contains("permission"), "{how}: {why}");
        assert_eq!(names(&shut), Vec::<String>::new(), "{how}");
    });
}

/// K-E1: a server refusing to set times (and modes) still copies; each refusal is a note.
#[test]
fn k_e1_a_server_refusing_to_set_times_still_copies_with_a_note() {
    let Some(mut fs) = SftpRemote::local_server_args(&["-P", "setstat,fsetstat"], None) else {
        return;
    };
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    std::fs::create_dir(src.join("d")).unwrap();
    let r = run(
        &job(Op::Copy, &src, &["a", "d"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert_eq!(
        r.items,
        vec![item("a", ItemOutcome::Done), item("d", ItemOutcome::Done)]
    );
    assert!(r.notes.iter().any(|n| n.contains("times")), "{r:?}");
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"a");
    assert!(dst.join("d").is_dir());
}

/// A folder copied into its own parent, renamed at the clash, holds the folder's contents once —
/// the walk is the folder as it was, so the new copy is never copied into itself.
#[test]
fn a_folder_copied_beside_itself_is_copied_once() {
    let (_t, src, _dst) = scratch();
    let d = src.join("d");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("sub/x"), "x").unwrap();
    let mut ctl = Script::answers([reply(Answer::Rename("d-1".into()))]);
    let r = run(
        &job(Op::Copy, &src, &["d"], &src),
        Hosts::Same(&mut LocalFs::new()),
        &mut ctl,
    );
    assert_eq!(r.items, vec![item("d", ItemOutcome::Renamed("d-1".into()))]);
    assert_eq!(names(&src.join("d-1")), ["sub"]);
    assert_eq!(names(&src.join("d-1/sub")), ["x"]);
}

/// The worker: a clash comes as an event and waits for its answer; Esc answers a pending
/// question with Cancel; the report comes last.
#[test]
fn a_spawned_operation_asks_over_its_channel_and_reports() {
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "new").unwrap();
    std::fs::write(dst.join("a"), "old").unwrap();
    let wakes = Arc::new(Mutex::new(0usize));
    let start = |wakes: Arc<Mutex<usize>>| {
        spawn(
            job(Op::Copy, &src, &["a"], &dst),
            Box::new(|| Ok(Conns::Same(Box::new(LocalFs::new())))),
            Box::new(move || *wakes.lock().unwrap() += 1),
        )
    };
    let next = |running: &Running| {
        running
            .event_within(std::time::Duration::from_secs(10))
            .expect("an event")
    };
    let mut running = start(wakes.clone());
    let clash = loop {
        match next(&running) {
            Event::Clash(c) => break c,
            Event::Progress(_) => continue,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(clash.free, "a-1");
    running.answer(reply(Answer::Rename(clash.free.clone())));
    let report = loop {
        if let Event::Done(report) = next(&running) {
            break report;
        }
    };
    running.join();
    assert_eq!(
        report.items,
        vec![item("a", ItemOutcome::Renamed("a-1".into()))]
    );
    assert_eq!(std::fs::read(dst.join("a-1")).unwrap(), b"new");
    assert!(*wakes.lock().unwrap() >= 2);

    let mut running = start(wakes);
    while !matches!(next(&running), Event::Clash(_)) {}
    running.stop();
    let Event::Done(report) = next(&running) else {
        panic!("the report")
    };
    running.join();
    assert!(running.finished());
    assert_eq!(
        report.items,
        vec![item(
            "a",
            ItemOutcome::Failed("cancelled: nothing of it was copied".to_string())
        )]
    );
    assert_eq!(names(&dst), ["a", "a-1"]);
}

/// A move's check reads both files when the host copied the bytes itself (`copy-data` on a
/// server, `copy_file_range` here — sopen never saw them): a copy holding other bytes of the
/// same size fails the item and its source stays.
#[test]
fn a_move_checks_the_bytes_of_a_copy_the_host_made() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        let f = src.join("f");
        std::fs::write(&f, "the right bytes").unwrap();
        let kept = (ino(&f), sha(&f));
        let mut fs = Faulty::new(inner);
        fs.host_made = true;
        let from = s(&f);
        fs.rename = Box::new(move |fr, _| {
            (fr == from).then(|| Err(anyhow::anyhow!(NoRename("elsewhere".into()))))
        });
        fs.copied = Box::new(|temp| std::fs::write(temp, "the WRONG bytes").unwrap());
        let r = run(
            &job(Op::Move, &src, &["f"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        let [Item {
            outcome: ItemOutcome::Failed(why),
            ..
        }] = &r.items[..]
        else {
            panic!("{how}: {r:?}")
        };
        assert!(why.contains("does not hold the same bytes"), "{how}: {why}");
        assert_eq!((ino(&f), sha(&f)), kept, "{how}: the source stays");
        assert_eq!(names(&dst), Vec::<String>::new(), "{how}: no copy, no temp");
    });
}

/// A name that appears between the look and the publish is never overwritten: the filesystem's
/// "exists" asks the dialog; Skip leaves the newcomer as it is and no temp behind, Rename puts
/// the copy beside it.
#[test]
fn a_name_taken_at_the_publish_asks_and_never_overwrites() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join("a"), "mine").unwrap();
        let racer = dst.join("a");
        let made: Arc<Mutex<Option<Ident>>> = Arc::default();
        let mut fs = Faulty::new(inner);
        let (target, at, seen) = (s(&racer), racer.clone(), made.clone());
        fs.rename = Box::new(move |from, to| {
            if from.contains(".sopen-") && to == target && !at.exists() {
                std::fs::write(&at, "theirs").unwrap();
                *seen.lock().unwrap() = Some((ino(&at), sha(&at)));
            }
            None
        });
        let newcomer = || made.lock().unwrap().expect("the racer was made");
        let mut ctl = Script::answers([reply(Answer::Skip)]);
        let r = run(
            &job(Op::Copy, &src, &["a"], &dst),
            Hosts::Same(&mut fs),
            &mut ctl,
        );
        assert_eq!(ctl.asked.len(), 1, "{how}");
        assert_eq!(r.items, vec![item("a", ItemOutcome::Skipped)], "{how}");
        assert_eq!((ino(&racer), sha(&racer)), newcomer(), "{how}: untouched");
        assert_eq!(names(&dst), ["a"], "{how}: no temp left");

        std::fs::remove_file(&racer).unwrap();
        *made.lock().unwrap() = None;
        let mut ctl = Script::answers([reply(Answer::Rename("a-1".into()))]);
        let r = run(
            &job(Op::Copy, &src, &["a"], &dst),
            Hosts::Same(&mut fs),
            &mut ctl,
        );
        let renamed = ItemOutcome::Renamed("a-1".into());
        assert_eq!(r.items, vec![item("a", renamed)], "{how}");
        assert_eq!(std::fs::read(dst.join("a-1")).unwrap(), b"mine", "{how}");
        assert_eq!((ino(&racer), sha(&racer)), newcomer(), "{how}: untouched");
        assert_eq!(names(&dst), ["a", "a-1"], "{how}");
    });
}

/// A move: a name in the way asks (Skip leaves both, Rename moves it beside); a name that
/// appears between the look and the rename is the filesystem's "exists" — never replaced.
#[test]
fn a_move_never_replaces_a_name_in_the_way() {
    both(|mut inner, how| {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join("a"), "mine").unwrap();
        std::fs::write(dst.join("a"), "theirs").unwrap();
        let theirs = (ino(&dst.join("a")), sha(&dst.join("a")));
        let mine = ino(&src.join("a"));
        let move_a = job(Op::Move, &src, &["a"], &dst);
        let mut ctl = Script::answers([reply(Answer::Skip)]);
        let r = run(&move_a, Hosts::Same(&mut *inner), &mut ctl);
        assert_eq!(r.items, vec![item("a", ItemOutcome::Skipped)], "{how}");
        assert_eq!(names(&src), ["a"], "{how}");
        assert_eq!((ino(&dst.join("a")), sha(&dst.join("a"))), theirs, "{how}");

        let mut ctl = Script::answers([reply(Answer::Rename("a-1".into()))]);
        let r = run(&move_a, Hosts::Same(&mut *inner), &mut ctl);
        let renamed = ItemOutcome::Renamed("a-1".into());
        assert_eq!(r.items, vec![item("a", renamed)], "{how}");
        assert_eq!(ino(&dst.join("a-1")), mine, "{how}: renamed, not copied");
        assert_eq!((ino(&dst.join("a")), sha(&dst.join("a"))), theirs, "{how}");
        assert_eq!(names(&src), Vec::<String>::new(), "{how}");

        // The racer: `b` appears at the target right before the rename reaches the disk.
        std::fs::write(src.join("b"), "mine").unwrap();
        let racer = dst.join("b");
        let mut fs = Faulty::new(inner);
        let (from, at) = (s(&src.join("b")), racer.clone());
        fs.rename = Box::new(move |f, _| {
            if f == from && !at.exists() {
                std::fs::write(&at, "theirs").unwrap();
            }
            None
        });
        let mut ctl = Script::answers([reply(Answer::Skip)]);
        let r = run(
            &job(Op::Move, &src, &["b"], &dst),
            Hosts::Same(&mut fs),
            &mut ctl,
        );
        assert_eq!(ctl.asked.len(), 1, "{how}");
        assert_eq!(r.items, vec![item("b", ItemOutcome::Skipped)], "{how}");
        assert_eq!(std::fs::read(&racer).unwrap(), b"theirs", "{how}");
        assert_eq!(std::fs::read(src.join("b")).unwrap(), b"mine", "{how}");
    });
}

/// On this machine a moved source is checked again by its inode too: a file put in its place
/// with the same size and mtime after the copy is kept, both copies listed.
#[test]
fn a_local_source_replaced_by_a_lookalike_is_kept() {
    let (_t, src, dst) = scratch();
    let f = src.join("f");
    std::fs::write(&f, "same").unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    let (path, twin) = (f.clone(), src.join("twin"));
    fs.renamed = Box::new(move |_, _| {
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&twin, "SAME").unwrap();
        let t = std::fs::File::options().write(true).open(&twin).unwrap();
        t.set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
        std::fs::rename(&twin, &path).unwrap();
    });
    // Streamed (two connections), so the removal's check is what meets the lookalike.
    let r = run(
        &job(Op::Move, &src, &["f"], &dst),
        Hosts::Two {
            src: &mut LocalFs::new(),
            dst: &mut fs,
        },
        &mut Script::default(),
    );
    let [Item {
        outcome: ItemOutcome::Failed(why),
        ..
    }] = &r.items[..]
    else {
        panic!("{r:?}")
    };
    assert!(why.contains("another file"), "{why}");
    assert_eq!(std::fs::read(&f).unwrap(), b"SAME");
    assert_eq!(std::fs::read(dst.join("f")).unwrap(), b"same");
}

/// A moved source that is gone once its copy is published: the read-back cannot confirm a
/// move of it — no verified move, the copy stays (never "both copies exist").
#[test]
fn a_moved_source_gone_before_its_check_is_no_verified_move() {
    let (_t, src, dst) = scratch();
    let f = src.join("f");
    std::fs::write(&f, "x").unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    let path = f.clone();
    fs.renamed = Box::new(move |_, _| std::fs::remove_file(&path).unwrap());
    // Streamed: a host-made copy is read back first, and a source gone then is no verified move
    // (r14).
    let r = run(
        &job(Op::Move, &src, &["f"], &dst),
        Hosts::Two {
            src: &mut LocalFs::new(),
            dst: &mut fs,
        },
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("no verified move") && !why.contains("both copies")),
        "{r:?}"
    );
    assert_eq!(
        std::fs::read(dst.join("f")).unwrap(),
        b"x",
        "the copy stays"
    );
}

/// K-C4 on a real server: `sftp-server -P hardlink` stops advertising `hardlink@openssh.com`
/// (the "stand-in that hides it"), and without the session's plain-rename channel copy and move
/// there are refused before anything happens, with the reason (with it: the probe, below).
#[test]
fn k_c4_a_server_hiding_hardlink_refuses_copy_and_move() {
    let Some(mut fs) = SftpRemote::local_server_args(&["-P", "hardlink"], None) else {
        return;
    };
    assert!(!fs.can_link(), "-P hides the extension");
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    for op in [Op::Copy, Op::Move] {
        let r = run(
            &job(op, &src, &["a"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        assert!(refusal(&r).contains("hardlink@openssh.com"), "{r:?}");
    }
    assert_eq!(names(&src), ["a"]);
    assert_eq!(names(&dst), Vec::<String>::new());
}

/// K-M1's permission reason through a real server: the link refused with a permission error
/// (here a read-only target folder; on Linux also `fs.protected_hardlinks=1` for a file the
/// account does not own) fails the move with the server's reason and its likely cause, and the
/// file stays where it was.
#[test]
fn k_m1_a_remote_link_refused_for_permission_leaves_the_file_with_the_reason() {
    let Some(mut fs) = SftpRemote::local_server() else {
        return;
    };
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    set_mode(&dst, 0o555);
    let r = run(
        &job(Op::Move, &src, &["a"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    set_mode(&dst, 0o755);
    let [Item {
        outcome: ItemOutcome::Failed(why),
        ..
    }] = &r.items[..]
    else {
        panic!("{r:?}")
    };
    assert!(why.to_lowercase().contains("permission"), "{why}");
    assert!(why.contains("protected_hardlinks"), "{why}");
    assert_eq!(names(&src), ["a"]);
    assert_eq!(names(&dst), Vec::<String>::new());
}

/// `sftp-server -P copy-data` stops advertising `copy-data`: the "streamed" leg of
/// [`an_empty_file_copies_on_every_path`] is the copy through sopen of a server without it.
#[test]
fn a_server_refusing_copy_data_does_not_advertise_it() {
    let Some(fs) = SftpRemote::local_server_args(&["-P", "copy-data"], None) else {
        return;
    };
    assert!(!fs.offers_copy_data());
}

/// A worker that panics still ends with a report, `End::Broken`, so the dialog never waits.
#[test]
fn a_panicking_worker_still_reports() {
    let (_t, src, dst) = scratch();
    let mut running = spawn(
        job(Op::Copy, &src, &["a"], &dst),
        Box::new(|| panic!("injected")),
        Box::new(|| {}),
    );
    let report = loop {
        match running.event_within(std::time::Duration::from_secs(10)) {
            Some(Event::Done(report)) => break report,
            Some(_) => continue,
            None => panic!("no report"),
        }
    };
    running.join();
    assert!(refusal(&report).contains("injected"), "{report:?}");
}

// ---- review round 1: each race or fault injected at its exact step ----

/// Replaces the entry at `from` (a file or a link) by another of a different size: what a
/// second writer does between a rename's new name and its removal of the old one.
fn replace_entry(from: &str) {
    let p = Path::new(from);
    let link = std::fs::symlink_metadata(p)
        .unwrap()
        .file_type()
        .is_symlink();
    std::fs::remove_file(p).unwrap();
    if link {
        std::os::unix::fs::symlink("another, longer target", p).unwrap();
    } else {
        std::fs::write(p, "a replacement, longer than the original").unwrap();
    }
}

/// The entries `a` (a file) and `l` (a link) a rename race test moves.
fn file_and_link(src: &Path) -> u64 {
    std::fs::write(src.join("a"), "original").unwrap();
    std::os::unix::fs::symlink("target", src.join("l")).unwrap();
    ino(&src.join("a"))
}

/// After a move whose sources were replaced right before their removal: the originals moved,
/// the replacements stay under the old names, and the report names both paths.
fn replacements_survived(r: &Report, src: &Path, dst: &Path, original: u64, how: &str) {
    assert_eq!(r.items.len(), 2, "{how}: {r:?}");
    for i in &r.items {
        assert!(
            matches!(&i.outcome, ItemOutcome::Failed(why) if why.contains("old name remains")),
            "{how}: {r:?}"
        );
    }
    assert_eq!(ino(&dst.join("a")), original, "{how}: the original moved");
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"original", "{how}");
    assert_eq!(
        std::fs::read_link(dst.join("l")).unwrap(),
        PathBuf::from("target")
    );
    assert_eq!(
        std::fs::read(src.join("a")).unwrap(),
        b"a replacement, longer than the original",
        "{how}: the replacement survived"
    );
    assert_eq!(
        std::fs::read_link(src.join("l")).unwrap(),
        PathBuf::from("another, longer target"),
        "{how}: the replacement link survived"
    );
    for name in ["a", "l"] {
        let (old, new) = (s(&src.join(name)), s(&dst.join(name)));
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains(&old) && n.contains(&new) && n.contains("both names")),
            "{how}: {name}: {r:?}"
        );
    }
}

/// Finding 1: an SFTP rename (`hardlink@` or the link made again, then `REMOVE`) whose old name
/// is replaced between the new name and the removal keeps the replacement and names both paths.
#[test]
fn r1_sftp_a_source_replaced_before_its_remove_survives() {
    let Some(mut fs) = SftpRemote::local_server() else {
        return;
    };
    let (_t, src, dst) = scratch();
    let original = file_and_link(&src);
    let _seam = SftpRemote::seam_before_remove(replace_entry);
    let r = run(
        &job(Op::Move, &src, &["a", "l"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    replacements_survived(&r, &src, &dst, original, "sftp");
}

/// Finding 2: the same through this machine's link + unlink fallback (a filesystem without the
/// exclusive rename), for the hard-link branch and the link branch. On a volume without hard
/// links (FAT, exFAT) there is no fallback for a file — it moves by copy, check and removal —
/// and the link branch alone is raced.
#[test]
fn r2_local_fallback_a_source_replaced_before_its_unlink_survives() {
    let (_t, src, dst) = scratch();
    let original = file_and_link(&src);
    let probe = dst.join("probe");
    let hard_links = std::fs::hard_link(src.join("a"), &probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    let _excl = LocalFs::seam_no_exclusive_rename();
    let _seam = LocalFs::seam_before_unlink(replace_entry);
    let r = run(
        &job(Op::Move, &src, &["a", "l"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    if hard_links {
        return replacements_survived(&r, &src, &dst, original, "local");
    }
    assert_eq!(r.items[0], item("a", ItemOutcome::Done), "{r:?}");
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"original");
    assert!(!src.join("a").exists(), "{r:?}");
    assert!(
        matches!(&r.items[1].outcome, ItemOutcome::Failed(why) if why.contains("old name remains")),
        "{r:?}"
    );
    assert_eq!(
        std::fs::read_link(dst.join("l")).unwrap(),
        PathBuf::from("target")
    );
    assert_eq!(
        std::fs::read_link(src.join("l")).unwrap(),
        PathBuf::from("another, longer target"),
        "the replacement link survived"
    );
    let (old, new) = (s(&src.join("l")), s(&dst.join("l")));
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains(&old) && n.contains(&new) && n.contains("both names")),
        "{r:?}"
    );
}

/// Finding 3: a moved folder's link replaced, after the copy and before the removal, by a
/// lookalike (another inode, a target as long, the same mtime) stays; the folder is kept.
#[test]
fn r3_a_link_replaced_by_a_lookalike_before_removal_stays() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        let d = src.join("d");
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join("f"), "f").unwrap();
        std::os::unix::fs::symlink("aaaa", d.join("l")).unwrap();
        let mut fs = Faulty::new(inner);
        fs.rename = no_rename_from(&[&d]);
        let (root, link) = (s(&dst.join("d")), d.join("l"));
        fs.after = Box::new(move |op, path| {
            if op == "set_meta" && path == root {
                swap_link(&link, "bbbb");
            }
        });
        let r = run(
            &job(Op::Move, &src, &["d"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        assert!(
            matches!(&r.items[0].outcome, ItemOutcome::Failed(why) if why.contains("both")),
            "{how}: {r:?}"
        );
        assert_eq!(
            std::fs::read_link(d.join("l")).unwrap(),
            PathBuf::from("bbbb"),
            "{how}: the lookalike stays"
        );
        assert_eq!(
            std::fs::read_link(dst.join("d/l")).unwrap(),
            PathBuf::from("aaaa"),
            "{how}"
        );
        assert_eq!(names(&d), ["l"], "{how}: the file moved");
    });
}

/// Finding 3: a link changed right after its copy was made fails the item, its copy goes, and
/// the source (the new link) stays.
#[test]
fn r3_a_link_changed_while_copied_fails_and_its_copy_goes() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        let l = src.join("l");
        std::os::unix::fs::symlink("aaaa", &l).unwrap();
        let mut fs = Faulty::new(inner);
        fs.rename = no_rename_from(&[&l]);
        let (made, link) = (s(&dst.join("l")), l.clone());
        fs.after = Box::new(move |op, path| {
            if op == "make_symlink" && path == made {
                swap_link(&link, "bbbb");
            }
        });
        let r = run(
            &job(Op::Move, &src, &["l"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        assert!(
            matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
                if why.contains("changed while it was copied")),
            "{how}: {r:?}"
        );
        assert_eq!(std::fs::read_link(&l).unwrap(), PathBuf::from("bbbb"));
        assert_eq!(names(&dst), Vec::<String>::new(), "{how}: its copy went");
    });
}

/// Finding 3: on this machine a source whose identity cannot be read is not moved (fail closed),
/// a file and a link alike.
#[test]
fn r3_a_local_source_without_an_identity_is_not_moved() {
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("f"), "f").unwrap();
    std::os::unix::fs::symlink("f", src.join("l")).unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&src.join("f"), &src.join("l")]);
    let blind = [s(&src.join("f")), s(&src.join("l"))];
    fs.fault = Box::new(move |op, path| {
        (op == "identity" && blind.iter().any(|b| b == path))
            .then(|| anyhow::anyhow!("injected: no stat"))
    });
    let r = run(
        &job(Op::Move, &src, &["f", "l"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    let failed = |name: &str| {
        let line = r.items.iter().find(|i| i.name == name);
        matches!(line, Some(Item { outcome: ItemOutcome::Failed(why), .. })
            if why.contains("injected: no stat"))
    };
    assert_eq!(r.items.len(), 2, "{r:?}");
    assert!(failed("f") && failed("l"), "{r:?}");
    assert_eq!(names(&src), ["f", "l"]);
    assert_eq!(names(&dst), Vec::<String>::new());
}

/// A moved file whose host-made copy (`copy_file_range`, `copy-data`) is corrupted
/// in its temp: the size check passes, the bytes do not.
fn corrupt_the_copy(fs: &mut Faulty, f: &Path) {
    fs.rename = no_rename_from(&[f]);
    fs.copied = Box::new(|temp| {
        let len = std::fs::metadata(temp).unwrap().len() as usize;
        std::fs::write(temp, vec![b'X'; len]).unwrap();
    });
}

/// Finding 4: every host-made copy is read back in full before its source is removed, whatever
/// its size (here above a lowered limit): other bytes of the same length keep the source.
#[test]
fn r4_a_move_verifies_a_host_made_copy_of_any_size() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        let f = src.join("f");
        std::fs::write(&f, vec![7u8; 64 * 1024]).unwrap();
        let before = sha(&f);
        let mut fs = Faulty::new(inner);
        fs.hash_limit = Some(16);
        fs.host_made = true;
        corrupt_the_copy(&mut fs, &f);
        let r = run(
            &job(Op::Move, &src, &["f"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        assert!(
            matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
                if why.contains("does not hold the same bytes")),
            "{how}: {r:?}"
        );
        assert_eq!(sha(&f), before, "{how}: the source stays");
        assert_eq!(names(&dst), Vec::<String>::new(), "{how}: no copy, no temp");
        assert!(r.notes.iter().all(|n| !n.contains("checked by its size")));
    });
}

/// Finding 4: Esc while a moved copy is checked stops there: the copy stays published, the
/// source is not removed, and the report says both copies exist.
#[test]
fn r4_esc_while_checking_a_moved_copy_keeps_both() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        let f = src.join("f");
        std::fs::write(&f, vec![3u8; 300 * 1024]).unwrap();
        let before = sha(&f);
        let mut fs = Faulty::new(inner);
        fs.host_made = true;
        fs.rename = no_rename_from(&[&f]);
        let mut ctl = Script {
            stop_when: Some("checking"),
            ..Script::default()
        };
        let r = run(
            &job(Op::Move, &src, &["f"], &dst),
            Hosts::Same(&mut fs),
            &mut ctl,
        );
        assert!(
            ctl.seen.iter().any(|p| p.verb == "checking"),
            "{how}: shown as checking"
        );
        assert_eq!(r.items.len(), 1, "{how}: {r:?}");
        let why = stop_reason(&r.items[0]);
        assert!(why.contains("both copies"), "{how}: {why}");
        assert_eq!(sha(&f), before, "{how}: the source stays");
        assert_eq!(sha(&dst.join("f")), before, "{how}: the copy stays");
        assert_eq!(temps(&dst), Vec::<String>::new(), "{how}");
    });
}

/// Finding 5: two hard-link names of one file are two entries: renaming one onto the other is
/// "exists" at once, with no other mutating call (never the two-step temp rename).
#[test]
fn r5_a_rename_onto_another_hard_link_of_it_changes_nothing() {
    let (_t, src, _dst) = scratch();
    let mut pairs = vec![("a", "b")];
    if !one_name(&src, "probe", "PROBE") {
        pairs.push(("readme", "README"));
    }
    for (from, to) in pairs {
        std::fs::write(src.join(from), "one file").unwrap();
        std::fs::hard_link(src.join(from), src.join(to)).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut fs = Faulty::new(Box::new(LocalFs::new()));
        let counted = calls.clone();
        fs.rename = Box::new(move |_, _| {
            counted.fetch_add(1, Ordering::Relaxed);
            None
        });
        let r = run(
            &job(Op::Rename { to: to.into() }, &src, &[from], &src),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        assert_eq!(
            r.items,
            vec![item(from, ItemOutcome::Failed(format!("{to} exists")))],
            "{from} → {to}"
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1, "{from} → {to}: one try");
        assert_eq!(ino(&src.join(from)), ino(&src.join(to)));
        std::fs::remove_file(src.join(from)).unwrap();
        std::fs::remove_file(src.join(to)).unwrap();
    }
}

/// Finding 6: on a filesystem without the exclusive rename a local folder is not renamed (that
/// could replace an empty folder): F6 moves it by copy, check and removal, F2 refuses with the
/// reason and changes nothing.
#[test]
fn r6_a_local_folder_without_the_exclusive_rename_moves_by_copy() {
    let (_t, src, dst) = scratch();
    let d = src.join("d");
    std::fs::create_dir(&d).unwrap();
    std::fs::write(d.join("x"), "x").unwrap();
    let was = ino(&d);
    let _excl = LocalFs::seam_no_exclusive_rename();
    let r = run(
        &job(Op::Move, &src, &["d"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(std::fs::read(dst.join("d/x")).unwrap(), b"x");
    assert_ne!(ino(&dst.join("d")), was, "copied, not renamed");
    assert_eq!(names(&src), Vec::<String>::new());

    std::fs::create_dir(src.join("e")).unwrap();
    let r = run(
        &job(Op::Rename { to: "f".into() }, &src, &["e"], &src),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("cannot rename the folder")),
        "{r:?}"
    );
    assert_eq!(names(&src), ["e"]);
}

/// Finding 7: Esc while the name-in-the-way dialog is open (its answer Rename) moves nothing.
#[test]
fn r7_esc_at_a_clash_moves_nothing() {
    both(|mut fs, how| {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join("a"), "mine").unwrap();
        std::fs::write(dst.join("a"), "theirs").unwrap();
        let mut ctl = Script {
            stop_on_ask: true,
            ..Script::answers([reply(Answer::Rename("a-1".into()))])
        };
        let r = run(
            &job(Op::Move, &src, &["a"], &dst),
            Hosts::Same(&mut *fs),
            &mut ctl,
        );
        assert_eq!(r.items.len(), 1, "{how}: {r:?}");
        stop_reason(&r.items[0]);
        assert_eq!(std::fs::read(src.join("a")).unwrap(), b"mine", "{how}");
        assert_eq!(names(&dst), ["a"], "{how}: nothing renamed");
        assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"theirs", "{how}");
    });
}

/// Finding 10: a hard link refused for permission (as Linux's `fs.protected_hardlinks=1`
/// refuses one; the server's own reason, injected): no copy is tried, the source stays, and the
/// reason carries the hint.
#[test]
fn r10_a_hard_link_refused_for_permission_tries_no_copy() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join("a"), "a").unwrap();
        let tries = Arc::new(AtomicUsize::new(0));
        let mut fs = Faulty::new(inner);
        let from = s(&src.join("a"));
        fs.rename = Box::new(move |f, t| {
            (f == from).then(|| Err(SftpRemote::link_refused(f, t, &"Permission denied")))
        });
        let counted = tries.clone();
        fs.copy = Box::new(move |_| {
            counted.fetch_add(1, Ordering::Relaxed);
            None
        });
        let r = run(
            &job(Op::Move, &src, &["a"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        assert!(
            matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
                if why.contains("fs.protected_hardlinks")),
            "{how}: {r:?}"
        );
        assert_eq!(tries.load(Ordering::Relaxed), 0, "{how}: no copy tried");
        assert_eq!(names(&src), ["a"], "{how}");
        assert_eq!(names(&dst), Vec::<String>::new(), "{how}");
    });
}

/// Finding 11: a source that changes during its copy — its size, or its mtime alone — fails the
/// item with the reason; no final name and no temp are left, and a move keeps the source.
#[test]
fn r11_a_source_changed_during_its_copy_fails_the_item() {
    both(|mut inner, how| {
        let grow = |f: &Path| {
            let mut file = std::fs::OpenOptions::new().append(true).open(f).unwrap();
            file.write_all(b" and more").unwrap();
        };
        let touch = |f: &Path| set_mtime(f, 1_700_000_000);
        let changes: [(&str, Change); 2] = [("size", grow), ("mtime", touch)];
        for (what, change) in changes {
            for op in [Op::Copy, Op::Move] {
                let (_t, src, dst) = scratch();
                let f = src.join("f");
                std::fs::write(&f, "before").unwrap();
                set_mtime(&f, 1_600_000_000);
                let mut fs = Faulty::new(inner);
                fs.rename = no_rename_from(&[&f]);
                let source = f.clone();
                fs.copied = Box::new(move |_| change(&source));
                let r = run(
                    &job(op.clone(), &src, &["f"], &dst),
                    Hosts::Same(&mut fs),
                    &mut Script::default(),
                );
                inner = fs.into_inner();
                assert!(
                    matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
                        if why.contains("changed while it was copied")),
                    "{how} {what} {op:?}: {r:?}"
                );
                assert_eq!(names(&dst), Vec::<String>::new(), "{how} {what} {op:?}");
                assert!(f.exists(), "{how} {what} {op:?}: the source stays");
            }
        }
    });
}

/// A copy from another host whose stream is corrupted on its way into the temp (the digest of
/// the bytes read kept): the move fails with the reason and the source stays.
#[test]
fn a_corrupted_stream_fails_the_move_and_keeps_the_source() {
    let mut targets: Vec<(&str, Layer)> = vec![("local", Box::new(LocalFs::new()))];
    if let Some(sftp) = SftpRemote::local_server() {
        targets.push(("sftp", Box::new(sftp)));
    }
    for (how, target) in targets {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join("f"), "the right bytes").unwrap();
        let before = sha(&src.join("f"));
        let mut fs = Faulty::new(target);
        fs.corrupt = true;
        let r = run(
            &job(Op::Move, &src, &["f"], &dst),
            Hosts::Two {
                src: &mut LocalFs::new(),
                dst: &mut fs,
            },
            &mut Script::default(),
        );
        assert!(
            matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
                if why.contains("differ from the bytes read")),
            "{how}: {r:?}"
        );
        assert_eq!(sha(&src.join("f")), before, "{how}");
        assert_eq!(names(&dst), Vec::<String>::new(), "{how}");
    }
}

/// Esc during the removals of a flushed batch does not cut them short: its items were copied,
/// checked and made durable, so they all finish moving.
#[test]
fn esc_during_a_batchs_removals_lets_them_finish() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        let d = three_files(&src);
        std::fs::write(src.join("later"), "later").unwrap();
        let files = ["a.txt", "b.txt", "c.txt"];
        let before = payloads(&d, &files);
        let mut ctl = Script::default();
        let stop = ctl.cancel.clone();
        let mut fs = Faulty::new(inner);
        fs.rename = no_rename_from(&[&d, &src.join("later")]);
        let inside = format!("{}/", s(&d));
        fs.fault = Box::new(move |op, path| {
            if op == "remove" && path.starts_with(&inside) {
                stop.store(true, Ordering::Relaxed);
            }
            None
        });
        let r = run(
            &job(Op::Move, &src, &["d", "later"], &dst),
            Hosts::Same(&mut fs),
            &mut ctl,
        );
        assert_eq!(
            r.items,
            vec![
                item("d", ItemOutcome::Done),
                item("later", ItemOutcome::Done)
            ],
            "{how}: the batch's checked items finish moving"
        );
        assert_eq!(payloads(&dst.join("d"), &files), before, "{how}: complete");
        assert!(!d.exists() && !src.join("later").exists(), "{how}");
        assert_eq!(std::fs::read(dst.join("later")).unwrap(), b"later");
    });
}

/// A remote source that cannot be removed after its hard link (`sftp-server -P remove`): the
/// same outcome as a moved copy whose source stays — failed, both copies exist, the reason in
/// a note.
#[test]
fn a_remote_source_that_cannot_be_removed_keeps_both_names() {
    let Some(mut fs) = SftpRemote::local_server_args(&["-P", "remove"], None) else {
        return;
    };
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    let r = run(
        &job(Op::Move, &src, &["a"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("both copies exist")),
        "{r:?}"
    );
    assert!(
        r.notes.iter().any(|n| n.contains("both names exist")),
        "{r:?}"
    );
    assert!(src.join("a").exists() && dst.join("a").exists());
}

/// A folder whose walk fails makes nothing: the walk comes before the new folder (F4), so there
/// is no folder to remove.
#[test]
fn a_failed_walk_makes_no_folder() {
    let (_t, src, dst) = scratch();
    std::fs::create_dir(src.join("d")).unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    let walked = s(&src.join("d"));
    fs.fault = Box::new(move |op, path| {
        (op == "list_entries" && path == walked).then(|| anyhow::anyhow!("injected: no listing"))
    });
    let r = run(
        &job(Op::Copy, &src, &["d"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("injected: no listing")),
        "{r:?}"
    );
    assert_eq!(names(&dst), Vec::<String>::new(), "nothing made");
}

// ---- review round 2 ----

/// Finding r2-1: another process of this account puts a hard link to an unrelated file at the
/// temp's name right after the temp is made. The copy's bytes, mode and times go through the
/// descriptor sopen holds, never that file; nothing is published under the final name; a move
/// keeps its source.
#[test]
fn r12_a_temp_name_replaced_after_its_create_redirects_nothing() {
    for op in [Op::Copy, Op::Move] {
        let (_t, src, dst) = scratch();
        let f = src.join("f");
        std::fs::write(&f, "the data").unwrap();
        set_mode(&f, 0o640);
        set_mtime(&f, 1_600_000_000);
        let victim = dst.parent().unwrap().join("victim");
        std::fs::write(&victim, "the victim's own").unwrap();
        set_mode(&victim, 0o600);
        set_mtime(&victim, 1_000_000_000);
        let v = victim.clone();
        let _seam = LocalFs::seam_after_create(move |temp| {
            std::fs::remove_file(temp).unwrap();
            std::fs::hard_link(&v, temp).unwrap();
        });
        let mut fs = Faulty::new(Box::new(LocalFs::new()));
        fs.rename = no_rename_from(&[&f]);
        let r = run(
            &job(op.clone(), &src, &["f"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        assert!(
            matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
                if why.contains("not the copy")),
            "{op:?}: {r:?}"
        );
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"the victim's own",
            "{op:?}"
        );
        assert_eq!(
            (mode(&victim), mtime_secs(&victim)),
            (0o600, 1_000_000_000),
            "{op:?}: the victim's attributes untouched"
        );
        assert!(!dst.join("f").exists(), "{op:?}: nothing published");
        assert_eq!(std::fs::read(&f).unwrap(), b"the data", "{op:?}");
    }
}

/// Finding r2-2: an SFTP rename whose old name cannot be checked again before `REMOVE` (its
/// folder made unreadable) leaves it, and says the outcome could not be confirmed — not that
/// both names exist.
#[test]
fn r13_sftp_a_name_that_cannot_be_checked_before_remove_is_unconfirmed() {
    let Some(mut fs) = SftpRemote::local_server() else {
        return;
    };
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    let folder = src.clone();
    let _seam = SftpRemote::seam_before_remove(move |_| set_mode(&folder, 0o000));
    let r = run(
        &job(Op::Move, &src, &["a"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    set_mode(&src, 0o755);
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("could not be confirmed") && !why.contains("both")),
        "{r:?}"
    );
    assert!(
        r.notes.iter().any(|n| n.contains("could not be confirmed")),
        "{r:?}"
    );
    assert!(
        r.notes.iter().all(|n| !n.contains("both names exist")),
        "{r:?}"
    );
    assert!(src.join("a").exists(), "left");
}

/// Finding r2-3: a failed read-back never lets a move remove its source — not when the source
/// cannot be stat'ed at that moment (one transient failure right after the check read it), and a
/// source gone before the check completed is no verified move. The report says the copy did not
/// verify.
#[test]
fn r14_a_failed_check_never_removes_the_source() {
    both(|mut inner, how| {
        // A corrupt copy, and one transient stat failure of the source after the check's read.
        {
            let (_t, src, dst) = scratch();
            let f = src.join("f");
            std::fs::write(&f, vec![9u8; 4096]).unwrap();
            let before = sha(&f);
            let mut fs = Faulty::new(inner);
            fs.host_made = true;
            corrupt_the_copy(&mut fs, &f);
            let armed = Arc::new(AtomicBool::new(false));
            let (arm, fire, path) = (armed.clone(), armed.clone(), s(&f));
            let path2 = path.clone();
            fs.after = Box::new(move |op, p| {
                if op == "read_to" && p == path {
                    arm.store(true, Ordering::Relaxed);
                }
            });
            fs.fault = Box::new(move |op, p| {
                (op == "lstat" && p == path2 && fire.swap(false, Ordering::Relaxed))
                    .then(|| anyhow::anyhow!("injected: a transient stat failure"))
            });
            let r = run(
                &job(Op::Move, &src, &["f"], &dst),
                Hosts::Same(&mut fs),
                &mut Script::default(),
            );
            assert!(
                matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
                if why.contains("does not hold the same bytes")),
                "{how}: {r:?}"
            );
            assert_eq!(sha(&f), before, "{how}: the source survives");
            inner = fs.into_inner();
        }

        // A good copy whose source goes right after the check read it: not a verified move.
        let (_t, src, dst) = scratch();
        let f = src.join("f");
        std::fs::write(&f, "good bytes").unwrap();
        let mut fs = Faulty::new(inner);
        fs.host_made = true;
        fs.rename = no_rename_from(&[&f]);
        let (path, gone) = (s(&f), f.clone());
        fs.after = Box::new(move |op, p| {
            if op == "read_to" && p == path {
                std::fs::remove_file(&gone).unwrap();
            }
        });
        let r = run(
            &job(Op::Move, &src, &["f"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        assert!(
            matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
                if why.contains("verified")),
            "{how}: {r:?}"
        );
        assert_eq!(
            std::fs::read(dst.join("f")).unwrap(),
            b"good bytes",
            "{how}: the copy stays"
        );
    });
}

/// Finding r2-4 (G1): on this machine a staged copy whose identity cannot be read is never
/// published: the item fails with that error, nothing reaches the final name, the source stays.
#[test]
fn r15_a_copy_whose_identity_cannot_be_read_is_not_published() {
    let (_t, src, dst) = scratch();
    let f = src.join("f");
    std::fs::write(&f, vec![5u8; 2048]).unwrap();
    let before = sha(&f);
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.host_made = true;
    corrupt_the_copy(&mut fs, &f);
    let inside = format!("{}/.f.sopen-", s(&dst));
    fs.fault = Box::new(move |op, p| {
        (op == "identity" && p.starts_with(&inside))
            .then(|| anyhow::anyhow!("injected: no identity"))
    });
    let published: Arc<Mutex<Option<String>>> = Arc::default();
    let seen = published.clone();
    fs.renamed = Box::new(move |from, to| {
        if from.contains(".sopen-") {
            *seen.lock().unwrap() = Some(to.to_string());
        }
    });
    let r = run(
        &job(Op::Move, &src, &["f"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("injected: no identity")),
        "{r:?}"
    );
    assert_eq!(*published.lock().unwrap(), None, "nothing was published");
    assert!(!dst.join("f").exists(), "nothing under the final name");
    assert_eq!(sha(&f), before, "the source survives");
}

/// A file another process puts at the published name right after the publish, with the copy's
/// size and mtime, is never taken for the copy: it survives a failed check, and so does the
/// source. On a volume that writes in place (exFAT) no rename publishes the copy: the name is
/// replaced right after the copy was written under it.
#[test]
fn r15_a_replacement_at_the_published_name_survives() {
    let (_t, src, dst) = scratch();
    let f = src.join("f");
    std::fs::write(&f, vec![5u8; 2048]).unwrap();
    let before = sha(&f);
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.host_made = true;
    corrupt_the_copy(&mut fs, &f);
    let replaced: Arc<Mutex<Option<u64>>> = Arc::default();
    let (seen, target) = (replaced.clone(), dst.join("f"));
    let replace = move || {
        let was = std::fs::metadata(&target).unwrap();
        std::fs::remove_file(&target).unwrap();
        std::fs::write(&target, vec![b'R'; was.len() as usize]).unwrap();
        let t = std::fs::File::options().write(true).open(&target).unwrap();
        t.set_times(std::fs::FileTimes::new().set_modified(was.modified().unwrap()))
            .unwrap();
        *seen.lock().unwrap() = Some(ino(&target));
    };
    let (at_publish, in_place, name) = (replace.clone(), replace, s(&dst.join("f")));
    fs.copied = Box::new(move |temp| {
        let len = std::fs::metadata(temp).unwrap().len() as usize;
        std::fs::write(temp, vec![b'X'; len]).unwrap();
        if temp == name {
            in_place();
        }
    });
    let name = s(&dst.join("f"));
    fs.renamed = Box::new(move |from, to| {
        if from.contains(".sopen-") && to == name {
            at_publish();
        }
    });
    let r = run(
        &job(Op::Move, &src, &["f"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(
        matches!(
            &r.items[..],
            [Item {
                outcome: ItemOutcome::Failed(_),
                ..
            }]
        ),
        "{r:?}"
    );
    let replacement = replaced
        .lock()
        .unwrap()
        .unwrap_or_else(|| panic!("published, then replaced: {r:?}"));
    assert_eq!(ino(&dst.join("f")), replacement, "the replacement survives");
    assert_eq!(sha(&f), before, "the source survives");
}

/// Finding r2-5: Esc between items and Cancel at a clash each leave a note saying why the rest
/// was not done.
#[test]
fn r16_esc_between_items_and_cancel_are_explained() {
    let (_t, src, dst) = scratch();
    for name in ["a", "b", "c"] {
        std::fs::write(src.join(name), name).unwrap();
    }
    // Esc right after `a` was published.
    let mut ctl = Script::default();
    let stop = ctl.cancel.clone();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.renamed = Box::new(move |_, to| {
        if to.ends_with("/a") {
            stop.store(true, Ordering::Relaxed);
        }
    });
    let r = run(
        &job(Op::Copy, &src, &["a", "b", "c"], &dst),
        Hosts::Same(&mut fs),
        &mut ctl,
    );
    assert_eq!(
        r.items,
        vec![
            item("a", ItemOutcome::Done),
            item("b", ItemOutcome::NotReached),
            item("c", ItemOutcome::NotReached)
        ]
    );
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("stopped") && n.contains("2")),
        "{r:?}"
    );

    // Cancel at the name in the way of `b`.
    std::fs::write(dst.join("b"), "theirs").unwrap();
    let mut ctl = Script::answers([reply(Answer::Cancel)]);
    let r = run(
        &job(Op::Copy, &src, &["b", "c"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut ctl,
    );
    // The item Cancel was answered at is not "skipped" (nobody chose Skip): it failed, the
    // reason saying it was cancelled and that nothing of it was copied.
    assert_eq!(
        r.items,
        vec![
            item(
                "b",
                ItemOutcome::Failed("cancelled: nothing of it was copied".to_string())
            ),
            item("c", ItemOutcome::NotReached)
        ]
    );
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("cancelled") && n.contains("b")),
        "{r:?}"
    );
}

// ---- the live probe (round 4) ----

/// Overwrites 8 bytes in the middle of a staged copy, in place (its size kept).
fn scribble(temp: &str) {
    use std::io::{Seek, SeekFrom};
    let len = std::fs::metadata(temp).unwrap().len();
    let mut f = std::fs::OpenOptions::new().write(true).open(temp).unwrap();
    f.seek(SeekFrom::Start(len / 2)).unwrap();
    f.write_all(b"SCRIBBLE").unwrap();
}

/// The copy paths a move can take: on one host (this machine; this machine where nothing can
/// publish atomically, written in place; a server's `copy-data`; a server without it, streamed
/// through sopen) and between two (this machine and a server both ways, two sessions to one
/// server, two connections to this machine). `None` where `sftp-server` is missing. The
/// `copy-data` server logs its requests to `log`, and must offer the extension.
fn every_path(name: &str, log: &Path) -> Option<(Layer, Option<Layer>)> {
    let sftp =
        |args: &[&str]| SftpRemote::local_server_args(args, None).map(|f| Box::new(f) as Layer);
    Some(match name {
        "local" | "in-place" => (Box::new(LocalFs::new()), None),
        "copy-data" => {
            let fs = SftpRemote::local_server_args(&["-l", "DEBUG1"], Some(log))?;
            assert!(fs.offers_copy_data(), "this server must offer copy-data");
            (Box::new(fs), None)
        }
        "sftp-streamed" => (sftp(&["-P", "copy-data"])?, None),
        "here-to-remote" => (Box::new(LocalFs::new()), Some(sftp(&[])?)),
        "remote-to-here" => (sftp(&[])?, Some(Box::new(LocalFs::new()))),
        "remote-to-remote" => (sftp(&[])?, Some(sftp(&[])?)),
        "here-to-here" => (Box::new(LocalFs::new()), Some(Box::new(LocalFs::new()))),
        _ => unreachable!(),
    })
}

const PATHS: [&str; 8] = [
    "local",
    "in-place",
    "copy-data",
    "sftp-streamed",
    "here-to-remote",
    "remote-to-here",
    "remote-to-remote",
    "here-to-here",
];

/// F2: a move reads back what the target stored, on every path and at any size (the size cap
/// lowered to 16 bytes here; `f2_every_path_at_320_mib` runs the real size): a copy scribbled on
/// after its bytes were sent never lets the source go.
fn f2_case(path: &str, size: usize) {
    let (_t, src, dst) = scratch();
    let log = src.parent().unwrap().join("server.log");
    let _in_place = (path == "in-place").then(|| {
        (
            LocalFs::seam_no_exclusive_rename(),
            LocalFs::seam_no_links(),
        )
    });
    let Some((mut a, b)) = every_path(path, &log) else {
        return;
    };
    let f = src.join("f");
    std::fs::write(&f, (0..size).map(|i| (i % 249) as u8).collect::<Vec<u8>>()).unwrap();
    let before = sha(&f);
    let r = match b {
        None => {
            let mut fs = Faulty::new(a);
            fs.hash_limit = Some(16);
            fs.rename = no_rename_from(&[&f]);
            fs.copied = Box::new(scribble);
            run(
                &job(Op::Move, &src, &["f"], &dst),
                Hosts::Same(&mut fs),
                &mut Script::default(),
            )
        }
        Some(b) => {
            let mut fs = Faulty::new(b);
            fs.hash_limit = Some(16);
            fs.copied = Box::new(scribble);
            let hosts = Hosts::Two {
                src: a.as_mut(),
                dst: &mut fs,
            };
            run(
                &job(Op::Move, &src, &["f"], &dst),
                hosts,
                &mut Script::default(),
            )
        }
    };
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("does not hold the same bytes")),
        "{path}: {r:?}"
    );
    assert_eq!(sha(&f), before, "{path}: the source survives");
    assert!(!dst.join("f").exists(), "{path}: the bad copy went");
    assert_eq!(temps(&dst), Vec::<String>::new(), "{path}");
    if path == "in-place" {
        assert!(
            r.notes.iter().any(|n| n.contains("written in place")),
            "{r:?}"
        );
    }
    if path == "copy-data" {
        // Its server ended with the layer (dropped with `fs` above): the log is complete.
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("copy-data from"), "the server copied: {text}");
    }
}

#[test]
fn f2_a_move_reads_back_its_copy_on_every_path() {
    for path in PATHS {
        f2_case(path, 1024 * 1024);
    }
}

/// F2 at the probe's size, on every path. Slow in a debug build: run by hand,
/// `cargo test --release --lib -- --ignored f2_every_path_at_320_mib`.
#[test]
#[ignore]
fn f2_every_path_at_320_mib() {
    for path in PATHS {
        let t = std::time::Instant::now();
        f2_case(path, 320 * 1024 * 1024);
        eprintln!("f2 {path} 320 MiB: ok in {:?}", t.elapsed());
    }
}

/// F3: a volume with neither an exclusive rename nor links (exFAT, FAT): copies are written
/// straight into their final names, created exclusively — never replacing — and the report says
/// so; a clash still asks; a move copies, checks and removes; Esc leaves nothing under the name.
#[test]
fn f3_a_volume_without_exclusive_rename_or_links_writes_in_place() {
    let _excl = LocalFs::seam_no_exclusive_rename();
    let _links = LocalFs::seam_no_links();
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    std::fs::write(src.join("b"), "b").unwrap();
    std::fs::create_dir(src.join("d")).unwrap();
    std::fs::write(src.join("d/x"), "x").unwrap();
    let in_place = |r: &Report| r.notes.iter().any(|n| n.contains("written in place"));

    let r = run(
        &job(Op::Copy, &src, &["a", "d"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    assert_eq!(
        r.items,
        vec![item("a", ItemOutcome::Done), item("d", ItemOutcome::Done)],
        "{r:?}"
    );
    assert!(in_place(&r), "{r:?}");
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"a");
    assert_eq!(std::fs::read(dst.join("d/x")).unwrap(), b"x");
    assert_eq!(temps(&dst), Vec::<String>::new());

    // A name in the way still asks; Rename writes the other name in place.
    let mut ctl = Script::answers([reply(Answer::Rename("a-1".into()))]);
    let r = run(
        &job(Op::Copy, &src, &["a"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut ctl,
    );
    assert_eq!(
        r.items,
        vec![item("a", ItemOutcome::Renamed("a-1".into()))],
        "{r:?}"
    );
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"a");

    // A move: copied in place, checked, the source removed.
    let r = run(
        &job(Op::Move, &src, &["b"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    assert_eq!(r.items, vec![item("b", ItemOutcome::Done)], "{r:?}");
    assert!(!src.join("b").exists());
    assert_eq!(std::fs::read(dst.join("b")).unwrap(), b"b");

    // Esc during the copy: nothing is left under the name.
    std::fs::write(src.join("big"), vec![1u8; 4 * 1024 * 1024]).unwrap();
    let mut ctl = Script {
        stop_at: Some(1),
        ..Script::default()
    };
    let r = run(
        &job(Op::Copy, &src, &["big"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut ctl,
    );
    stop_reason(&r.items[0]);
    assert!(!dst.join("big").exists(), "{r:?}");
}

/// F4: a folder copied into itself over two connections to one filesystem is refused — on this
/// machine twice, and through two `sftp-server` sessions.
#[test]
fn f4_into_itself_over_two_connections_is_refused() {
    let mut pairs: Vec<(&str, Layer, Layer)> =
        vec![("local", Box::new(LocalFs::new()), Box::new(LocalFs::new()))];
    if let (Some(a), Some(b)) = (SftpRemote::local_server(), SftpRemote::local_server()) {
        pairs.push(("sftp", Box::new(a), Box::new(b)));
    }
    for (how, mut a, mut b) in pairs {
        let (_t, src, _dst) = scratch();
        let d = src.join("F");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("x"), "x").unwrap();
        let r = run(
            &job(Op::Copy, &src, &["F"], &d.join("sub")),
            Hosts::Two {
                src: a.as_mut(),
                dst: b.as_mut(),
            },
            &mut Script::default(),
        );
        assert!(refusal(&r).contains("into itself"), "{how}: {r:?}");
        assert_eq!(names(&d.join("sub")), Vec::<String>::new(), "{how}");
    }
}

/// F4: where two connections cannot tell whether they share a filesystem (no id on a side) and
/// the resolved paths nest, the copy is refused — safety first.
#[test]
fn f4_without_a_filesystem_id_nested_paths_are_refused() {
    let (_t, src, _dst) = scratch();
    let d = src.join("F");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("x"), "x").unwrap();
    let (mut a, b) = (LocalFs::new(), LocalFs::new());
    let mut fs = Faulty::new(Box::new(b));
    fs.no_fs_id = true;
    let r = run(
        &job(Op::Copy, &src, &["F"], &d.join("sub")),
        Hosts::Two {
            src: &mut a,
            dst: &mut fs,
        },
        &mut Script::default(),
    );
    assert!(refusal(&r).contains("cannot tell"), "{r:?}");
    assert_eq!(names(&d.join("sub")), Vec::<String>::new());
}

/// F4: a folder is walked before its copy is made (the trace's order).
#[test]
fn f4_the_walk_comes_before_the_new_folder() {
    let (_t, src, dst) = scratch();
    let d = src.join("F");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("x"), "x").unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    let trace = fs.trace.clone();
    let r = run(
        &job(Op::Copy, &src, &["F"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    let t = trace.lock().unwrap().clone();
    let at = |e: String| {
        t.iter()
            .position(|x| *x == e)
            .unwrap_or_else(|| panic!("{e}: {t:?}"))
    };
    assert!(
        at(format!("list:{}", s(&d.join("sub")))) < at(format!("mkdir:{}", s(&dst.join("F")))),
        "{t:?}"
    );
}

/// F5: "published, but the old name remains" is one outcome on every path: failed, both copies
/// exist — a server refusing `REMOVE` after the hard link, and this machine's link + unlink
/// fallback refused the unlink.
#[test]
fn f5_an_old_name_that_remains_is_one_outcome() {
    let both = |r: &Report, how: &str| {
        assert!(
            matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
                if why.contains("both copies exist")),
            "{how}: {r:?}"
        );
        assert!(
            r.notes.iter().any(|n| n.contains("was not removed")),
            "{how}: the reason in a note: {r:?}"
        );
    };
    if let Some(mut fs) = SftpRemote::local_server_args(&["-P", "remove"], None) {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join("a"), "a").unwrap();
        let r = run(
            &job(Op::Move, &src, &["a"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        both(&r, "sftp");
        assert!(src.join("a").exists() && dst.join("a").exists());
    }
    let _excl = LocalFs::seam_no_exclusive_rename();
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    set_mode(&src, 0o555);
    let r = run(
        &job(Op::Move, &src, &["a"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    set_mode(&src, 0o755);
    both(&r, "local");
    assert!(src.join("a").exists() && dst.join("a").exists());
}

/// F6: one durability barrier before each moved item's source is removed and one at the end —
/// not one per file.
#[test]
fn f6_one_barrier_per_batch_of_moved_items_and_one_at_the_end() {
    let (_t, src, dst) = scratch();
    let d = three_files(&src);
    std::fs::write(src.join("f"), "f").unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&d, &src.join("f")]);
    let barriers = fs.barriers.clone();
    let r = run(
        &job(Op::Move, &src, &["d", "f"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(
        barriers.load(Ordering::Relaxed),
        2,
        "one batch for d and f, and the end"
    );

    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    let barriers = fs.barriers.clone();
    let to = dst.join("copies");
    std::fs::create_dir(&to).unwrap();
    let r = run(
        &job(Op::Copy, &dst, &["d", "f"], &to),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(barriers.load(Ordering::Relaxed), 1, "a copy: the end only");
}

/// Lower: a source that grows during a streamed move fails as changed while it was copied.
#[test]
fn a_source_growing_during_its_stream_is_changed() {
    let (_t, src, dst) = scratch();
    let f = src.join("f");
    std::fs::write(&f, "before").unwrap();
    let mut from = Faulty::new(Box::new(LocalFs::new()));
    let grown = f.clone();
    from.after = Box::new(move |op, _| {
        if op == "open_read" {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&grown)
                .unwrap();
            file.write_all(b" and after").unwrap();
        }
    });
    let r = run(
        &job(Op::Move, &src, &["f"], &dst),
        Hosts::Two {
            src: &mut from,
            dst: &mut LocalFs::new(),
        },
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("changed while it was copied")),
        "{r:?}"
    );
    assert!(f.exists());
    assert!(!dst.join("f").exists());
}

/// Lower: a folder copy that fails part-way leaves its partial folder with the source folder's
/// mode, not the 0700 it was filled at.
#[test]
fn a_partial_folder_gets_its_source_mode() {
    let (_t, src, dst) = scratch();
    let d = three_files(&src);
    set_mode(&d, 0o750);
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.copy = Box::new(|from| {
        from.ends_with("b.txt")
            .then(|| anyhow::anyhow!("injected read error"))
    });
    let r = run(
        &job(Op::Copy, &src, &["d"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[0].outcome, ItemOutcome::Failed(_)),
        "{r:?}"
    );
    assert_eq!(mode(&dst.join("d")), 0o750, "{r:?}");
}

/// F6 receipt (run by hand, not a gate): 500 files of 1 KiB copied and then moved on this
/// machine, timed — one barrier per operation, not one per file.
/// `cargo test --release --lib -- --ignored f6_timing --nocapture`
#[test]
#[ignore]
fn f6_timing_of_500_small_files() {
    let (_t, src, dst) = scratch();
    let d = src.join("many");
    std::fs::create_dir(&d).unwrap();
    for i in 0..500 {
        std::fs::write(d.join(format!("f{i:03}")), vec![(i % 256) as u8; 1024]).unwrap();
    }
    let t = std::time::Instant::now();
    let r = run(
        &job(Op::Copy, &src, &["many"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    eprintln!("f6: copy of 500 x 1 KiB in {:?}", t.elapsed());
    let other = dst.join("moved");
    std::fs::create_dir(&other).unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&dst.join("many")]);
    let t = std::time::Instant::now();
    let r = run(
        &job(Op::Move, &dst, &["many"], &other),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    eprintln!(
        "f6: move (copy, read-back, removal) of 500 x 1 KiB in {:?}, {} barrier(s)",
        t.elapsed(),
        fs.barriers.load(Ordering::Relaxed)
    );
}

/// A real volume (run by hand, not a gate): `SOPEN_TEST_VOLUME=<a folder on an exFAT, FAT or
/// other volume>`; copy and move a file and a folder there from this machine, and report how
/// they were published. `cargo test --release --lib -- --ignored real_volume --nocapture`
#[test]
#[ignore]
fn real_volume_receipt() {
    let Some(volume) = std::env::var_os("SOPEN_TEST_VOLUME") else {
        eprintln!("skipping: SOPEN_TEST_VOLUME is not set");
        return;
    };
    let target = PathBuf::from(volume).join(format!("sopen-{}", crate::remote::random_hex()));
    std::fs::create_dir(&target).unwrap();
    let (_t, src, _dst) = scratch();
    std::fs::write(src.join("a.bin"), vec![3u8; 3 * 1024 * 1024]).unwrap();
    std::fs::write(src.join("b.txt"), "b").unwrap();
    std::fs::create_dir(src.join("d")).unwrap();
    std::fs::write(src.join("d/x"), "x").unwrap();
    // An empty file: on FAT and exFAT its inode number follows its name through a rename.
    std::fs::write(src.join("empty"), "").unwrap();
    let mode = LocalFs::new().publish_mode(&s(&target)).unwrap();
    eprintln!("volume: publish mode {mode:?}");
    assert!(mode.1.is_empty(), "the probe left something: {mode:?}");
    let r = run(
        &job(Op::Copy, &src, &["a.bin", "d", "empty"], &target),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    eprintln!("copy: {r:?}");
    assert!(
        r.items.iter().all(|i| i.outcome == ItemOutcome::Done),
        "{r:?}"
    );
    assert!(
        r.notes.iter().all(|n| !n.contains("probe")),
        "a probe was left behind: {r:?}"
    );
    let mut ctl = Script::answers([reply(Answer::Rename("a-1.bin".into()))]);
    let r = run(
        &job(Op::Copy, &src, &["a.bin"], &target),
        Hosts::Same(&mut LocalFs::new()),
        &mut ctl,
    );
    eprintln!("clash, renamed: {r:?}");
    let r = run(
        &job(Op::Move, &src, &["b.txt"], &target),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    eprintln!("move: {r:?}; source left: {}", src.join("b.txt").exists());
    let mut listed: Vec<String> = std::fs::read_dir(&target)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    listed.sort();
    eprintln!("target holds: {listed:?}");
    assert!(
        listed.iter().all(|n| !n.contains(".sopen-")),
        "something of sopen's was left: {listed:?}"
    );
    assert_eq!(std::fs::read(target.join("empty")).unwrap(), b"");
    assert_eq!(sha(&target.join("a.bin")), sha(&src.join("a.bin")));
    assert_eq!(std::fs::read(target.join("b.txt")).unwrap(), b"b");
    assert!(!src.join("b.txt").exists());
    std::fs::remove_dir_all(&target).unwrap();
}

// ---- round 5 ----

/// The trace's index of the first event matching `pred`, and of the last.
fn first(t: &[String], pred: impl Fn(&str) -> bool) -> Option<usize> {
    t.iter().position(|e| pred(e))
}

fn last(t: &[String], pred: impl Fn(&str) -> bool) -> Option<usize> {
    t.iter().rposition(|e| pred(e))
}

fn writes(e: &str) -> bool {
    ["write:", "rename:", "mkdir:", "symlink:"]
        .iter()
        .any(|p| e.starts_with(p))
}

fn removes_under(e: &str, prefix: &str) -> bool {
    let path = e
        .strip_prefix("remove:")
        .or_else(|| e.strip_prefix("rmdir:"));
    path.is_some_and(|p| p == prefix || p.starts_with(&format!("{prefix}/")))
}

/// F6 (order): one ordered trace — each moved item's barrier comes after that item's last write
/// and before any of its source removals, flushes every folder that received an entry, deepest
/// first; the end's barrier comes after the last write.
#[test]
fn f6_each_barrier_comes_after_its_writes_and_before_its_removals() {
    let (_t, src, dst) = scratch();
    let d = src.join("d");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("a"), "a").unwrap();
    std::fs::write(d.join("sub/b"), "b").unwrap();
    std::fs::write(src.join("f"), "f").unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&d, &src.join("f")]);
    let trace = fs.trace.clone();
    let r = run(
        &job(Op::Move, &src, &["d", "f"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    let t = trace.lock().unwrap().clone();
    let (dd, ds) = (s(&dst.join("d")), s(&dst.join("d/sub")));
    // One batch: both items' folders flushed, deepest first, before either's sources go.
    let barrier = format!("barrier:{ds};{dd};{}", s(&dst));
    let cases = [(s(&d), dd.clone()), (s(&src.join("f")), s(&dst.join("f")))];
    for (source, copy) in cases {
        let w = last(&t, |e| writes(e) && e.contains(&copy)).expect("written");
        let r = first(&t, |e| removes_under(e, &source)).expect("removed");
        let b = t
            .iter()
            .position(|e| *e == barrier)
            .unwrap_or_else(|| panic!("{barrier}: {t:?}"));
        assert!(
            w < b && b < r,
            "{source}: write {w} < barrier {b} < removal {r}: {t:?}"
        );
    }
    let w = last(&t, writes).unwrap();
    let b = last(&t, |e| e.starts_with("barrier:")).unwrap();
    assert!(w < b, "the end's barrier after the last write: {t:?}");
    let src_s = s(&src);
    let gone = last(&t, |e| removes_under(e, &src_s)).unwrap();
    assert!(
        gone < b,
        "the end's barrier after the last source removal: {t:?}"
    );
}

/// F6 / engine 5: a barrier that fails (after a good read-back) removes no source of that item:
/// it stays, and the report says the copy's durability was not confirmed.
#[test]
fn a_failed_barrier_removes_no_source() {
    let (_t, src, dst) = scratch();
    let d = three_files(&src);
    std::fs::write(src.join("f"), "f").unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&d, &src.join("f")]);
    fs.fault = Box::new(|op, _| {
        (op == "barrier").then(|| anyhow::anyhow!("injected: the drive did not flush"))
    });
    let trace = fs.trace.clone();
    let r = run(
        &job(Op::Move, &src, &["d", "f"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    for name in ["d", "f"] {
        let line = r.items.iter().find(|i| i.name == name);
        assert!(
            matches!(line, Some(Item { outcome: ItemOutcome::Failed(why), .. })
                if why.contains("durability was not confirmed") && why.contains("stay")),
            "{name}: {r:?}"
        );
    }
    let t = trace.lock().unwrap().clone();
    let src_s = s(&src);
    assert!(
        first(&t, |e| removes_under(e, &src_s)).is_none(),
        "no source removal: {t:?}"
    );
    assert_eq!(names(&d), ["a.txt", "b.txt", "c.txt"]);
    assert!(src.join("f").exists() && dst.join("f").exists());
}

/// Engine 6: an in-place copy whose final name changes hands right after it was made, then
/// stopped: what is under the name now is not sopen's, so it survives, named in the report.
#[test]
fn an_in_place_copy_stopped_after_its_name_changed_hands_leaves_the_newcomer() {
    let _excl = LocalFs::seam_no_exclusive_rename();
    let _links = LocalFs::seam_no_links();
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("big"), vec![1u8; 4 * 1024 * 1024]).unwrap();
    let target = dst.join("big");
    let t2 = target.clone();
    let _seam = LocalFs::seam_after_create(move |path| {
        if Path::new(path) == t2 {
            std::fs::remove_file(path).unwrap();
            std::fs::write(path, "someone else's").unwrap();
        }
    });
    let mut ctl = Script {
        stop_at: Some(1),
        ..Script::default()
    };
    let r = run(
        &job(Op::Copy, &src, &["big"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut ctl,
    );
    stop_reason(&r.items[0]);
    assert_eq!(std::fs::read(&target).unwrap(), b"someone else's", "{r:?}");
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains(&s(&target)) && n.contains("left as it is")),
        "{r:?}"
    );
}

/// Engine 6 / contract: an in-place copy that fails part-way (its source stops answering) is
/// removed — it is sopen's — and the item says why; Esc part-way says nothing of it is left.
#[test]
fn an_in_place_copy_that_fails_part_way_is_removed_and_said() {
    let _excl = LocalFs::seam_no_exclusive_rename();
    let _links = LocalFs::seam_no_links();
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("big"), vec![1u8; 1024 * 1024]).unwrap();
    let mut from = Faulty::new(Box::new(LocalFs::new()));
    from.fail_read_after = Some(100_000);
    let r = run(
        &job(Op::Move, &src, &["big"], &dst),
        Hosts::Two {
            src: &mut from,
            dst: &mut LocalFs::new(),
        },
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("injected: the source stopped answering")),
        "{r:?}"
    );
    assert!(!dst.join("big").exists(), "the partial copy went");
    assert!(src.join("big").exists(), "the source stays");

    let mut ctl = Script {
        stop_at: Some(1),
        ..Script::default()
    };
    std::fs::write(src.join("huge"), vec![2u8; 4 * 1024 * 1024]).unwrap();
    let r = run(
        &job(Op::Move, &src, &["huge"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut ctl,
    );
    assert!(stop_reason(&r.items[0]).contains("nothing of it"), "{r:?}");
    assert!(!dst.join("huge").exists());
    assert!(src.join("huge").exists());
}

/// Contract: an in-place copy replaced at its name before its read-back is left and named; a
/// move keeps its source.
#[test]
fn an_in_place_copy_replaced_before_its_check_is_left_and_named() {
    let _excl = LocalFs::seam_no_exclusive_rename();
    let _links = LocalFs::seam_no_links();
    let (_t, src, dst) = scratch();
    let f = src.join("f");
    std::fs::write(&f, vec![4u8; 4096]).unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&f]);
    fs.copied = Box::new(|path| {
        std::fs::remove_file(path).unwrap();
        std::fs::write(path, "a newcomer").unwrap();
    });
    let r = run(
        &job(Op::Move, &src, &["f"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("not the copy") && why.contains(&s(&dst.join("f")))),
        "{r:?}"
    );
    assert_eq!(std::fs::read(dst.join("f")).unwrap(), b"a newcomer");
    assert!(f.exists(), "the source stays");
}

/// Engine 8: a two-step rename whose steps cannot be confirmed claims nothing it does not know.
#[test]
fn a_two_step_rename_that_cannot_be_confirmed_claims_nothing() {
    let (_t, src, _dst) = scratch();
    if !one_name(&src, "probe", "PROBE") {
        eprintln!("skipping: {} is case-sensitive", src.display());
        return;
    }
    let readme = src.join("readme");
    std::fs::write(&readme, "r").unwrap();
    let to_upper = job(
        Op::Rename {
            to: "README".into(),
        },
        &src,
        &["readme"],
        &src,
    );
    let failed = |r: &Report| match &r.items[..] {
        [Item {
            outcome: ItemOutcome::Failed(why),
            ..
        }] => why.clone(),
        _ => panic!("{r:?}"),
    };
    // The first step unconfirmed.
    let calls = AtomicUsize::new(0);
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = Box::new(move |_, to| match calls.fetch_add(1, Ordering::Relaxed) {
        0 => Some(Err(taken(to))),
        _ => Some(Ok(Renamed::Unconfirmed("injected: no answer".into()))),
    });
    let why = failed(&run(
        &to_upper,
        Hosts::Same(&mut fs),
        &mut Script::default(),
    ));
    assert!(why.contains("could not be confirmed"), "{why}");
    assert!(
        !why.contains("in place") && !why.contains("names it too"),
        "{why}"
    );

    // The way back unconfirmed.
    let calls = AtomicUsize::new(0);
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = Box::new(move |_, to| match calls.fetch_add(1, Ordering::Relaxed) {
        0 => Some(Err(taken(to))),
        1 => None,
        2 => Some(Err(anyhow::anyhow!("injected: second step"))),
        _ => Some(Ok(Renamed::Unconfirmed("injected: no answer".into()))),
    });
    let why = failed(&run(
        &to_upper,
        Hosts::Same(&mut fs),
        &mut Script::default(),
    ));
    assert!(why.contains("could not be confirmed"), "{why}");
    assert!(!why.contains("back under"), "{why}");
    assert!(
        why.contains(".sopen-renaming-"),
        "names where it may be: {why}"
    );
}

/// Ops 9: the publish probe never touches a name that is not its own: the name it vacated (now
/// someone else's) and a second name swapped for another file both survive, and the report names
/// what it left; a cleanup that fails is named too.
#[test]
fn the_publish_probe_cleans_up_only_its_own_names() {
    let (_t, _src, dst) = scratch();
    let dir = s(&dst);
    let _seam = LocalFs::seam_after_probe(|names| {
        let (vacated, second) = names.split_once('\n').unwrap();
        std::fs::write(vacated, "someone else's, at the vacated name").unwrap();
        std::fs::remove_file(second).unwrap();
        std::fs::write(second, "someone else's, swapped in").unwrap();
    });
    let (mode, notes) = LocalFs::new().publish_mode(&dir).unwrap();
    assert_eq!(mode, Publish::Atomic);
    let left: Vec<String> = names(&dst);
    assert_eq!(left.len(), 2, "both survive: {left:?}");
    for name in &left {
        let body = std::fs::read_to_string(dst.join(name)).unwrap();
        assert!(body.starts_with("someone else's"), "{name}: {body}");
    }
    let second = left
        .iter()
        .find(|n| {
            std::fs::read_to_string(dst.join(n))
                .unwrap()
                .contains("swapped")
        })
        .unwrap();
    assert!(
        notes.iter().any(|n| n.contains(&s(&dst.join(second)))),
        "{notes:?}"
    );
}

#[test]
fn the_publish_probe_names_a_cleanup_that_fails() {
    let (_t, _src, dst) = scratch();
    let folder = dst.clone();
    let _seam = LocalFs::seam_after_probe(move |_| set_mode(&folder, 0o555));
    let got = LocalFs::new().publish_mode(&s(&dst));
    set_mode(&dst, 0o755);
    let (_, notes) = got.unwrap();
    let left = names(&dst);
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(
        notes.iter().any(|n| n.contains(&s(&dst.join(&left[0])))),
        "the leftover named: {notes:?}"
    );
}

/// Ops 11: `F_FULLFSYNC` refused as unsupported falls back to `fsync`; a real I/O error fails the
/// barrier (and so keeps a moved source); an interrupted one is tried again.
#[cfg(target_os = "macos")]
#[test]
fn a_full_fsync_error_falls_back_only_when_unsupported() {
    let (_t, _src, dst) = scratch();
    let dirs = [s(&dst)];
    for (errno, ok) in [
        (libc::ENOTSUP, true),
        (libc::EINTR, true),
        (libc::EIO, false),
    ] {
        let _seam = LocalFs::seam_full_fsync_errno(errno);
        let got = LocalFs::new().barrier(&dirs);
        assert_eq!(got.is_ok(), ok, "errno {errno}: {got:?}");
    }
}

// ---- round 6 ----

/// Batches: moved items are copied and checked, then made durable together — one barrier for
/// the batch, then their sources removed — not one barrier per item.
#[test]
fn batched_moves_share_one_barrier() {
    let (_t, src, dst) = scratch();
    let names_ = ["a", "b", "c"];
    for n in names_ {
        std::fs::write(src.join(n), n).unwrap();
    }
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    let sources: Vec<PathBuf> = names_.iter().map(|n| src.join(n)).collect();
    fs.rename = no_rename_from(&sources.iter().map(|p| p.as_path()).collect::<Vec<_>>());
    let (trace, barriers) = (fs.trace.clone(), fs.barriers.clone());
    let r = run(
        &job(Op::Move, &src, &names_, &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(barriers.load(Ordering::Relaxed), 2, "the batch and the end");
    let t = trace.lock().unwrap().clone();
    let b = first(&t, |e| e.starts_with("barrier:")).unwrap();
    let src_s = s(&src);
    assert!(last(&t, writes).unwrap() < b, "{t:?}");
    assert!(
        first(&t, |e| removes_under(e, &src_s)).unwrap() > b,
        "{t:?}"
    );
}

/// Batches: 64 moved items at most, then the batch is flushed — by the trace's order: its
/// barrier comes after the 64th item's copy was published and read back and before anything of
/// the 65th, and the 64 removals come between that barrier and the 65th item. (The clock stands
/// still: only the count flushes.)
#[test]
fn a_batch_is_flushed_at_64_items() {
    let (_t, src, dst) = scratch();
    let all: Vec<String> = (0..65).map(|i| format!("f{i:02}")).collect();
    for n in &all {
        std::fs::write(src.join(n), n).unwrap();
    }
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = Box::new(|from, _| {
        (!from.contains(".sopen-")).then(|| Err(anyhow::anyhow!(NoRename("elsewhere".into()))))
    });
    let (trace, barriers) = (fs.trace.clone(), fs.barriers.clone());
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    let r = run(
        &job(Op::Move, &src, &refs, &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(names(&src), Vec::<String>::new());
    let t = trace.lock().unwrap().clone();
    let is_barrier = |e: &str| e.starts_with("barrier:");
    let published = |n: &str| {
        let to = format!("->{}", s(&dst.join(n)));
        first(&t, |e| e.starts_with("rename:") && e.ends_with(&to)).expect("published")
    };
    let checked = |n: &str| {
        let read = format!("read:{}", s(&dst.join(n)));
        last(&t, |e| e == read).expect("read back")
    };
    let removed = |n: &str| {
        let gone = format!("remove:{}", s(&src.join(n)));
        first(&t, |e| e == gone).expect("removed")
    };
    let batch = first(&t, is_barrier).unwrap();
    // By its full paths: a temp's random suffix can spell any name.
    let of_65th = [
        s(&src.join("f64")),
        s(&dst.join("f64")),
        s(&dst.join(".f64.sopen-")),
    ];
    let the_65th = first(&t, |e| of_65th.iter().any(|p| e.contains(p.as_str()))).unwrap();
    assert!(
        published("f63") < batch && checked("f63") < batch,
        "the barrier after the 64th item's copy and check: {t:?}"
    );
    assert!(batch < the_65th, "and before anything of the 65th: {t:?}");
    for n in &all[..64] {
        let gone = removed(n);
        assert!(batch < gone && gone < the_65th, "{n}: {t:?}");
    }
    let rest: Vec<usize> = (0..t.len())
        .filter(|&i| i > batch && is_barrier(&t[i]))
        .collect();
    let [second, end] = rest[..] else {
        panic!("three barriers — 64 items, 1 item, the end: {t:?}")
    };
    assert!(checked("f64") < second && second < removed("f64") && removed("f64") < end);
    assert_eq!(barriers.load(Ordering::Relaxed), 3);
}

/// Batches: a name-in-the-way question flushes the batch first — no verified item waits for an
/// answer with both copies in place.
#[test]
fn a_batch_is_flushed_before_a_question() {
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    std::fs::write(src.join("b"), "b").unwrap();
    std::fs::write(dst.join("b"), "theirs").unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&src.join("a"), &src.join("b")]);
    let seen = Arc::new(AtomicBool::new(true));
    let (gone, a) = (seen.clone(), src.join("a"));
    let mut ctl = Script {
        on_ask: Some(Box::new(move || gone.store(a.exists(), Ordering::Relaxed))),
        ..Script::answers([reply(Answer::Skip)])
    };
    let r = run(
        &job(Op::Move, &src, &["a", "b"], &dst),
        Hosts::Same(&mut fs),
        &mut ctl,
    );
    assert_eq!(
        r.items,
        vec![
            item("a", ItemOutcome::Done),
            item("b", ItemOutcome::Skipped)
        ]
    );
    assert!(
        !seen.load(Ordering::Relaxed),
        "a's source was gone when b was asked about"
    );
}

/// Batches: Esc flushes the batch — the items already checked finish moving after the barrier —
/// and starts nothing more.
#[test]
fn esc_flushes_the_batch_of_checked_items() {
    let (_t, src, dst) = scratch();
    for n in ["a", "b", "c"] {
        std::fs::write(src.join(n), n).unwrap();
    }
    let mut ctl = Script::default();
    let stop = ctl.cancel.clone();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    let target = s(&dst.join("b"));
    fs.rename = Box::new(|from, _| {
        (!from.contains(".sopen-")).then(|| Err(anyhow::anyhow!(NoRename("elsewhere".into()))))
    });
    fs.renamed = Box::new(move |_, to| {
        if to == target {
            stop.store(true, Ordering::Relaxed);
        }
    });
    let trace = fs.trace.clone();
    let r = run(
        &job(Op::Move, &src, &["a", "b", "c"], &dst),
        Hosts::Same(&mut fs),
        &mut ctl,
    );
    assert_eq!(r.items[0], item("a", ItemOutcome::Done), "{r:?}");
    assert!(stop_reason(&r.items[1]).contains("both copies"), "{r:?}");
    assert_eq!(r.items[2], item("c", ItemOutcome::NotReached), "{r:?}");
    assert!(!src.join("a").exists() && src.join("b").exists() && src.join("c").exists());
    let t = trace.lock().unwrap().clone();
    let a_src = s(&src.join("a"));
    let b = first(&t, |e| e.starts_with("barrier:")).unwrap();
    assert!(
        first(&t, |e| removes_under(e, &a_src)).unwrap() > b,
        "{t:?}"
    );
}

/// Kill mid-batch (here an abort at the batch's barrier, in a child process): nothing is lost —
/// every source is still there, and every copy that was published is whole.
#[test]
fn a_kill_mid_batch_loses_nothing() {
    const CHILD: &str = "SOPEN_TEST_KILL_BATCH";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = PathBuf::from(root);
        let (src, dst) = (root.join("src"), root.join("dst"));
        let items = ["a", "b", "c", "d", "e"];
        let mut fs = Faulty::new(Box::new(LocalFs::new()));
        fs.rename = Box::new(|from, _| {
            (!from.contains(".sopen-")).then(|| Err(anyhow::anyhow!(NoRename("elsewhere".into()))))
        });
        fs.fault = Box::new(|op, _| {
            if op == "barrier" {
                std::process::abort();
            }
            None
        });
        let _ = run(
            &job(Op::Move, &src, &items, &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        return;
    }
    let (_t, src, dst) = scratch();
    let root = src.parent().unwrap().to_path_buf();
    let items = ["a", "b", "c", "d", "e"];
    for n in items {
        std::fs::write(src.join(n), format!("the bytes of {n}")).unwrap();
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "fileops::tests::a_kill_mid_batch_loses_nothing",
            "--exact",
            "--test-threads=1",
        ])
        .env(CHILD, &root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // A child that neither dies nor ends is killed and reaped: the test fails, it never hangs.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        match child.try_wait().unwrap() {
            Some(status) => break status,
            None if std::time::Instant::now() >= deadline => {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("the child did not end within its deadline");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    };
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(libc::SIGABRT),
        "the child was to die of its abort at the barrier: {status:?}"
    );
    for n in items {
        let want = format!("the bytes of {n}");
        assert_eq!(
            std::fs::read_to_string(src.join(n)).unwrap(),
            want,
            "{n}: the source"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join(n)).unwrap(),
            want,
            "{n}: the copy, published before the batch's barrier"
        );
    }
}

/// The in-place note is said once per target folder in an operation.
#[test]
fn the_in_place_note_is_said_once_per_folder() {
    let _excl = LocalFs::seam_no_exclusive_rename();
    let _links = LocalFs::seam_no_links();
    let (_t, src, dst) = scratch();
    for n in ["a", "b", "c"] {
        std::fs::write(src.join(n), n).unwrap();
    }
    let r = run(
        &job(Op::Copy, &src, &["a", "b", "c"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    let said = r
        .notes
        .iter()
        .filter(|n| n.contains("written in place"))
        .count();
    assert_eq!(said, 1, "{r:?}");
}

/// A folder `fsync` refused (here as unsupported) is no durability: a move keeps its source and
/// says why; a copy is unaffected.
#[test]
fn a_folder_that_cannot_be_flushed_keeps_the_source() {
    let _seam = LocalFs::seam_dir_fsync_errno(libc::EINVAL);
    let (_t, src, dst) = scratch();
    let f = src.join("f");
    std::fs::write(&f, "f").unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&f]);
    let r = run(
        &job(Op::Move, &src, &["f"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("cannot confirm") && why.contains("source stays")),
        "{r:?}"
    );
    assert!(f.exists() && dst.join("f").exists());

    std::fs::write(src.join("g"), "g").unwrap();
    let r = run(
        &job(Op::Copy, &src, &["g"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    assert_eq!(r.items, vec![item("g", ItemOutcome::Done)], "{r:?}");
}

/// A publish probe that cannot check its own file says so, naming it.
#[test]
fn a_probe_that_cannot_be_checked_is_named() {
    let _seam = LocalFs::seam_probe_stat_fails();
    let (_t, _src, dst) = scratch();
    let (_, notes) = LocalFs::new().publish_mode(&s(&dst)).unwrap();
    let left = names(&dst);
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(
        notes
            .iter()
            .any(|n| n.contains(&s(&dst.join(&left[0]))) && n.contains("injected")),
        "{notes:?}"
    );
}

/// Unlinking one name of a file bumps the status-change time of the others, so a moved source
/// is known again by the inode it holds, not by its ctime: two names of one file, in a batch and
/// below a moved folder, all move.
#[test]
fn hard_links_of_one_file_all_move() {
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "linked").unwrap();
    std::fs::hard_link(src.join("a"), src.join("b")).unwrap();
    let d = src.join("d");
    std::fs::create_dir(&d).unwrap();
    std::fs::write(d.join("x"), "inside").unwrap();
    std::fs::hard_link(d.join("x"), d.join("y")).unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&src.join("a"), &src.join("b"), &d]);
    let r = run(
        &job(Op::Move, &src, &["a", "b", "d"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(names(&src), Vec::<String>::new(), "every source went");
    assert_eq!(std::fs::read(dst.join("b")).unwrap(), b"linked");
    assert_eq!(std::fs::read(dst.join("d/y")).unwrap(), b"inside");
}

/// Where no descriptor can be held (too many open files) a source is known by its device, inode
/// and status-change time. A hard-linked sibling's unlink changes the last, so that sibling is
/// kept and both copies exist — the safe side: nothing is lost, and the report says so.
#[test]
fn without_pins_a_hard_linked_sibling_is_kept_not_lost() {
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "linked").unwrap();
    std::fs::hard_link(src.join("a"), src.join("b")).unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.no_pins = true;
    fs.rename = no_rename_from(&[&src.join("a"), &src.join("b")]);
    let r = run(
        &job(Op::Move, &src, &["a", "b"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert_eq!(r.items[0], item("a", ItemOutcome::Done), "{r:?}");
    assert!(
        matches!(&r.items[1].outcome, ItemOutcome::Failed(why)
            if why.contains("another file has its name now") && why.contains("both copies exist")),
        "{r:?}"
    );
    assert!(!src.join("a").exists());
    assert_eq!(std::fs::read(src.join("b")).unwrap(), b"linked", "kept");
    assert_eq!(std::fs::read(dst.join("b")).unwrap(), b"linked", "copied");
}

/// A pinned source stays the same entry through a change of its metadata (a chmod, a tag): only
/// another inode in its name is "another file".
#[test]
fn a_moved_source_whose_mode_changed_after_its_copy_still_moves() {
    let (_t, src, dst) = scratch();
    let f = src.join("f");
    std::fs::write(&f, "f").unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&f]);
    let source = f.clone();
    fs.renamed = Box::new(move |from, _| {
        if from.contains(".sopen-") {
            std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    });
    let r = run(
        &job(Op::Move, &src, &["f"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert!(!f.exists());
}

/// Batches: a second after the batch began, the next item boundary flushes it — its sources go
/// before the following item is copied, not at the very end.
#[test]
fn a_batch_is_flushed_a_second_after_it_began() {
    let (_t, src, dst) = scratch();
    for n in ["a", "b", "c"] {
        std::fs::write(src.join(n), n).unwrap();
    }
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = Box::new(|from, _| {
        (!from.contains(".sopen-")).then(|| Err(anyhow::anyhow!(NoRename("elsewhere".into()))))
    });
    let barriers = fs.barriers.clone();
    let (slow, last) = (s(&dst.join("b")), s(&dst.join("c")));
    let gone: Arc<Mutex<Option<(bool, bool)>>> = Arc::default();
    let (seen, a, b) = (gone.clone(), src.join("a"), src.join("b"));
    fs.renamed = Box::new(move |from, to| {
        if !from.contains(".sopen-") {
            return;
        }
        if to == slow {
            super::engine::advance_clock(std::time::Duration::from_millis(1000));
        }
        if to == last {
            *seen.lock().unwrap() = Some((a.exists(), b.exists()));
        }
    });
    let r = run(
        &job(Op::Move, &src, &["a", "b", "c"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(
        *gone.lock().unwrap(),
        Some((false, false)),
        "a and b were removed before c was copied"
    );
    assert_eq!(barriers.load(Ordering::Relaxed), 3, "a+b, c, and the end");
}

/// Batches: a long folder does not hold the sources of the items before it past the batch's age
/// — the batch is flushed between the folder's entries too.
#[test]
fn a_batch_is_flushed_between_the_entries_of_a_long_folder() {
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    let d = src.join("d");
    std::fs::create_dir(&d).unwrap();
    std::fs::write(d.join("x"), "x").unwrap();
    std::fs::write(d.join("y"), "y").unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&src.join("a"), &d]);
    let inside = format!("{}/", s(&dst.join("d")));
    let (n, a_was_there) = (AtomicUsize::new(0), Arc::new(AtomicBool::new(true)));
    let (seen, a) = (a_was_there.clone(), src.join("a"));
    fs.renamed = Box::new(move |from, to| {
        if !from.contains(".sopen-") || !to.starts_with(&inside) {
            return;
        }
        match n.fetch_add(1, Ordering::Relaxed) {
            0 => super::engine::advance_clock(std::time::Duration::from_millis(1000)),
            _ => seen.store(a.exists(), Ordering::Relaxed),
        }
    });
    let r = run(
        &job(Op::Move, &src, &["a", "d"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert!(
        !a_was_there.load(Ordering::Relaxed),
        "a's source was gone while the folder's second entry was copied"
    );
}

/// One operation holds 64 source descriptors at most: the sources past that are known by
/// device, inode and status-change time, and move all the same. The count is the operation's
/// own: another operation, run while this one holds all 64, pins its sources.
#[test]
fn an_operation_pins_64_sources_at_most() {
    let (_t, src, dst) = scratch();
    let d = src.join("d");
    std::fs::create_dir(&d).unwrap();
    for i in 0..70 {
        std::fs::write(d.join(format!("f{i:02}")), "x").unwrap();
    }
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&d]);
    let pinned = fs.pinned.clone();
    // The other operation: started when this one publishes its last file, all 64 pins held.
    let (others, held) = (Arc::new(AtomicUsize::new(0)), pinned.clone());
    let (seen, last) = (others.clone(), s(&dst.join("d/f69")));
    fs.renamed = Box::new(move |_, to| {
        if to != last {
            return;
        }
        assert_eq!(held.load(Ordering::Relaxed), 64);
        let (_t2, src2, dst2) = scratch();
        std::fs::write(src2.join("a"), "a").unwrap();
        std::fs::write(src2.join("b"), "b").unwrap();
        let mut other = Faulty::new(Box::new(LocalFs::new()));
        other.rename = no_rename_from(&[&src2.join("a"), &src2.join("b")]);
        let count = other.pinned.clone();
        let r = run(
            &job(Op::Move, &src2, &["a", "b"], &dst2),
            Hosts::Same(&mut other),
            &mut Script::default(),
        );
        assert!(r.all_done(), "{r:?}");
        seen.store(count.load(Ordering::Relaxed), Ordering::Relaxed);
    });
    let r = run(
        &job(Op::Move, &src, &["d"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(pinned.load(Ordering::Relaxed), 64);
    assert_eq!(others.load(Ordering::Relaxed), 2, "the other operation's");
    assert!(!d.exists());
    assert_eq!(names(&dst.join("d")).len(), 70);
}

/// Batch receipt (run by hand, not a gate): 500 selected files of 1 KiB moved by copy,
/// read-back and removal, timed.
/// `cargo test --release --lib -- --ignored batch_timing --nocapture`
#[test]
#[ignore]
fn batch_timing_of_500_selected_files() {
    let (_t, src, dst) = scratch();
    let all: Vec<String> = (0..500).map(|i| format!("f{i:03}")).collect();
    for (i, n) in all.iter().enumerate() {
        std::fs::write(src.join(n), vec![(i % 256) as u8; 1024]).unwrap();
    }
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = Box::new(|from, _| {
        (!from.contains(".sopen-")).then(|| Err(anyhow::anyhow!(NoRename("elsewhere".into()))))
    });
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    let t = std::time::Instant::now();
    let r = run(
        &job(Op::Move, &src, &refs, &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    eprintln!(
        "batch: move (copy, read-back, removal) of 500 selected x 1 KiB in {:?}, {} barrier(s)",
        t.elapsed(),
        fs.barriers.load(Ordering::Relaxed)
    );
}

// ---- round 7 ----

/// The status-change time of `path` moved on with its device and inode unchanged: what a new
/// entry on a reused inode number looks like to a comparison of numbers alone.
fn bump_ctime(path: &Path) {
    let was = std::fs::symlink_metadata(path).unwrap();
    let mode = was.mode() & 0o777;
    for _ in 0..200 {
        set_mode(path, mode ^ 0o100);
        set_mode(path, mode);
        let now = std::fs::symlink_metadata(path).unwrap();
        if (now.ctime(), now.ctime_nsec()) != (was.ctime(), was.ctime_nsec()) {
            assert_eq!((now.dev(), now.ino()), (was.dev(), was.ino()));
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the status-change time of {path:?} never moved");
}

/// A pin is taken only of the entry whose identity was read, in full: the same device and
/// inode with another status-change time — an entry put there since, on the reused number —
/// is not pinned, whether it came before the pin looked at the name or between that look and
/// the open.
#[test]
fn a_pin_is_refused_when_the_entry_changed_since_its_identity() {
    let (_t, src, _dst) = scratch();
    let f = src.join("f");
    std::fs::write(&f, "f").unwrap();
    let mut fs = LocalFs::new();
    let id = fs.identity(&s(&f), false).unwrap().unwrap();
    assert!(fs.pin(&s(&f), &id).is_some(), "unchanged: pinned");
    {
        let at = f.clone();
        let _seam = LocalFs::seam_before_pin(move |_| bump_ctime(&at));
        assert!(fs.pin(&s(&f), &id).is_none(), "changed before the pin");
    }
    let id = fs.identity(&s(&f), false).unwrap().unwrap();
    let at = f.clone();
    let _seam = LocalFs::seam_before_pin_open(move |_| bump_ctime(&at));
    assert!(
        fs.pin(&s(&f), &id).is_none(),
        "changed between the look and the open"
    );
}

/// A moved source that changed between its identity and its pin is not pinned as if it were
/// the entry read: it is kept, and the report says another file has its name.
#[test]
fn a_source_changed_before_its_pin_is_kept() {
    let (_t, src, dst) = scratch();
    let f = src.join("f");
    std::fs::write(&f, "f").unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.rename = no_rename_from(&[&f]);
    let at = f.clone();
    let _seam = LocalFs::seam_before_pin(move |_| bump_ctime(&at));
    let r = run(
        &job(Op::Move, &src, &["f"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("another file has its name now") && why.contains("both copies exist")),
        "{r:?}"
    );
    assert_eq!(std::fs::read(&f).unwrap(), b"f", "kept");
    assert_eq!(std::fs::read(dst.join("f")).unwrap(), b"f", "copied");
}

/// Only a regular file or a link is ever opened for a pin: a fifo (as a device or a socket) is
/// not — the open is never reached.
#[test]
fn a_fifo_is_never_opened_for_a_pin() {
    let (_t, src, _dst) = scratch();
    let fifo = src.join("fifo");
    let c = std::ffi::CString::new(s(&fifo)).unwrap();
    // SAFETY: a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
    let opened = Arc::new(AtomicBool::new(false));
    let seen = opened.clone();
    let _seam = LocalFs::seam_before_pin_open(move |_| seen.store(true, Ordering::Relaxed));
    let mut fs = LocalFs::new();
    let id = fs.identity(&s(&fifo), false).unwrap().unwrap();
    assert!(fs.pin(&s(&fifo), &id).is_none());
    assert!(!opened.load(Ordering::Relaxed), "the open was reached");
    let f = src.join("f");
    std::fs::write(&f, "f").unwrap();
    let id = fs.identity(&s(&f), false).unwrap().unwrap();
    assert!(fs.pin(&s(&f), &id).is_some());
    assert!(opened.load(Ordering::Relaxed), "a file is opened");
}

/// A checked copy that is gone or replaced when its batch is flushed (here at the batch's
/// barrier, between the check and the removal) keeps its source: a file, and a file below a
/// moved folder; the report names the copy.
#[test]
fn a_copy_changed_before_the_flush_keeps_its_source() {
    type Harm = fn(&Path);
    let delete: Harm = |p| std::fs::remove_file(p).unwrap();
    let replace: Harm = |p| {
        std::fs::remove_file(p).unwrap();
        std::fs::write(p, "theirs").unwrap();
    };
    for (what, harm) in [("deleted", delete), ("replaced", replace)] {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join("a"), "mine a").unwrap();
        std::fs::write(src.join("b"), "mine b").unwrap();
        let d = src.join("d");
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join("x"), "mine x").unwrap();
        std::fs::write(d.join("y"), "mine y").unwrap();
        let mut fs = Faulty::new(Box::new(LocalFs::new()));
        fs.rename = no_rename_from(&[&src.join("a"), &src.join("b"), &d]);
        let (copies, once) = ([dst.join("a"), dst.join("d/x")], AtomicBool::new(false));
        fs.fault = Box::new(move |op, _| {
            if op == "barrier" && !once.swap(true, Ordering::Relaxed) {
                copies.iter().for_each(|c| harm(c));
            }
            None
        });
        let r = run(
            &job(Op::Move, &src, &["a", "b", "d"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        let said = |name: &str, copy: &Path| {
            let line = r.items.iter().find(|i| i.name == name).expect("a line");
            let want = format!(
                "its copy at {} changed before the source was removed; the source stays",
                s(copy)
            );
            assert!(
                matches!(&line.outcome, ItemOutcome::Failed(why) if why.contains(&want)),
                "{what}: {name}: {r:?}"
            );
        };
        said("a", &dst.join("a"));
        said("d/x", &dst.join("d/x"));
        assert_eq!(std::fs::read(src.join("a")).unwrap(), b"mine a", "{what}");
        assert_eq!(std::fs::read(d.join("x")).unwrap(), b"mine x", "{what}");
        // The untouched copies' sources went.
        assert!(!src.join("b").exists() && !d.join("y").exists(), "{what}");
        assert_eq!(std::fs::read(dst.join("b")).unwrap(), b"mine b", "{what}");
        assert_eq!(std::fs::read(dst.join("d/y")).unwrap(), b"mine y", "{what}");
        if what == "replaced" {
            assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"theirs");
        }
    }
}

/// The same over SFTP, where a copy is known by its type, size and mtime.
#[test]
fn a_remote_copy_gone_before_the_flush_keeps_its_source() {
    let Some(sftp) = SftpRemote::local_server() else {
        return;
    };
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "mine a").unwrap();
    let mut fs = Faulty::new(Box::new(sftp));
    fs.rename = no_rename_from(&[&src.join("a")]);
    let (copy, once) = (dst.join("a"), AtomicBool::new(false));
    fs.fault = Box::new(move |op, _| {
        if op == "barrier" && !once.swap(true, Ordering::Relaxed) {
            std::fs::remove_file(&copy).unwrap();
        }
        None
    });
    let r = run(
        &job(Op::Move, &src, &["a"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("changed before the source was removed; the source stays")),
        "{r:?}"
    );
    assert_eq!(std::fs::read(src.join("a")).unwrap(), b"mine a");
}

/// On this machine a temp whose identity could not be read when it was created is never taken
/// for an SFTP copy (which has none) and removed by its name: it is left, and named.
#[test]
fn a_local_temp_without_an_identity_is_left_and_named() {
    let _seam = LocalFs::seam_temp_stat_fails();
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("f"), "f").unwrap();
    let r = run(
        &job(Op::Copy, &src, &["f"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    let left = names(&dst);
    assert_eq!(left.len(), 1, "the temp is left: {left:?}");
    assert!(left[0].contains(".sopen-"), "{left:?}");
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains(&s(&dst.join(&left[0]))) && why.contains("left as it is")),
        "{r:?}"
    );
    assert!(src.join("f").exists());
}

// ---- round 8 ----

/// The link + unlink fallback (no exclusive rename) never links an entry that is not the one
/// it looked at: a file put under the name between that look and the pin's open — here the
/// same device and inode with another status-change time, as a reused inode number gives — is
/// "changed", not "no descriptor at hand": nothing is linked and the name stays. The same
/// where no descriptor can be held at all: the name is looked at again before the link.
#[test]
fn the_fallback_links_nothing_when_the_entry_changed_before_its_pin() {
    for no_pins in [false, true] {
        let (_t, src, dst) = scratch();
        let a = src.join("a");
        std::fs::write(&a, "a replacement").unwrap();
        let _excl = LocalFs::seam_no_exclusive_rename();
        let _pins = no_pins.then(LocalFs::seam_no_pins);
        let (at, once) = (a.clone(), AtomicBool::new(false));
        let _seam = LocalFs::seam_before_pin_open(move |_| {
            if !once.swap(true, Ordering::Relaxed) {
                bump_ctime(&at);
            }
        });
        let r = run(
            &job(Op::Move, &src, &["a"], &dst),
            Hosts::Same(&mut LocalFs::new()),
            &mut Script::default(),
        );
        assert!(
            matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
                if why.contains("changed while it was being renamed")),
            "no_pins {no_pins}: {r:?}"
        );
        assert_eq!(
            std::fs::read(&a).unwrap(),
            b"a replacement",
            "no_pins {no_pins}: its name survives"
        );
        assert_eq!(names(&dst), Vec::<String>::new(), "no_pins {no_pins}");
    }
}

/// Without a descriptor the fallback still moves a file, and still keeps a replacement put
/// under the old name after the link: the old name must be the very entry the new name is.
#[test]
fn the_fallback_without_a_pin_moves_and_keeps_a_replacement() {
    let _excl = LocalFs::seam_no_exclusive_rename();
    let _pins = LocalFs::seam_no_pins();
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a"), "original").unwrap();
    let r = run(
        &job(Op::Move, &src, &["a"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"original");
    assert!(!src.join("a").exists());

    std::fs::write(src.join("b"), "original").unwrap();
    let _seam = LocalFs::seam_before_unlink(replace_entry);
    let r = run(
        &job(Op::Move, &src, &["b"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    assert!(
        matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
            if why.contains("old name remains")),
        "{r:?}"
    );
    assert_eq!(std::fs::read(dst.join("b")).unwrap(), b"original");
    assert_eq!(
        std::fs::read(src.join("b")).unwrap(),
        b"a replacement, longer than the original"
    );
}

/// A copy written into, in place, right after its read-back succeeded (here while the source
/// is checked again) is not what was checked: its mark was taken before the read-back and is
/// never renewed, so the source stays.
#[test]
fn a_copy_overwritten_right_after_its_read_back_keeps_the_source() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        let f = src.join("f");
        std::fs::write(&f, "the right bytes").unwrap();
        let mut fs = Faulty::new(inner);
        fs.rename = no_rename_from(&[&f]);
        let armed = Arc::new(AtomicBool::new(false));
        let (arm, fire) = (armed.clone(), armed.clone());
        let (copy, source, target) = (dst.join("f"), s(&f), s(&dst.join("f")));
        fs.after = Box::new(move |op, p| {
            if op == "read_to" && p == target {
                arm.store(true, Ordering::Relaxed);
            }
        });
        fs.fault = Box::new(move |op, p| {
            if op == "lstat" && p == source && fire.swap(false, Ordering::Relaxed) {
                let mut file = std::fs::OpenOptions::new().write(true).open(&copy).unwrap();
                file.write_all(b"OTHER bytes, and more of them").unwrap();
            }
            None
        });
        let r = run(
            &job(Op::Move, &src, &["f"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        assert!(
            matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
                if why.contains("changed while it was checked; the source stays")),
            "{how}: {r:?}"
        );
        assert_eq!(std::fs::read(&f).unwrap(), b"the right bytes", "{how}");
    });
}

/// A link just made that is replaced before it is marked — here by a regular file — is not
/// the copy: the source link stays.
#[test]
fn a_link_replaced_before_its_mark_keeps_the_source() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        let l = src.join("l");
        std::os::unix::fs::symlink("target", &l).unwrap();
        let mut fs = Faulty::new(inner);
        fs.rename = no_rename_from(&[&l]);
        let made = s(&dst.join("l"));
        fs.after = Box::new(move |op, p| {
            if op == "make_symlink" && p == made {
                std::fs::remove_file(p).unwrap();
                std::fs::write(p, "a file in its place").unwrap();
            }
        });
        let r = run(
            &job(Op::Move, &src, &["l"], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        assert!(
            matches!(&r.items[..], [Item { outcome: ItemOutcome::Failed(why), .. }]
                if why.contains("is not the link that was made any more") && why.contains("stays")),
            "{how}: {r:?}"
        );
        assert_eq!(
            std::fs::read_link(&l).unwrap(),
            PathBuf::from("target"),
            "{how}: the source link stays"
        );
        assert_eq!(
            std::fs::read(dst.join("l")).unwrap(),
            b"a file in its place",
            "{how}"
        );
    });
}

// ---- S6: F7, F8 and SFTP's plain rename (04 §5.4–§5.6) ----

/// A job on the folder `dir` itself (F7, F8).
fn here(op: Op, dir: &Path, items: &[&str]) -> Job {
    job(op, dir, items, dir)
}

/// The failure reason of a report's only item.
fn only_failure(r: &Report) -> String {
    match &r.items[..] {
        [Item {
            outcome: ItemOutcome::Failed(why),
            ..
        }] => why.clone(),
        _ => panic!("not one failed item: {r:?}"),
    }
}

/// The lines of a request log (`sftp-server -e -l INFO`) that hold every one of `words`.
fn log_lines(text: &str, words: &[&str]) -> usize {
    text.lines()
        .filter(|l| words.iter().all(|w| l.contains(w)))
        .count()
}

/// K-K1: F7 `new` makes the folder — with the mode `mkdir` gives, not a copy's 0700 — and F7
/// `new` again is an error with nothing changed, as is a name a file holds; a name with `/` is
/// refused before anything happens. On this machine and through `sftp-server`.
#[test]
fn k_k1_make_folder_then_the_same_name_again_is_an_error() {
    both(|mut fs, how| {
        let (_t, src, _dst) = scratch();
        let r = run(
            &here(Op::MakeDir, &src, &["new"]),
            Hosts::Same(fs.as_mut()),
            &mut Script::default(),
        );
        assert!(r.all_done(), "{how}: {r:?}");
        assert!(src.join("new").is_dir(), "{how}");
        std::fs::create_dir(src.join("by-mkdir")).unwrap();
        assert_eq!(
            mode(&src.join("new")),
            mode(&src.join("by-mkdir")),
            "{how}: the mode mkdir gives"
        );
        std::fs::write(src.join("new/inside"), "x").unwrap();
        let r = run(
            &here(Op::MakeDir, &src, &["new"]),
            Hosts::Same(fs.as_mut()),
            &mut Script::default(),
        );
        assert_eq!(only_failure(&r), "new exists", "{how}");
        assert_eq!(names(&src.join("new")), ["inside"], "{how}");
        std::fs::write(src.join("f"), "f").unwrap();
        let r = run(
            &here(Op::MakeDir, &src, &["f"]),
            Hosts::Same(fs.as_mut()),
            &mut Script::default(),
        );
        assert_eq!(only_failure(&r), "f exists", "{how}");
        assert_eq!(std::fs::read(src.join("f")).unwrap(), b"f", "{how}");
        let r = run(
            &here(Op::MakeDir, &src, &["a/b"]),
            Hosts::Same(fs.as_mut()),
            &mut Script::default(),
        );
        assert!(refusal(&r).contains("'/'"), "{how}: {r:?}");
        assert_eq!(names(&src), ["by-mkdir", "f", "new"], "{how}");
    });
}

/// The folder K-D1 deletes: files, a subfolder two deep, a link to a file outside it, a link to
/// a folder outside it and a dangling link. What lies outside is in `outside`.
fn doomed(src: &Path, outside: &Path) -> PathBuf {
    std::fs::write(outside.join("keep.txt"), "keep").unwrap();
    std::fs::create_dir(outside.join("keepdir")).unwrap();
    std::fs::write(outside.join("keepdir/k"), "k").unwrap();
    let d = src.join("d");
    std::fs::create_dir_all(d.join("sub/deeper")).unwrap();
    std::fs::write(d.join("a"), "a").unwrap();
    std::fs::write(d.join("sub/b"), "b").unwrap();
    std::fs::write(d.join("sub/deeper/c"), "c").unwrap();
    std::os::unix::fs::symlink(outside.join("keep.txt"), d.join("to-file")).unwrap();
    std::os::unix::fs::symlink(outside.join("keepdir"), d.join("sub/to-dir")).unwrap();
    std::os::unix::fs::symlink("nowhere", d.join("dangling")).unwrap();
    d
}

/// What lies outside the doomed folder is untouched.
fn outside_kept(outside: &Path, how: &str) {
    assert_eq!(
        std::fs::read(outside.join("keep.txt")).unwrap(),
        b"keep",
        "{how}"
    );
    assert_eq!(names(&outside.join("keepdir")), ["k"], "{how}");
}

/// K-D1 on a remote (through `sftp-server`, its request log): SFTP has no Trash, so
/// [`trash_here`] says permanent, and a delete to the Trash is refused before anything happens;
/// the permanent delete removes the folder and everything inside it bottom-up — `REMOVE` for
/// files and links, `RMDIR` for each folder after its contents — a link (to a file, to a folder,
/// dangling) as a link, its target untouched; a file and a link given as items too.
#[test]
fn k_d1_a_remote_folder_is_removed_bottom_up_links_as_links() {
    let (_t, src, dst) = scratch();
    let log = src.parent().unwrap().join("server.log");
    let Some(mut fs) = logged_server(&log, &[]) else {
        return;
    };
    assert!(
        matches!(trash_here(&fs, &s(&src)), Deletes::Permanent(why) if why.contains("no Trash")),
        "{:?}",
        trash_here(&fs, &s(&src))
    );
    let d = doomed(&src, &dst);
    std::fs::write(src.join("f"), "f").unwrap();
    std::os::unix::fs::symlink(dst.join("keepdir"), src.join("l")).unwrap();
    let r = run(
        &here(Op::Delete { trash: true }, &src, &["d"]),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(refusal(&r).contains("Trash"), "{r:?}");
    assert!(d.join("sub/deeper/c").exists());
    let r = run(
        &here(Op::Delete { trash: false }, &src, &["d", "f", "l"]),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(names(&src), Vec::<String>::new());
    outside_kept(&dst, "sftp");
    drop(fs);
    let text = std::fs::read_to_string(&log).unwrap();
    let at = |line: &str| {
        text.find(line)
            .unwrap_or_else(|| panic!("no {line:?} in\n{text}"))
    };
    let deepest = at(&format!("remove name \"{}\"", s(&d.join("sub/deeper/c"))));
    let deeper = at(&format!("rmdir name \"{}\"", s(&d.join("sub/deeper"))));
    let link = at(&format!("remove name \"{}\"", s(&d.join("sub/to-dir"))));
    let sub = at(&format!("rmdir name \"{}\"", s(&d.join("sub"))));
    let top = at(&format!("rmdir name \"{}\"", s(&d)));
    assert!(
        deepest < deeper && deeper < sub && link < sub && sub < top,
        "{text}"
    );
    at(&format!("remove name \"{}\"", s(&src.join("l"))));
    assert!(!text.contains("keepdir/k"), "never inside a link: {text}");
    assert!(!text.contains("posix-rename"), "{text}");
}

/// K-D1 on this machine: F8 puts a file, a folder (whole) and a link (as a link) into the Trash —
/// in tests a stand-in, never the owner's Trash: each is gone from its folder and there with
/// what it held; [`trash_here`] says Trash.
#[test]
fn k_d1_a_local_file_goes_to_the_trash() {
    let (_t, src, dst) = scratch();
    std::fs::write(src.join("a.txt"), "bytes of a").unwrap();
    let d = doomed(&src, &dst);
    std::os::unix::fs::symlink("a.txt", src.join("l")).unwrap();
    let mut fs = LocalFs::new();
    assert_eq!(trash_here(&fs, &s(&src)), Deletes::Trash);
    let r = run(
        &here(Op::Delete { trash: true }, &src, &["a.txt", "d", "l"]),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(names(&src), Vec::<String>::new());
    let trashed = |p: &Path| {
        PathBuf::from(
            LocalFs::trashed_by_tests(&s(p)).unwrap_or_else(|| panic!("{p:?} is not in the Trash")),
        )
    };
    assert_eq!(
        std::fs::read(trashed(&src.join("a.txt"))).unwrap(),
        b"bytes of a"
    );
    let d_at = trashed(&d);
    assert_eq!(std::fs::read(d_at.join("sub/deeper/c")).unwrap(), b"c");
    let l_at = trashed(&src.join("l"));
    assert_eq!(std::fs::read_link(&l_at).unwrap(), PathBuf::from("a.txt"));
    outside_kept(&dst, "local");
}

/// A Trash that refuses: F8 then fails that item with the reason and removes nothing of it —
/// never a permanent delete instead.
#[test]
fn a_trash_that_refuses_removes_nothing() {
    struct Refusing;
    impl crate::remote::ops::Trash for Refusing {
        fn available(&self, _dir: &str) -> Result<(), String> {
            Ok(())
        }
        fn put(&self, path: &str) -> anyhow::Result<String> {
            Err(anyhow::anyhow!("the Trash refused {path} (a test's)"))
        }
    }
    let (_t, src, dst) = scratch();
    let d = doomed(&src, &dst);
    std::fs::write(src.join("f"), "f").unwrap();
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    fs.trash = Some(Box::new(Refusing));
    let r = run(
        &here(Op::Delete { trash: true }, &src, &["d", "f"]),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    for (i, name) in ["d", "f"].iter().enumerate() {
        assert_eq!(r.items[i].name, *name);
        assert!(
            matches!(&r.items[i].outcome, ItemOutcome::Failed(why)
                if why.contains("the Trash refused") && why.contains("nothing")),
            "{r:?}"
        );
    }
    assert!(d.join("sub/deeper/c").exists());
    assert_eq!(std::fs::read(src.join("f")).unwrap(), b"f");
    let trace = fs.trace.lock().unwrap().clone();
    assert!(
        !trace
            .iter()
            .any(|t| t.starts_with("remove") || t.starts_with("rmdir")),
        "{trace:?}"
    );
}

/// Shift+F8 on this machine: permanent, bottom-up, a link to a file and one to a folder removed
/// as links — never their targets — and nothing goes to the Trash.
#[test]
fn shift_f8_deletes_locally_without_the_trash() {
    let (_t, src, dst) = scratch();
    let d = doomed(&src, &dst);
    let looked = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = looked.clone();
    let _seam = crate::remote::ops::seam_tree_looked(move |p| seen.lock().unwrap().push(p.into()));
    let mut fs = Faulty::new(Box::new(LocalFs::new()));
    let r = run(
        &here(Op::Delete { trash: false }, &src, &["d"]),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(names(&src), Vec::<String>::new());
    outside_kept(&dst, "local");
    assert_eq!(LocalFs::trashed_by_tests(&s(&d)), None);
    let looked = looked.lock().unwrap().clone();
    assert!(looked.contains(&s(&d.join("sub/to-dir"))), "{looked:?}");
    assert!(
        !looked.iter().any(|p| p.starts_with(&s(&dst))),
        "never inside a link: {looked:?}"
    );
}

/// A permanent delete that cannot remove everything — a read-only subfolder's entry — removes
/// what it can and keeps the rest with its folders: the item fails, each entry kept has a line
/// with the reason. On this machine and through `sftp-server`.
#[test]
fn a_delete_keeps_what_it_cannot_remove_and_lists_it() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: root ignores a read-only folder");
        return;
    }
    both(|mut fs, how| {
        let (_t, src, _dst) = scratch();
        let d = src.join("d");
        std::fs::create_dir_all(d.join("locked")).unwrap();
        std::fs::write(d.join("free.txt"), "free").unwrap();
        std::fs::write(d.join("locked/x"), "x").unwrap();
        set_mode(&d.join("locked"), 0o500);
        let r = run(
            &here(Op::Delete { trash: false }, &src, &["d"]),
            Hosts::Same(fs.as_mut()),
            &mut Script::default(),
        );
        set_mode(&d.join("locked"), 0o700);
        assert_eq!(r.items[0].name, "d", "{how}: {r:?}");
        assert!(
            matches!(&r.items[0].outcome, ItemOutcome::Failed(why) if why.contains("stay")),
            "{how}: {r:?}"
        );
        for kept in ["d/locked/x", "d/locked"] {
            assert!(
                r.items
                    .iter()
                    .any(|i| i.name == kept && matches!(i.outcome, ItemOutcome::Failed(_))),
                "{how}: {kept}: {r:?}"
            );
        }
        assert_eq!(names(&d), ["locked"], "{how}: free.txt went");
        assert_eq!(names(&d.join("locked")), ["x"], "{how}");
    });
}

/// Esc at a delete's first item: nothing of it is removed, and the items after it are not
/// reached.
#[test]
fn esc_stops_a_delete_between_items() {
    let (_t, src, _dst) = scratch();
    std::fs::write(src.join("a"), "a").unwrap();
    std::fs::write(src.join("b"), "b").unwrap();
    let mut ctl = Script {
        stop_when: Some("deleting"),
        ..Script::default()
    };
    let r = run(
        &here(Op::Delete { trash: false }, &src, &["a", "b"]),
        Hosts::Same(&mut LocalFs::new()),
        &mut ctl,
    );
    assert!(stop_reason(&r.items[0]).contains("nothing"), "{r:?}");
    assert_eq!(r.items[1], item("b", ItemOutcome::NotReached));
    assert_eq!(names(&src), ["a", "b"]);
}

/// Esc inside a folder being deleted permanently: it stops between entries — what was removed
/// stays removed, the rest stays with its folder — and the item says it is partly deleted.
#[test]
fn esc_inside_a_folder_leaves_it_partly_deleted() {
    let (_t, src, _dst) = scratch();
    let d = src.join("d");
    std::fs::create_dir(&d).unwrap();
    for name in ["a", "b", "c"] {
        std::fs::write(d.join(name), name).unwrap();
    }
    let mut ctl = Script::default();
    let esc = ctl.cancel.clone();
    // Esc is pressed while the second entry is looked at, after the first was removed.
    let inside = format!("{}/", s(&d));
    let mut entries = 0;
    let _seam = crate::remote::ops::seam_tree_looked(move |p| {
        if p.starts_with(&inside) {
            entries += 1;
            if entries == 2 {
                esc.store(true, Ordering::Relaxed);
            }
        }
    });
    let r = run(
        &here(Op::Delete { trash: false }, &src, &["d"]),
        Hosts::Same(&mut LocalFs::new()),
        &mut ctl,
    );
    let why = stop_reason(&r.items[0]);
    assert!(
        why.contains("partly deleted") && why.contains("1 of its entries"),
        "{why}"
    );
    assert_eq!(names(&d).len(), 2);
}

/// K-N1 for a remote folder from S6, through `sftp-server` and the session's plain-rename
/// channel (both request logs): F2 renames it with one plain `SSH_FXP_RENAME`; onto an existing
/// name — a folder, a file, a dangling link — an error, both left; F2 of a link onto a name a
/// dangling link holds: an error, the dangling link kept, and no `SSH_FXP_RENAME` sent for it.
/// Never `posix-rename@openssh.com`.
#[test]
fn k_n1_a_remote_folder_renames_by_one_plain_rename() {
    let (_t, src, _dst) = scratch();
    let root = src.parent().unwrap().to_path_buf();
    let (log, plain_log) = (root.join("server.log"), root.join("plain.log"));
    let Some(mut sftp) = logged_server(&log, &[]) else {
        return;
    };
    let Some(plain) = SftpRemote::seam_plain(&[], Some(&plain_log), false) else {
        return;
    };
    std::fs::create_dir(src.join("d")).unwrap();
    std::fs::write(src.join("d/x"), "x").unwrap();
    std::fs::create_dir(src.join("taken")).unwrap();
    std::fs::write(src.join("taken/keep"), "keep").unwrap();
    std::fs::write(src.join("f"), "f").unwrap();
    std::os::unix::fs::symlink("nowhere", src.join("dl")).unwrap();
    std::os::unix::fs::symlink("x", src.join("ln")).unwrap();
    let rename = |from: &str, to: &str| job(Op::Rename { to: to.into() }, &src, &[from], &src);
    let ino_d = ino(&src.join("d"));
    let r = run(
        &rename("d", "e"),
        Hosts::Same(&mut sftp),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(ino(&src.join("e")), ino_d, "renamed, not copied");
    assert_eq!(names(&src.join("e")), ["x"]);
    for onto in ["taken", "f", "dl"] {
        let r = run(
            &rename("e", onto),
            Hosts::Same(&mut sftp),
            &mut Script::default(),
        );
        assert_eq!(only_failure(&r), format!("{onto} exists"));
    }
    let r = run(
        &rename("ln", "dl"),
        Hosts::Same(&mut sftp),
        &mut Script::default(),
    );
    assert_eq!(only_failure(&r), "dl exists");
    assert_eq!(names(&src), ["dl", "e", "f", "ln", "taken"]);
    assert_eq!(names(&src.join("taken")), ["keep"]);
    assert_eq!(std::fs::read(src.join("f")).unwrap(), b"f");
    assert_eq!(
        std::fs::read_link(src.join("dl")).unwrap(),
        PathBuf::from("nowhere")
    );
    assert_eq!(
        std::fs::read_link(src.join("ln")).unwrap(),
        PathBuf::from("x")
    );
    drop(sftp);
    drop(plain);
    let main = std::fs::read_to_string(&log).unwrap();
    let plain = std::fs::read_to_string(&plain_log).unwrap();
    eprintln!("main:\n{main}\nplain:\n{plain}");
    assert_eq!(log_lines(&plain, &["rename old"]), 4, "{plain}");
    assert_eq!(log_lines(&plain, &["rename old", "/ln\""]), 0, "{plain}");
    assert!(!main.contains("rename old"), "{main}");
    assert!(!main.contains("posix-rename") && !plain.contains("posix-rename"));
}

/// K-M1 for a remote folder from S6: F6 within one filesystem is one plain `SSH_FXP_RENAME` —
/// the folder keeps its inode, nothing is copied or removed — and a folder name in the way asks
/// (Skip leaves both); never `posix-rename@openssh.com`.
#[test]
fn k_m1_a_remote_folder_moves_by_one_plain_rename() {
    let (_t, src, dst) = scratch();
    let root = src.parent().unwrap().to_path_buf();
    let (log, plain_log) = (root.join("server.log"), root.join("plain.log"));
    let Some(mut fs) = logged_server(&log, &[]) else {
        return;
    };
    let Some(plain) = SftpRemote::seam_plain(&[], Some(&plain_log), false) else {
        return;
    };
    std::fs::create_dir_all(src.join("d/sub")).unwrap();
    std::fs::write(src.join("d/sub/c"), "c").unwrap();
    std::fs::create_dir(src.join("e")).unwrap();
    std::fs::create_dir(dst.join("e")).unwrap();
    let before = ino(&src.join("d"));
    let mut ctl = Script::answers([reply(Answer::Skip)]);
    let r = run(
        &job(Op::Move, &src, &["d", "e"], &dst),
        Hosts::Same(&mut fs),
        &mut ctl,
    );
    assert_eq!(
        r.items,
        vec![
            item("d", ItemOutcome::Done),
            item("e", ItemOutcome::Skipped)
        ]
    );
    assert_eq!(ino(&dst.join("d")), before);
    assert_eq!(std::fs::read(dst.join("d/sub/c")).unwrap(), b"c");
    assert_eq!(names(&src), ["e"]);
    drop(fs);
    drop(plain);
    let main = std::fs::read_to_string(&log).unwrap();
    let plain = std::fs::read_to_string(&plain_log).unwrap();
    assert_eq!(log_lines(&plain, &["rename old"]), 1, "{plain}");
    for word in [
        "hardlink old",
        "remove name",
        "rmdir name",
        "mkdir name",
        "rename old",
    ] {
        assert!(!main.contains(word), "{word}: {main}");
    }
    assert!(!main.contains("posix-rename") && !plain.contains("posix-rename"));
}

/// K-C4 on a real server without `hardlink@openssh.com` (`sftp-server -P hardlink`), from S6,
/// with the session's plain-rename channel (to the same server): the server's rename is probed
/// once per session — `MKDIR` of `.sopen-probe-…`, two exclusive creates, one plain
/// `SSH_FXP_RENAME` answered with status 4, the read-back, two `REMOVE`, one `RMDIR` — and each
/// temp is published by one plain `SSH_FXP_RENAME`; a name in the way still asks (before the
/// copy, and at its publish); no `.sopen-probe-` entry is left; a move in the same session is a
/// copy, a check and a removal published the same way, without a second probe.
#[test]
fn k_c4_without_hardlink_publishes_through_the_probed_plain_rename() {
    let (_t, src, dst) = scratch();
    let root = src.parent().unwrap().to_path_buf();
    let (log, plain_log) = (root.join("server.log"), root.join("plain.log"));
    let Some(sftp) = logged_server(&log, &["-P", "hardlink"]) else {
        return;
    };
    assert!(!sftp.can_link(), "-P hides the extension");
    let Some(plain) = SftpRemote::seam_plain(&["-P", "hardlink"], Some(&plain_log), false) else {
        return;
    };
    for name in ["a", "b", "c", "m"] {
        std::fs::write(src.join(name), format!("source {name}")).unwrap();
    }
    std::fs::write(dst.join("c"), "theirs c").unwrap();
    // `b`'s name is taken at its publish, between its copy and the rename.
    let mut fs = Faulty::new(Box::new(sftp));
    let taken_b = s(&dst.join("b"));
    fs.rename = Box::new(move |_, to| {
        if to == taken_b && !Path::new(&taken_b).exists() {
            std::fs::write(&taken_b, "theirs b").unwrap();
        }
        None
    });
    // `b` is asked about at its publish, `c` before its copy.
    let mut ctl = Script::answers([
        reply(Answer::Rename("b-1".into())),
        reply(Answer::Rename("c-1".into())),
    ]);
    let r = run(
        &job(Op::Copy, &src, &["a", "b", "c"], &dst),
        Hosts::Same(&mut fs),
        &mut ctl,
    );
    assert_eq!(
        r.items,
        vec![
            item("a", ItemOutcome::Done),
            item("b", ItemOutcome::Renamed("b-1".into())),
            item("c", ItemOutcome::Renamed("c-1".into())),
        ],
        "{r:?}"
    );
    assert_eq!(ctl.asked.len(), 2, "{:?}", ctl.asked);
    assert_eq!(std::fs::read(dst.join("b")).unwrap(), b"theirs b");
    assert_eq!(std::fs::read(dst.join("c")).unwrap(), b"theirs c");
    assert_eq!(std::fs::read(dst.join("b-1")).unwrap(), b"source b");
    assert_eq!(std::fs::read(dst.join("c-1")).unwrap(), b"source c");
    let r = run(
        &job(Op::Move, &src, &["m"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(std::fs::read(dst.join("m")).unwrap(), b"source m");
    assert_eq!(names(&src), ["a", "b", "c"]);
    assert_eq!(temps(&dst), Vec::<String>::new());
    drop(fs);
    drop(plain);
    let main = std::fs::read_to_string(&log).unwrap();
    let plain = std::fs::read_to_string(&plain_log).unwrap();
    eprintln!("main:\n{main}\nplain:\n{plain}");
    let probe = ".sopen-probe-";
    assert_eq!(log_lines(&main, &["mkdir name", probe]), 1, "{main}");
    assert_eq!(
        log_lines(&main, &["open", probe, "CREATE,EXCL"]),
        2,
        "{main}"
    );
    assert_eq!(log_lines(&main, &["remove name", probe]), 2, "{main}");
    assert_eq!(log_lines(&main, &["rmdir name", probe]), 1, "{main}");
    assert_eq!(log_lines(&plain, &["rename old", probe]), 1, "{plain}");
    let after_probe = plain
        .lines()
        .skip_while(|l| !(l.contains("rename old") && l.contains(probe)))
        .nth(1)
        .unwrap_or_default();
    assert!(after_probe.contains("sent status Failure"), "{plain}");
    // a, b (refused at its publish), b-1, c-1 and m.
    assert_eq!(log_lines(&plain, &["rename old", ".sopen-"]), 6, "{plain}");
    assert!(!main.contains("rename old"), "{main}");
    assert!(!main.contains("posix-rename") && !plain.contains("posix-rename"));
}

/// K-C4's other half: the same server behind a tap that answers the probe's rename as done, as
/// a server whose plain rename replaces would: the copy is refused with the reason, nothing
/// written over and nothing staged, no `.sopen-probe-` entry left — and every later copy of the
/// session is refused the same, without a second probe.
#[test]
fn k_c4_a_server_whose_plain_rename_replaces_is_refused() {
    let (_t, src, dst) = scratch();
    let log = src.parent().unwrap().join("server.log");
    let Some(mut fs) = logged_server(&log, &["-P", "hardlink"]) else {
        return;
    };
    let Some(plain) = SftpRemote::seam_plain(&["-P", "hardlink"], None, true) else {
        return;
    };
    std::fs::write(src.join("a"), "source a").unwrap();
    std::fs::write(src.join("b"), "source b").unwrap();
    std::fs::write(dst.join("z"), "theirs").unwrap();
    for name in ["a", "b"] {
        let r = run(
            &job(Op::Copy, &src, &[name], &dst),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        let why = only_failure(&r);
        assert!(
            why.contains("plain rename replaces an existing name"),
            "{name}: {why}"
        );
    }
    assert_eq!(names(&dst), ["z"]);
    assert_eq!(std::fs::read(dst.join("z")).unwrap(), b"theirs");
    assert_eq!(names(&src), ["a", "b"]);
    drop(fs);
    drop(plain);
    let main = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        log_lines(&main, &["mkdir name", ".sopen-probe-"]),
        1,
        "{main}"
    );
    assert_eq!(
        log_lines(&main, &["rmdir name", ".sopen-probe-"]),
        1,
        "{main}"
    );
    assert_eq!(
        log_lines(&main, &["open", "/.a.sopen-"]),
        0,
        "nothing staged: {main}"
    );
    assert_eq!(
        log_lines(&main, &["open", "/.b.sopen-"]),
        0,
        "nothing staged: {main}"
    );
}

/// A probe that cannot run (here the target folder is read-only, so its `MKDIR` is refused)
/// refuses that copy with the reason and records nothing: the next copy of the session probes
/// again, and publishes.
#[test]
fn a_probe_that_cannot_run_refuses_the_copy_and_is_tried_again() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: root ignores a read-only folder");
        return;
    }
    let (_t, src, dst) = scratch();
    let log = src.parent().unwrap().join("server.log");
    let Some(mut fs) = logged_server(&log, &["-P", "hardlink"]) else {
        return;
    };
    let Some(plain) = SftpRemote::seam_plain(&["-P", "hardlink"], None, false) else {
        return;
    };
    std::fs::write(src.join("a"), "source a").unwrap();
    set_mode(&dst, 0o500);
    let r = run(
        &job(Op::Copy, &src, &["a"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    set_mode(&dst, 0o700);
    let why = only_failure(&r);
    assert!(why.contains("probe") && why.contains("could not"), "{why}");
    assert_eq!(names(&dst), Vec::<String>::new());
    let r = run(
        &job(Op::Copy, &src, &["a"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"source a");
    drop(fs);
    drop(plain);
    let main = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        log_lines(&main, &["mkdir name", ".sopen-probe-"]),
        2,
        "{main}"
    );
}

// ---- S6-A review round 1: delete races, stop, probe ledger ----

/// A folder to delete holding `sub` (with a file), and a folder outside it with a sentinel.
fn swap_fixture(src: &Path, dst: &Path) -> (PathBuf, PathBuf) {
    let d = src.join("d");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("sub/x"), "x").unwrap();
    std::fs::write(d.join("a"), "a").unwrap();
    let keep = dst.join("keep");
    std::fs::create_dir(&keep).unwrap();
    std::fs::write(keep.join("sentinel"), "sentinel").unwrap();
    (d, keep)
}

/// A folder inside a folder being deleted permanently is swapped for a link to a folder
/// outside the selection between the walk's look at it and its descent (another process of the
/// account): nothing outside is removed — on this machine the walk goes through folder
/// descriptors (`O_NOFOLLOW`); over SFTP each request is made only while the folder it goes
/// through still resolves where it was listed. The link stays, named, and so does `d`.
#[test]
fn a_folder_swapped_for_a_link_mid_delete_is_never_gone_through() {
    both(|mut fs, how| {
        let (_t, src, dst) = scratch();
        let (d, keep) = swap_fixture(&src, &dst);
        let (sub, away) = (s(&d.join("sub")), src.join("moved-away"));
        let target = keep.clone();
        let _seam = crate::remote::ops::seam_tree_looked(move |p| {
            if p == sub && !Path::new(&sub).is_symlink() {
                std::fs::rename(&sub, &away).unwrap();
                std::os::unix::fs::symlink(&target, &sub).unwrap();
            }
        });
        let r = run(
            &here(Op::Delete { trash: false }, &src, &["d"]),
            Hosts::Same(fs.as_mut()),
            &mut Script::default(),
        );
        assert_eq!(names(&keep), ["sentinel"], "{how}: {r:?}");
        assert_eq!(std::fs::read(keep.join("sentinel")).unwrap(), b"sentinel");
        assert!(d.join("sub").is_symlink(), "{how}: the link stays");
        assert_eq!(
            std::fs::read(src.join("moved-away/x")).unwrap(),
            b"x",
            "{how}"
        );
        assert!(
            r.items
                .iter()
                .any(|i| i.name == "d/sub" && matches!(i.outcome, ItemOutcome::Failed(_))),
            "{how}: {r:?}"
        );
        assert!(
            matches!(&r.items[0].outcome, ItemOutcome::Failed(why) if why.contains("stay")),
            "{how}: {r:?}"
        );
    });
}

/// On this machine a folder swapped for another folder between the look and the descent is
/// not the folder that was looked at (its device and inode): nothing in it is removed.
#[test]
fn a_folder_swapped_for_another_folder_mid_delete_keeps_the_newcomer() {
    let (_t, src, dst) = scratch();
    let (d, _keep) = swap_fixture(&src, &dst);
    let (sub, away) = (s(&d.join("sub")), src.join("moved-away"));
    let _seam = crate::remote::ops::seam_tree_looked(move |p| {
        if p == sub && !away.exists() {
            std::fs::rename(&sub, &away).unwrap();
            std::fs::create_dir(&sub).unwrap();
            std::fs::write(Path::new(&sub).join("theirs"), "theirs").unwrap();
        }
    });
    let r = run(
        &here(Op::Delete { trash: false }, &src, &["d"]),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    assert_eq!(
        std::fs::read(d.join("sub/theirs")).unwrap(),
        b"theirs",
        "{r:?}"
    );
    assert!(
        r.items.iter().any(|i| i.name == "d/sub"
            && matches!(&i.outcome, ItemOutcome::Failed(why) if why.contains("another folder"))),
        "{r:?}"
    );
}

/// Esc while a delete looks at an entry (its `lstat` under way): no removal starts after it —
/// not the item's own `REMOVE`, not the Trash's move, not an entry inside a folder.
#[test]
fn a_stop_during_a_look_starts_no_removal() {
    // The item's own look.
    for trash in [false, true] {
        let (_t, src, _dst) = scratch();
        std::fs::write(src.join("a"), "a").unwrap();
        let mut ctl = Script::default();
        let esc = ctl.cancel.clone();
        let a = s(&src.join("a"));
        let mut fs = Faulty::new(Box::new(LocalFs::new()));
        fs.fault = Box::new(move |op, p| {
            if op == "lstat" && p == a {
                esc.store(true, Ordering::Relaxed);
            }
            None
        });
        let r = run(
            &here(Op::Delete { trash }, &src, &["a"]),
            Hosts::Same(&mut fs),
            &mut ctl,
        );
        assert!(stop_reason(&r.items[0]).contains("nothing"), "{r:?}");
        assert_eq!(std::fs::read(src.join("a")).unwrap(), b"a", "trash={trash}");
        assert_eq!(LocalFs::trashed_by_tests(&s(&src.join("a"))), None);
        let trace = fs.trace.lock().unwrap().clone();
        assert!(!trace.iter().any(|t| t.starts_with("remove")), "{trace:?}");
    }
    // An entry's look inside a folder, on both file layers.
    both(|mut fs, how| {
        let (_t, src, _dst) = scratch();
        let d = src.join("d");
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join("x"), "x").unwrap();
        let mut ctl = Script::default();
        let esc = ctl.cancel.clone();
        let x = s(&d.join("x"));
        let _seam = crate::remote::ops::seam_tree_looked(move |p| {
            if p == x {
                esc.store(true, Ordering::Relaxed);
            }
        });
        let r = run(
            &here(Op::Delete { trash: false }, &src, &["d"]),
            Hosts::Same(fs.as_mut()),
            &mut ctl,
        );
        assert!(stop_reason(&r.items[0]).contains("nothing"), "{how}: {r:?}");
        assert_eq!(std::fs::read(d.join("x")).unwrap(), b"x", "{how}");
    });
}

/// K-C4 against a server whose plain rename really replaces (a tap sends it as
/// `posix-rename@openssh.com`): the copy is refused with the reason, nothing of it is staged or
/// written over, and what the probe could not take back — its second file, now holding the
/// first's bytes, and its folder — is named in the reason.
#[test]
fn k_c4_a_server_that_really_replaces_is_refused_and_the_probes_leftovers_named() {
    let (_t, src, dst) = scratch();
    let Some(mut fs) = SftpRemote::local_server_args(&["-P", "hardlink"], None) else {
        return;
    };
    let Some(plain) = SftpRemote::seam_plain_replacing(&["-P", "hardlink"], None) else {
        return;
    };
    std::fs::write(src.join("a"), "source a").unwrap();
    std::fs::write(dst.join("z"), "theirs").unwrap();
    let r = run(
        &job(Op::Copy, &src, &["a"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    let why = only_failure(&r);
    assert!(
        why.contains("plain rename replaces an existing name"),
        "{why}"
    );
    assert!(why.contains("sopen's rename probe left"), "{why}");
    let probe: Vec<String> = names(&dst)
        .into_iter()
        .filter(|n| n.starts_with(".sopen-probe-"))
        .collect();
    assert_eq!(probe.len(), 1, "{:?}", names(&dst));
    assert_eq!(names(&dst.join(&probe[0])), ["b"]);
    assert_eq!(std::fs::read(dst.join("z")).unwrap(), b"theirs");
    assert!(!names(&dst).contains(&"a".to_string()));
    drop(plain);
}

/// The selected folder itself is swapped for a link to a folder outside the selection, right
/// after the walk's look at it: nothing outside is removed — on this machine the folder is
/// opened without following a link; over SFTP its canonical path is taken before the look, so
/// the link does not resolve there.
#[test]
fn the_selected_folder_swapped_for_a_link_mid_delete_is_never_gone_through() {
    both(|mut fs, how| {
        let (_t, src, dst) = scratch();
        let (d, keep) = swap_fixture(&src, &dst);
        let (top, away) = (s(&d), src.join("moved-away"));
        let target = keep.clone();
        let _seam = crate::remote::ops::seam_tree_looked(move |p| {
            if p == top && !Path::new(&top).is_symlink() {
                std::fs::rename(&top, &away).unwrap();
                std::os::unix::fs::symlink(&target, &top).unwrap();
            }
        });
        let r = run(
            &here(Op::Delete { trash: false }, &src, &["d"]),
            Hosts::Same(fs.as_mut()),
            &mut Script::default(),
        );
        assert_eq!(names(&keep), ["sentinel"], "{how}: {r:?}");
        assert!(d.is_symlink(), "{how}: the link stays");
        assert_eq!(
            std::fs::read(src.join("moved-away/sub/x")).unwrap(),
            b"x",
            "{how}"
        );
        assert!(
            matches!(&r.items[0].outcome, ItemOutcome::Failed(_)),
            "{how}: {r:?}"
        );
    });
}
