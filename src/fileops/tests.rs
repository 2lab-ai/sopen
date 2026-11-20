//! The engine's part of 04 §8 (K-C1–K-C5, K-M1, K-M2, K-N1, K-E1), each on this machine's file
//! layer and through this machine's own OpenSSH `sftp-server` (a real server over pipes; skipped
//! with a note where it is not installed, a failure under `SOPEN_REQUIRE_SFTP_SERVER`). Faults
//! and races come from [`Faulty`], a file layer with hooks — never from sleeps.

use super::*;
use crate::remote::ops::{taken, FileMeta, NoRename, Opened, Publish, Renamed, Staged, COPY_STEP};
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
        if self.stop_on_ask {
            self.cancel.store(true, Ordering::Relaxed);
        }
        self.replies
            .pop_front()
            .unwrap_or_else(|| panic!("an unscripted question: {clash:?}"))
    }
}

/// Answers a rename before it reaches the disk (`None`: let it through).
type RenameHook<'a> = Box<dyn FnMut(&str, &str) -> Option<anyhow::Result<Renamed>> + 'a>;
/// Runs right after a rename that went through.
type RenamedHook<'a> = Box<dyn FnMut(&str, &str) + 'a>;
/// Fails a file's copy, by its source path (`None`: let it through).
type CopyHook<'a> = Box<dyn FnMut(&str) -> Option<anyhow::Error> + 'a>;
/// Edits a folder's listing.
type ListHook<'a> = Box<dyn FnMut(&str, &mut Vec<String>) + 'a>;
/// Runs right after a same-host copy into its temp succeeded, with the temp's path.
type CopiedHook<'a> = Box<dyn FnMut(&str) + 'a>;
/// Fails a call before it reaches the disk, by the call's name and its first path (`None`: let
/// it through) — also where a test does something at that exact step.
type FaultHook<'a> = Box<dyn FnMut(&str, &str) -> Option<anyhow::Error> + 'a>;
/// Runs right after a call went through, by the call's name and its first path.
type AfterHook<'a> = Box<dyn FnMut(&str, &str) + 'a>;
/// A file's inode and SHA-256: what proves it untouched.
type Ident = (u64, [u8; 32]);
/// A change made to a file at a test's step.
type Change = dyn Fn(&Path);
/// One file layer, owned.
type Layer = Box<dyn FileOps>;
/// Opens a file layer, or `None` where it is missing (no `sftp-server`).
type Opener = Box<dyn FnMut() -> Option<Box<dyn FileOps>>>;

/// A file layer that hands every call to `inner`, with faults and hooks (04 §8's "fault-injecting
/// file layer"): a rename answered or failed before it reaches the disk, something done right
/// after a rename (a publish, a move), a file's copy failed, a listing edited, a `realpath` that
/// resolves nothing, a server without `hardlink@openssh.com`.
struct Faulty<'a> {
    inner: &'a mut dyn FileOps,
    rename: RenameHook<'a>,
    renamed: RenamedHook<'a>,
    copy: CopyHook<'a>,
    copied: CopiedHook<'a>,
    list: ListHook<'a>,
    /// `identity`, `lstat`, `remove`, `remove_dir`, `list_entries`.
    fault: FaultHook<'a>,
    /// `make_symlink` (its link), `set_meta`, `read_to`, `open_read`.
    after: AfterHook<'a>,
    plain_realpath: bool,
    no_link: bool,
    /// A lower [`RemoteFs::hash_check_limit`].
    hash_limit: Option<u64>,
    /// A same-host copy is reported as made by the host itself (no digest of its bytes), as
    /// `copy-data` and `copy_file_range` are: a move then reads it back.
    host_made: bool,
    /// Barriers asked of this layer ([`FileOps::barrier`]).
    barriers: Arc<AtomicUsize>,
    /// No filesystem id ([`FileOps::fs_id`]): two connections cannot tell they share one.
    no_fs_id: bool,
    /// The stream a copy from another host writes is corrupted (its first byte flipped) on its
    /// way into the temp, while the reader's digest is of the bytes read.
    corrupt: bool,
}

impl<'a> Faulty<'a> {
    fn new(inner: &'a mut dyn FileOps) -> Self {
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
            no_fs_id: false,
            corrupt: false,
        }
    }
}

impl RemoteFs for Faulty<'_> {
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
        self.inner.remove(path)
    }
    fn hash_check_limit(&self) -> u64 {
        self.hash_limit
            .unwrap_or_else(|| self.inner.hash_check_limit())
    }
}

impl FileOps for Faulty<'_> {
    fn identity(&mut self, path: &str, follow: bool) -> anyhow::Result<Option<(u64, u64)>> {
        if let Some(e) = (self.fault)("identity", path) {
            return Err(e);
        }
        self.inner.identity(path, follow)
    }
    fn meta(&mut self, path: &str) -> anyhow::Result<FileMeta> {
        self.inner.meta(path)
    }
    fn rename_new(&mut self, from: &str, to: &str) -> anyhow::Result<Renamed> {
        if let Some(answer) = (self.rename)(from, to) {
            return answer;
        }
        let done = self.inner.rename_new(from, to)?;
        (self.renamed)(from, to);
        Ok(done)
    }
    fn can_rename_dir(&self) -> bool {
        self.inner.can_rename_dir()
    }
    fn publish_mode(&mut self, dir: &str) -> anyhow::Result<Publish> {
        self.inner.publish_mode(dir)
    }
    fn barrier(&mut self, dir: &str) -> anyhow::Result<()> {
        self.barriers.fetch_add(1, Ordering::Relaxed);
        self.inner.barrier(dir)
    }
    fn fs_id(&mut self, path: &str) -> Option<u64> {
        if self.no_fs_id {
            return None;
        }
        self.inner.fs_id(path)
    }
    fn make_dir(&mut self, path: &str) -> anyhow::Result<()> {
        self.inner.make_dir(path)
    }
    fn remove_dir(&mut self, path: &str) -> anyhow::Result<()> {
        if let Some(e) = (self.fault)("remove_dir", path) {
            return Err(e);
        }
        self.inner.remove_dir(path)
    }
    fn read_link(&mut self, path: &str) -> anyhow::Result<String> {
        self.inner.read_link(path)
    }
    fn make_symlink(&mut self, target: &str, link: &str) -> anyhow::Result<()> {
        self.inner.make_symlink(target, link)?;
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
    ) -> Result<Staged, CreateError> {
        if let Some(e) = (self.copy)(from) {
            return Err(CreateError::new(crate::remote::Created::No, e));
        }
        let mut staged = self.inner.copy_within(from, temp, cancel, progress)?;
        if self.host_made {
            staged.sha256 = None;
        }
        (self.copied)(temp);
        Ok(staged)
    }
    fn open_read(&mut self, path: &str) -> anyhow::Result<Opened<'_>> {
        if let Some(e) = (self.copy)(path) {
            return Err(e);
        }
        let opened = self.inner.open_read(path)?;
        (self.after)("open_read", path);
        Ok(opened)
    }
    fn stage_from(
        &mut self,
        src: &mut dyn Read,
        meta: &FileMeta,
        temp: &str,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<Staged, CreateError> {
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
        (self.copied)(temp);
        Ok(staged)
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
fn both(test: impl Fn(&mut dyn FileOps, &str)) {
    test(&mut LocalFs::new(), "local");
    if let Some(mut sftp) = SftpRemote::local_server() {
        test(&mut sftp, "sftp");
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
    both(|fs, how| {
        let (_t, src, dst) = scratch();
        let a = src.join("a.txt");
        std::fs::write(&a, b"hello copy").unwrap();
        set_mode(&a, 0o640);
        set_mtime(&a, 1_600_000_000);
        let before = (sha(&a), ino(&a), mtime_secs(&a));
        let mut ctl = Script::default();
        let report = run(
            &job(Op::Copy, &src, &["a.txt"], &dst),
            Hosts::Same(fs),
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
    both(|fs, how| {
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
                item("a.txt", ItemOutcome::Skipped),
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
    both(|fs, how| {
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
            Hosts::Same(fs),
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
    let mut local = LocalFs::new();
    for op in [Op::Copy, Op::Move] {
        let mut fs = Faulty::new(&mut local);
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
                SftpRemote::local_server_with(&["-P", "copy-data"], None).map(|f| Box::new(f) as _)
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
    SftpRemote::local_server_with(args, Some(log))
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
    let calls = AtomicUsize::new(0);
    let fail_back = AtomicBool::new(false);
    let mut local = LocalFs::new();
    let mut run_with_faults = |back_fails: bool| {
        calls.store(0, Ordering::Relaxed);
        fail_back.store(back_fails, Ordering::Relaxed);
        let mut fs = Faulty::new(&mut local);
        let (calls, fail_back, readme) = (&calls, &fail_back, readme_s.clone());
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
    assert!(now[0].contains(".sopen-"), "{now:?}");
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
        let mut fs = Faulty::new(&mut sftp);
        fs.list = Box::new(|_, names| names.push("README".into()));
        let r = run(
            &rename("readme", "README"),
            Hosts::Same(&mut fs),
            &mut Script::default(),
        );
        assert!(failed(&r).contains("exists"), "{r:?}");
        assert!(names(&src).contains(&"readme".to_string()));
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
    }
    drop(sftp);
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
    both(|inner, how| {
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
    let mut local = LocalFs::new();
    let mut fs = Faulty::new(&mut local);
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
    both(|fs, how| {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join(nfc), "nfc").unwrap();
        std::fs::write(dst.join(nfd), "nfd").unwrap();
        let normalising = dst.join(nfc).exists();
        let mut ctl = Script::answers([reply(Answer::Skip)]);
        let r = run(
            &job(Op::Copy, &src, &[nfc], &dst),
            Hosts::Same(fs),
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
    both(|fs, how| {
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
    let Some(mut fs) = SftpRemote::local_server_with(&["-P", "setstat,fsetstat"], None) else {
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
    assert_eq!(report.items, vec![item("a", ItemOutcome::Skipped)]);
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
    both(|inner, how| {
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
        let mut fs = Faulty::new(&mut *inner);
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
    let mut local = LocalFs::new();
    let mut fs = Faulty::new(&mut local);
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
    let mut local = LocalFs::new();
    let mut fs = Faulty::new(&mut local);
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
/// (the "stand-in that hides it"), and copy and move there are refused before anything happens,
/// with the reason.
#[test]
fn k_c4_a_server_hiding_hardlink_refuses_copy_and_move() {
    let Some(mut fs) = SftpRemote::local_server_with(&["-P", "hardlink"], None) else {
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
    let Some(fs) = SftpRemote::local_server_with(&["-P", "copy-data"], None) else {
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
/// exclusive rename), for the hard-link branch and the link branch.
#[test]
fn r2_local_fallback_a_source_replaced_before_its_unlink_survives() {
    let (_t, src, dst) = scratch();
    let original = file_and_link(&src);
    let _excl = LocalFs::seam_no_exclusive_rename();
    let _seam = LocalFs::seam_before_unlink(replace_entry);
    let r = run(
        &job(Op::Move, &src, &["a", "l"], &dst),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    replacements_survived(&r, &src, &dst, original, "local");
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
    let mut local = LocalFs::new();
    let mut fs = Faulty::new(&mut local);
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
fn corrupt_the_copy(fs: &mut Faulty<'_>, f: &Path) {
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
        let mut local = LocalFs::new();
        let mut fs = Faulty::new(&mut local);
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
    both(|fs, how| {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join("a"), "mine").unwrap();
        std::fs::write(dst.join("a"), "theirs").unwrap();
        let mut ctl = Script {
            stop_on_ask: true,
            ..Script::answers([reply(Answer::Rename("a-1".into()))])
        };
        let r = run(
            &job(Op::Move, &src, &["a"], &dst),
            Hosts::Same(fs),
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
    both(|inner, how| {
        let grow = |f: &Path| {
            let mut file = std::fs::OpenOptions::new().append(true).open(f).unwrap();
            file.write_all(b" and more").unwrap();
        };
        let touch = |f: &Path| set_mtime(f, 1_700_000_000);
        let changes: [(&str, &Change); 2] = [("size", &grow), ("mtime", &touch)];
        for (what, change) in changes {
            for op in [Op::Copy, Op::Move] {
                let (_t, src, dst) = scratch();
                let f = src.join("f");
                std::fs::write(&f, "before").unwrap();
                set_mtime(&f, 1_600_000_000);
                let mut fs = Faulty::new(&mut *inner);
                fs.rename = no_rename_from(&[&f]);
                let source = f.clone();
                fs.copied = Box::new(move |_| change(&source));
                let r = run(
                    &job(op.clone(), &src, &["f"], &dst),
                    Hosts::Same(&mut fs),
                    &mut Script::default(),
                );
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
    let mut targets: Vec<(&str, Box<dyn FileOps>)> = vec![("local", Box::new(LocalFs::new()))];
    if let Some(sftp) = SftpRemote::local_server() {
        targets.push(("sftp", Box::new(sftp)));
    }
    for (how, mut target) in targets {
        let (_t, src, dst) = scratch();
        std::fs::write(src.join("f"), "the right bytes").unwrap();
        let before = sha(&src.join("f"));
        let mut fs = Faulty::new(target.as_mut());
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

/// Esc during a moved folder's bottom-up removal: what was not removed yet stays as it was, the
/// target is complete, and the report says both copies exist.
#[test]
fn esc_while_removing_a_moved_folder_keeps_the_rest() {
    both(|inner, how| {
        let (_t, src, dst) = scratch();
        let d = three_files(&src);
        let files = ["a.txt", "b.txt", "c.txt"];
        let before = payloads(&d, &files);
        let mut ctl = Script::default();
        let stop = ctl.cancel.clone();
        let mut fs = Faulty::new(inner);
        fs.rename = no_rename_from(&[&d]);
        let inside = format!("{}/", s(&d));
        fs.fault = Box::new(move |op, path| {
            if op == "remove" && path.starts_with(&inside) {
                stop.store(true, Ordering::Relaxed);
            }
            None
        });
        let r = run(
            &job(Op::Move, &src, &["d"], &dst),
            Hosts::Same(&mut fs),
            &mut ctl,
        );
        assert_eq!(r.items.len(), 1, "{how}: {r:?}");
        assert!(
            stop_reason(&r.items[0]).contains("both places"),
            "{how}: {r:?}"
        );
        assert_eq!(payloads(&dst.join("d"), &files), before, "{how}: complete");
        // Bottom-up: the last entry went first; the rest is as it was.
        assert_eq!(names(&d), ["a.txt", "b.txt"], "{how}");
        assert_eq!(payloads(&d, &files[..2]), before[..2], "{how}");
    });
}

/// A remote source that cannot be removed after its hard link (`sftp-server -P remove`): the
/// same outcome as a moved copy whose source stays — failed, both copies exist, the reason in
/// a note.
#[test]
fn a_remote_source_that_cannot_be_removed_keeps_both_names() {
    let Some(mut fs) = SftpRemote::local_server_with(&["-P", "remove"], None) else {
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
    let mut local = LocalFs::new();
    let mut fs = Faulty::new(&mut local);
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
        let mut local = LocalFs::new();
        let mut fs = Faulty::new(&mut local);
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
    both(|inner, how| {
        // A corrupt copy, and one transient stat failure of the source after the check's read.
        {
            let (_t, src, dst) = scratch();
            let f = src.join("f");
            std::fs::write(&f, vec![9u8; 4096]).unwrap();
            let before = sha(&f);
            let mut fs = Faulty::new(&mut *inner);
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
        }

        // A good copy whose source goes right after the check read it: not a verified move.
        let (_t, src, dst) = scratch();
        let f = src.join("f");
        std::fs::write(&f, "good bytes").unwrap();
        let mut fs = Faulty::new(&mut *inner);
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
    let mut local = LocalFs::new();
    let mut fs = Faulty::new(&mut local);
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
/// source.
#[test]
fn r15_a_replacement_at_the_published_name_survives() {
    let (_t, src, dst) = scratch();
    let f = src.join("f");
    std::fs::write(&f, vec![5u8; 2048]).unwrap();
    let before = sha(&f);
    let mut local = LocalFs::new();
    let mut fs = Faulty::new(&mut local);
    fs.host_made = true;
    corrupt_the_copy(&mut fs, &f);
    let replaced: Arc<Mutex<Option<u64>>> = Arc::default();
    let (seen, target) = (replaced.clone(), dst.join("f"));
    fs.renamed = Box::new(move |from, to| {
        if from.contains(".sopen-") && Path::new(to) == target {
            let was = std::fs::metadata(&target).unwrap();
            std::fs::remove_file(&target).unwrap();
            std::fs::write(&target, vec![b'R'; was.len() as usize]).unwrap();
            let t = std::fs::File::options().write(true).open(&target).unwrap();
            t.set_times(std::fs::FileTimes::new().set_modified(was.modified().unwrap()))
                .unwrap();
            *seen.lock().unwrap() = Some(ino(&target));
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
    let replacement = replaced.lock().unwrap().expect("published, then replaced");
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
    let mut local = LocalFs::new();
    let mut fs = Faulty::new(&mut local);
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
    assert_eq!(
        r.items,
        vec![
            item("b", ItemOutcome::Skipped),
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

/// The copy paths a move can take: on one host (this machine; a server's `copy-data`; a server
/// without it, streamed through sopen) and between two (this machine and a server both ways,
/// two connections to this machine). `None` where `sftp-server` is missing.
fn every_path(name: &str) -> Option<(Layer, Option<Layer>)> {
    let sftp = |args: &[&str]| {
        SftpRemote::local_server_with(args, None).map(|f| Box::new(f) as Box<dyn FileOps>)
    };
    Some(match name {
        "local" => (Box::new(LocalFs::new()), None),
        "copy-data" => (sftp(&[])?, None),
        "sftp-streamed" => (sftp(&["-P", "copy-data"])?, None),
        "here-to-remote" => (Box::new(LocalFs::new()), Some(sftp(&[])?)),
        "remote-to-here" => (sftp(&[])?, Some(Box::new(LocalFs::new()))),
        "here-to-here" => (Box::new(LocalFs::new()), Some(Box::new(LocalFs::new()))),
        _ => unreachable!(),
    })
}

const PATHS: [&str; 6] = [
    "local",
    "copy-data",
    "sftp-streamed",
    "here-to-remote",
    "remote-to-here",
    "here-to-here",
];

/// F2: a move reads back what the target stored, on every path and at any size (the size cap
/// lowered to 16 bytes here; `f2_every_path_at_320_mib` runs the real size): a copy scribbled on
/// after its bytes were sent never lets the source go.
fn f2_case(path: &str, size: usize) {
    let Some((mut a, b)) = every_path(path) else {
        return;
    };
    let (_t, src, dst) = scratch();
    let f = src.join("f");
    std::fs::write(&f, (0..size).map(|i| (i % 249) as u8).collect::<Vec<u8>>()).unwrap();
    let before = sha(&f);
    let r = match b {
        None => {
            let mut fs = Faulty::new(a.as_mut());
            fs.hash_limit = Some(16);
            fs.rename = no_rename_from(&[&f]);
            fs.copied = Box::new(scribble);
            run(
                &job(Op::Move, &src, &["f"], &dst),
                Hosts::Same(&mut fs),
                &mut Script::default(),
            )
        }
        Some(mut b) => {
            let mut fs = Faulty::new(b.as_mut());
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

/// F4: where two connections cannot tell they share a filesystem, the copy is not refused, but
/// it walks its source before it makes anything: it holds the folder as it was, once.
#[test]
fn f4_the_walk_comes_before_the_new_folder() {
    let (_t, src, _dst) = scratch();
    let d = src.join("F");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("x"), "x").unwrap();
    let (mut a, mut b) = (LocalFs::new(), LocalFs::new());
    let mut fs = Faulty::new(&mut b);
    fs.no_fs_id = true;
    fs.plain_realpath = true;
    let r = run(
        &job(Op::Copy, &src, &["F"], &d.join("sub")),
        Hosts::Two {
            src: &mut a,
            dst: &mut fs,
        },
        &mut Script::default(),
    );
    assert_eq!(r.items, vec![item("F", ItemOutcome::Done)], "{r:?}");
    let copy = d.join("sub/F");
    assert_eq!(names(&copy), ["sub", "x"]);
    assert_eq!(
        names(&copy.join("sub")),
        Vec::<String>::new(),
        "never copied into itself"
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
    if let Some(mut fs) = SftpRemote::local_server_with(&["-P", "remove"], None) {
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
fn f6_one_barrier_per_moved_item_and_one_at_the_end() {
    let (_t, src, dst) = scratch();
    let d = three_files(&src);
    std::fs::write(src.join("f"), "f").unwrap();
    let mut local = LocalFs::new();
    let mut fs = Faulty::new(&mut local);
    fs.rename = no_rename_from(&[&d, &src.join("f")]);
    let barriers = fs.barriers.clone();
    let r = run(
        &job(Op::Move, &src, &["d", "f"], &dst),
        Hosts::Same(&mut fs),
        &mut Script::default(),
    );
    assert!(r.all_done(), "{r:?}");
    assert_eq!(barriers.load(Ordering::Relaxed), 3, "d, f, and the end");

    let mut local = LocalFs::new();
    let mut fs = Faulty::new(&mut local);
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
    let mut local = LocalFs::new();
    let mut from = Faulty::new(&mut local);
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
    let mut local = LocalFs::new();
    let mut fs = Faulty::new(&mut local);
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
    let mut local = LocalFs::new();
    let mut fs = Faulty::new(&mut local);
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
    let mode = LocalFs::new().publish_mode(&s(&target)).unwrap();
    eprintln!("volume: publish mode {mode:?}");
    let r = run(
        &job(Op::Copy, &src, &["a.bin", "d"], &target),
        Hosts::Same(&mut LocalFs::new()),
        &mut Script::default(),
    );
    eprintln!("copy: {r:?}");
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
    assert_eq!(sha(&target.join("a.bin")), sha(&src.join("a.bin")));
    assert_eq!(std::fs::read(target.join("b.txt")).unwrap(), b"b");
    assert!(!src.join("b.txt").exists());
    std::fs::remove_dir_all(&target).unwrap();
}
