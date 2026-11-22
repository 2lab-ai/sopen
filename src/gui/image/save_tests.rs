//! An image Save cut off at each step (07 §3.1, E18's image cases), checked against the real
//! folder of the file: the journal records each step before the request that follows it, and
//! the next open — or the next Save — reconciles it from fresh reads, never writing twice. Also
//! Esc stopping a Save before its rename (§10), a scripted run's journal (none kept, unless an
//! outcome is unknown), and `SOPEN_KEYS` played into a window of its own.

use super::edit_tests::{bytes_of, current, journal_dirs_of, words};
use super::*;
use crate::gui::testkit::{command, Driver};
use crate::remote::patch::{CopyCtl, Each, Handle, OpenFor, PatchFs};
use crate::remote::{
    CreateError, Created, Entry, Like, LocalFs, RemoteFs, SftpRemote, Stat, Uploaded,
};
use eframe::egui::{Event, Key, Modifiers};
use std::sync::atomic::AtomicBool;

/// How the connection cuts a Save off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Cut {
    #[default]
    None,
    /// The temp's create gets no reply, and nothing was made.
    CreateNoReply,
    /// The temp's create gets no reply, after it made the (empty) temp — through the backend.
    CreatedNoReply,
    /// The connection dies once the temp, written, was stat'ed: every request fails from then
    /// on, a remove included.
    DieAfterTempStat,
    /// The connection dies right before the rename (the temp staged, the record `renaming`):
    /// the rename and every request after fail.
    DieBeforeRename,
    /// The rename lands, and its reply is lost.
    RenameReplyLost,
    /// The temp's create waits for `release`.
    HoldCreate,
    /// A remove waits for `release`.
    HoldRemove,
    /// A remove is refused.
    FailRemove,
}

#[derive(Default)]
struct Control {
    cut: std::sync::Mutex<Cut>,
    dead: AtomicBool,
    holding: AtomicBool,
    release: AtomicBool,
    /// Every request, as `op path` (offsets for reads), in order.
    log: std::sync::Mutex<Vec<String>>,
}

impl Control {
    fn note(&self, line: String) {
        self.log.lock().unwrap().push(line);
    }

    fn taken(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }

    fn writes(&self) -> Vec<String> {
        let mutating = [
            "create",
            "create-temp",
            "rename",
            "remove",
            "set-mode",
            "set-owner",
            "set-handle-mode",
            "link",
            "write",
            "copy",
        ];
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|l| mutating.iter().any(|m| l.starts_with(&format!("{m} "))))
            .cloned()
            .collect()
    }

    fn hold(&self) {
        self.holding.store(true, Ordering::SeqCst);
        while !self.release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// A connection — the local disk, or a real `sftp-server` — that cuts a Save off as `Control`
/// says and logs every request, those of its handles included.
struct Cutting {
    inner: Box<dyn RemoteFs + Send>,
    control: Arc<Control>,
}

fn is_temp(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .unwrap_or_default()
        .contains(".sopen-")
}

impl Cutting {
    fn cut(&self) -> Cut {
        *self.control.cut.lock().unwrap()
    }

    fn alive(&self) -> Result<()> {
        if self.control.dead.load(Ordering::SeqCst) {
            bail!("simulated: the connection is gone");
        }
        Ok(())
    }

    fn handles(&mut self) -> &mut dyn PatchFs {
        self.inner.patcher().expect("the backend takes handles")
    }
}

impl RemoteFs for Cutting {
    fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.alive()?;
        self.inner.stat_opt(path)
    }
    fn lstat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.alive()?;
        self.control.note(format!("lstat {path}"));
        let st = self.inner.lstat_opt(path)?;
        if self.cut() == Cut::DieAfterTempStat && is_temp(path) {
            self.control.dead.store(true, Ordering::SeqCst);
        }
        Ok(st)
    }
    fn realpath(&mut self, path: &str) -> Result<String> {
        self.alive()?;
        self.inner.realpath(path)
    }
    fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>> {
        self.alive()?;
        self.control.note(format!("read-head {path} {max}"));
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
        self.control.note(format!("read-all {path}"));
        self.inner.read_to(path, out, progress)
    }
    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<Like>,
    ) -> std::result::Result<Uploaded, CreateError> {
        if let Err(e) = self.alive() {
            return Err(CreateError::new(Created::No, e));
        }
        self.control.note(format!("create {path}"));
        match self.cut() {
            Cut::CreateNoReply => {
                return Err(CreateError::new(
                    Created::Unknown,
                    anyhow!("simulated: the create got no reply"),
                ))
            }
            Cut::CreatedNoReply => {
                // Made through the backend, empty, then the reply is lost.
                let empty = tempfile::NamedTempFile::new().unwrap();
                let _ = self.inner.create_from(empty.path(), path, None)?;
                return Err(CreateError::new(
                    Created::Unknown,
                    anyhow!("simulated: the create got no reply"),
                ));
            }
            Cut::HoldCreate => self.control.hold(),
            _ => {}
        }
        self.inner.create_from(local, path, like)
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        self.alive()?;
        self.control.note(format!("set-mode {path}"));
        self.inner.set_mode(path, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        if self.cut() == Cut::DieBeforeRename {
            self.control.dead.store(true, Ordering::SeqCst);
        }
        self.alive()?;
        self.control.note(format!("rename {from} {to}"));
        self.inner.rename_over(from, to)?;
        if self.cut() == Cut::RenameReplyLost {
            return Err(crate::remote::unknown(
                "simulated: the rename landed, its reply was lost",
            ));
        }
        Ok(())
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.alive()?;
        self.control.note(format!("link {from} {to}"));
        self.inner.link_new(from, to)
    }
    fn can_link(&self) -> bool {
        self.inner.can_link()
    }
    fn can_replace(&self) -> bool {
        self.inner.can_replace()
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        if self.cut() == Cut::HoldRemove {
            self.control.hold();
        }
        if self.cut() == Cut::FailRemove {
            self.control.note(format!("remove-refused {path}"));
            bail!("simulated: the remove was refused");
        }
        self.alive()?;
        self.control.note(format!("remove {path}"));
        self.inner.remove(path)
    }
    fn hash_check_limit(&self) -> u64 {
        self.inner.hash_check_limit()
    }
    fn patcher(&mut self) -> Option<&mut dyn PatchFs> {
        self.inner.patcher()?;
        Some(self)
    }
}

impl PatchFs for Cutting {
    fn can_copy(&self) -> bool {
        false
    }
    fn open(&mut self, path: &str, how: OpenFor) -> Result<Handle> {
        self.alive()?;
        self.handles().open(path, how)
    }
    fn create_temp(&mut self, path: &str) -> std::result::Result<Handle, CreateError> {
        self.control.note(format!("create-temp {path}"));
        self.handles().create_temp(path)
    }
    fn fstat(&mut self, file: &mut Handle) -> Result<Stat> {
        self.alive()?;
        self.handles().fstat(file)
    }
    fn read_at(&mut self, file: &mut Handle, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.alive()?;
        let got = self.handles().read_at(file, offset, len)?;
        self.control
            .note(format!("read {} {offset} {}", file.path(), got.len()));
        Ok(got)
    }
    fn read_through(
        &mut self,
        src: &mut Handle,
        size: u64,
        dst: Option<&mut Handle>,
        each: &mut Each<'_>,
    ) -> Result<u64> {
        self.alive()?;
        self.control
            .note(format!("read-through {} {size}", src.path()));
        self.handles().read_through(src, size, dst, each)
    }
    fn write_at(&mut self, file: &mut Handle, offset: u64, bytes: &[u8]) -> Result<()> {
        self.alive()?;
        self.control.note(format!("write {} {offset}", file.path()));
        self.handles().write_at(file, offset, bytes)
    }
    fn set_owner(&mut self, file: &mut Handle, uid: u32, gid: u32) -> Result<()> {
        self.control.note(format!("set-owner {}", file.path()));
        self.handles().set_owner(file, uid, gid)
    }
    fn set_handle_mode(&mut self, file: &mut Handle, mode: u32) -> Result<()> {
        self.control
            .note(format!("set-handle-mode {}", file.path()));
        self.handles().set_handle_mode(file, mode)
    }
    fn fsync(&mut self, file: &mut Handle) -> Result<()> {
        self.control.note(format!("fsync {}", file.path()));
        self.handles().fsync(file)
    }
    fn copy(
        &mut self,
        src: &mut Handle,
        dst: &mut Handle,
        size: u64,
        ctl: &mut CopyCtl<'_>,
    ) -> Result<()> {
        self.control
            .note(format!("copy {} {}", src.path(), dst.path()));
        self.handles().copy(src, dst, size, ctl)
    }
    fn close(&mut self, file: Handle) {
        self.handles().close(file)
    }
}

/// Which connection the file is reached through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Local,
    Sftp,
}

fn connection(side: Side) -> Option<Box<dyn RemoteFs + Send>> {
    match side {
        Side::Local => Some(Box::new(LocalFs::new())),
        Side::Sftp => SftpRemote::local_server().map(|s| Box::new(s) as Box<dyn RemoteFs + Send>),
    }
}

/// A file, its cache, and the window on it through a connection that cuts as told.
struct Rig {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    path: PathBuf,
    cache: PathBuf,
    side: Side,
    control: Arc<Control>,
    win: ImageWindow,
    drv: Driver,
}

fn window_through(path: &Path, cache: &Path, mut fs: Box<dyn RemoteFs + Send>) -> ImageWindow {
    let bytes = std::fs::read(path).unwrap();
    let abs = path.to_str().unwrap().to_string();
    // As the connection reports it (SFTP v3 carries whole seconds).
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(fs, None, Some("host".into()), abs, at);
    let file = ImageFile::new(origin, bytes, cache);
    let doc = Doc::decode(file.original()).unwrap();
    ImageWindow::new(file, doc, Selection::default(), DEFAULT_BLOCK, Vec::new())
}

fn rig(side: Side) -> Option<Rig> {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("remote");
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("shot.png");
    std::fs::write(&path, bytes_of(ImageFormat::Png)).unwrap();
    let cache = tmp.path().join("cache");
    let control = Arc::new(Control::default());
    let fs = Box::new(Cutting {
        inner: connection(side)?,
        control: control.clone(),
    });
    let win = window_through(&path, &cache, fs);
    let mut rig = Rig {
        _tmp: tmp,
        dir,
        path,
        cache,
        side,
        control,
        win,
        drv: Driver::new(1000.0, 700.0),
    };
    rig.settle();
    Some(rig)
}

impl Rig {
    fn settle(&mut self) {
        self.drv.until(&mut self.win, 20, |w| w.listed && w.quiet());
        self.drv.idle(&mut self.win, 2);
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.drv.key(&mut self.win, key, modifiers);
    }

    /// A crop, kept in the journal.
    fn crop(&mut self) {
        self.win.sel.set([0, 0, 90, 60]);
        self.drv.idle(&mut self.win, 1);
        self.key(Key::C, Modifiers::NONE);
        let lost = vec![Event::WindowFocused(false)];
        self.drv.frame(&mut self.win, lost, Modifiers::NONE);
        self.drv.until(&mut self.win, 10, |w| w.quiet());
    }

    fn cut(&self, cut: Cut) {
        *self.control.cut.lock().unwrap() = cut;
    }

    /// The window gone without a word (a kill), and the file opened again through a healthy
    /// connection of the same side — logged from its first request (`control`, a new one).
    fn reopen(&mut self) {
        self.control = Arc::new(Control::default());
        let fs = Box::new(Cutting {
            inner: connection(self.side).unwrap(),
            control: self.control.clone(),
        });
        let next = window_through(&self.path, &self.cache, fs);
        drop(std::mem::replace(&mut self.win, next));
        self.drv = Driver::new(1000.0, 700.0);
        self.settle();
    }

    /// The temps in the file's folder.
    fn temps(&self) -> Vec<String> {
        std::fs::read_dir(&self.dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".sopen-"))
            .collect()
    }

    fn journals(&self) -> Vec<PathBuf> {
        journal_dirs_of(&self.cache, "host", &self.path)
    }

    /// The `save` record of the one journal, if any: its phase.
    fn phase(&self) -> Option<String> {
        let dirs = self.journals();
        let bytes = std::fs::read(dirs.first()?.join("save")).ok()?;
        let intent = journal::parse_save(&bytes).unwrap();
        Some(
            format!("{:?}", intent.phase)
                .split([' ', '{'])
                .next()?
                .to_string(),
        )
    }

    fn texts(&self) -> Vec<String> {
        self.drv.texts().into_iter().map(|(_, _, t)| t).collect()
    }

    fn save(&mut self) {
        self.key(Key::S, command(false));
        self.settle();
    }
}

fn journal_edits_of(rig: &Rig) -> Vec<String> {
    words(&current(&rig.journals()[0]))
}

/// What a cut-off Save left on disk, read before any reopen: the phase its record is in, the
/// temp it names, and the edits of the generation it names.
struct Left {
    phase: &'static str,
    temp: Option<PathBuf>,
    saved: Vec<String>,
}

fn left_by(rig: &Rig) -> Left {
    let dirs = rig.journals();
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    let intent =
        journal::parse_save(&std::fs::read(dirs[0].join("save")).expect("a save record")).unwrap();
    let (phase, temp) = match &intent.phase {
        journal::Phase::Naming { temp } => ("naming", Some(temp)),
        journal::Phase::Created { temp, .. } => ("created", Some(temp)),
        journal::Phase::Staged { temp, .. } => ("staged", Some(temp)),
        journal::Phase::Renaming { temp, .. } => ("renaming", Some(temp)),
        journal::Phase::Writing { .. } => ("writing", None),
    };
    let gen = journal::parse_generation(
        &std::fs::read(dirs[0].join(format!("gen-{}", intent.saved.n))).unwrap(),
    )
    .unwrap();
    Left {
        phase,
        temp: temp.map(PathBuf::from),
        saved: words(&gen),
    }
}

/// What is at a cut-off Save's temp path.
#[derive(Debug, PartialEq, Eq)]
enum TempIs {
    Absent,
    Empty,
    /// The encoded output, whole.
    Output,
}

fn temp_is(path: &Path, output: &[u8]) -> TempIs {
    match std::fs::read(path) {
        Err(_) => TempIs::Absent,
        Ok(b) if b.is_empty() => TempIs::Empty,
        Ok(b) => {
            assert_eq!(b, output, "the temp holds the encoded output whole");
            TempIs::Output
        }
    }
}

/// The sides a cut-off Save is run through: the local disk, and a real `sftp-server` — which
/// fails, rather than skips, under `SOPEN_REQUIRE_SFTP_SERVER` when there is none.
fn sides() -> Vec<Side> {
    let mut sides = vec![Side::Local];
    if SftpRemote::local_server().is_some() {
        sides.push(Side::Sftp);
    }
    sides
}

/// E18 for an image Save, cut at each step, through both sides. Before any reopen, what is on
/// disk is read as it was left: the record's phase at the cut, the temp it names (absent,
/// empty, or holding the encoded output), the file, and the generation the record names (the
/// crop). Then the next open:
/// - cut before the temp (the create's reply lost, nothing made): not landed, the record ends,
///   the edits offered;
/// - cut after the temp was created (reply lost, an empty temp): only the name was recorded, so
///   the temp is asked about — Remove it, Keep it; Restore and Discard wait; Later asks again;
/// - cut after the temp was written (`created`, the connection gone, its remove too) and cut
///   right before the rename (`renaming`, the temp staged): the temp equals the record — removed
///   unasked; the edits offered;
/// - the rename landed, its reply lost: landed, the journal retired, nothing written again.
#[test]
fn e18_an_image_save_cut_at_each_step_is_reconciled_on_reopen() {
    for side in sides() {
        let cases = [
            (Cut::CreateNoReply, "naming", TempIs::Absent, false),
            (Cut::CreatedNoReply, "naming", TempIs::Empty, false),
            (Cut::DieAfterTempStat, "created", TempIs::Output, false),
            (Cut::DieBeforeRename, "renaming", TempIs::Output, false),
            (Cut::RenameReplyLost, "renaming", TempIs::Absent, true),
        ];
        for (cut, phase, temp, lands) in cases {
            let case = format!("{side:?} {cut:?}");
            let mut rig = rig(side).unwrap();
            let original = std::fs::read(&rig.path).unwrap();
            rig.crop();
            let output = rig.win.doc.encode().unwrap().bytes;
            rig.cut(cut);
            rig.save();
            // What the cut left, before any reopen.
            let left = left_by(&rig);
            assert_eq!(left.phase, phase, "{case}");
            assert_eq!(left.saved, ["crop"], "{case}: the generation it names");
            let temp_path = left.temp.expect("a temp is named");
            assert_eq!(temp_path.parent(), Some(rig.dir.as_path()), "{case}");
            assert_eq!(temp_is(&temp_path, &output), temp, "{case}");
            let target = std::fs::read(&rig.path).unwrap();
            assert_eq!(&target, if lands { &output } else { &original }, "{case}");
            assert!(
                rig.win.doc.unsaved(),
                "{case}: the outcome is unknown, the edits stay"
            );
            let before = std::fs::metadata(&rig.path).unwrap().modified().unwrap();
            rig.reopen();
            let now = std::fs::metadata(&rig.path).unwrap().modified().unwrap();
            assert_eq!(now, before, "{case}: nothing is written to the file");
            // Every request the reopened window sent that changes anything: none of the file,
            // and only the removal of a temp provably the cut-off Save's own.
            let target_s = rig.path.to_str().unwrap().to_string();
            let temp_s = temp_path.to_str().unwrap().to_string();
            let sent = rig.control.writes();
            assert!(
                sent.iter().all(|w| !w.contains(&target_s)),
                "{case}: {sent:?}"
            );
            let want: Vec<String> = match cut {
                Cut::DieAfterTempStat | Cut::DieBeforeRename => vec![format!("remove {temp_s}")],
                _ => Vec::new(),
            };
            assert_eq!(sent, want, "{case}: what recovery changed");
            match cut {
                Cut::CreatedNoReply => {
                    assert!(rig.drv.find_text("Remove it").is_some(), "{case}");
                    assert!(rig.drv.find_text("Keep it").is_some(), "{case}");
                    assert!(rig.drv.find_text("Restore").is_none(), "{case}: waits");
                    assert!(rig.drv.find_text("Discard").is_none(), "{case}");
                    assert_eq!(
                        temp_is(&temp_path, &output),
                        TempIs::Empty,
                        "{case}: unasked, kept"
                    );
                    assert_eq!(
                        rig.phase().as_deref(),
                        Some("Naming"),
                        "{case}: the record stays"
                    );
                    rig.drv.click_text(&mut rig.win, "Later");
                    rig.reopen();
                    assert!(
                        rig.drv.find_text("Remove it").is_some(),
                        "{case}: asked again"
                    );
                    assert_eq!(rig.control.writes(), Vec::<String>::new(), "{case}");
                    rig.drv.click_text(&mut rig.win, "Remove it");
                    rig.drv.until(&mut rig.win, 10, |w| w.quiet());
                    rig.drv.idle(&mut rig.win, 2);
                    assert_eq!(
                        temp_is(&temp_path, &output),
                        TempIs::Absent,
                        "{case}: removed"
                    );
                    assert_eq!(
                        rig.control.writes(),
                        [format!("remove {temp_s}")],
                        "{case}: only the file asked about"
                    );
                    assert!(
                        rig.drv.find_text("Restore").is_some(),
                        "{case}: {:?}",
                        rig.texts()
                    );
                    assert_eq!(rig.phase(), None, "{case}: answered, the record ended");
                }
                Cut::RenameReplyLost => {
                    assert!(
                        rig.texts()
                            .iter()
                            .any(|t| t
                                .ends_with("landed (found on reopen): nothing was written again")),
                        "{case}: {:?}",
                        rig.texts()
                    );
                    assert!(rig.journals().is_empty(), "{case}: retired");
                    assert!(rig.drv.find_text("Restore").is_none(), "{case}");
                }
                _ => {
                    assert_eq!(temp_is(&temp_path, &output), TempIs::Absent, "{case}");
                    assert!(rig.drv.find_text("Remove it").is_none(), "{case}");
                    assert!(
                        rig.drv.find_text("Restore").is_some(),
                        "{case}: {:?}",
                        rig.texts()
                    );
                    assert_eq!(rig.phase(), None, "{case}: not landed, the record ended");
                    assert_eq!(journal_edits_of(&rig), ["crop"], "{case}");
                }
            }
            assert_eq!(
                std::fs::read(&rig.path).unwrap(),
                target,
                "{case}: as the cut left it"
            );
            assert!(rig.temps().is_empty(), "{case}: {:?}", rig.temps());
        }
    }
}

/// P1: Remove it removes only the file the question was about. A file put at the temp's name
/// after the question was shown — other content, another size — survives Remove it; the record
/// stays, and the row says why. Through both sides.
#[test]
fn remove_it_keeps_a_file_that_changed_since_the_question_was_shown() {
    for side in sides() {
        let mut rig = rig(side).unwrap();
        rig.crop();
        rig.cut(Cut::CreatedNoReply);
        rig.save();
        let temp = left_by(&rig).temp.unwrap();
        rig.reopen();
        assert!(rig.drv.find_text("Remove it").is_some(), "{side:?}");
        // Another program's file takes the name meanwhile.
        std::fs::remove_file(&temp).unwrap();
        std::fs::write(&temp, b"someone else's file, not sopen's").unwrap();
        rig.drv.click_text(&mut rig.win, "Remove it");
        rig.drv.until(&mut rig.win, 10, |w| w.quiet());
        rig.drv.idle(&mut rig.win, 2);
        assert_eq!(
            std::fs::read(&temp).unwrap(),
            b"someone else's file, not sopen's",
            "{side:?}: it survives"
        );
        assert_eq!(
            rig.phase().as_deref(),
            Some("Naming"),
            "{side:?}: the record stays"
        );
        assert!(
            rig.texts()
                .iter()
                .any(|t| t.contains("changed since it was shown")),
            "{side:?}: says why: {:?}",
            rig.texts()
        );
        assert!(
            rig.drv.find_text("Remove it").is_some(),
            "{side:?}: still asked"
        );
    }
}

/// After 10 s it runs `release` and says it fired: a pass that waits on what a test holds fails
/// the test instead of hanging it. Stops when dropped.
struct Watchdog {
    fired: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl Watchdog {
    fn new(release: impl Fn() + Send + 'static) -> Self {
        let (fired, done) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (f, d) = (fired.clone(), done.clone());
        std::thread::spawn(move || {
            let until = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < until {
                if d.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            f.store(true, Ordering::SeqCst);
            release();
        });
        Self { fired, done }
    }

    fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
    }
}

/// P2: Remove it runs on a worker. The removal is held at a barrier: the passes that took the
/// click, and those after it, return while it is held (a pass that waited for it would be
/// freed only by the watchdog, failing the test); meanwhile the row takes no answer; released,
/// the removal lands and the row is answered.
#[test]
fn remove_it_does_not_hold_the_window() {
    let mut rig = rig(Side::Local).unwrap();
    rig.crop();
    rig.cut(Cut::CreatedNoReply);
    rig.save();
    let temp = left_by(&rig).temp.unwrap();
    let control = Arc::new(Control::default());
    *control.cut.lock().unwrap() = Cut::HoldRemove;
    let fs = Box::new(Cutting {
        inner: Box::new(LocalFs::new()),
        control: control.clone(),
    });
    let next = window_through(&rig.path, &rig.cache, fs);
    drop(std::mem::replace(&mut rig.win, next));
    rig.drv = Driver::new(1000.0, 700.0);
    rig.settle();
    let at = rig.drv.find_text("Remove it").expect("asked").center();
    let late = control.clone();
    let dog = Watchdog::new(move || late.release.store(true, Ordering::SeqCst));
    rig.drv.click(&mut rig.win, at);
    rig.drv
        .until(&mut rig.win, 5, |_| control.holding.load(Ordering::SeqCst));
    assert!(rig.win.removing.is_some(), "running");
    // Its row takes no answer meanwhile.
    rig.drv.click_text(&mut rig.win, "Keep it");
    rig.drv.idle(&mut rig.win, 3);
    assert!(!dog.fired(), "a pass waited for the removal");
    assert!(!control.release.load(Ordering::SeqCst), "still held");
    assert!(temp.exists());
    assert_eq!(rig.phase().as_deref(), Some("Naming"), "no answer taken");
    control.release.store(true, Ordering::SeqCst);
    rig.drv.until(&mut rig.win, 10, |w| w.quiet());
    assert!(!temp.exists(), "removed once it was back");
    assert_eq!(rig.phase(), None);
}

/// The same lost reply in the same session: the next Save reconciles first, finds it landed,
/// and writes nothing again; the edits count as saved.
#[test]
fn the_next_save_finds_a_landed_save_and_writes_nothing_again() {
    let mut rig = rig(Side::Local).unwrap();
    rig.crop();
    let output = rig.win.doc.encode().unwrap().bytes;
    rig.cut(Cut::RenameReplyLost);
    rig.save();
    assert!(rig.win.doc.unsaved());
    let landed = std::fs::metadata(&rig.path).unwrap().modified().unwrap();
    rig.cut(Cut::None);
    rig.save();
    assert!(
        rig.win
            .status
            .text
            .ends_with("landed (found now): nothing was written again"),
        "{}",
        rig.win.status.text
    );
    assert!(!rig.win.doc.unsaved(), "saved");
    assert_eq!(std::fs::read(&rig.path).unwrap(), output);
    let now = std::fs::metadata(&rig.path).unwrap().modified().unwrap();
    assert_eq!(now, landed, "not written again");
    rig.drv.idle(&mut rig.win, 3);
    assert!(rig.journals().is_empty(), "nothing unsaved: retired");
    // Editing goes on against the saved file.
    rig.win.sel.set([0, 0, 10, 10]);
    rig.drv.idle(&mut rig.win, 1);
    rig.key(Key::P, Modifiers::NONE);
    rig.drv.frame(
        &mut rig.win,
        vec![Event::WindowFocused(false)],
        Modifiers::NONE,
    );
    rig.drv.until(&mut rig.win, 10, |w| w.quiet());
    let gen = current(&rig.journals()[0]);
    assert_eq!(words(&gen), ["pixelize"]);
    assert_eq!(gen.head.baseline.sha256, Some(journal::sha256(&output)));
}

/// 07 §10: Esc stops a Save before its rename — the status row says where it is and how to
/// stop it, then that nothing was written; the temp is removed, the file untouched, the edits
/// unsaved and kept, the `save` record ended.
#[test]
fn esc_stops_a_save_before_its_rename() {
    let mut rig = rig(Side::Local).unwrap();
    let original = std::fs::read(&rig.path).unwrap();
    rig.crop();
    rig.cut(Cut::HoldCreate);
    rig.key(Key::S, command(false));
    let holding = rig.control.clone();
    rig.drv
        .until(&mut rig.win, 10, |_| holding.holding.load(Ordering::SeqCst));
    rig.drv.idle(&mut rig.win, 1);
    assert!(
        rig.win.status.text.starts_with(SAVING) && rig.win.status.text.ends_with("Esc stops it"),
        "{}",
        rig.win.status.text
    );
    rig.key(Key::Escape, Modifiers::NONE);
    rig.control.release.store(true, Ordering::SeqCst);
    rig.settle();
    assert_eq!(rig.win.status.text, STOPPED);
    assert_eq!(std::fs::read(&rig.path).unwrap(), original);
    assert!(rig.temps().is_empty(), "{:?}", rig.temps());
    assert!(rig.win.doc.unsaved());
    assert_eq!(rig.phase(), None);
    assert_eq!(journal_edits_of(&rig), ["crop"]);
}

/// A scripted run (`SOPEN_SCREENSHOT`) keeps nothing of its state's edits — unless a Save's
/// outcome is unknown, whose record the next open reconciles.
#[test]
fn a_scripted_run_keeps_no_journal_unless_an_outcome_is_unknown() {
    let mut rig = rig(Side::Local).unwrap();
    rig.win.scripted = true;
    rig.crop();
    assert_eq!(rig.journals().len(), 1, "kept while it runs");
    let next = window_through(&rig.path, &rig.cache, Box::new(LocalFs::new()));
    drop(std::mem::replace(&mut rig.win, next));
    assert!(rig.journals().is_empty(), "gone with the run");
    rig.win.scripted = true;
    rig.settle();
    rig.crop();
    rig.cut(Cut::RenameReplyLost);
    let fs = Box::new(Cutting {
        inner: Box::new(LocalFs::new()),
        control: rig.control.clone(),
    });
    let mut win = window_through(&rig.path, &rig.cache, fs);
    win.scripted = true;
    drop(std::mem::replace(&mut rig.win, win));
    rig.settle();
    rig.crop();
    rig.save();
    drop(std::mem::replace(
        &mut rig.win,
        window_through(&rig.path, &rig.cache, Box::new(LocalFs::new())),
    ));
    assert_eq!(rig.journals().len(), 1, "an unknown outcome is kept");
}

/// `SOPEN_AUTOSAVE` records its steps as the window does, and keeps no journal when it lands —
/// one when its outcome is unknown.
#[test]
fn autosave_records_its_save_and_keeps_a_journal_only_when_the_outcome_is_unknown() {
    for cut in [Cut::None, Cut::RenameReplyLost] {
        let rig = rig(Side::Local).unwrap();
        let control = Arc::new(Control::default());
        *control.cut.lock().unwrap() = cut;
        let fs = Box::new(Cutting {
            inner: Box::new(LocalFs::new()),
            control,
        });
        let bytes = std::fs::read(&rig.path).unwrap();
        let abs = rig.path.to_str().unwrap().to_string();
        let at = LocalFs::new().stat(&abs).unwrap();
        let origin = Origin::new(fs, None, Some("host".into()), abs, at);
        let file = ImageFile::new(origin, bytes, &rig.cache);
        let mut doc = Doc::decode(file.original()).unwrap();
        doc.crop([0, 0, 50, 40]).unwrap();
        let hooks = Hooks {
            autosave: true,
            ..Hooks::default()
        };
        let done = autosave(file, &doc, &hooks);
        match cut {
            Cut::None => {
                assert!(done.unwrap().saved);
                assert!(rig.journals().is_empty());
            }
            _ => {
                assert!(done.is_err());
                assert_eq!(rig.journals().len(), 1, "kept for the next open");
                assert_eq!(rig.phase().as_deref(), Some("Renaming"));
            }
        }
    }
}

/// `SOPEN_KEYS` in a window of its own: the keys are played, one each time it has settled, and
/// it is ready only once they are — a crop, then Save.
#[test]
fn keys_are_played_into_a_window_of_its_own() {
    let mut rig = rig(Side::Local).unwrap();
    rig.win.sel.set([10, 10, 60, 40]);
    rig.win.script = crate::gui::browser::parse_keys("C,Cmd+S").unwrap().into();
    assert!(!crate::gui::Window::ready(&rig.win));
    rig.drv.until(&mut rig.win, 20, crate::gui::Window::ready);
    let saved = Doc::decode(&std::fs::read(&rig.path).unwrap()).unwrap();
    assert_eq!(saved.size(), (60, 40));
    assert!(!rig.win.doc.unsaved());
}

/// A Save whose outcome was unknown, an edit made after it, then a Save that finds the first
/// landed: nothing is written again, the newer edit stays unsaved, and the journal holds it at
/// once against the file the first Save wrote — its baseline that output, its edits only the
/// newer one.
#[test]
fn an_edit_after_an_unknown_save_is_kept_against_the_file_it_turns_out_to_have_written() {
    let mut rig = rig(Side::Local).unwrap();
    rig.crop();
    let output = rig.win.doc.encode().unwrap().bytes;
    rig.cut(Cut::RenameReplyLost);
    rig.save();
    rig.cut(Cut::None);
    rig.win.sel.set([0, 0, 10, 10]);
    rig.drv.idle(&mut rig.win, 1);
    rig.key(Key::P, Modifiers::NONE);
    rig.save();
    assert!(
        rig.win
            .status
            .text
            .ends_with("landed (found now): nothing was written again"),
        "{}",
        rig.win.status.text
    );
    assert_eq!(std::fs::read(&rig.path).unwrap(), output, "nothing more");
    assert!(rig.win.doc.unsaved(), "the newer edit");
    let dirs = rig.journals();
    assert_eq!(dirs.len(), 1);
    let gen = current(&dirs[0]);
    assert_eq!(words(&gen), ["pixelize"]);
    assert_eq!(gen.head.baseline.sha256, Some(journal::sha256(&output)));
    assert_eq!(rig.phase(), None);
    // Killed and opened again: Restore gives the picture the window had — the newer edit on the
    // file that Save wrote.
    let shown = rig.win.doc.img.clone();
    rig.reopen();
    assert!(rig.drv.find_text("Restore").is_some(), "{:?}", rig.texts());
    rig.drv.click_text(&mut rig.win, "Restore");
    rig.drv.idle(&mut rig.win, 2);
    assert_eq!(rig.win.doc.img, shown, "restored on the saved file");
    assert!(rig.win.doc.unsaved());
}

/// P5: two Saves whose outcomes were unknown, an edit between them: the first did not land (its
/// create got no reply), the second did (its rename's reply was lost). The next Save finds the
/// second landed and marks that Save's picture saved — not the first one's: nothing is unsaved,
/// the journal goes, nothing is written again.
#[test]
fn a_landed_save_found_later_saves_its_own_picture_not_an_earlier_ones() {
    let mut rig = rig(Side::Local).unwrap();
    rig.win.sel.set([0, 0, 10, 10]);
    rig.drv.idle(&mut rig.win, 1);
    rig.key(Key::P, Modifiers::NONE);
    rig.cut(Cut::CreateNoReply);
    rig.save();
    assert!(rig.win.doc.unsaved(), "the first: unknown");
    rig.crop();
    let output = rig.win.doc.encode().unwrap().bytes;
    rig.cut(Cut::RenameReplyLost);
    rig.save();
    assert!(rig.win.doc.unsaved(), "the second: unknown");
    assert_eq!(std::fs::read(&rig.path).unwrap(), output, "it landed");
    let landed = std::fs::metadata(&rig.path).unwrap().modified().unwrap();
    rig.cut(Cut::None);
    rig.save();
    assert!(
        rig.win
            .status
            .text
            .ends_with("landed (found now): nothing was written again"),
        "{}",
        rig.win.status.text
    );
    assert!(
        !rig.win.doc.unsaved(),
        "the picture the second Save wrote is saved"
    );
    rig.drv.idle(&mut rig.win, 3);
    assert!(rig.journals().is_empty(), "nothing unsaved: no journal");
    let now = std::fs::metadata(&rig.path).unwrap().modified().unwrap();
    assert_eq!(now, landed, "not written again");
}

/// A Save waiting in its queue (its generation held) is a Save under way: a close asked for
/// right then is refused without the prompt, and the window closes once that Save has landed.
#[test]
fn a_close_while_a_save_is_queued_waits_for_it() {
    let mut rig = rig(Side::Local).unwrap();
    let gates = Arc::new(GatedDisk::default());
    rig.win.keeper = file::Keeper::at(gates.clone(), &rig.win.file.lock().unwrap());
    rig.crop();
    gates.write_held.store(true, Ordering::SeqCst);
    rig.win.sel.set([0, 0, 10, 10]);
    rig.drv.idle(&mut rig.win, 1);
    rig.key(Key::P, Modifiers::NONE);
    let want = rig.win.doc.encode().unwrap().bytes;
    rig.key(Key::S, command(false));
    assert!(rig.win.queued.is_some() && rig.win.job.is_none(), "queued");
    assert!(
        !crate::gui::Window::close_requested(&mut rig.win),
        "refused"
    );
    rig.drv.idle(&mut rig.win, 2);
    assert!(
        !rig.win.prompt,
        "no prompt: it closes when the save is done"
    );
    assert!(
        rig.texts().iter().any(|t| t == CLOSES_AFTER_SAVE),
        "{:?}",
        rig.texts()
    );
    gates.write_held.store(false, Ordering::SeqCst);
    rig.drv.until(&mut rig.win, 10, |w| w.close_ok);
    assert!(rig.drv.closed(), "closed once it landed");
    assert_eq!(std::fs::read(&rig.path).unwrap(), want, "saved");
}

/// P1 for a Save that landed: a file at its temp's name after the rename (its reply lost) is
/// another program's, maybe. The next open says the Save landed and asks about that file only —
/// Remove it, Keep it, Later; no Restore, no Discard — and the journal and its `renaming` record
/// stay until it is answered. Later asks again on the next open; a file put there after it was
/// shown survives Remove it, the row saying why; Remove it of the file shown removes it and
/// retires the journal. Nothing is written to the file. Through both sides.
#[test]
fn a_landed_saves_journal_stays_until_a_file_at_its_temp_is_answered() {
    for side in sides() {
        let mut rig = rig(side).unwrap();
        rig.crop();
        let output = rig.win.doc.encode().unwrap().bytes;
        rig.cut(Cut::RenameReplyLost);
        rig.save();
        let temp = left_by(&rig).temp.unwrap();
        assert!(!temp.exists(), "{side:?}: renamed");
        std::fs::write(&temp, b"a file of another program").unwrap();
        let landed = std::fs::metadata(&rig.path).unwrap().modified().unwrap();
        rig.reopen();
        let asked = |rig: &Rig| {
            assert!(
                rig.drv.find_text("Remove it").is_some(),
                "{side:?}: {:?}",
                rig.texts()
            );
            assert!(rig.drv.find_text("Keep it").is_some(), "{side:?}");
            assert!(rig.drv.find_text("Later").is_some(), "{side:?}");
            assert!(rig.drv.find_text("Restore").is_none(), "{side:?}");
            assert!(rig.drv.find_text("Discard").is_none(), "{side:?}");
            assert!(
                rig.texts()
                    .iter()
                    .any(|t| t.contains("nothing was written again")),
                "{side:?}: says it landed: {:?}",
                rig.texts()
            );
            assert_eq!(rig.journals().len(), 1, "{side:?}: the journal stays");
            assert_eq!(rig.phase().as_deref(), Some("Renaming"), "{side:?}");
        };
        asked(&rig);
        rig.drv.click_text(&mut rig.win, "Later");
        rig.reopen();
        asked(&rig);
        // Another file takes the name after it was shown: it survives Remove it.
        std::fs::remove_file(&temp).unwrap();
        std::fs::write(&temp, b"yet another program's file").unwrap();
        rig.drv.click_text(&mut rig.win, "Remove it");
        rig.drv.until(&mut rig.win, 10, |w| w.quiet());
        rig.drv.idle(&mut rig.win, 2);
        assert_eq!(
            std::fs::read(&temp).unwrap(),
            b"yet another program's file",
            "{side:?}"
        );
        assert!(
            rig.texts()
                .iter()
                .any(|t| t.contains("changed since it was shown")),
            "{side:?}: {:?}",
            rig.texts()
        );
        assert_eq!(rig.phase().as_deref(), Some("Renaming"), "{side:?}: kept");
        // Shown again, then removed as asked: the journal is retired.
        rig.reopen();
        asked(&rig);
        rig.drv.click_text(&mut rig.win, "Remove it");
        rig.drv.until(&mut rig.win, 10, |w| w.quiet());
        rig.drv.idle(&mut rig.win, 2);
        assert!(!temp.exists(), "{side:?}: removed");
        assert!(rig.journals().is_empty(), "{side:?}: retired");
        assert!(rig.win.rows.is_empty(), "{side:?}");
        assert_eq!(std::fs::read(&rig.path).unwrap(), output, "{side:?}");
        let now = std::fs::metadata(&rig.path).unwrap().modified().unwrap();
        assert_eq!(now, landed, "{side:?}: nothing written to the file");
    }
}

/// A file at a landed Save's temp name that cannot be read to be checked: it is kept, the row
/// says why, and the journal and its record stay for the next open — not retired as if nothing
/// were there. Through both sides.
#[test]
fn a_temp_that_cannot_be_checked_keeps_the_record() {
    use std::os::unix::fs::PermissionsExt;
    for side in sides() {
        let mut rig = rig(side).unwrap();
        rig.crop();
        rig.cut(Cut::RenameReplyLost);
        rig.save();
        let temp = left_by(&rig).temp.unwrap();
        std::fs::write(&temp, b"unreadable").unwrap();
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o000)).unwrap();
        rig.reopen();
        let said = rig.texts();
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            said.iter().any(|t| t.contains("it could not be checked")),
            "{side:?}: {said:?}"
        );
        assert!(temp.exists(), "{side:?}: kept");
        assert_eq!(rig.journals().len(), 1, "{side:?}: the journal stays");
        assert_eq!(
            rig.phase().as_deref(),
            Some("Renaming"),
            "{side:?}: so does its record"
        );
    }
}

/// The cut-off Save of [`Cut::CreatedNoReply`] in the window, and the next Save asked for in
/// the same session: it reconciles first — reading only — and, the empty file at the temp path
/// not provably its own, asks in the window, bound to that file. Nothing is written, the record
/// stays, and the Save waits in its queue (Cmd+S again changes nothing).
fn to_the_question(side: Side) -> (Rig, PathBuf, Vec<u8>, Vec<u8>) {
    let mut rig = rig(side).unwrap();
    let original = std::fs::read(&rig.path).unwrap();
    rig.crop();
    let output = rig.win.doc.encode().unwrap().bytes;
    rig.cut(Cut::CreatedNoReply);
    rig.save();
    let temp = left_by(&rig).temp.unwrap();
    rig.cut(Cut::None);
    rig.control.taken();
    rig.key(Key::S, command(false));
    rig.drv.until(&mut rig.win, 10, |w| w.asking.is_some());
    rig.drv.idle(&mut rig.win, 2);
    let line = rig.win.asking.as_ref().unwrap().line.clone();
    assert!(
        line.ends_with("— remove it? The save waits for the answer"),
        "{side:?}: {line}"
    );
    assert!(
        rig.texts().contains(&line),
        "{side:?}: shown whole: {:?}",
        rig.texts()
    );
    assert!(rig.drv.find_text("Remove it").is_some(), "{side:?}");
    assert!(rig.drv.find_text("Keep it").is_some(), "{side:?}");
    assert!(
        rig.win.queued.is_some() && rig.win.job.is_none(),
        "{side:?}: waits"
    );
    assert_eq!(
        rig.control.writes(),
        Vec::<String>::new(),
        "{side:?}: nothing sent"
    );
    assert_eq!(
        rig.phase().as_deref(),
        Some("Naming"),
        "{side:?}: the record stays"
    );
    assert_eq!(
        std::fs::read(&temp).unwrap(),
        b"",
        "{side:?}: the temp untouched"
    );
    assert_eq!(std::fs::read(&rig.path).unwrap(), original, "{side:?}");
    rig.key(Key::S, command(false));
    rig.drv.idle(&mut rig.win, 3);
    assert!(
        rig.win.job.is_none(),
        "{side:?}: Save again does not go past the question"
    );
    assert_eq!(rig.control.writes(), Vec::<String>::new(), "{side:?}");
    (rig, temp, original, output)
}

/// 07 §8: a Save meeting a temp of uncertain ownership in the same session asks in the window
/// and goes on after the answer — Keep it: the file stays as it was, the record ends, the Save
/// lands; Remove it: only that file is removed (checked again on a worker), then the Save lands.
/// Through both sides.
#[test]
fn a_save_asks_about_an_uncertain_temp_and_goes_on_after_the_answer() {
    for side in sides() {
        for keep in [true, false] {
            let case = format!("{side:?} keep={keep}");
            let (mut rig, temp, _, output) = to_the_question(side);
            let temp_s = temp.to_str().unwrap().to_string();
            rig.drv
                .click_text(&mut rig.win, if keep { "Keep it" } else { "Remove it" });
            rig.drv.until(&mut rig.win, 10, |w| w.quiet());
            rig.drv.idle(&mut rig.win, 2);
            assert!(
                rig.win.status.text.starts_with("saved "),
                "{case}: {}",
                rig.win.status.text
            );
            assert_eq!(std::fs::read(&rig.path).unwrap(), output, "{case}: landed");
            assert!(!rig.win.doc.unsaved(), "{case}");
            assert!(rig.journals().is_empty(), "{case}: retired");
            if keep {
                assert_eq!(std::fs::read(&temp).unwrap(), b"", "{case}: kept as it was");
                assert!(rig
                    .control
                    .writes()
                    .iter()
                    .all(|w| w != &format!("remove {temp_s}")));
            } else {
                assert!(!temp.exists(), "{case}: removed");
                assert_eq!(
                    rig.control.writes().first(),
                    Some(&format!("remove {temp_s}")),
                    "{case}: removed before anything was written"
                );
            }
        }
    }
}

/// Remove it for that question removes only the file shown: one put at the name after it was
/// shown survives; the Save is refused, saying why; the record stays; the next Save asks about
/// the file there now, and Keep it lets it land. Through both sides.
#[test]
fn a_saves_question_keeps_a_file_that_changed_and_the_save_is_refused() {
    for side in sides() {
        let (mut rig, temp, original, output) = to_the_question(side);
        std::fs::remove_file(&temp).unwrap();
        std::fs::write(&temp, b"another program's file").unwrap();
        rig.drv.click_text(&mut rig.win, "Remove it");
        rig.drv.until(&mut rig.win, 10, |w| {
            w.answering.is_none() && w.queued.is_none()
        });
        rig.drv.idle(&mut rig.win, 2);
        assert_eq!(
            std::fs::read(&temp).unwrap(),
            b"another program's file",
            "{side:?}"
        );
        assert!(rig.win.status.error, "{side:?}: {}", rig.win.status.text);
        assert!(
            rig.win.status.text.contains("changed since it was shown"),
            "{side:?}: {}",
            rig.win.status.text
        );
        assert!(
            rig.win.job.is_none() && rig.win.asking.is_none(),
            "{side:?}: refused"
        );
        assert_eq!(
            rig.phase().as_deref(),
            Some("Naming"),
            "{side:?}: the record stays"
        );
        assert_eq!(
            std::fs::read(&rig.path).unwrap(),
            original,
            "{side:?}: not written"
        );
        assert!(rig.win.doc.unsaved(), "{side:?}");
        // The next Save asks about the file there now.
        rig.key(Key::S, command(false));
        rig.drv.until(&mut rig.win, 10, |w| w.asking.is_some());
        rig.drv.idle(&mut rig.win, 2);
        rig.drv.click_text(&mut rig.win, "Keep it");
        rig.drv.until(&mut rig.win, 10, |w| w.quiet());
        assert_eq!(
            std::fs::read(&rig.path).unwrap(),
            output,
            "{side:?}: landed"
        );
        assert_eq!(
            std::fs::read(&temp).unwrap(),
            b"another program's file",
            "{side:?}"
        );
        assert!(rig.journals().is_empty(), "{side:?}");
    }
}

/// A cut-off Save's own temp — provably, by its record — whose removal fails when the next
/// Save reconciles it: that Save asks about it in the window instead of being refused, and
/// goes on once it is removed. Through both sides.
#[test]
fn a_save_asks_when_removing_its_own_earlier_temp_fails() {
    for side in sides() {
        let mut rig = rig(side).unwrap();
        let original = std::fs::read(&rig.path).unwrap();
        rig.crop();
        let output = rig.win.doc.encode().unwrap().bytes;
        rig.cut(Cut::DieAfterTempStat);
        rig.save();
        let temp = left_by(&rig).temp.unwrap();
        assert_eq!(rig.phase().as_deref(), Some("Created"), "{side:?}");
        // The connection is back; its removes are refused.
        rig.control.dead.store(false, Ordering::SeqCst);
        rig.cut(Cut::FailRemove);
        rig.key(Key::S, command(false));
        rig.drv.until(&mut rig.win, 10, |w| w.asking.is_some());
        let line = rig.win.asking.as_ref().unwrap().line.clone();
        assert!(line.contains("its removal failed"), "{side:?}: {line}");
        assert!(
            rig.win.queued.is_some() && rig.win.job.is_none(),
            "{side:?}: waits"
        );
        assert_eq!(
            std::fs::read(&temp).unwrap(),
            output,
            "{side:?}: the temp as staged"
        );
        assert_eq!(std::fs::read(&rig.path).unwrap(), original, "{side:?}");
        assert_eq!(
            rig.phase().as_deref(),
            Some("Created"),
            "{side:?}: the record stays"
        );
        rig.cut(Cut::None);
        rig.drv.idle(&mut rig.win, 2);
        rig.drv.click_text(&mut rig.win, "Remove it");
        rig.drv.until(&mut rig.win, 10, |w| w.quiet());
        assert!(!temp.exists(), "{side:?}: removed");
        assert_eq!(
            std::fs::read(&rig.path).unwrap(),
            output,
            "{side:?}: landed"
        );
        assert!(rig.journals().is_empty(), "{side:?}");
    }
}

/// Keep it is kept: the cut-off Save's own temp, whose removal failed when the next Save
/// reconciled it, is asked about; the owner says Keep it while a removal would now succeed. The
/// resumed Save removes nothing there — no further remove request of it — and lands. Through
/// both sides.
#[test]
fn keep_it_keeps_the_saves_own_temp_even_when_removing_it_would_work_now() {
    for side in sides() {
        let mut rig = rig(side).unwrap();
        rig.crop();
        let output = rig.win.doc.encode().unwrap().bytes;
        rig.cut(Cut::DieAfterTempStat);
        rig.save();
        let temp = left_by(&rig).temp.unwrap();
        let temp_s = temp.to_str().unwrap().to_string();
        rig.control.dead.store(false, Ordering::SeqCst);
        rig.cut(Cut::FailRemove);
        rig.key(Key::S, command(false));
        rig.drv.until(&mut rig.win, 10, |w| w.asking.is_some());
        rig.drv.idle(&mut rig.win, 2);
        // A removal would work now.
        rig.cut(Cut::None);
        rig.control.taken();
        rig.drv.click_text(&mut rig.win, "Keep it");
        rig.drv.until(&mut rig.win, 10, |w| w.quiet());
        rig.drv.idle(&mut rig.win, 2);
        assert_eq!(
            std::fs::read(&temp).unwrap(),
            output,
            "{side:?}: kept as it was"
        );
        let sent = rig.control.taken();
        assert!(
            sent.iter()
                .all(|r| !r.starts_with("remove") || !r.ends_with(&temp_s)),
            "{side:?}: a remove of the kept temp was sent: {sent:?}"
        );
        assert_eq!(
            std::fs::read(&rig.path).unwrap(),
            output,
            "{side:?}: landed"
        );
        assert!(rig.journals().is_empty(), "{side:?}");
    }
}

/// Remove it for the Save's question, its removal refused by the server: the reason is shown,
/// the record and the temp stay, the file is untouched, and no Save starts. Through both sides.
#[test]
fn remove_it_refused_at_answer_time_starts_no_save() {
    for side in sides() {
        let (mut rig, temp, original, _) = to_the_question(side);
        rig.cut(Cut::FailRemove);
        rig.drv.click_text(&mut rig.win, "Remove it");
        rig.drv.until(&mut rig.win, 10, |w| {
            w.answering.is_none() && w.asking.is_none()
        });
        rig.drv.idle(&mut rig.win, 3);
        assert!(rig.win.status.error, "{side:?}: {}", rig.win.status.text);
        assert!(
            rig.win.status.text.contains("could not be removed"),
            "{side:?}: {}",
            rig.win.status.text
        );
        assert!(
            rig.win.job.is_none() && rig.win.queued.is_none(),
            "{side:?}: no Save"
        );
        assert_eq!(
            rig.phase().as_deref(),
            Some("Naming"),
            "{side:?}: the record stays"
        );
        assert_eq!(
            std::fs::read(&temp).unwrap(),
            b"",
            "{side:?}: the temp stays"
        );
        assert_eq!(
            std::fs::read(&rig.path).unwrap(),
            original,
            "{side:?}: untouched"
        );
        assert!(
            rig.control
                .writes()
                .iter()
                .all(|w| !w.contains(rig.path.to_str().unwrap())),
            "{side:?}: nothing sent for the file"
        );
    }
}

/// Esc drops a Save still in its queue — here waiting on its question: nothing is sent, the
/// question goes with it and its record stays (the next Save asks again); with a close waiting
/// on that Save, the close prompt comes up.
#[test]
fn esc_drops_a_save_waiting_on_its_question() {
    let (mut rig, temp, original, _) = to_the_question(Side::Local);
    rig.key(Key::Escape, Modifiers::NONE);
    rig.drv.idle(&mut rig.win, 2);
    assert!(
        rig.win.queued.is_none() && rig.win.asking.is_none(),
        "dropped"
    );
    assert_eq!(rig.win.status.text, STOPPED);
    assert!(!rig.win.prompt, "no close was waiting");
    assert_eq!(rig.control.writes(), Vec::<String>::new(), "nothing sent");
    assert_eq!(rig.phase().as_deref(), Some("Naming"), "the record stays");
    assert_eq!(std::fs::read(&temp).unwrap(), b"");
    assert!(rig.win.doc.unsaved());
    // Asked again by the next Save; a close now waits on it, and Esc brings up its prompt.
    rig.key(Key::S, command(false));
    rig.drv.until(&mut rig.win, 10, |w| w.asking.is_some());
    assert!(
        !crate::gui::Window::close_requested(&mut rig.win),
        "waits for the save"
    );
    rig.key(Key::Escape, Modifiers::NONE);
    rig.drv.idle(&mut rig.win, 2);
    assert!(rig.win.queued.is_none(), "dropped");
    assert!(rig.win.prompt, "the close prompt, the edits unsaved");
    assert_eq!(rig.control.writes(), Vec::<String>::new());
    assert_eq!(std::fs::read(&rig.path).unwrap(), original);
}

/// Q1: a generation due for a picture that has nothing to keep any more is not waited for. A
/// pixelize schedules one; in the same pass, before it is due, Undo takes the picture back to
/// the file's pixels (no operation left to journal) and Save copy is asked for: the Save copy
/// runs and writes its sibling.
#[test]
fn a_save_never_waits_on_a_generation_due_for_an_earlier_picture() {
    let mut rig = rig(Side::Local).unwrap();
    rig.win.sel.set([0, 0, 10, 10]);
    rig.drv.idle(&mut rig.win, 1);
    rig.key(Key::P, Modifiers::NONE);
    assert!(rig.win.doc.unsaved());
    assert!(rig.win.keeper.due().is_some(), "a generation is due");
    let key = |key, modifiers| Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    };
    rig.drv.frame(
        &mut rig.win,
        vec![key(Key::Z, command(false)), key(Key::S, command(true))],
        command(true),
    );
    assert!(!rig.win.doc.can_undo(), "undone");
    assert_eq!(
        rig.win.doc.journal_edits().map(<[Edit]>::len),
        Some(0),
        "nothing to journal"
    );
    assert!(
        rig.win.queued.is_some() || rig.win.job.is_some(),
        "Save copy asked for"
    );
    rig.drv.until(&mut rig.win, 10, |w| w.quiet());
    let copy = rig.dir.join("shot.edited.png");
    assert!(copy.exists(), "{:?}: {}", rig.texts(), rig.win.status.text);
}

/// Q2: a removal asked for while a generation is being written (nothing unsaved at that moment)
/// does not take a newer generation with it. A held, the removal asked for, a newer edit flushed
/// (B); A released: the journal is on disk holding B.
#[test]
fn a_removal_waiting_on_a_write_does_not_drop_a_newer_generation() {
    let mut rig = rig(Side::Local).unwrap();
    let gates = Arc::new(GatedDisk::default());
    rig.win.keeper = file::Keeper::at(gates.clone(), &rig.win.file.lock().unwrap());
    rig.crop();
    gates.write_held.store(true, Ordering::SeqCst);
    gates.write_reached.store(false, Ordering::SeqCst);
    rig.win.sel.set([0, 0, 10, 10]);
    rig.drv.idle(&mut rig.win, 1);
    rig.key(Key::P, Modifiers::NONE);
    let g = gates.clone();
    rig.drv
        .until(&mut rig.win, 5, |_| g.write_reached.load(Ordering::SeqCst));
    // What the clean-state pass asks for when it finds the journal free between A's last write
    // and its result.
    rig.win.keeper.remove().unwrap();
    rig.win.sel.set([20, 20, 10, 10]);
    rig.drv.idle(&mut rig.win, 1);
    rig.key(Key::P, Modifiers::NONE);
    rig.drv.frame(
        &mut rig.win,
        vec![Event::WindowFocused(false)],
        Modifiers::NONE,
    );
    gates.write_held.store(false, Ordering::SeqCst);
    rig.drv.until(&mut rig.win, 10, |w| w.quiet());
    let dirs = rig.journals();
    assert_eq!(dirs.len(), 1, "the journal stays");
    assert_eq!(
        words(&current(&dirs[0])),
        ["crop", "pixelize", "pixelize"],
        "B"
    );
}

/// P4: a Save of a picture Undo took back past an earlier Save is the file's fresh start: the
/// edits after it are journaled — edit, Save, Undo, Save, edit, kept, killed, Restore gives the
/// picture back.
#[test]
fn edits_after_saving_an_undone_picture_are_kept() {
    let mut rig = rig(Side::Local).unwrap();
    rig.crop();
    rig.save();
    assert!(!rig.win.doc.unsaved());
    rig.key(Key::Z, command(false));
    assert!(rig.win.doc.unsaved(), "undone past the save");
    rig.save();
    assert!(!rig.win.doc.unsaved(), "saved again");
    assert_eq!(rig.win.doc.size(), (120, 80), "the file as at first");
    rig.win.sel.set([0, 0, 20, 20]);
    rig.drv.idle(&mut rig.win, 1);
    rig.key(Key::P, Modifiers::NONE);
    rig.drv.frame(
        &mut rig.win,
        vec![Event::WindowFocused(false)],
        Modifiers::NONE,
    );
    rig.drv.until(&mut rig.win, 10, |w| w.quiet());
    rig.drv.idle(&mut rig.win, 1);
    assert!(
        rig.texts()
            .iter()
            .any(|t| t.starts_with("unsaved — kept at ")),
        "{:?}",
        rig.texts()
    );
    let dirs = rig.journals();
    assert_eq!(dirs.len(), 1, "kept");
    assert_eq!(words(&current(&dirs[0])), ["pixelize"]);
    let shown = rig.win.doc.img.clone();
    rig.reopen();
    rig.drv.click_text(&mut rig.win, "Restore");
    rig.drv.idle(&mut rig.win, 2);
    assert_eq!(rig.win.doc.img, shown);
}

/// Item 6 (07 §3 Recovery): the lock retry waits only for this file's own journals — a folder
/// named for a journal of it, whose generation (when it can be read) is of this host and path.
/// Not another file's whose name starts the same, nor one written for another path that maps to
/// the same place, nor one being removed.
#[test]
fn the_lock_retry_counts_only_this_files_journals() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let abs = "/data/shot.png";
    let base = crate::cache::edits_path(&cache, "box", abs);
    let home = base.parent().unwrap();
    std::fs::create_dir_all(home).unwrap();
    let ours = home.join("shot.png.journal-20261122T010203Z-0123abcd");
    std::fs::create_dir(&ours).unwrap();
    // Another file's: `shot.png.journal-notes.txt` has journals too.
    std::fs::create_dir(home.join("shot.png.journal-notes.txt.journal-20261122T010203Z-0123abcd"))
        .unwrap();
    std::fs::create_dir(home.join("shot.png.journal-20261122T010203Z-0123abce.gone")).unwrap();
    // A whole journal written for another path, under a name of this one's.
    let mut other = journal::Journal::create(
        journal::disk(),
        &cache,
        "box",
        "/data/other.png",
        journal::Kind::Image,
    )
    .unwrap();
    other
        .publish(&journal::Snapshot {
            baseline: crate::remote::Fingerprint {
                size: 1,
                mtime: std::time::UNIX_EPOCH,
                sha256: Some([1; 32]),
            },
            seen: journal::Seen::whole([1; 32]),
            edits: "crop 0,0,1,1".to_string(),
            pastes: Vec::new(),
        })
        .unwrap();
    let theirs = other.dir().to_path_buf();
    drop(other);
    std::fs::rename(
        &theirs,
        home.join("shot.png.journal-20261122T010203Z-89abcdef"),
    )
    .unwrap();
    assert_eq!(
        crate::journal::this_files_journals(&cache, "box", abs),
        vec![ours]
    );
}

/// The local disk for the journal, with gates and a trace: `list` (the look for earlier
/// journals) waits while `list_held`, `write_new` (a generation's files) while `write_held`;
/// every listing and every directory made is traced, in order.
#[derive(Default)]
struct GatedDisk {
    list_held: AtomicBool,
    write_held: AtomicBool,
    /// A generation's write got to its gate.
    write_reached: AtomicBool,
    trace: std::sync::Mutex<Vec<String>>,
}

impl GatedDisk {
    fn wait(gate: &AtomicBool) {
        while gate.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn trace(&self) -> Vec<String> {
        self.trace.lock().unwrap().clone()
    }
}

impl journal::Fs for GatedDisk {
    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        self.trace.lock().unwrap().push(format!("mkdir {name}"));
        journal::Disk.create_dir(path)
    }
    fn write_new(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        self.write_reached.store(true, Ordering::SeqCst);
        Self::wait(&self.write_held);
        journal::Disk.write_new(path, bytes)
    }
    fn sync_file(&self, path: &Path) -> std::io::Result<()> {
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
        Self::wait(&self.list_held);
        let name = dir.file_name().unwrap_or_default().to_string_lossy();
        self.trace.lock().unwrap().push(format!("list {name}"));
        journal::Disk.list(dir)
    }
    fn node(&self, path: &Path, follow: bool) -> std::io::Result<Option<journal::Node>> {
        journal::Disk.node(path, follow)
    }
    fn lock(&self, path: &Path, create: bool) -> std::io::Result<Option<journal::Held>> {
        journal::Disk.lock(path, create)
    }
}

/// The journals' folder of `path`, made (as an earlier session leaves it): only a journal's own
/// directory is made then, and the look for earlier journals lists it.
fn journal_home(cache: &Path, host: &str, path: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    let base = crate::cache::edits_path(cache, host, path.to_str().unwrap());
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(base.parent().unwrap())
        .unwrap();
}

/// P3, T3: nothing the journal or the look for earlier journals waits on holds the window. The
/// look is held, then a generation's write: every pass returns at once meanwhile — through an
/// edit, a focus loss and Cmd+S — and the Save waits in its queue, sending nothing. Once both are
/// in, the Save starts; at its first write-side request (the temp's create, held there) the
/// generation holding the edit is on disk, readable, and the record says `naming`.
#[test]
fn nothing_the_journal_waits_on_holds_the_window_and_a_save_waits_for_the_journal() {
    let mut rig = rig(Side::Local).unwrap();
    // A fresh window on the same file, its journal on a gated disk, before its first pass.
    journal_home(&rig.cache, "host", &rig.path);
    let gates = Arc::new(GatedDisk::default());
    gates.list_held.store(true, Ordering::SeqCst);
    gates.write_held.store(true, Ordering::SeqCst);
    let fs = Box::new(Cutting {
        inner: Box::new(LocalFs::new()),
        control: rig.control.clone(),
    });
    let mut win = window_through(&rig.path, &rig.cache, fs);
    win.keeper = file::Keeper::at(gates.clone(), &win.file.lock().unwrap());
    drop(std::mem::replace(&mut rig.win, win));
    rig.drv = Driver::new(1000.0, 700.0);
    let original = std::fs::read(&rig.path).unwrap();
    let (g, c) = (gates.clone(), rig.control.clone());
    let dog = Watchdog::new(move || {
        g.list_held.store(false, Ordering::SeqCst);
        g.write_held.store(false, Ordering::SeqCst);
        c.release.store(true, Ordering::SeqCst);
    });
    let mut slowest = Duration::ZERO;
    let mut pass = |rig: &mut Rig, events: Vec<Event>, mods: Modifiers| {
        let started = std::time::Instant::now();
        rig.drv.frame(&mut rig.win, events, mods);
        slowest = slowest.max(started.elapsed());
    };
    for _ in 0..3 {
        pass(&mut rig, Vec::new(), Modifiers::NONE);
    }
    assert!(rig.win.recovering.is_some(), "the look is held");
    rig.win.sel.set([0, 0, 90, 60]);
    let key = |key, pressed, modifiers| Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    };
    pass(
        &mut rig,
        vec![key(Key::C, true, Modifiers::NONE)],
        Modifiers::NONE,
    );
    pass(&mut rig, vec![Event::WindowFocused(false)], Modifiers::NONE);
    let cmd = command(false);
    pass(&mut rig, vec![key(Key::S, true, cmd)], cmd);
    for _ in 0..10 {
        pass(&mut rig, Vec::new(), Modifiers::NONE);
    }
    assert!(rig.win.queued.is_some() && rig.win.job.is_none(), "queued");
    assert!(rig.control.writes().is_empty(), "nothing sent");
    // The look comes back; the generation is asked for, and held.
    gates.list_held.store(false, Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while rig.win.recovering.is_some() {
        assert!(std::time::Instant::now() < deadline);
        pass(&mut rig, Vec::new(), Modifiers::NONE);
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..10 {
        pass(&mut rig, Vec::new(), Modifiers::NONE);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!rig.win.keeper.settled(), "the generation is held");
    assert!(
        rig.win.queued.is_some() && rig.win.job.is_none(),
        "still queued"
    );
    assert!(rig.control.writes().is_empty(), "nothing sent");
    assert!(!dog.fired(), "a pass waited on a held gate");
    // The watchdog catches a pass that waits until a gate opens; this catches one that waits on
    // a held gate with a timeout of its own (each pass would take at least that long).
    assert!(
        slowest < Duration::from_millis(300),
        "a pass waited: {slowest:?}"
    );
    // The generation goes in: the Save starts, and is held at its first write-side request.
    rig.cut(Cut::HoldCreate);
    gates.write_held.store(false, Ordering::SeqCst);
    let control = rig.control.clone();
    rig.drv
        .until(&mut rig.win, 10, |_| control.holding.load(Ordering::SeqCst));
    let left = left_by(&rig);
    assert_eq!(left.phase, "naming");
    assert_eq!(left.saved, ["crop"], "durable before the first write");
    assert_eq!(words(&current(&rig.journals()[0])), ["crop"]);
    assert_eq!(std::fs::read(&rig.path).unwrap(), original);
    rig.control.release.store(true, Ordering::SeqCst);
    rig.settle();
    assert!(!rig.win.doc.unsaved(), "saved");
    assert!(rig.journals().is_empty());
}

/// A Save waits in its queue until the journal holds its edits, with no job yet: a wait for an
/// empty `job` returns before it ran, and the file read then is still the original (the macOS
/// preview failure on b3cf430). With the generation held: queued, no job, the file the original
/// — and the Save has not ended; released, it ends with the file saved.
#[test]
fn a_save_waiting_in_its_queue_has_not_ended() {
    let mut rig = rig(Side::Local).unwrap();
    let gates = Arc::new(GatedDisk::default());
    rig.win.keeper = file::Keeper::at(gates.clone(), &rig.win.file.lock().unwrap());
    rig.crop();
    let original = std::fs::read(&rig.path).unwrap();
    gates.write_held.store(true, Ordering::SeqCst);
    rig.win.sel.set([0, 0, 10, 10]);
    rig.drv.idle(&mut rig.win, 1);
    rig.key(Key::P, Modifiers::NONE);
    let want = rig.win.doc.encode().unwrap().bytes;
    let since = rig.win.saves_ended;
    rig.key(Key::S, command(false));
    for pass in 0..5 {
        rig.drv.idle(&mut rig.win, 1);
        assert!(
            rig.win.job.is_none() && rig.win.queued.is_some(),
            "queued ({pass})"
        );
        assert!(
            !rig.win.save_ended(since),
            "not ended while queued ({pass})"
        );
        assert_eq!(
            std::fs::read(&rig.path).unwrap(),
            original,
            "not written ({pass})"
        );
    }
    gates.write_held.store(false, Ordering::SeqCst);
    rig.drv.until(&mut rig.win, 10, |w| w.save_ended(since));
    assert_eq!(
        std::fs::read(&rig.path).unwrap(),
        want,
        "saved once it ended"
    );
}

/// 07 §7: Alt+Cmd/Ctrl+S is Save As, not Save — egui's own check would take it for Cmd/Ctrl+S.
/// No request that writes is sent while the modal is up; Esc closes it, nothing written.
#[test]
fn alt_command_s_opens_save_as_and_sends_nothing() {
    let mut rig = rig(Side::Local).unwrap();
    rig.crop();
    let original = std::fs::read(&rig.path).unwrap();
    rig.control.taken();
    rig.key(Key::S, super::edit_tests::alt_command());
    rig.drv.until(&mut rig.win, 10, |w| w.saving_as.is_some());
    rig.drv.idle(&mut rig.win, 5);
    assert!(
        rig.texts()
            .iter()
            .any(|t| t == "Save shot.png with your edits to a new local file"),
        "{:?}",
        rig.texts()
    );
    assert_eq!(
        rig.control.writes(),
        Vec::<String>::new(),
        "nothing written"
    );
    assert_eq!(std::fs::read(&rig.path).unwrap(), original);
    assert!(rig.win.doc.unsaved());
    rig.key(Key::Escape, Modifiers::NONE);
    rig.drv.idle(&mut rig.win, 2);
    assert!(rig.win.saving_as.is_none());
    assert_eq!(rig.control.writes(), Vec::<String>::new());
    assert!(rig.win.doc.unsaved());
}

/// The first pass of a window with an edit and a close asked for at once (`close=ask`), while
/// the look for earlier journals is kept going (another live session's journal of the file is
/// tried again for a while): no journal directory is made until the look is over — the trace
/// has every listing before the window's journal is made — so the look never takes it for an
/// earlier one's. No row; the live one and this window's.
#[test]
fn the_first_journal_waits_for_the_look_for_earlier_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("shot.png");
    std::fs::write(&path, bytes_of(ImageFormat::Png)).unwrap();
    let cache = tmp.path().join("cache");
    journal_home(&cache, crate::cache::LOCAL_HOST, &path);
    let live = journal::Journal::create(
        journal::disk(),
        &cache,
        crate::cache::LOCAL_HOST,
        path.to_str().unwrap(),
        journal::Kind::Image,
    )
    .unwrap();
    let gates = Arc::new(GatedDisk::default());
    let mut win = super::edit_tests::window_on(&path, &cache);
    win.keeper = file::Keeper::at(gates.clone(), &win.file.lock().unwrap());
    win.doc.crop([0, 0, 50, 40]).unwrap();
    win.ask_close = true;
    let mut drv = Driver::new(1000.0, 700.0);
    drv.until(&mut win, 10, |w| w.listed && w.quiet());
    drv.idle(&mut win, 2);
    assert!(win.rows.is_empty(), "no row");
    // The look lists the journals' folder (the file's folder in the cache); a journal lists
    // only its own directory.
    let home = path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let look = format!("list {home}");
    let trace = gates.trace();
    let made = trace
        .iter()
        .position(|e| e.starts_with("mkdir shot.png.journal-"))
        .unwrap_or_else(|| panic!("the window's journal was made: {trace:?}"));
    assert!(trace[..made].contains(&look), "{trace:?}");
    assert!(
        !trace[made..].contains(&look),
        "the look listed after the journal was made: {trace:?}"
    );
    let dirs = super::edit_tests::journal_dirs(&cache, &path);
    assert_eq!(dirs.len(), 2, "the live one and this window's: {dirs:?}");
    drop(live);
}

/// A file of `size` bytes, sparse but for its first and last MiB (distinct bytes): a Save's
/// output above 256 MiB, as reconciliation reads it.
fn big_output(path: &Path, size: u64) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = std::fs::File::create(path).unwrap();
    f.set_len(size).unwrap();
    f.write_all(&[0xA5; 1 << 20]).unwrap();
    f.seek(SeekFrom::Start(size - (1 << 20))).unwrap();
    f.write_all(&[0x5A; 1 << 20]).unwrap();
}

/// A journal left by a Save of `target` cut off right after its rename (phase `renaming`), whose
/// output — what `target` holds now — is above 256 MiB: its post-save evidence the staged stat,
/// the first and last MiB, and (for the next baseline) the output's whole hash.
fn cut_off_big_save(cache: &Path, target: &Path) {
    let abs = target.to_str().unwrap();
    let mut j = journal::Journal::create(journal::disk(), cache, "host", abs, journal::Kind::Image)
        .unwrap();
    let before = crate::remote::Fingerprint {
        size: 10,
        mtime: std::time::UNIX_EPOCH,
        sha256: Some([7; 32]),
    };
    let snap = journal::Snapshot {
        baseline: before,
        seen: journal::Seen::whole([7; 32]),
        edits: "crop 0,0,1,1".to_string(),
        pastes: Vec::new(),
    };
    j.publish(&snap).unwrap();
    let temp = target.with_file_name(".big.png.sopen-0123456789abcdef");
    let plan = journal::Plan::Temp {
        route: journal::Route::Upload,
        target: abs.to_string(),
        temp: temp.to_str().unwrap().to_string(),
    };
    j.begin_save(&snap, plan).unwrap();
    let staged = journal::FileStat::of(&LocalFs::new().lstat_opt(abs).unwrap().unwrap());
    j.save_created(staged).unwrap();
    let mut first = vec![0u8; 1 << 20];
    let mut last = vec![0u8; 1 << 20];
    {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(target).unwrap();
        f.read_exact(&mut first).unwrap();
        f.seek(SeekFrom::End(-(1 << 20))).unwrap();
        f.read_exact(&mut last).unwrap();
    }
    let ends = journal::Ends {
        first: journal::sha256(&first),
        last: journal::sha256(&last),
    };
    let post = journal::PostEvidence::image(&staged, [9; 32], Some(ends)).unwrap();
    j.save_staged(staged, post).unwrap();
    j.save_renaming().unwrap();
    // Killed here: the reply never recorded.
}

/// What the next open's look finds for `target`.
fn reopen_look(cache: &Path, target: &Path) -> (file::Recovered, Vec<String>) {
    let abs = target.to_str().unwrap().to_string();
    let at = LocalFs::new().stat(&abs).unwrap();
    let control = Arc::new(Control::default());
    let fs = Box::new(Cutting {
        inner: Box::new(LocalFs::new()),
        control: control.clone(),
    });
    let origin = Origin::new(fs, None, Some("host".into()), abs, at);
    let file = ImageFile::new(origin, b"the bytes as read".to_vec(), cache);
    let basis = file.basis();
    let found = file::recover(&Mutex::new(file), &basis, &journal::disk());
    let log = control.taken();
    (found, log)
}

/// Overwrites `at` of `target` with `bytes`, its modification time put back.
fn overwrite_keeping_mtime(target: &Path, at: u64, bytes: &[u8]) {
    use std::io::{Seek, SeekFrom, Write};
    let mtime = std::fs::metadata(target).unwrap().modified().unwrap();
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(target)
        .unwrap();
    f.seek(SeekFrom::Start(at)).unwrap();
    f.write_all(bytes).unwrap();
    f.set_modified(mtime).unwrap();
}

/// E18, an image whose encoded output is above 256 MiB, landed with its reply lost. The journal
/// is built as the Save's steps build it (`PostEvidence::image` with the staged stat, the first
/// and last MiB and the whole hash), not by a real encode of that size. The next open:
/// - the output as staged: landed, found by the staged stat and the first and last MiB — the
///   reads of the file are those two MiB and nothing between — the journal retired, nothing
///   written or renamed;
/// - its middle changed by another writer, stat and ends the same: landed too (07 §3.1 says
///   reconciliation reads nothing between, the same best effort as §4.5's);
/// - its first or its last MiB changed: neither the output nor the file it started from —
///   unknown, the row says a save was cut off, and the journal is kept.
#[test]
fn a_landed_save_above_256_mib_is_found_by_its_first_and_last_mib() {
    const SIZE: u64 = crate::remote::HASH_CHECK_MAX + (16 << 20);
    const MIB: u64 = 1 << 20;
    for (case, change) in [
        ("as staged", None),
        ("middle changed", Some(SIZE / 2)),
        ("first MiB changed", Some(10)),
        ("last MiB changed", Some(SIZE - 4)),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let target = tmp.path().join("big.png");
        big_output(&target, SIZE);
        cut_off_big_save(&cache, &target);
        if let Some(at) = change {
            overwrite_keeping_mtime(&target, at, b"edge");
        }
        let (found, log) = reopen_look(&cache, &target);
        let shown = target.to_str().unwrap();
        // What the look read of the file, and that it changed nothing: no whole read (read-all,
        // read-through, read-head) of it at all, and its ranged reads only the first and the
        // last MiB — for a landed case exactly those two, once each.
        let first = format!("read {shown} 0 {MIB}");
        let last = format!("read {shown} {} {MIB}", SIZE - MIB);
        let of_it: Vec<&String> = log
            .iter()
            .filter(|l| l.starts_with("read") && l.contains(shown))
            .collect();
        for read in &of_it {
            assert!(**read == first || **read == last, "{case}: {read}");
        }
        let lands = matches!(case, "as staged" | "middle changed");
        if lands {
            let mut sorted: Vec<&str> = of_it.iter().map(|r| r.as_str()).collect();
            sorted.sort();
            let mut want = vec![first.as_str(), last.as_str()];
            want.sort();
            assert_eq!(sorted, want, "{case}: the reads of the file");
        } else {
            assert!(of_it.contains(&&first), "{case}: {of_it:?}");
        }
        let writes: Vec<&String> = log
            .iter()
            .filter(|l| {
                [
                    "create",
                    "rename",
                    "remove",
                    "set-mode",
                    "set-owner",
                    "set-handle-mode",
                    "link",
                    "write ",
                    "copy",
                ]
                .iter()
                .any(|m| l.starts_with(m))
            })
            .collect();
        assert!(writes.is_empty(), "{case}: {writes:?}");
        if lands {
            assert!(found.rows.is_empty(), "{case}");
            assert_eq!(found.notes.len(), 1, "{case}: {:?}", found.notes);
            assert!(
                found.notes[0].ends_with("landed (found on reopen): nothing was written again"),
                "{case}: {:?}",
                found.notes
            );
            assert!(
                journal_dirs_of(&cache, "host", &target).is_empty(),
                "{case}: retired"
            );
        } else {
            assert!(found.notes.is_empty(), "{case}: {:?}", found.notes);
            assert_eq!(found.rows.len(), 1, "{case}");
            assert!(
                found.rows[0]
                    .said
                    .iter()
                    .any(|s| s.starts_with("a save was cut off")),
                "{case}: {:?}",
                found.rows[0].said
            );
            assert_eq!(
                journal_dirs_of(&cache, "host", &target).len(),
                1,
                "{case}: kept"
            );
        }
        assert_eq!(std::fs::metadata(&target).unwrap().len(), SIZE);
    }
}
