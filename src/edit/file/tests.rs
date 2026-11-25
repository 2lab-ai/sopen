//! ED-save's tests (12 §14 ES-1's model half, ES-2, ES-3): the read, Save and its guard, the
//! conflicts and Overwrite, Save copy and Save As, the journal and Restore — on the local disk and
//! against `sftp-server` (a failure under `SOPEN_REQUIRE_SFTP_SERVER` where there is none). A
//! crash is the process stopping: [`Dying`] fails every request, and the journal's every step,
//! from the moment it is killed, so what is on disk is what the moment left.

use super::*;
use crate::remote::{CreateError, Created, Entry, LocalFs, SftpRemote, Uploaded};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, UNIX_EPOCH};

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn sha(bytes: &[u8]) -> Sha {
    journal::sha256(bytes)
}

const OLD: &[u8] = b"line one\nline two\nline three\n";
const NEW: &[u8] = b"x-line one\nline two\nline three\n";
const THEIRS: &[u8] = b"line one\nTHEIR two\nline three\n";

/// A file `notes.txt` (mode 0640, a whole-second mtime) in a folder of its own, and a cache.
struct Rig {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    cache: PathBuf,
    target: PathBuf,
}

fn rig(body: &[u8]) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("files");
    let cache = dir.with_file_name("cache");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    let target = dir.join("notes.txt");
    std::fs::write(&target, body).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
    set_mtime(&target, 1_700_000_000);
    Rig {
        _tmp: tmp,
        dir,
        cache,
        target,
    }
}

fn set_mtime(path: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

fn never() -> AtomicBool {
    AtomicBool::new(false)
}

fn read(rig: &Rig, encoding: Encoding) -> Read {
    read_local(p(&rig.target), encoding, &mut |_, _| {}, &never()).unwrap()
}

fn editor(read: &Read) -> Editor {
    Editor::open(read.original.clone(), Doubt::of(&read.original, false))
}

/// Types `text` at byte `at`: done, nothing said.
fn typed(ed: &mut Editor, at: u64, text: &str) {
    ed.place(at, false);
    assert_eq!(ed.type_text(text), None, "{text}");
}

fn doc(ed: &Editor) -> Vec<u8> {
    let mut out = Vec::new();
    ed.buffer().write_to(&mut out).unwrap();
    out
}

/// Keeps until the journal says kept (its original is written on a worker).
fn kept(s: &mut Session, ed: &Editor) {
    let t0 = Instant::now();
    loop {
        match s.keep(ed) {
            Keep::Kept => return,
            Keep::Keeping if t0.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_millis(2))
            }
            other => panic!("{other:?}"),
        }
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// What is left of a Save beside the file: temps and links (never), and the kept versions.
fn leftovers(dir: &Path) -> (Vec<String>, Vec<String>) {
    let all = names(dir);
    let temps = all
        .iter()
        .filter(|n| n.contains(".sopen-") && !n.contains(".sopen-replaced-"))
        .cloned()
        .collect();
    let kept = all
        .into_iter()
        .filter(|n| n.contains(".sopen-replaced-"))
        .collect();
    (temps, kept)
}

fn backups(rig: &Rig) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![crate::cache::backup_root(&rig.cache)];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            match e.file_type().unwrap().is_dir() {
                true => stack.push(e.path()),
                false => out.push(e.path()),
            }
        }
    }
    out.sort();
    out
}

fn written(saved: &Saved) -> (&Option<PathBuf>, &Vec<String>) {
    match saved {
        Saved::Written { backup, notes, .. } => (backup, notes),
        other => panic!("{other:?}"),
    }
}

/// Writes `body` into `path` in place (the same inode) and puts its mtime back.
fn write_in_place(path: &Path, body: &[u8]) {
    let meta = std::fs::metadata(path).unwrap();
    let mut f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.write_all(body).unwrap();
    f.set_len(body.len() as u64).unwrap();
    f.set_modified(meta.modified().unwrap()).unwrap();
}

/// A byte reader over a [`crate::remote::Ranged`] stream: 64 KiB reads in order, as the
/// stream's `Bulk` reader gives the product's whole read.
struct OverRanged {
    r: Box<dyn crate::remote::Ranged>,
    off: u64,
}

impl std::io::Read for OverRanged {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.off >= self.r.size() || buf.is_empty() {
            return Ok(0);
        }
        let len = buf.len().min(64 * 1024) as u32;
        self.r.read(self.off, len);
        let c = self.r.next(Some(Duration::from_secs(30))).unwrap();
        let crate::remote::Reply::Data(bytes) = c.reply.map_err(std::io::Error::other)? else {
            return Err(std::io::Error::other("a read answered with a stat"));
        };
        buf[..bytes.len()].copy_from_slice(&bytes);
        self.off += bytes.len() as u64;
        Ok(bytes.len())
    }
}

/// The remote side of a rig: a connection to `sftp-server`, and the whole read through the
/// `Ranged` Fake streaming the same bytes, against the file's stat on the server.
fn remote_read(rig: &Rig, encoding: Encoding) -> Option<(SftpRemote, Read)> {
    let mut fs = SftpRemote::local_server()?;
    let at = fs.lstat_opt(p(&rig.target)).unwrap().unwrap();
    let bytes = std::fs::read(&rig.target).unwrap();
    let file = crate::remote::FakeFile::bytes(bytes);
    let (r, _control) = crate::remote::fake(p(&rig.target), file, crate::remote::Link::instant());
    let mut reader = OverRanged { r, off: 0 };
    let read = read_stream(
        &mut reader,
        p(&rig.target),
        at,
        encoding,
        &mut |_, _| {},
        &never(),
    )
    .unwrap();
    Some((fs, read))
}

/// A connection for `remote`, and the file read through it.
fn opened(r: &Rig, remote: bool) -> Option<(Box<dyn RemoteFs + Send>, Read)> {
    Some(match remote {
        false => (Box::new(LocalFs::new()), read(r, Encoding::Utf8)),
        true => {
            let (fs, rd) = remote_read(r, Encoding::Utf8)?;
            (Box::new(fs), rd)
        }
    })
}

// ---------------------------------------------------------------------------------------------
// The read (§7.4).

/// §7.4: the whole read holds the file's bytes and records the baseline — its size, mtime and
/// SHA-256, and locally its device and inode; through a link, the file the link names (as every
/// opening's `abs` is its real path). A read through the stream equals the whole file. Above
/// 256 MiB nothing is read; Esc stops the read.
#[test]
fn the_whole_read_records_the_baseline() {
    use std::os::unix::fs::MetadataExt as _;
    let r = rig(OLD);
    let link = r.dir.join("link.txt");
    std::os::unix::fs::symlink(&r.target, &link).unwrap();
    let read = read_local(p(&link), Encoding::Utf8, &mut |_, _| {}, &never()).unwrap();
    assert_eq!(read.path, p(&r.target));
    assert_eq!(read.original.bytes(), OLD);
    let meta = std::fs::metadata(&r.target).unwrap();
    assert_eq!(
        read.baseline,
        Baseline {
            size: OLD.len() as u64,
            mtime: meta.modified().unwrap(),
            sha256: sha(OLD),
            id: Some(FileId {
                dev: meta.dev(),
                ino: meta.ino(),
            }),
        }
    );
    if let Some((_, streamed)) = remote_read(&r, Encoding::Utf8) {
        assert_eq!(streamed.original.bytes(), OLD, "ranged equals whole");
        assert_eq!(streamed.baseline.sha256, sha(OLD));
        assert_eq!(streamed.baseline.id, None);
    }
    let big = Stat {
        size: HASH_CHECK_MAX + 1,
        ..LocalFs::new().stat(p(&r.target)).unwrap()
    };
    let e = read_stream(
        &mut std::io::empty(),
        "/x/big.txt",
        big,
        Encoding::Utf8,
        &mut |_, _| {},
        &never(),
    )
    .err()
    .unwrap();
    assert!(format!("{e:#}").contains("over 256 MiB"), "{e:#}");
    let stop = AtomicBool::new(true);
    let e = read_local(p(&r.target), Encoding::Utf8, &mut |_, _| {}, &stop)
        .err()
        .unwrap();
    assert!(remote::is_cancelled(&e), "Esc goes back to viewing: {e:#}");
}

// ---------------------------------------------------------------------------------------------
// Save (§9.1; ES-1's model half).

/// ES-1's model half, on the local disk and against `sftp-server`: Save → the file holds the
/// edit with its owner and mode kept; the backup is the original as opened, made at the
/// session's first Save only; the next baseline is the bytes staged; the undo stack is marked
/// saved; the journal stays across the Save (§9.5) and goes when the editor closes clean.
#[test]
fn es1_a_save_lands_with_one_backup_and_the_staged_baseline() {
    for remote in [false, true] {
        let r = rig(OLD);
        let Some((mut fs, rd)) = opened(&r, remote) else {
            return;
        };
        let label = remote.then_some("box");
        let mut ed = editor(&rd);
        let mut s = Session::new(&rd, label, &r.cache, journal::disk());
        typed(&mut ed, 9, "the ");
        kept(&mut s, &ed);
        let saved = s.save(&mut ed, fs.as_mut()).unwrap();
        let want = b"line one\nthe line two\nline three\n";
        assert_eq!(std::fs::read(&r.target).unwrap(), want, "{remote}");
        let mode = std::fs::metadata(&r.target).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o640, "{remote}");
        let (backup, notes) = written(&saved);
        assert!(notes.is_empty(), "{remote}: {notes:?}");
        let backup = backup.clone().unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), OLD, "{remote}");
        let Saved::Written {
            bytes, encoding, ..
        } = &saved
        else {
            unreachable!()
        };
        assert_eq!((*bytes, *encoding), (want.len() as u64, Encoding::Utf8));
        assert_eq!(s.baseline().sha256, sha(want), "{remote}");
        assert_eq!(s.baseline().size, want.len() as u64);
        assert!(ed.buffer().is_saved());
        let journal = s.journal_dir().unwrap().to_path_buf();
        assert_eq!(leftovers(&r.dir), (vec![], vec![]), "{remote}");
        assert_eq!(
            saved.line(),
            format!("saved {} · backup {}", s.show(s.target()), backup.display())
        );

        typed(&mut ed, 0, "# ");
        let again = s.save(&mut ed, fs.as_mut()).unwrap();
        assert_eq!(written(&again).0, &None, "{remote}: one backup a session");
        assert_eq!(backups(&r).len(), 1, "{remote}");
        let mut want2 = b"# ".to_vec();
        want2.extend_from_slice(want);
        assert_eq!(std::fs::read(&r.target).unwrap(), want2, "{remote}");
        assert!(journal.exists(), "{remote}: one journal per session");
        s.close(&ed);
        assert!(
            !journal.exists(),
            "{remote}: closed clean, the journal goes"
        );
    }
}

/// The backup is a hard link of the journal's `original` where the cache is one volume (§9.1
/// step 3), owner-only.
#[test]
fn the_backup_is_a_link_of_the_journals_original() {
    use std::os::unix::fs::MetadataExt as _;
    let r = rig(OLD);
    let rd = read(&r, Encoding::Utf8);
    let mut ed = editor(&rd);
    let mut s = Session::new(&rd, None, &r.cache, journal::disk());
    typed(&mut ed, 0, "x");
    kept(&mut s, &ed);
    let original = s.journal_dir().unwrap().join("original");
    let saved = s.save(&mut ed, &mut LocalFs::new()).unwrap();
    let backup = written(&saved).0.clone().unwrap();
    assert_eq!(
        std::fs::metadata(&backup).unwrap().ino(),
        std::fs::metadata(&original).unwrap().ino()
    );
    assert_eq!(std::fs::metadata(&backup).unwrap().mode() & 0o777, 0o600);
}

/// UTF-16LE with a BOM, Hangul and CR LF, and CP949 with a byte it cannot decode: an edit saved
/// byte for byte — the bytes before and after it exactly as read, the text typed in the file's
/// encoding.
#[test]
fn utf16_and_cp949_files_are_saved_byte_exact() {
    for encoding in [Encoding::Utf16Le, Encoding::Cp949] {
        let text = "첫째 줄\r\n둘째 줄\r\n";
        let mut body = match encoding {
            Encoding::Utf16Le => vec![0xFF, 0xFE],
            _ => Vec::new(),
        };
        body.extend(encoding.encode(text).unwrap());
        if encoding == Encoding::Cp949 {
            body.push(0xFF);
        }
        let r = rig(&body);
        let rd = read(&r, encoding);
        let mut ed = editor(&rd);
        let mut s = Session::new(&rd, None, &r.cache, journal::disk());
        let line2 = ed.buffer().line_start(1).unwrap();
        ed.place(line2, false);
        // E37: the byte CP949 cannot decode asks once before the first Hangul is typed.
        if let Some(asked) = ed.type_text("한") {
            assert!(
                matches!(asked, super::super::ops::Said::Asked(_)),
                "{asked:?}"
            );
            assert_eq!(ed.answer(super::super::ops::Answer::Type), None);
        }
        let saved = s.save(&mut ed, &mut LocalFs::new()).unwrap();
        let mut want = body[..line2 as usize].to_vec();
        want.extend(encoding.encode("한").unwrap());
        want.extend_from_slice(&body[line2 as usize..]);
        assert_eq!(std::fs::read(&r.target).unwrap(), want, "{encoding:?}");
        let Saved::Written { encoding: e, .. } = saved else {
            panic!("{saved:?}")
        };
        assert_eq!(e, encoding);
    }
}

/// A file opened through a symbolic link is saved into the file the link names, and the link
/// stays a link; a file with a second hard link is saved at its own name, the other name keeping
/// the version before.
#[test]
fn a_symlinked_and_a_hard_linked_target() {
    let r = rig(OLD);
    let link = r.dir.join("link.txt");
    std::os::unix::fs::symlink(&r.target, &link).unwrap();
    let rd = read_local(p(&link), Encoding::Utf8, &mut |_, _| {}, &never()).unwrap();
    let mut ed = editor(&rd);
    let mut s = Session::new(&rd, None, &r.cache, journal::disk());
    typed(&mut ed, 0, "x");
    written(&s.save(&mut ed, &mut LocalFs::new()).unwrap());
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(&std::fs::read(&r.target).unwrap()[..1], b"x");

    let r = rig(OLD);
    let other = r.dir.join("other.txt");
    std::fs::hard_link(&r.target, &other).unwrap();
    let rd = read(&r, Encoding::Utf8);
    let mut ed = editor(&rd);
    let mut s = Session::new(&rd, None, &r.cache, journal::disk());
    typed(&mut ed, 0, "x");
    written(&s.save(&mut ed, &mut LocalFs::new()).unwrap());
    assert_eq!(&std::fs::read(&r.target).unwrap()[..1], b"x");
    assert_eq!(std::fs::read(&other).unwrap(), OLD);
}

/// Save copy writes `<stem>.edited<ext>` beside the file and leaves the file; Save As writes a
/// local path, never over anything: a name taken is refused, untouched.
#[test]
fn save_copy_and_save_as_write_new_files_only() {
    let r = rig(OLD);
    let rd = read(&r, Encoding::Utf8);
    let mut ed = editor(&rd);
    let mut s = Session::new(&rd, None, &r.cache, journal::disk());
    typed(&mut ed, 0, "x");
    let copy = s.save_copy(&ed, &mut LocalFs::new()).unwrap();
    assert_eq!(copy, p(&r.dir.join("notes.edited.txt")));
    assert_eq!(&std::fs::read(&copy).unwrap()[..1], b"x");
    assert_eq!(std::fs::read(&r.target).unwrap(), OLD);

    let taken = r.dir.join("taken.txt");
    std::fs::write(&taken, b"someone's").unwrap();
    match s.save_as(&ed, &taken) {
        Err(SaveError::Exists { .. }) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&taken).unwrap(), b"someone's");
    let fresh = r.dir.join("fresh.txt");
    let got = s.save_as(&ed, &fresh).unwrap();
    assert_eq!(got.bytes, OLD.len() as u64 + 1);
    assert_eq!(std::fs::read(&fresh).unwrap(), doc(&ed));
    assert!(
        !ed.buffer().is_saved(),
        "Save As does not mark the file saved"
    );
}

/// §9.1 step 1: a server that cannot replace a file in one step refuses Save before anything is
/// written, offering Save copy.
#[test]
fn a_server_that_cannot_replace_refuses_save() {
    let Some(mut fs) = SftpRemote::local_server_args(&["-P", "posix-rename"], None) else {
        return;
    };
    assert!(!fs.can_replace());
    let r = rig(OLD);
    let rd = read(&r, Encoding::Utf8);
    let mut ed = editor(&rd);
    let mut s = Session::new(&rd, Some("box"), &r.cache, journal::disk());
    typed(&mut ed, 0, "x");
    let saved = s.save(&mut ed, &mut fs).unwrap();
    assert!(
        matches!(&saved, Saved::CannotReplace(line) if line.contains("Save copy")),
        "{saved:?}"
    );
    assert_eq!(std::fs::read(&r.target).unwrap(), OLD);
    assert!(backups(&r).is_empty());
}

// ---------------------------------------------------------------------------------------------
// Conflicts (§9.2; ES-2).

/// ES-2: between the read and the Save, the file changed — one byte (same size, mtime set back),
/// its size, deleted, replaced by a link — on the local disk and against `sftp-server`: nothing
/// written over it, the line names the change, Overwrite is offered for the first two only, the
/// staged copy kept, the edits still in the editor.
#[test]
fn es2_a_change_since_the_read_is_a_conflict() {
    type Change = fn(&Path);
    let changes: [(&str, Change); 4] = [
        ("one byte", |t| {
            write_in_place(t, b"line one\nline TWO\nline three\n")
        }),
        ("its size", |t| std::fs::write(t, b"short\n").unwrap()),
        ("deleted", |t| std::fs::remove_file(t).unwrap()),
        ("a link", |t| {
            let real = t.with_file_name("real.txt");
            std::fs::rename(t, &real).unwrap();
            std::os::unix::fs::symlink(&real, t).unwrap();
        }),
    ];
    for remote in [false, true] {
        for (what, change) in changes {
            let r = rig(OLD);
            let Some((mut fs, rd)) = opened(&r, remote) else {
                return;
            };
            let mut ed = editor(&rd);
            let mut s = Session::new(&rd, remote.then_some("box"), &r.cache, journal::disk());
            typed(&mut ed, 0, "x");
            change(&r.target);
            let before = std::fs::read(&r.target).ok();
            let saved = s.save(&mut ed, fs.as_mut()).unwrap();
            let Saved::Conflict {
                line,
                overwrite,
                staged,
                ..
            } = &saved
            else {
                panic!("{remote} {what}: {saved:?}")
            };
            assert!(
                line.starts_with("notes.txt changed since it was read ("),
                "{line}"
            );
            let named = match what {
                "one byte" => "its content changed",
                "its size" => "size 29 → 6",
                "deleted" => "it was deleted",
                _ => "symbolic link",
            };
            assert!(line.contains(named), "{remote} {what}: {line}");
            assert_eq!(
                *overwrite,
                matches!(what, "one byte" | "its size"),
                "{remote} {what}"
            );
            assert_eq!(std::fs::read(staged).unwrap(), doc(&ed), "{remote} {what}");
            let after = std::fs::read(&r.target).ok();
            assert_eq!(after, before, "{remote} {what}: nothing written over it");
            assert!(!ed.buffer().is_saved());
            assert_eq!(leftovers(&r.dir).0, Vec::<String>::new(), "{remote} {what}");
        }
    }
}

/// ES-2 / E23: Overwrite reads the file as it is now into the backup folder, then writes it,
/// against that read — so a change between Overwrite's read and its replace is caught again.
/// Refused for a deleted file. Reload takes the file as it is now, and the journal goes.
#[test]
fn es2_overwrite_backs_up_the_changed_version_and_is_checked_again() {
    let r = rig(OLD);
    let rd = read(&r, Encoding::Utf8);
    let mut ed = editor(&rd);
    let mut s = Session::new(&rd, None, &r.cache, journal::disk());
    typed(&mut ed, 0, "x");
    kept(&mut s, &ed);
    write_in_place(&r.target, THEIRS);
    let first = s.save(&mut ed, &mut LocalFs::new()).unwrap();
    assert!(matches!(first, Saved::Conflict { .. }), "{first:?}");
    let saved = s.overwrite(&mut ed, &mut LocalFs::new()).unwrap();
    let backup = written(&saved).0.clone().unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), THEIRS);
    assert_eq!(std::fs::read(&r.target).unwrap(), doc(&ed));
    assert_eq!(
        backups(&r).len(),
        2,
        "the session's first backup, and Overwrite's"
    );

    // A third writer between Overwrite's read and its replace: caught again.
    typed(&mut ed, 0, "y");
    write_in_place(&r.target, b"x-line one\nTHIRD\n");
    let target = r.target.clone();
    let mut once = true;
    let again = s
        .overwrite_with(&mut ed, &mut LocalFs::new(), &mut |g, _| {
            if once && matches!(g, Guard::Step(Step::Named { .. })) {
                once = false;
                write_in_place(&target, b"x-line one\nFOURTH\n");
            }
            Ok(())
        })
        .unwrap();
    assert!(matches!(again, Saved::Conflict { .. }), "{again:?}");
    assert_eq!(std::fs::read(&r.target).unwrap(), b"x-line one\nFOURTH\n");

    std::fs::remove_file(&r.target).unwrap();
    let e = s.overwrite(&mut ed, &mut LocalFs::new()).unwrap_err();
    assert!(format!("{e:#}").contains("there is no Overwrite"), "{e:#}");

    std::fs::write(&r.target, b"as it is now\n").unwrap();
    let journal = s.journal_dir().unwrap().to_path_buf();
    let now = read(&r, Encoding::Utf8);
    let doubt = Doubt::of(&now.original, false);
    s.reload(&mut ed, now, doubt);
    assert_eq!(doc(&ed), b"as it is now\n");
    assert!(ed.buffer().is_saved());
    assert!(!journal.exists(), "the journal goes");
}

/// The once-per-filesystem line: said with the first Save that ran without the guard, and not
/// again for the same key.
#[test]
fn the_residual_line_is_said_once() {
    let key = format!("test {}", remote::random_hex());
    assert!(first_time(key.clone()));
    assert!(!first_time(key));
}

// ---------------------------------------------------------------------------------------------
// A crash at each phase (ES-2's kill rows; 12 §9.1's reconcile).

/// A file layer that is killed: from the moment `dead` is set — by a test's hook, or right after
/// the request named `after` got its answer — every request fails.
struct Dying<F> {
    inner: F,
    dead: Arc<AtomicBool>,
    after: Option<&'static str>,
    exchanges: usize,
    /// The request named here fails, nothing done (a status reply).
    fail: Option<&'static str>,
    /// The request named here takes effect and its reply is lost.
    lose: Option<&'static str>,
}

impl<F> Dying<F> {
    fn over(inner: F, dead: &Arc<AtomicBool>, after: Option<&'static str>) -> Self {
        Dying {
            inner,
            dead: dead.clone(),
            after,
            exchanges: 0,
            fail: None,
            lose: None,
        }
    }
}

impl<F> Dying<F> {
    fn alive(&self) -> Result<()> {
        match self.dead.load(Ordering::SeqCst) {
            true => bail!("killed"),
            false => Ok(()),
        }
    }

    fn done<T>(&self, op: &str, r: Result<T>) -> Result<T> {
        if r.is_ok() && self.after == Some(op) {
            self.dead.store(true, Ordering::SeqCst);
        }
        if r.is_ok() && self.lose == Some(op) {
            return Err(remote::unknown(format!("{op}: the reply was lost")));
        }
        r
    }

    fn refused(&self, op: &str) -> Result<()> {
        match self.fail == Some(op) {
            true => bail!("{op}: refused (simulated)"),
            false => self.alive(),
        }
    }
}

impl<F: RemoteFs + ?Sized + Send + 'static> RemoteFs for Dying<Box<F>> {
    fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.alive()?;
        self.inner.stat_opt(path)
    }
    fn lstat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.alive()?;
        self.inner.lstat_opt(path)
    }
    fn realpath(&mut self, path: &str) -> Result<String> {
        self.alive()?;
        self.inner.realpath(path)
    }
    fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>> {
        self.alive()?;
        self.inner.read_head(path, max)
    }
    fn list_dir(&mut self, path: &str) -> Result<Vec<String>> {
        self.alive()?;
        self.inner.list_dir(path)
    }
    fn list_entries(&mut self, path: &str) -> Result<Vec<Entry>> {
        self.alive()?;
        self.inner.list_entries(path)
    }
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        self.alive()?;
        self.inner.read_to(path, out, progress)
    }
    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<remote::Like>,
    ) -> std::result::Result<Uploaded, CreateError> {
        self.alive().map_err(|e| CreateError::new(Created::No, e))?;
        self.inner.create_from(local, path, like)
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        self.alive()?;
        self.inner.set_mode(path, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.refused("rename")?;
        let r = self.inner.rename_over(from, to);
        self.done("rename", r)
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.alive()?;
        let r = self.inner.link_new(from, to);
        self.done("link", r)
    }
    fn can_link(&self) -> bool {
        self.inner.can_link()
    }
    fn can_replace(&self) -> bool {
        self.inner.can_replace()
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        self.alive()?;
        self.inner.remove(path)
    }
    fn exchange(&mut self, a: &str, b: &str) -> Result<()> {
        self.refused("exchange")?;
        self.exchanges += 1;
        let r = self.inner.exchange(a, b);
        match self.exchanges {
            1 => self.done("exchange", r),
            _ => self.done("exchange back", r),
        }
    }
    fn file_id(&mut self, path: &str) -> Result<Option<(FileId, u64)>> {
        self.alive()?;
        self.inner.file_id(path)
    }
    fn sync_dir(&mut self, dir: &str) -> Result<()> {
        self.alive()?;
        self.inner.sync_dir(dir)
    }
}

/// The local disk for the journal, killed with the file layer: no step of it after the moment.
struct DyingDisk(Arc<AtomicBool>);

impl DyingDisk {
    fn alive(&self) -> std::io::Result<()> {
        match self.0.load(Ordering::SeqCst) {
            true => Err(std::io::Error::other("killed")),
            false => Ok(()),
        }
    }
}

impl journal::Fs for DyingDisk {
    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        self.alive()?;
        journal::Disk.create_dir(path)
    }
    fn write_new(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        self.alive()?;
        journal::Disk.write_new(path, bytes)
    }
    fn sync_file(&self, path: &Path) -> std::io::Result<()> {
        self.alive()?;
        journal::Disk.sync_file(path)
    }
    fn sync_dir(&self, path: &Path) -> std::io::Result<()> {
        self.alive()?;
        journal::Disk.sync_dir(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.alive()?;
        journal::Disk.rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        self.alive()?;
        journal::Disk.remove_file(path)
    }
    fn remove_dir(&self, path: &Path) -> std::io::Result<()> {
        self.alive()?;
        journal::Disk.remove_dir(path)
    }
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        self.alive()?;
        journal::Disk.read(path)
    }
    fn list(&self, dir: &Path) -> std::io::Result<Vec<String>> {
        self.alive()?;
        journal::Disk.list(dir)
    }
    fn node(&self, path: &Path, follow: bool) -> std::io::Result<Option<journal::Node>> {
        self.alive()?;
        journal::Disk.node(path, follow)
    }
    fn lock(&self, path: &Path, create: bool) -> std::io::Result<Option<journal::Held>> {
        self.alive()?;
        journal::Disk.lock(path, create)
    }
}

/// Where a kill comes: once the step `at` is recorded (its request not sent), or right after the
/// request `after` got its answer (the next step not recorded); `change` writes another version
/// into the file in place first, at the step it names.
#[derive(Clone, Copy, Debug)]
struct Kill {
    at: Option<&'static str>,
    after: Option<&'static str>,
    change: Option<&'static str>,
}

fn word(g: &Guard<'_>) -> &'static str {
    match g {
        Guard::Step(Step::Named { .. }) => "named",
        Guard::Step(Step::Written { .. }) => "written",
        Guard::Step(Step::Staged { .. }) => "staged",
        Guard::Step(Step::Renamed { .. }) => "renamed",
        Guard::Step(Step::Removed { .. }) => "removed",
        Guard::Step(Step::Left { .. }) => "left",
        Guard::Exchanging { .. } => "exchanging",
        Guard::SwappedBack { .. } => "swapped back",
        Guard::Linked { .. } => "linked",
        Guard::Committed { .. } => "committed",
        Guard::Verified { .. } => "verified",
        Guard::Kept { .. } => "kept",
        Guard::Unguarded(_) => "unguarded",
    }
}

/// The next F4's recovery row: the text journals of the file, listed again, for a while, until
/// every one on disk is found — on the real disk a lock stays held for a moment after its
/// session is dropped, by the copy a child another test forks holds until its exec (07 §3
/// "Recovery").
fn listed(r: &Rig, host: &str) -> Vec<Recovery> {
    let t0 = Instant::now();
    let want = journal::this_files_journals(&r.cache, host, p(&r.target)).len();
    loop {
        let recs = recoveries(&journal::disk(), &r.cache, host, p(&r.target)).unwrap();
        if recs.len() >= want || t0.elapsed() > Duration::from_secs(30) {
            return recs;
        }
        drop(recs);
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Runs a Save killed at `kill`, then the next F4: lists the journal and restores it — which
/// reconciles the cut-off Save. The file as it is then, what Restore said, and the restored
/// editor's bytes (`None`: the Save had landed and nothing is left to restore).
type Killed = (Vec<u8>, Vec<String>, Option<Vec<u8>>);

fn killed_save(r: &Rig, remote: bool, kill: Kill) -> Option<Killed> {
    let label = remote.then_some("box");
    let (inner, rd) = opened(r, remote)?;
    let dead = Arc::new(AtomicBool::new(false));
    {
        let mut ed = editor(&rd);
        let mut s = Session::new(&rd, label, &r.cache, Arc::new(DyingDisk(dead.clone())));
        typed(&mut ed, 0, "x-");
        kept(&mut s, &ed);
        let mut fs = Dying::over(inner, &dead, kill.after);
        let target = r.target.clone();
        let _ = s.save_with(&mut ed, &mut fs, &mut |g, intent| {
            if kill.change == Some(word(g)) {
                write_in_place(&target, THEIRS);
            }
            if kill.at == Some(word(g)) {
                assert!(intent.is_some(), "{kill:?}: recorded first");
                dead.store(true, Ordering::SeqCst);
            }
            Ok(())
        });
        assert!(dead.load(Ordering::SeqCst), "{kill:?}: the kill came");
    }
    let (mut fresh, _) = opened(r, remote)?;
    let jfs = journal::disk();
    let host = label.unwrap_or(crate::cache::LOCAL_HOST);
    let mut recs = listed(r, host);
    assert_eq!(recs.len(), 1, "{kill:?}");
    let rec = recs.remove(0);
    let restored = restore(rec, fresh.as_mut(), label, &r.cache, jfs, false).unwrap();
    let file = std::fs::read(&r.target).unwrap();
    Some(match restored {
        Restoring::Landed(lines) => (file, lines, None),
        Restoring::Restored(r) => (file, r.lines, Some(doc(&r.editor))),
    })
}

/// ES-2's kill rows: a Save killed at each phase of the guarded replace — once the phase is
/// recorded, and once its request is done but the next phase not — locally and against
/// `sftp-server`, the next F4's reconcile finds the old file or the new one, never a mix, and the
/// journal says which: landed (nothing to restore), or not (the edits restored). A temp or a
/// link that is the version read is removed; another writer's version is kept beside the file
/// and named; nothing is removed unhashed.
#[test]
fn a_save_killed_at_any_phase_reconciles_to_the_old_file_or_the_new_one() {
    let k = |at, after, change| Kill { at, after, change };
    // (remote, the kill, the file after, the version kept beside it, landed)
    type Row = (bool, Kill, &'static [u8], Option<&'static [u8]>, bool);
    let rows: Vec<Row> = vec![
        (false, k(Some("named"), None, None), OLD, None, false),
        (false, k(Some("written"), None, None), OLD, None, false),
        (false, k(Some("staged"), None, None), OLD, None, false),
        (false, k(Some("exchanging"), None, None), OLD, None, false),
        (false, k(None, Some("exchange"), None), NEW, None, true),
        (
            false,
            k(None, Some("exchange"), Some("exchanging")),
            NEW,
            Some(THEIRS),
            true,
        ),
        (
            false,
            k(Some("swapped back"), None, Some("exchanging")),
            NEW,
            Some(THEIRS),
            true,
        ),
        (
            false,
            k(None, Some("exchange back"), Some("exchanging")),
            THEIRS,
            None,
            false,
        ),
        (true, k(Some("written"), None, None), OLD, None, false),
        (true, k(Some("linked"), None, None), OLD, None, false),
        (true, k(None, Some("link"), None), OLD, None, false),
        (true, k(Some("staged"), None, None), OLD, None, false),
        (true, k(None, Some("rename"), None), NEW, None, true),
        (true, k(Some("committed"), None, None), NEW, None, true),
        (
            true,
            k(Some("committed"), None, Some("staged")),
            NEW,
            Some(THEIRS),
            true,
        ),
        (true, k(Some("verified"), None, None), NEW, None, true),
        (
            true,
            k(Some("kept"), None, Some("staged")),
            NEW,
            Some(THEIRS),
            true,
        ),
    ];
    for (remote, kill, file, kept, landed) in rows {
        let r = rig(OLD);
        let Some((now, lines, restored)) = killed_save(&r, remote, kill) else {
            return;
        };
        let tag = format!("{} {kill:?}", if remote { "remote" } else { "local" });
        assert_eq!(now, file, "{tag}: {lines:?}");
        let (temps, kept_names) = leftovers(&r.dir);
        assert_eq!(temps, Vec::<String>::new(), "{tag}: {lines:?}");
        match kept {
            Some(theirs) => {
                assert_eq!(kept_names.len(), 1, "{tag}: {kept_names:?}");
                let at = r.dir.join(&kept_names[0]);
                assert_eq!(std::fs::read(&at).unwrap(), theirs, "{tag}");
                let shown = match remote {
                    true => format!("box:{}", p(&at)),
                    false => p(&at).to_string(),
                };
                let named = format!("the other version is kept at {shown}");
                assert!(lines.contains(&named), "{tag}: {lines:?}");
            }
            None => assert_eq!(kept_names, Vec::<String>::new(), "{tag}"),
        }
        match landed {
            true => {
                assert_eq!(restored, None, "{tag}: nothing left to restore");
                assert!(
                    lines[0].contains("landed (found on reopen)"),
                    "{tag}: {lines:?}"
                );
            }
            false => {
                assert_eq!(restored.as_deref(), Some(NEW), "{tag}: the edits restored");
                assert!(lines[0].contains("did not land"), "{tag}: {lines:?}");
            }
        }
    }
}

/// ES-2's local hook row through the Session: a writer between the checks and the exchange →
/// the intent shows `exchanging`, then `swapped back`; the other version in place byte for byte;
/// the conflict, with Overwrite.
#[test]
fn the_intent_shows_exchanging_then_swapped_back() {
    let r = rig(OLD);
    let rd = read(&r, Encoding::Utf8);
    let mut ed = editor(&rd);
    let mut s = Session::new(&rd, None, &r.cache, journal::disk());
    typed(&mut ed, 0, "x-");
    kept(&mut s, &ed);
    let target = r.target.clone();
    let mut guards = Vec::new();
    let saved = s
        .save_with(&mut ed, &mut LocalFs::new(), &mut |g, intent| {
            let shown = match intent.and_then(|i| i.guard.clone()) {
                Some(Guarded::Exchanging { .. }) => Some("exchanging"),
                Some(Guarded::SwappedBack { .. }) => Some("swapped back"),
                _ => None,
            };
            if let Some(w) = shown.filter(|w| guards.last() != Some(w)) {
                guards.push(w);
            }
            if matches!(g, Guard::Exchanging { .. }) {
                write_in_place(&target, THEIRS);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(guards, ["exchanging", "swapped back"]);
    assert!(
        matches!(
            saved,
            Saved::Conflict {
                overwrite: true,
                ..
            }
        ),
        "{saved:?}"
    );
    assert_eq!(std::fs::read(&r.target).unwrap(), THEIRS);
    assert_eq!(leftovers(&r.dir), (vec![], vec![]));
}

// ---------------------------------------------------------------------------------------------
// The journal (§9.5; ES-3).

/// The next F4's one recovery, restored.
fn restored(r: &Rig) -> Restored {
    let jfs = journal::disk();
    let mut recs = listed(r, "local");
    assert_eq!(recs.len(), 1);
    match restore(
        recs.remove(0),
        &mut LocalFs::new(),
        None,
        &r.cache,
        jfs,
        false,
    )
    .unwrap()
    {
        Restoring::Restored(r) => *r,
        Restoring::Landed(lines) => panic!("{lines:?}"),
    }
}

/// ES-3: killed while typing → the next F4 offers Restore · Later · Discard with the journal's
/// size and date; Later keeps it, offered again; Restore → the edits up to the last kept
/// generation, byte for byte, unsaved, against the file as it is.
#[test]
fn es3_killed_while_typing_restores_the_last_kept_generation() {
    let r = rig(OLD);
    let rd = read(&r, Encoding::Utf8);
    {
        let mut ed = editor(&rd);
        let mut s = Session::new(&rd, None, &r.cache, journal::disk());
        assert_eq!(s.keep(&ed), Keep::Clean);
        typed(&mut ed, 0, "A");
        kept(&mut s, &ed);
        typed(&mut ed, 1, "B");
    }
    let recs = listed(&r, "local");
    assert_eq!(recs.len(), 1);
    let dir = recs[0].orphan.dir().to_path_buf();
    let size: u64 = names(&dir)
        .iter()
        .map(|n| std::fs::metadata(dir.join(n)).unwrap().len())
        .sum();
    assert_eq!(recs[0].size, size, "the recovery row's size");
    assert!(recs[0].size >= OLD.len() as u64, "the original is in it");
    assert!(recs[0].date.is_some());
    assert!(!recs[0].saved());
    // Later: dropped, it stays and is offered again.
    drop(recs);
    let back = restored(&r);
    let mut want = b"A".to_vec();
    want.extend_from_slice(OLD);
    assert_eq!(doc(&back.editor), want);
    assert!(!back.editor.buffer().is_saved());
    assert!(back.lines.is_empty(), "{:?}", back.lines);
}

/// ES-3: Save, Undo past it, kill, Restore → the buffer as it was before the kill, unsaved
/// against the saved file; a Save from there writes it.
#[test]
fn es3_undo_past_a_save_survives_a_kill() {
    let r = rig(OLD);
    let rd = read(&r, Encoding::Utf8);
    let saved_bytes;
    {
        let mut ed = editor(&rd);
        let mut s = Session::new(&rd, None, &r.cache, journal::disk());
        typed(&mut ed, 0, "A");
        kept(&mut s, &ed);
        written(&s.save(&mut ed, &mut LocalFs::new()).unwrap());
        saved_bytes = std::fs::read(&r.target).unwrap();
        assert!(ed.undo());
        assert_eq!(doc(&ed), OLD);
        kept(&mut s, &ed);
    }
    let mut back = restored(&r);
    assert_eq!(doc(&back.editor), OLD);
    assert!(!back.editor.buffer().is_saved());
    assert!(
        back.lines.is_empty(),
        "against the saved file: {:?}",
        back.lines
    );
    assert_eq!(back.session.baseline().sha256, sha(&saved_bytes));
    let saved = back
        .session
        .save(&mut back.editor, &mut LocalFs::new())
        .unwrap();
    written(&saved);
    assert_eq!(std::fs::read(&r.target).unwrap(), OLD);
}

/// ES-3: Edit, kill, replace the file → Restore rebuilds from the journal (the file is not read
/// for it), with the `changed since` line; Save → the conflict, with Overwrite. The same with the
/// file deleted → the conflict without Overwrite (Save As… and Save copy).
#[test]
fn es3_a_file_changed_after_the_kill_restores_and_save_asks() {
    for deleted in [false, true] {
        let r = rig(OLD);
        let rd = read(&r, Encoding::Utf8);
        {
            let mut ed = editor(&rd);
            let mut s = Session::new(&rd, None, &r.cache, journal::disk());
            typed(&mut ed, 0, "A");
            kept(&mut s, &ed);
        }
        match deleted {
            true => std::fs::remove_file(&r.target).unwrap(),
            false => std::fs::write(&r.target, b"replaced meanwhile\n").unwrap(),
        }
        let mut back = restored(&r);
        let mut want = b"A".to_vec();
        want.extend_from_slice(OLD);
        assert_eq!(doc(&back.editor), want, "{deleted}");
        let line = format!("notes.txt {}", journal::NOTE_TEXT_CHANGED);
        assert_eq!(back.lines, [line], "{deleted}");
        let saved = back
            .session
            .save(&mut back.editor, &mut LocalFs::new())
            .unwrap();
        assert!(
            matches!(saved, Saved::Conflict { overwrite, .. } if overwrite == !deleted),
            "{deleted}: {saved:?}"
        );
    }
}

/// A journal double whose disk is slow to make `original` durable — held until released — or
/// has no room for it.
struct SlowDisk {
    gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
    free: Option<u64>,
}

impl journal::Fs for SlowDisk {
    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        journal::Disk.create_dir(path)
    }
    fn write_new(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        journal::Disk.write_new(path, bytes)
    }
    fn sync_file(&self, path: &Path) -> std::io::Result<()> {
        if path.ends_with("original.tmp") {
            let (lock, cv) = &*self.gate;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
        }
        journal::Disk.sync_file(path)
    }
    fn sync_dir(&self, path: &Path) -> std::io::Result<()> {
        journal::Disk.sync_dir(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        journal::Disk.rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        journal::Disk.remove_file(path)
    }
    fn remove_dir(&self, path: &Path) -> std::io::Result<()> {
        journal::Disk.remove_dir(path)
    }
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        journal::Disk.read(path)
    }
    fn list(&self, dir: &Path) -> std::io::Result<Vec<String>> {
        journal::Disk.list(dir)
    }
    fn node(&self, path: &Path, follow: bool) -> std::io::Result<Option<journal::Node>> {
        journal::Disk.node(path, follow)
    }
    fn lock(&self, path: &Path, create: bool) -> std::io::Result<Option<journal::Held>> {
        journal::Disk.lock(path, create)
    }
    fn free_space(&self, path: &Path) -> Option<u64> {
        self.free.or_else(|| journal::Disk.free_space(path))
    }
}

/// ES-3: a slow disk holding `original`'s fsync → no generation published and `keeping…` until it
/// returns, then `kept`. A disk with no room for a copy of the file → no journal, the row says
/// `NOT kept on disk (no room for a copy of the file)`, and the edits still count as unsaved.
#[test]
fn es3_kept_only_once_the_original_is_durable_and_never_without_room() {
    let r = rig(OLD);
    let rd = read(&r, Encoding::Utf8);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let slow = Arc::new(SlowDisk {
        gate: gate.clone(),
        free: None,
    });
    let mut ed = editor(&rd);
    let mut s = Session::new(&rd, None, &r.cache, slow);
    typed(&mut ed, 0, "A");
    for _ in 0..3 {
        assert_eq!(s.keep(&ed), Keep::Keeping);
        std::thread::sleep(Duration::from_millis(20));
    }
    let dir = s.journal_dir().unwrap().to_path_buf();
    assert!(
        names(&dir)
            .iter()
            .all(|n| !n.starts_with("gen-") && n != "CURRENT"),
        "{:?}",
        names(&dir)
    );
    assert_eq!(Keep::Keeping.line("~/j"), "unsaved — keeping…");
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    kept(&mut s, &ed);
    assert!(
        names(&dir).contains(&"gen-1".to_string()),
        "{:?}",
        names(&dir)
    );

    let r = rig(OLD);
    let rd = read(&r, Encoding::Utf8);
    let full = Arc::new(SlowDisk {
        gate: Arc::new((Mutex::new(true), std::sync::Condvar::new())),
        free: Some(4),
    });
    let mut ed = editor(&rd);
    let mut s = Session::new(&rd, None, &r.cache, full);
    typed(&mut ed, 0, "A");
    let t0 = Instant::now();
    let status = loop {
        match s.keep(&ed) {
            Keep::Keeping if t0.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_millis(2))
            }
            other => break other,
        }
    };
    assert_eq!(
        status.line("~/j"),
        "unsaved — NOT kept on disk (no room for a copy of the file)"
    );
    assert!(s.journal_dir().is_none(), "no journal published");
    assert!(
        recoveries(&journal::disk(), &r.cache, "local", p(&r.target))
            .unwrap()
            .is_empty()
    );
    assert!(!ed.buffer().is_saved(), "the close prompt still asks");
}

/// ES-3: Edit, Save, a clean re-read (E30), edit, kill → Restore rebuilds against the new
/// original (`k = 2`), whose bytes are the re-read file's.
#[test]
fn es3_a_reread_starts_a_new_original_generation() {
    let r = rig(OLD);
    let rd = read(&r, Encoding::Utf8);
    let reread_bytes;
    {
        let mut ed = editor(&rd);
        let mut s = Session::new(&rd, None, &r.cache, journal::disk());
        typed(&mut ed, 0, "A");
        kept(&mut s, &ed);
        written(&s.save(&mut ed, &mut LocalFs::new()).unwrap());
        // Someone's checkout changed the file; the clean editor reads it again.
        std::fs::write(&r.target, b"checked out\n").unwrap();
        let again = read(&r, Encoding::Utf8);
        reread_bytes = again.original.bytes().to_vec();
        let doubt = Doubt::of(&again.original, false);
        s.reread(&mut ed, again, doubt);
        assert_eq!(ed.buffer().generation(), 2);
        typed(&mut ed, 0, "B");
        kept(&mut s, &ed);
    }
    let back = restored(&r);
    assert_eq!(back.editor.buffer().generation(), 2);
    assert_eq!(back.editor.buffer().original().bytes(), reread_bytes);
    assert_eq!(doc(&back.editor), b"Bchecked out\n");
    assert!(back.lines.is_empty(), "{:?}", back.lines);
}

/// E32 / 07 §3.1: a Save started before the journal's `original` is durable waits for it, so its
/// every phase is recorded — never a guarded replace that the journal does not hold.
#[test]
fn a_save_before_the_original_is_durable_is_recorded() {
    let r = rig(OLD);
    let rd = read(&r, Encoding::Utf8);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let slow = Arc::new(SlowDisk {
        gate: gate.clone(),
        free: None,
    });
    let mut ed = editor(&rd);
    let mut s = Session::new(&rd, None, &r.cache, slow);
    typed(&mut ed, 0, "x-");
    assert_eq!(s.keep(&ed), Keep::Keeping);
    let opener = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
    });
    let mut recorded = None;
    let saved = s
        .save_with(&mut ed, &mut LocalFs::new(), &mut |g, intent| {
            if matches!(g, Guard::Exchanging { .. }) {
                recorded = Some(intent.and_then(|i| i.guard.clone()));
            }
            Ok(())
        })
        .unwrap();
    opener.join().unwrap();
    written(&saved);
    assert!(
        matches!(recorded, Some(Some(Guarded::Exchanging { .. }))),
        "{recorded:?}"
    );
}

/// §9.1 step 2: a Save of this session that failed after `exchanging` was recorded, or whose
/// rename got no reply, is reconciled by the next Save before anything is written again — not
/// landed, it goes on and saves; landed, it is found and nothing is written again.
#[test]
fn the_next_save_reconciles_a_cut_off_one_first() {
    let r = rig(OLD);
    let rd = read(&r, Encoding::Utf8);
    let mut ed = editor(&rd);
    let mut s = Session::new(&rd, None, &r.cache, journal::disk());
    typed(&mut ed, 0, "x-");
    kept(&mut s, &ed);
    let dead = Arc::new(AtomicBool::new(false));
    let mut fs = Dying::over(Box::new(LocalFs::new()), &dead, None);
    fs.fail = Some("exchange");
    let e = s.save(&mut ed, &mut fs).unwrap_err();
    assert!(format!("{e:#}").contains("not saved"), "{e:#}");
    assert!(
        s.journal.as_ref().unwrap().pending_save().is_some(),
        "the record stays"
    );
    assert_eq!(std::fs::read(&r.target).unwrap(), OLD);
    let saved = s.save(&mut ed, &mut LocalFs::new()).unwrap();
    written(&saved);
    assert_eq!(std::fs::read(&r.target).unwrap(), NEW);
    assert!(s.journal.as_ref().unwrap().pending_save().is_none());

    let Some((conn, rd)) = opened(&r, true) else {
        return;
    };
    let mut ed = editor(&rd);
    let mut s = Session::new(&rd, Some("box"), &r.cache, journal::disk());
    typed(&mut ed, 0, "y-");
    kept(&mut s, &ed);
    let mut fs = Dying::over(conn, &dead, None);
    fs.lose = Some("rename");
    let e = s.save(&mut ed, &mut fs).unwrap_err();
    assert!(
        remote::is_outcome_unknown(&e) || format!("{e:#}").contains("outcome unknown"),
        "{e:#}"
    );
    let mut want = b"y-".to_vec();
    want.extend_from_slice(NEW);
    assert_eq!(
        std::fs::read(&r.target).unwrap(),
        want,
        "the rename took effect"
    );
    let found = s.save(&mut ed, fs.inner.as_mut()).unwrap();
    let Saved::Found { notes } = &found else {
        panic!("{found:?}")
    };
    assert!(
        notes[0].contains("landed (found now): nothing was written again"),
        "{notes:?}"
    );
    assert!(ed.buffer().is_saved());
    assert_eq!(s.baseline().sha256, sha(&want));
    assert_eq!(leftovers(&r.dir), (vec![], vec![]));
}
