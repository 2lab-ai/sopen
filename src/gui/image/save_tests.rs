//! An image Save cut off at each step (07 §3.1, E18's image cases), checked against the real
//! folder of the file: the journal records each step before the request that follows it, and
//! the next open — or the next Save — reconciles it from fresh reads, never writing twice. Also
//! Esc stopping a Save before its rename (§10), a scripted run's journal (none kept, unless an
//! outcome is unknown), and `SOPEN_KEYS` played into a window of its own.

use super::edit_tests::{bytes_of, current, journal_dirs_of, words};
use super::*;
use crate::gui::testkit::{command, Driver};
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
    /// The temp's create gets no reply, after it made the (empty) temp.
    CreatedNoReply,
    /// The connection dies once the temp, written, was stat'ed: every request fails from then
    /// on, a remove included.
    DieAfterTempStat,
    /// The rename lands, and its reply is lost.
    RenameReplyLost,
    /// The temp's create waits for `release`.
    HoldCreate,
}

#[derive(Default)]
struct Control {
    cut: std::sync::Mutex<Cut>,
    dead: AtomicBool,
    holding: AtomicBool,
    release: AtomicBool,
}

/// A connection — the local disk, or a real `sftp-server` — that cuts a Save off as `Control`
/// says.
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
}

impl RemoteFs for Cutting {
    fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.alive()?;
        self.inner.stat_opt(path)
    }
    fn lstat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.alive()?;
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
        like: Option<Like>,
    ) -> std::result::Result<Uploaded, CreateError> {
        if let Err(e) = self.alive() {
            return Err(CreateError::new(Created::No, e));
        }
        match self.cut() {
            Cut::CreateNoReply => {
                return Err(CreateError::new(
                    Created::Unknown,
                    anyhow!("simulated: the create got no reply"),
                ))
            }
            Cut::CreatedNoReply => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .unwrap();
                return Err(CreateError::new(
                    Created::Unknown,
                    anyhow!("simulated: the create got no reply"),
                ));
            }
            Cut::HoldCreate => {
                self.control.holding.store(true, Ordering::SeqCst);
                while !self.control.release.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            _ => {}
        }
        self.inner.create_from(local, path, like)
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        self.alive()?;
        self.inner.set_mode(path, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.alive()?;
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
        self.inner.link_new(from, to)
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
    fn hash_check_limit(&self) -> u64 {
        self.inner.hash_check_limit()
    }
    fn patcher(&mut self) -> Option<&mut dyn crate::remote::patch::PatchFs> {
        self.inner.patcher()
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
        self.drv.until(&mut self.win, 20, |w| {
            w.listed && w.recovering.is_none() && w.job.is_none()
        });
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
    }

    fn cut(&self, cut: Cut) {
        *self.control.cut.lock().unwrap() = cut;
    }

    /// The window gone without a word (a kill), and the file opened again through a healthy
    /// connection of the same side.
    fn reopen(&mut self) {
        let fs = connection(self.side).unwrap();
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

    fn shows(&self, prefix: &str) -> bool {
        self.texts().iter().any(|t| t.starts_with(prefix))
    }

    fn save(&mut self) {
        self.key(Key::S, command(false));
        self.settle();
    }
}

fn journal_edits_of(rig: &Rig) -> Vec<String> {
    words(&current(&rig.journals()[0]))
}

/// E18, cut before the temp: the create got no reply and made nothing. The record (phase
/// `naming`) stays; the next open finds the file as it was — not landed — clears it, and offers
/// the edits back. Nothing is left in the folder.
#[test]
fn e18_cut_before_the_temp_offers_the_edits_back() {
    let mut rig = rig(Side::Local).unwrap();
    let original = std::fs::read(&rig.path).unwrap();
    rig.crop();
    rig.cut(Cut::CreateNoReply);
    rig.save();
    assert_eq!(
        rig.phase().as_deref(),
        Some("Naming"),
        "recorded before the create"
    );
    assert!(rig.temps().is_empty());
    assert!(
        rig.win.doc.unsaved(),
        "the outcome is unknown: the edits stay"
    );
    rig.reopen();
    assert_eq!(rig.phase(), None, "not landed: the record ended");
    assert!(rig.drv.find_text("Restore").is_some(), "{:?}", rig.texts());
    assert!(rig.shows("unsaved edits from "));
    assert_eq!(std::fs::read(&rig.path).unwrap(), original);
    assert_eq!(journal_edits_of(&rig), ["crop"]);
}

/// E18, cut after the temp was created (its create's reply lost): the temp is there, but only
/// the name was recorded, so sopen cannot prove it its own. The next open asks — Remove it, Keep
/// it — and Restore and Discard wait; Later asks again on the next open; Remove it removes it,
/// and then the edits are offered.
#[test]
fn e18_cut_after_the_temp_was_created_asks_before_removing_it() {
    let mut rig = rig(Side::Local).unwrap();
    rig.crop();
    rig.cut(Cut::CreatedNoReply);
    rig.save();
    assert_eq!(rig.temps().len(), 1);
    assert_eq!(rig.phase().as_deref(), Some("Naming"));
    rig.reopen();
    assert!(
        rig.drv.find_text("Remove it").is_some(),
        "{:?}",
        rig.texts()
    );
    assert!(rig.drv.find_text("Keep it").is_some());
    assert!(
        rig.drv.find_text("Restore").is_none(),
        "waits for the answer"
    );
    assert!(rig.drv.find_text("Discard").is_none());
    assert!(rig.shows("unsaved edits from "));
    assert_eq!(rig.temps().len(), 1, "never removed unasked");
    // Later: asked again.
    rig.drv.click_text(&mut rig.win, "Later");
    rig.reopen();
    assert!(rig.drv.find_text("Remove it").is_some(), "asked again");
    rig.drv.click_text(&mut rig.win, "Remove it");
    rig.drv.idle(&mut rig.win, 2);
    assert!(rig.temps().is_empty(), "removed as asked");
    assert!(rig.drv.find_text("Restore").is_some(), "{:?}", rig.texts());
    assert_eq!(rig.phase(), None, "answered: the record ended");
}

/// E18, cut after the temp was written and its stat recorded (phase `created`), the connection
/// gone — its remove too: the temp is left. The next open finds it equal to the record — the
/// one this Save made — and removes it unasked; the file is as it was; the edits are offered.
#[test]
fn e18_cut_after_the_temp_was_written_removes_it_on_reopen() {
    let mut rig = rig(Side::Local).unwrap();
    let original = std::fs::read(&rig.path).unwrap();
    rig.crop();
    rig.cut(Cut::DieAfterTempStat);
    rig.save();
    assert_eq!(rig.phase().as_deref(), Some("Created"));
    assert_eq!(rig.temps().len(), 1, "left: its remove failed too");
    rig.reopen();
    assert!(rig.temps().is_empty(), "provably its own: removed");
    assert!(rig.drv.find_text("Remove it").is_none());
    assert!(rig.drv.find_text("Restore").is_some(), "{:?}", rig.texts());
    assert_eq!(std::fs::read(&rig.path).unwrap(), original);
    assert_eq!(rig.phase(), None);
}

/// E18, the rename landed and its reply was lost, then a kill — through the local disk and a
/// real `sftp-server`. The next open finds the file holding the output: landed, the journal
/// retired, and nothing written again.
#[test]
fn e18_a_landed_rename_whose_reply_was_lost_is_found_on_reopen() {
    for side in [Side::Local, Side::Sftp] {
        let Some(mut rig) = rig(side) else {
            continue;
        };
        rig.crop();
        let output = rig.win.doc.encode().unwrap().bytes;
        rig.cut(Cut::RenameReplyLost);
        rig.save();
        assert_eq!(rig.phase().as_deref(), Some("Renaming"), "{side:?}");
        assert_eq!(
            std::fs::read(&rig.path).unwrap(),
            output,
            "{side:?}: it landed"
        );
        let landed = std::fs::metadata(&rig.path).unwrap().modified().unwrap();
        rig.reopen();
        assert!(
            rig.shows("the save at ")
                && rig
                    .texts()
                    .iter()
                    .any(|t| t.ends_with("landed (found on reopen): nothing was written again")),
            "{side:?}: {:?}",
            rig.texts()
        );
        assert!(rig.journals().is_empty(), "{side:?}: retired");
        assert!(rig.drv.find_text("Restore").is_none());
        assert_eq!(std::fs::read(&rig.path).unwrap(), output);
        let now = std::fs::metadata(&rig.path).unwrap().modified().unwrap();
        assert_eq!(now, landed, "{side:?}: not written again");
        assert!(rig.temps().is_empty());
    }
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
    assert_eq!(file::this_files_journals(&cache, "box", abs), vec![ours]);
}

/// The first pass of a window with an edit and a close asked for at once (`close=ask`): the
/// journal it writes is not taken for an earlier session's by the look for those, which runs
/// in the same pass — no row, one journal, every time.
#[test]
fn the_first_journal_is_never_listed_as_an_earlier_one() {
    for _ in 0..20 {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("shot.png");
        std::fs::write(&path, bytes_of(ImageFormat::Png)).unwrap();
        let cache = tmp.path().join("cache");
        let mut win = super::edit_tests::window_on(&path, &cache);
        win.doc.crop([0, 0, 50, 40]).unwrap();
        win.ask_close = true;
        let mut drv = Driver::new(1000.0, 700.0);
        drv.until(&mut win, 10, |w| w.listed && w.recovering.is_none());
        drv.idle(&mut win, 2);
        assert!(win.rows.is_empty() && win.temps.is_empty(), "no row");
        assert!(win.prompt, "asked");
        assert_eq!(super::edit_tests::journal_dirs(&cache, &path).len(), 1);
    }
}

/// The local disk, a directory's creation held a moment after it is made (before its journal
/// has its lock file): the moment a listing could take a journal being made for an earlier one.
struct SlowMkdir;

impl journal::Fs for SlowMkdir {
    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        journal::Disk.create_dir(path)?;
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    }
    fn write_new(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
        journal::Disk.list(dir)
    }
    fn node(&self, path: &Path, follow: bool) -> std::io::Result<Option<journal::Node>> {
        journal::Disk.node(path, follow)
    }
    fn lock(&self, path: &Path, create: bool) -> std::io::Result<Option<journal::Held>> {
        journal::Disk.lock(path, create)
    }
}

/// The same first pass with the journal's folder made slowly: the look for earlier journals,
/// running meanwhile, would find it without its lock and take it — unless no journal is written
/// while that look is out.
#[test]
fn the_first_journal_waits_for_the_look_for_earlier_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("shot.png");
    std::fs::write(&path, bytes_of(ImageFormat::Png)).unwrap();
    let cache = tmp.path().join("cache");
    // The journals' folder is there already (an earlier session's journal went, its folder
    // stays): only the journal's own directory is made now.
    let base = crate::cache::edits_path(&cache, crate::cache::LOCAL_HOST, path.to_str().unwrap());
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(base.parent().unwrap())
            .unwrap();
    }
    // Another live session's journal of the file: its lock is taken, so the look goes on
    // trying it for a while — the time a journal made meanwhile could be taken.
    let live = journal::Journal::create(
        journal::disk(),
        &cache,
        crate::cache::LOCAL_HOST,
        path.to_str().unwrap(),
        journal::Kind::Image,
    )
    .unwrap();
    let mut win = super::edit_tests::window_on(&path, &cache);
    win.keeper = file::Keeper::at(Arc::new(SlowMkdir), &win.file.lock().unwrap());
    win.doc.crop([0, 0, 50, 40]).unwrap();
    win.ask_close = true;
    let mut drv = Driver::new(1000.0, 700.0);
    drv.until(&mut win, 10, |w| w.listed && w.recovering.is_none());
    drv.idle(&mut win, 2);
    assert!(win.rows.is_empty() && win.temps.is_empty(), "no row");
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
fn reopen_look(cache: &Path, target: &Path) -> file::Recovered {
    let abs = target.to_str().unwrap().to_string();
    let at = LocalFs::new().stat(&abs).unwrap();
    let origin = Origin::new(Box::new(LocalFs::new()), None, Some("host".into()), abs, at);
    let file = ImageFile::new(origin, b"the bytes as read".to_vec(), cache);
    let basis = file.basis();
    file::recover(&Mutex::new(file), &basis)
}

/// E18, an image whose encoded output is above 256 MiB, landed with its reply lost: the next
/// open finds it by the staged stat and the first and last MiB (nothing between is read), retires
/// the journal and writes nothing again. A file whose middle differs, its stat and ends the
/// same, counts as landed too — reconciliation reads nothing between (07 §3.1).
#[test]
fn a_landed_save_above_256_mib_is_found_by_its_first_and_last_mib() {
    const SIZE: u64 = crate::remote::HASH_CHECK_MAX + (16 << 20);
    for middle_changed in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let target = tmp.path().join("big.png");
        big_output(&target, SIZE);
        cut_off_big_save(&cache, &target);
        if middle_changed {
            use std::io::{Seek, SeekFrom, Write};
            let mtime = std::fs::metadata(&target).unwrap().modified().unwrap();
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .open(&target)
                .unwrap();
            f.seek(SeekFrom::Start(SIZE / 2)).unwrap();
            f.write_all(b"another writer").unwrap();
            f.set_modified(mtime).unwrap();
        }
        let found = reopen_look(&cache, &target);
        assert!(found.rows.is_empty(), "middle_changed={middle_changed}");
        assert_eq!(found.notes.len(), 1, "{:?}", found.notes);
        assert!(
            found.notes[0].ends_with("landed (found on reopen): nothing was written again"),
            "{:?}",
            found.notes
        );
        assert!(
            journal_dirs_of(&cache, "host", &target).is_empty(),
            "retired"
        );
        assert_eq!(std::fs::metadata(&target).unwrap().len(), SIZE);
    }
    // Its ends changed: not the output — and not the file it started from either: unknown.
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let target = tmp.path().join("big.png");
    big_output(&target, SIZE);
    cut_off_big_save(&cache, &target);
    {
        use std::io::{Seek, SeekFrom, Write};
        let mtime = std::fs::metadata(&target).unwrap().modified().unwrap();
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .unwrap();
        f.seek(SeekFrom::Start(SIZE - 4)).unwrap();
        f.write_all(b"edge").unwrap();
        f.set_modified(mtime).unwrap();
    }
    let found = reopen_look(&cache, &target);
    assert!(found.notes.is_empty(), "{:?}", found.notes);
    assert_eq!(found.rows.len(), 1);
    assert!(
        found.rows[0]
            .said
            .iter()
            .any(|s| s.starts_with("a save was cut off")),
        "{:?}",
        found.rows[0].said
    );
    assert_eq!(journal_dirs_of(&cache, "host", &target).len(), 1, "kept");
}
