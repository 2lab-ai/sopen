//! Saving an edited remote video or sound (07 §5.3, §9, §10; §12 E15, E16, E20): the hard-link
//! backup made before `write_atomic` and removed only when its Save did not land, Save from a
//! stream's URL, the baseline pass, the Source and byte server closed after a Save lands, no
//! edits while a Save runs, Esc, the journal, the close prompt and Save As on F7. Each runs on
//! both servers (the local disk, and OpenSSH's `sftp-server`, required under
//! `SOPEN_REQUIRE_SFTP_SERVER`), and watches the remote through a log of the requests that
//! change it.

use super::edit_tests::{order, tones, tools_or_skip, walk};
use super::*;
use crate::gui::testkit::{command, Driver};
use crate::remote::{LocalFs, RemoteFs, SftpRemote};
use eframe::egui::{Modifiers, PointerButton};
use std::os::unix::fs::MetadataExt;
use std::sync::Weak;

/// Which server a test runs against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Local,
    Sftp,
}

fn sides() -> Vec<Side> {
    let mut sides = vec![Side::Local];
    if SftpRemote::local_server().is_some() {
        sides.push(Side::Sftp);
    }
    sides
}

fn conn(side: Side) -> Box<dyn RemoteFs + Send> {
    match side {
        Side::Local => Box::new(LocalFs::new()),
        Side::Sftp => Box::new(SftpRemote::local_server().expect("checked by sides()")),
    }
}

/// How the window reads the file: through a stream's URL (06 §7.4), or from a fetched copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    Stream,
    Copy,
}

/// The requests that change the remote, in order: `create <name>`, `rename <from> <to>`,
/// `link <from> <to>`, `remove <name>` — names without their folder.
#[derive(Clone, Default)]
struct Log(
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Vec<u64>>>,
    Arc<std::sync::atomic::AtomicU64>,
);

impl Log {
    fn push(&self, line: String) {
        self.0.lock().unwrap().push(line);
    }

    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    /// The size of each local file an upload sent, in order.
    fn uploaded(&self) -> Vec<u64> {
        self.1.lock().unwrap().clone()
    }

    /// The bytes whole-file reads (`read_to`) took from the remote.
    fn read(&self) -> u64 {
        self.2.load(SeqCst)
    }

    /// The verbs in order.
    fn verbs(&self) -> Vec<String> {
        self.lines()
            .iter()
            .map(|l| l.split(' ').next().unwrap().to_string())
            .collect()
    }
}

fn name_of(path: &str) -> String {
    crate::remote::split_parent(path).1.to_string()
}

/// What a test makes the Origin's connection do.
#[derive(Clone, Default)]
struct Knobs {
    /// The server has no hard links.
    no_links: bool,
    /// The next upload's create is refused (a status reply: nothing was made).
    refuse_create: Arc<AtomicBool>,
    /// The next rename of a temp over the file lands, but its reply is lost.
    lose_rename: Arc<AtomicBool>,
    /// The next rename lands and replies, and the `lstat` of the file right after it fails.
    blind_after_rename: Arc<AtomicBool>,
    /// The next upload holds at its second look at its stop (one chunk in) until released.
    hold_upload: Option<Gate>,
}

/// The Origin's connection: every request passes through to `inner`, and those that change the
/// remote are logged.
struct Logged {
    inner: Box<dyn RemoteFs + Send>,
    log: Log,
    knobs: Knobs,
    /// A rename landed under `blind_after_rename`: the next `lstat` of this path fails.
    blind: Option<String>,
}

impl RemoteFs for Logged {
    fn stat_opt(&mut self, path: &str) -> Result<Option<crate::remote::Stat>> {
        self.inner.stat_opt(path)
    }
    fn lstat_opt(&mut self, path: &str) -> Result<Option<crate::remote::Stat>> {
        if self.blind.as_deref() == Some(path) {
            self.blind = None;
            anyhow::bail!("the connection went quiet (the test's)");
        }
        self.inner.lstat_opt(path)
    }
    fn realpath(&mut self, path: &str) -> Result<String> {
        self.inner.realpath(path)
    }
    fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>> {
        self.inner.read_head(path, max)
    }
    fn list_dir(&mut self, path: &str) -> Result<Vec<String>> {
        self.inner.list_dir(path)
    }
    fn list_entries(&mut self, path: &str) -> Result<Vec<crate::remote::Entry>> {
        self.inner.list_entries(path)
    }
    fn free_space(&mut self, path: &str) -> Option<u64> {
        self.inner.free_space(path)
    }
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        let n = self.inner.read_to(path, out, progress)?;
        self.log.2.fetch_add(n, SeqCst);
        Ok(n)
    }
    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<crate::remote::Like>,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        self.create_from_stoppable(local, path, like, &|| false)
    }
    fn create_from_stoppable(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<crate::remote::Like>,
        stop: &dyn Fn() -> bool,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        self.log.push(format!("create {}", name_of(path)));
        let size = std::fs::metadata(local).map_or(0, |m| m.len());
        self.log.1.lock().unwrap().push(size);
        if self.knobs.refuse_create.swap(false, SeqCst) {
            return Err(crate::remote::CreateError::new(
                crate::remote::Created::No,
                anyhow::anyhow!("permission denied (the test's refusal)"),
            ));
        }
        match self.knobs.hold_upload.take() {
            Some(gate) => {
                let looks = std::cell::Cell::new(0);
                let mut hook = gate.hook("upload");
                let held = std::cell::RefCell::new(&mut hook);
                let checked = || {
                    looks.set(looks.get() + 1);
                    if looks.get() == 2 {
                        (held.borrow_mut())("upload");
                    }
                    stop()
                };
                self.inner
                    .create_from_stoppable(local, path, like, &checked)
            }
            None => self.inner.create_from_stoppable(local, path, like, stop),
        }
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        self.inner.set_mode(path, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.log
            .push(format!("rename {} {}", name_of(from), name_of(to)));
        self.inner.rename_over(from, to)?;
        if self.knobs.blind_after_rename.swap(false, SeqCst) {
            self.blind = Some(to.to_string());
        }
        if self.knobs.lose_rename.swap(false, SeqCst) {
            return Err(crate::remote::unknown(
                "the rename's reply was lost (the test's)",
            ));
        }
        Ok(())
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.log
            .push(format!("link {} {}", name_of(from), name_of(to)));
        if self.knobs.no_links {
            anyhow::bail!("this server has no hard links (the test's)");
        }
        self.inner.link_new(from, to)
    }
    fn can_link(&self) -> bool {
        !self.knobs.no_links && self.inner.can_link()
    }
    fn can_replace(&self) -> bool {
        self.inner.can_replace()
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        self.log.push(format!("remove {}", name_of(path)));
        self.inner.remove(path)
    }
    fn hash_check_limit(&self) -> u64 {
        self.inner.hash_check_limit()
    }
    fn patcher(&mut self) -> Option<&mut dyn crate::remote::patch::PatchFs> {
        self.inner.patcher()
    }
}

/// A Save held at one of its steps (`MediaFile::step_hook`) until the test lets it go: a
/// handshake, no clock.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<Held>, std::sync::Condvar)>);

/// Where a [`Gate`] holds: the step reached, and whether it was let go.
type Held = (Option<String>, bool);

impl Gate {
    /// The hook: at step `at`, says it is there and waits until released (and runs `then`
    /// first, once).
    fn hook(&self, at: &'static str) -> Box<dyn FnMut(&str) + Send> {
        let me = self.clone();
        Box::new(move |step| {
            if step != at {
                return;
            }
            let (lock, cv) = &*me.0;
            let mut s = lock.lock().unwrap();
            s.0 = Some(step.to_string());
            cv.notify_all();
            while !s.1 {
                s = cv.wait(s).unwrap();
            }
        })
    }

    fn reached(&self) -> bool {
        self.0 .0.lock().unwrap().0.is_some()
    }

    fn release(&self) {
        let (lock, cv) = &*self.0;
        lock.lock().unwrap().1 = true;
        cv.notify_all();
    }
}

/// The media window on a "remote" WAV (four tones, a second each) of host `box`, and what its
/// connection sent.
struct Ed {
    win: MediaWindow,
    drv: Driver,
    remote: PathBuf,
    log: Log,
    knobs: Knobs,
    /// A stream's byte server, as long as anything holds it.
    served: Option<Weak<ffmpeg::Served>>,
    cache: PathBuf,
    side: Side,
    tmp: tempfile::TempDir,
}

const HOST: &str = "box";

/// The journal folders of `remote` (of host [`HOST`]) under `cache`.
fn journals_of(cache: &Path, remote: &Path) -> Vec<PathBuf> {
    let home = crate::cache::edits_path(cache, HOST, remote.to_str().unwrap());
    let (dir, name) = (home.parent().unwrap(), home.file_name().unwrap());
    let prefix = format!("{}.journal-", name.to_string_lossy());
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let n = p.file_name().unwrap().to_string_lossy().into_owned();
                    n.starts_with(&prefix) && !n.ends_with(".gone")
                })
                .collect()
        })
        .unwrap_or_default()
}

fn no_reopen() -> crate::stream::Reopen {
    Box::new(|| anyhow::bail!("this test has no second connection"))
}

impl Ed {
    fn open(side: Side, read: Read, knobs: Knobs) -> Option<Ed> {
        let tools = tools_or_skip()?;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("remote");
        std::fs::create_dir(&dir).unwrap();
        let remote = dir.join("r.wav");
        tones(&tools, &remote, true);
        Some(Ed::on(side, read, knobs, tmp, remote))
    }

    /// The window on `remote` (already made), its cache in `tmp`.
    fn on(side: Side, read: Read, knobs: Knobs, tmp: tempfile::TempDir, remote: PathBuf) -> Ed {
        let mut ed = Ed::unsettled(side, read, knobs, tmp, remote);
        ed.settle();
        ed
    }

    /// [`Ed::on`] without a waveform decoder (a long file's takes a while, and all the CPU),
    /// planned at once: settled once the timeline is drawn and the recovery listed.
    fn on_quiet(side: Side, knobs: Knobs, tmp: tempfile::TempDir, remote: PathBuf) -> Ed {
        let mut ed = Ed::unsettled(side, Read::Copy, knobs, tmp, remote);
        ed.quiet();
        ed
    }

    /// No decoder and no encoder question: the plan is made here, and the window's first pass
    /// starts nothing; then passes until the timeline is drawn and the recovery listed.
    fn quiet(&mut self) {
        Arc::make_mut(&mut self.win.media).plan();
        self.win.unplanned = false;
        self.win.started = true;
        self.drv.until(&mut self.win, 60, |w| {
            w.timeline_rect.is_some() && w.recovering.is_none() && w.recovery_started
        });
        self.drv.idle(&mut self.win, 2);
    }

    fn unsettled(
        side: Side,
        read: Read,
        knobs: Knobs,
        tmp: tempfile::TempDir,
        remote: PathBuf,
    ) -> Ed {
        let abs = remote.to_str().unwrap().to_string();
        let name = remote.file_name().unwrap().to_string_lossy().into_owned();
        let shown = format!("{HOST}:{abs}");
        let cache = tmp.path().join("cache");
        let mut fs = conn(side);
        let (media, at, served) = match read {
            Read::Copy => {
                let copy = tmp.path().join("copy").join(&name);
                std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
                std::fs::copy(&remote, &copy).unwrap();
                let bytes = std::fs::read(&copy).unwrap();
                let mut at = fs.stat(&abs).unwrap();
                at.sha256 = Some(journal::sha256(&bytes));
                let media = Media::probe_shown(&Input::path(&copy), &name, &shown).unwrap();
                (media, at, None)
            }
            Read::Stream => {
                let transport = conn(side).into_ranged(&abs).map_err(|r| r.error).unwrap();
                let at = transport.opened();
                let source = Source::open(transport, no_reopen(), Arc::new(|| {})).unwrap();
                let media = Media::stream(source, &name, &shown).unwrap();
                let served = media.input.served().map(Arc::downgrade);
                (media, at, served)
            }
        };
        let log = Log::default();
        let logged = Logged {
            inner: fs,
            log: log.clone(),
            knobs: knobs.clone(),
            blind: None,
        };
        let origin = Origin::new(Box::new(logged), None, Some(HOST.into()), abs, at);
        let file = MediaFile::new(origin, &cache);
        let mut win = MediaWindow::new(
            media,
            file,
            crate::cache::exports_root(&cache),
            Marks::default(),
            Selection::default(),
            Vec::new(),
        );
        win.sink = Sink::Null;
        win.unplanned = true;
        Ed {
            win,
            drv: Driver::new(900.0, 520.0),
            remote,
            log,
            knobs,
            served,
            cache,
            side,
            tmp,
        }
    }

    /// Passes until the waveform is on screen, the window planned and what an earlier session
    /// left listed, and nothing runs.
    fn settle(&mut self) {
        self.drv.until(&mut self.win, 60, |w| {
            w.timeline_rect.is_some()
                && w.save.is_none()
                && w.planning.is_none()
                && !w.unplanned
                && w.recovering.is_none()
                && w.recovery_started
                && w.wave
                    .as_ref()
                    .is_some_and(|s| s.peaks.lock().unwrap().as_ref().is_some_and(|p| p.complete))
        });
        self.drv.idle(&mut self.win, 2);
    }

    fn at(&self, f: f32) -> Pos2 {
        let r = self.win.timeline_rect.expect("timeline drawn");
        Pos2::new(r.left() + f * r.width(), r.center().y)
    }

    /// Removes the second second (880 Hz) of the four: the file then plays 440, 1320, 1760.
    fn cut_second(&mut self) {
        self.cut(0.25, 0.5);
    }

    /// Removes the stretch from `from` to `to` (shares of the timeline's width).
    fn cut(&mut self, from: f32, to: f32) {
        let (a, b) = (self.at(from), self.at(to));
        self.drv.drag(&mut self.win, PointerButton::Primary, a, b);
        self.drv.key(&mut self.win, Key::Delete, Modifiers::NONE);
        assert!(self.win.unsaved());
    }

    fn save(&mut self) {
        self.drv.key(&mut self.win, Key::S, command(false));
        assert!(self.win.save.is_some(), "⌘S started a save");
        self.wait_save();
    }

    /// ⌘S and the Save's end, for a Save that may end within the key's own frames (one that
    /// renders nothing).
    fn save_briefly(&mut self) {
        self.drv.key(&mut self.win, Key::S, command(false));
        self.wait_save();
    }

    /// A Save's end; the bound only catches a hang (a loaded machine renders slowly).
    fn wait_save(&mut self) {
        self.drv.until(&mut self.win, 300, |w| w.save.is_none());
        self.drv.idle(&mut self.win, 2);
    }

    /// The hidden backup links beside the file.
    fn links(&self) -> Vec<PathBuf> {
        let dir = self.remote.parent().unwrap();
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".r.wav.sopen-backup-")
            })
            .collect();
        found.sort();
        found
    }

    /// Anything sopen left beside the file that is not a backup link (temps).
    fn temps(&self) -> Vec<PathBuf> {
        let dir = self.remote.parent().unwrap();
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let n = p.file_name().unwrap().to_string_lossy().into_owned();
                n.starts_with(".r.wav.sopen-") && !n.starts_with(".r.wav.sopen-backup-")
            })
            .collect()
    }

    /// The journal folders of the file.
    fn journals(&self) -> Vec<PathBuf> {
        journals_of(&self.cache, &self.remote)
    }

    fn texts(&self) -> String {
        let mut all: Vec<String> = self.win.notices.clone();
        all.push(self.win.status.text.clone());
        all.join("\n")
    }
}

/// E15, E20 (07 §5.3): the first Save of a remote file renders, then links the original to
/// `.<name>.sopen-backup-<UTC>` beside it — before anything is uploaded — and only then
/// `write_atomic` writes over the file. The link is the original's inode and size, and says so;
/// a write through a handle opened before the Save changes it, as the words say. A second Save
/// makes no link. From a stream and from a fetched copy, on both servers.
#[test]
fn the_first_remote_save_links_the_original_before_it_uploads() {
    for side in sides() {
        for read in [Read::Stream, Read::Copy] {
            let Some(mut ed) = Ed::open(side, read, Knobs::default()) else {
                return;
            };
            let original = std::fs::read(&ed.remote).unwrap();
            let meta = std::fs::metadata(&ed.remote).unwrap();
            let held = std::fs::OpenOptions::new()
                .write(true)
                .open(&ed.remote)
                .unwrap();
            // A stream's byte server, held here past the Save (the window lets it go): what the
            // render read through it.
            let server = ed.served.as_ref().and_then(Weak::upgrade);
            let rendered_before = server.as_ref().map(|s| s.server().sent(Kind::Render));
            ed.cut_second();
            ed.save();
            let what = format!("{side:?}, {read:?}");
            // E15: the render read the remote through the range server's `render` URL; the
            // upload is the output, its size the saved file's.
            if let (Some(server), Some(before)) = (&server, rendered_before) {
                assert!(
                    server.server().sent(Kind::Render) > before,
                    "{what}: the render read through the byte server"
                );
            }
            drop(server);
            assert_eq!(
                ed.log.uploaded(),
                [std::fs::metadata(&ed.remote).unwrap().len()],
                "{what}: the upload is the output"
            );
            assert!(
                ed.win.status.text.starts_with(&format!("saved {HOST}:")),
                "{what}: {}",
                ed.texts()
            );
            assert_eq!(order(&ed.remote), [440, 1320, 1760], "{what}: written");
            assert_eq!(
                ed.log.verbs(),
                ["link", "create", "rename"],
                "{what}: {:?}",
                ed.log.lines()
            );
            let links = ed.links();
            assert_eq!(links.len(), 1, "{what}: {links:?}");
            let link = std::fs::metadata(&links[0]).unwrap();
            assert_eq!(
                (link.ino(), link.len()),
                (meta.ino(), meta.len()),
                "{what}: the original's inode and size"
            );
            assert_eq!(std::fs::read(&links[0]).unwrap(), original);
            assert_eq!(
                ed.win.file.lock().unwrap().link().map(|l| l.path.clone()),
                Some(links[0].to_str().unwrap().to_string()),
                "{what}: the session's backup"
            );
            assert!(ed.temps().is_empty(), "{what}: {:?}", ed.temps());
            let said = format!(
                "the original is kept on {HOST} as {} ({}, a hard link: other links or open \
                 handles can still change it); delete it when no longer needed",
                links[0].display(),
                crate::human(meta.len())
            );
            assert!(
                ed.win.notices.contains(&said),
                "{what}: {:?}",
                ed.win.notices
            );
            assert!(ed.win.status.text.contains(&said), "{what}");
            // E20: the link is the original's data, not a copy of it.
            use std::os::unix::fs::FileExt;
            held.write_all_at(b"XXXX", 64).unwrap();
            assert_eq!(
                &std::fs::read(&links[0]).unwrap()[64..68],
                b"XXXX",
                "{what}"
            );
            // A second Save of the session makes no link: the first keeps the file as opened.
            ed.cut(1.0 / 3.0, 2.0 / 3.0);
            ed.save();
            assert!(ed.win.status.text.starts_with("saved "), "{}", ed.texts());
            assert_eq!(order(&ed.remote), [440, 1760], "{what}: the second save");
            assert_eq!(
                ed.log.verbs(),
                ["link", "create", "rename", "create", "rename"],
                "{what}: {:?}",
                ed.log.lines()
            );
            assert_eq!(ed.links(), links, "{what}");
            assert!(ed.journals().is_empty(), "{what}: {:?}", ed.journals());
        }
    }
}

/// A local file keeps PR #15's local backup copy and makes no link (07 §5.3).
#[test]
fn a_local_file_keeps_its_local_backup_and_makes_no_link() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("l.wav");
    tones(&tools, &path, true);
    let media = Media::probe(&path).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let log = Log::default();
    let logged = Logged {
        inner: Box::new(fs),
        log: log.clone(),
        knobs: Knobs::default(),
        blind: None,
    };
    let origin = Origin::new(Box::new(logged), None, None, abs, at);
    let cache = tmp.path().join("cache");
    let file = MediaFile::new(origin, &cache);
    let mut win = MediaWindow::new(
        media,
        file,
        tmp.path().join("work"),
        Marks::default(),
        Selection::default(),
        Vec::new(),
    );
    win.sink = Sink::Null;
    let mut drv = Driver::new(900.0, 520.0);
    drv.until(&mut win, 60, |w| {
        w.timeline_rect.is_some() && w.recovering.is_none() && w.recovery_started
    });
    drv.idle(&mut win, 3);
    let r = win.timeline_rect.unwrap();
    let (a, b) = (
        Pos2::new(r.left() + 0.25 * r.width(), r.center().y),
        Pos2::new(r.left() + 0.5 * r.width(), r.center().y),
    );
    drv.drag(&mut win, PointerButton::Primary, a, b);
    drv.key(&mut win, Key::Delete, Modifiers::NONE);
    drv.key(&mut win, Key::S, command(false));
    drv.until(&mut win, 60, |w| w.save.is_none());
    assert!(
        win.status.text.contains("original backed up at"),
        "{}",
        win.status.text
    );
    assert_eq!(walk(&cache.join(".backup")).len(), 1);
    assert_eq!(log.verbs(), ["create", "rename"], "{:?}", log.lines());
}

/// 07 §5.3: where the server has no hard links, media Save of a remote file is off with the
/// reason — the button, ⌘S and F5 send nothing — and Save As and Export are offered.
#[test]
fn without_hard_links_remote_save_is_off_and_save_as_is_offered() {
    for side in sides() {
        let knobs = Knobs {
            no_links: true,
            ..Knobs::default()
        };
        let Some(mut ed) = Ed::open(side, Read::Stream, knobs) else {
            return;
        };
        ed.cut_second();
        ed.drv.key(&mut ed.win, Key::S, command(false));
        assert!(ed.win.save.is_none(), "{side:?}: no save started");
        assert_eq!(ed.win.status.text, file::NO_LINKS, "{side:?}");
        ed.drv.key(&mut ed.win, Key::F5, Modifiers::NONE);
        assert!(ed.win.save.is_none());
        assert!(ed.log.lines().is_empty(), "{side:?}: {:?}", ed.log.lines());
        // Save As and Export are there.
        assert!(ed.drv.find_text("Save As…").is_some());
        assert!(ed.drv.find_text("Export  E").is_some());
        ed.drv.key(&mut ed.win, Key::F7, Modifiers::NONE);
        assert!(ed.win.save_as.is_some(), "{side:?}: F7 opens Save As");
    }
}

/// 07 §5.3 "When the Save does not land": refused because the file was replaced since it was
/// opened, the link it made — still exactly as made — is removed, and only that name. Changed
/// in place instead, the link (the same inode) is no longer as made: it is kept, and the words
/// say why. A refused upload removes the link too.
#[test]
fn a_save_that_does_not_land_removes_its_link_only_while_it_is_as_made() {
    for side in sides() {
        // Replaced by another file (a rename over it): the link still holds the original.
        let Some(mut ed) = Ed::open(side, Read::Copy, Knobs::default()) else {
            return;
        };
        let gate = Gate::default();
        ed.win.file.lock().unwrap().step_hook = Some(gate.hook("linked"));
        ed.cut_second();
        ed.drv.key(&mut ed.win, Key::S, command(false));
        ed.drv.until(&mut ed.win, 60, |_| gate.reached());
        let writer = ed.remote.with_file_name("writer.tmp");
        std::fs::write(&writer, b"someone else's sound").unwrap();
        std::fs::rename(&writer, &ed.remote).unwrap();
        let links = ed.links();
        assert_eq!(links.len(), 1, "{side:?}: linked before the upload");
        gate.release();
        ed.wait_save();
        assert_eq!(
            ed.win.status.text, "not saved: the file changed",
            "{side:?}"
        );
        assert_eq!(std::fs::read(&ed.remote).unwrap(), b"someone else's sound");
        assert!(ed.links().is_empty(), "{side:?}: the link went");
        let removed = format!("remove {}", name_of(links[0].to_str().unwrap()));
        assert_eq!(
            ed.log
                .lines()
                .iter()
                .filter(|l| l.starts_with("remove"))
                .collect::<Vec<_>>(),
            [&removed],
            "{side:?}: only that name"
        );
        assert!(!ed.log.verbs().contains(&"create".to_string()));
        assert!(ed.win.unsaved(), "the edits stay");

        // Changed in place: the link is the same file, changed — not as made, so kept.
        let Some(mut ed) = Ed::open(side, Read::Copy, Knobs::default()) else {
            return;
        };
        let gate = Gate::default();
        ed.win.file.lock().unwrap().step_hook = Some(gate.hook("linked"));
        ed.cut_second();
        ed.drv.key(&mut ed.win, Key::S, command(false));
        ed.drv.until(&mut ed.win, 60, |_| gate.reached());
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&ed.remote)
            .unwrap()
            .write_all(b"more")
            .unwrap();
        gate.release();
        ed.wait_save();
        let links = ed.links();
        assert_eq!(links.len(), 1, "{side:?}: kept");
        assert!(
            ed.texts()
                .contains("was not removed: it is not as sopen made it"),
            "{side:?}: {}",
            ed.texts()
        );
        assert!(!ed.log.verbs().contains(&"remove".to_string()));

        // The upload refused: nothing landed, the link goes.
        let Some(mut ed) = Ed::open(side, Read::Stream, Knobs::default()) else {
            return;
        };
        ed.knobs.refuse_create.store(true, SeqCst);
        let before = std::fs::read(&ed.remote).unwrap();
        ed.cut_second();
        ed.save();
        assert_eq!(
            ed.win.status.text,
            "save failed",
            "{side:?}: {}",
            ed.texts()
        );
        assert_eq!(std::fs::read(&ed.remote).unwrap(), before);
        assert!(ed.links().is_empty(), "{side:?}: {:?}", ed.log.lines());
        assert_eq!(
            ed.log.verbs(),
            ["link", "create", "remove"],
            "{:?}",
            ed.log.lines()
        );
        assert!(ed.win.unsaved() && ed.win.editable());
    }
}

/// 07 §5.3, §3.1: a rename whose reply is lost is an unknown outcome — the link is kept and
/// named, the journal keeps the Save's record, and editing waits. The next Save reconciles it
/// from fresh reads before it sends anything: it landed, so nothing is written again, the
/// window plays what it wrote and the journal goes.
#[test]
fn a_lost_reply_keeps_the_link_and_the_next_save_finds_it_landed() {
    for side in sides() {
        let Some(mut ed) = Ed::open(side, Read::Stream, Knobs::default()) else {
            return;
        };
        ed.knobs.lose_rename.store(true, SeqCst);
        ed.cut_second();
        ed.save();
        assert_eq!(
            ed.win.status.text,
            "save outcome unknown",
            "{side:?}: {}",
            ed.texts()
        );
        let links = ed.links();
        assert_eq!(links.len(), 1, "{side:?}: kept");
        assert!(
            ed.texts().contains(links[0].to_str().unwrap()),
            "{side:?}: named"
        );
        assert!(ed.win.unknown && !ed.win.editable(), "editing waits");
        let journals = ed.journals();
        assert_eq!(journals.len(), 1, "{side:?}");
        assert!(journals[0].join("save").exists(), "its record is kept");
        assert_eq!(order(&ed.remote), [440, 1320, 1760], "it did land");
        // The next Save: reconciled, nothing sent.
        let sent = ed.log.lines().len();
        ed.save();
        assert!(
            ed.win
                .status
                .text
                .contains("landed (found now): nothing was written again"),
            "{side:?}: {}",
            ed.texts()
        );
        assert_eq!(ed.log.lines().len(), sent, "{side:?}: {:?}", ed.log.lines());
        assert!(!ed.win.unknown && !ed.win.unsaved());
        let playing = ed.win.media.input.local().unwrap().to_path_buf();
        assert_eq!(order(&playing), [440, 1320, 1760]);
        assert!(ed.journals().is_empty(), "{:?}", ed.journals());
        assert_eq!(ed.links(), links, "the link stays the backup");
        // The next Save's baseline is what that Save wrote: an edit of it saves, no new link.
        ed.cut(1.0 / 3.0, 2.0 / 3.0);
        ed.save();
        assert!(
            ed.win.status.text.starts_with("saved "),
            "{side:?}: {}",
            ed.texts()
        );
        assert_eq!(order(&ed.remote), [440, 1760], "{side:?}");
        assert_eq!(ed.links(), links, "{side:?}: no new link");
    }
}

/// 07 §9, E15: after a remote Save from a stream lands, the window plays the staged render, the
/// window's byte server and Source are closed (a reader made before reads `Closed`), and no
/// `Replaced` line appears for its own save.
#[test]
fn after_a_save_lands_the_stream_and_its_server_are_closed() {
    for side in sides() {
        let Some(mut ed) = Ed::open(side, Read::Stream, Knobs::default()) else {
            return;
        };
        let served = ed.served.clone().expect("a stream");
        let mut reader = served.upgrade().unwrap().source().reader(ReaderKind::Bulk);
        ed.cut_second();
        ed.save();
        assert!(ed.win.status.text.starts_with("saved "), "{}", ed.texts());
        let playing = ed.win.media.input.local().map(Path::to_path_buf);
        let playing = playing.expect("the window plays a local render");
        assert!(
            playing.starts_with(ed.cache.join(".edits")),
            "{}",
            playing.display()
        );
        assert!(ed.win.media.input.served().is_none());
        ed.drv
            .until(&mut ed.win, 60, |_| served.upgrade().is_none());
        let mut buf = [0u8; 16];
        let got = reader.read_at(0, &mut buf, &AtomicBool::new(false));
        assert!(
            matches!(got, Err(crate::stream::ReadError::Closed)),
            "{side:?}: {got:?}"
        );
        ed.drv.idle(&mut ed.win, 5);
        assert!(
            !ed.texts().contains("Replaced") && !ed.texts().contains("changed"),
            "{}",
            ed.texts()
        );
    }
}

/// 07 §4.5, §9: a remote file's baseline is the baseline pass — a Bulk reader on the stream,
/// from the first edit — not taken before; a same-size, same-time change of its content after
/// the pass is refused by the overwrite's check right before the rename (the one full read of
/// the file a Save makes): nothing is renamed over it, the temp and the link go.
#[test]
fn a_streamed_baseline_comes_from_the_pass_and_catches_a_same_size_change() {
    for side in sides() {
        let Some(mut ed) = Ed::open(side, Read::Stream, Knobs::default()) else {
            return;
        };
        assert!(ed.win.pass.is_none(), "{side:?}: no pass before an edit");
        ed.cut_second();
        ed.drv.until(&mut ed.win, 60, |w| w.evidence_known());
        let size = std::fs::metadata(&ed.remote).unwrap().len();
        let pass = ed.win.pass.clone().unwrap();
        assert_eq!(pass.done.load(SeqCst), size, "{side:?}: read whole");
        let whole = journal::sha256(&std::fs::read(&ed.remote).unwrap());
        assert_eq!(pass.ended(), Some(Ok(file::Evidence::Whole(whole))));
        let meta = std::fs::metadata(&ed.remote).unwrap();
        let mut bytes = std::fs::read(&ed.remote).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x55;
        std::fs::write(&ed.remote, &bytes).unwrap();
        let f = std::fs::File::options()
            .write(true)
            .open(&ed.remote)
            .unwrap();
        f.set_modified(meta.modified().unwrap()).unwrap();
        drop(f);
        ed.save();
        assert_eq!(
            ed.win.status.text,
            "not saved: the file changed",
            "{}",
            ed.texts()
        );
        assert!(
            ed.texts()
                .contains("its content changed (same size and modification time)"),
            "{}",
            ed.texts()
        );
        assert_eq!(
            ed.log.verbs(),
            ["link", "create", "remove", "remove"],
            "{side:?}: {:?}",
            ed.log.lines()
        );
        assert_eq!(std::fs::read(&ed.remote).unwrap(), bytes);
        assert!(ed.links().is_empty() && ed.temps().is_empty());
    }
}

/// 07 §4.5: a Save of a remote file of at most 256 MiB reads it whole once — the overwrite's
/// check right before the rename — not also before the render (its stat suffices there: the
/// baseline's hash is what that check compares with). Counted at the Origin's reads; a stream's
/// baseline pass reads through the viewer's own connection, once per session.
#[test]
fn a_save_reads_the_remote_file_once_for_its_checks() {
    for side in sides() {
        for read in [Read::Stream, Read::Copy] {
            let Some(mut ed) = Ed::open(side, read, Knobs::default()) else {
                return;
            };
            let size = std::fs::metadata(&ed.remote).unwrap().len();
            ed.cut_second();
            ed.save();
            assert!(ed.win.status.text.starts_with("saved "), "{}", ed.texts());
            assert_eq!(
                ed.log.read(),
                size,
                "{side:?}, {read:?}: one whole read of {size} bytes"
            );
        }
    }
}

/// 07 §10: while a Save runs nothing is edited — cut, delete, paste, undo, redo, `M` do
/// nothing — and Esc stops it before its rename: held before its link, nothing is sent; held
/// after its link, nothing is uploaded and the link goes.
#[test]
fn no_edits_while_a_save_runs_and_esc_stops_it_before_the_rename() {
    for side in sides() {
        for at in ["rendered", "linked"] {
            let Some(mut ed) = Ed::open(side, Read::Stream, Knobs::default()) else {
                return;
            };
            let gate = Gate::default();
            ed.win.file.lock().unwrap().step_hook = Some(gate.hook(at));
            let before = std::fs::read(&ed.remote).unwrap();
            ed.cut_second();
            ed.drv.key(&mut ed.win, Key::S, command(false));
            ed.drv.until(&mut ed.win, 60, |_| gate.reached());
            // 07 §5.3 "before": while the first Save renders, how the original will be kept.
            assert_eq!(
                ed.win.status.text,
                file::link_ahead(HOST),
                "{side:?} at {at}"
            );
            let clips = ed.win.timeline.clips().to_vec();
            let (a, b) = (ed.at(0.6), ed.at(0.8));
            ed.drv.drag(&mut ed.win, PointerButton::Primary, a, b);
            ed.drv.key(&mut ed.win, Key::Delete, Modifiers::NONE);
            ed.drv.cut(&mut ed.win);
            ed.drv.key(&mut ed.win, Key::Z, command(false));
            ed.drv.key(&mut ed.win, Key::Z, command(true));
            assert_eq!(
                ed.win.timeline.clips(),
                &clips[..],
                "{side:?} at {at}: no edit"
            );
            ed.drv.key(&mut ed.win, Key::Escape, Modifiers::NONE);
            gate.release();
            ed.wait_save();
            assert!(
                ed.win.status.text.starts_with("save cancelled"),
                "{side:?} at {at}: {}",
                ed.texts()
            );
            assert_eq!(std::fs::read(&ed.remote).unwrap(), before, "untouched");
            let verbs = ed.log.verbs();
            assert!(!verbs.contains(&"rename".to_string()), "{verbs:?}");
            assert!(
                !verbs.contains(&"create".to_string()),
                "{at}: nothing uploaded: {verbs:?}"
            );
            match at {
                "rendered" => assert!(verbs.is_empty(), "nothing sent: {verbs:?}"),
                _ => assert_eq!(verbs, ["link", "remove"], "the link went"),
            }
            assert!(ed.links().is_empty() && ed.temps().is_empty());
            assert!(
                ed.win.editable() && ed.win.unsaved(),
                "the edits stay, editable"
            );
        }
    }
}

/// 07 §3: the edits are kept in a journal; a window ended as a kill ends it leaves the journal,
/// and the next window on the file offers it back — Restore applies it; a scripted run keeps
/// nothing.
#[test]
fn a_crash_leaves_the_journal_and_the_next_open_restores_it() {
    for side in sides() {
        let Some(mut ed) = Ed::open(side, Read::Copy, Knobs::default()) else {
            return;
        };
        ed.cut_second();
        // Kept with its evidence: the generation written after the pass ended.
        ed.drv.until(&mut ed.win, 60, |w| {
            w.journal_text.1 && w.schedule.status() == Keep::Kept
        });
        ed.drv.idle(&mut ed.win, 3);
        let kept = ed.win.kept_line().0;
        assert!(kept.starts_with("unsaved — kept at "), "{side:?}: {kept}");
        assert_eq!(ed.journals().len(), 1);
        let clips = ed.win.timeline.clips().to_vec();
        let Ed {
            win, tmp, remote, ..
        } = ed;
        win.crash();
        let mut again = Ed::on(side, Read::Copy, Knobs::default(), tmp, remote);
        assert_eq!(again.win.rows.len(), 1, "{side:?}: one row");
        let line = again
            .drv
            .text_starting_with("unsaved edits from ")
            .expect("the row");
        assert!(line.contains("(2 pieces) kept at"), "{line}");
        again.drv.click_text(&mut again.win, "Restore");
        again
            .drv
            .until(&mut again.win, 60, |w| w.restoring.is_none() && w.unsaved());
        assert_eq!(again.win.timeline.clips(), &clips[..], "{side:?}: restored");
        again.save();
        assert!(
            again.win.status.text.starts_with("saved "),
            "{side:?}: {}",
            again.texts()
        );
        assert_eq!(order(&again.remote), [440, 1320, 1760]);
        assert!(again.journals().is_empty(), "{:?}", again.journals());
    }
}

/// 07 §3 (the image and hex windows' rule, for media): a journal folder that holds nothing but
/// its lock — or not even that: its session ended between the `mkdir` and its first
/// generation — holds no edit. The next open removes it and shows no row. On both servers.
#[test]
fn a_journal_folder_holding_nothing_is_removed_and_never_offered() {
    let Some(tools) = tools_or_skip() else { return };
    for side in sides() {
        for locked in [false, true] {
            let what = format!("{side:?}, locked={locked}");
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("remote");
            std::fs::create_dir(&dir).unwrap();
            let remote = dir.join("r.wav");
            tones(&tools, &remote, true);
            let cache = tmp.path().join("cache");
            let base = crate::cache::edits_path(&cache, HOST, remote.to_str().unwrap());
            let name = base.file_name().unwrap().to_string_lossy().into_owned();
            let journal = base.with_file_name(format!("{name}.journal-20261122T010203Z-0123abcd"));
            std::fs::create_dir_all(&journal).unwrap();
            if locked {
                std::fs::write(journal.join("lock"), b"").unwrap();
            }
            let ed = Ed::on(side, Read::Copy, Knobs::default(), tmp, remote);
            assert!(ed.win.rows.is_empty(), "{what}: offered");
            assert!(
                ed.drv.text_starting_with("unsaved edits from ").is_none(),
                "{what}: {}",
                ed.texts()
            );
            let left: Vec<_> = std::fs::read_dir(journal.parent().unwrap())
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect();
            assert!(left.is_empty(), "{what}: left {left:?}");
        }
    }
}

/// 07 §3: a scripted run (`SOPEN_AUTOSAVE`) keeps no journal when its Save landed, and makes
/// the one backup link (07 §5.3). Its result line, which names the link, goes to stdout: the
/// live receipt reads it; the window's words are asserted in
/// `the_first_remote_save_links_the_original_before_it_uploads`.
#[test]
fn an_autosave_keeps_no_journal_and_leaves_one_link() {
    let Some(tools) = tools_or_skip() else { return };
    for side in sides() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.wav");
        tones(&tools, &path, true);
        let abs = path.to_str().unwrap().to_string();
        let mut fs = conn(side);
        let mut at = fs.stat(&abs).unwrap();
        at.sha256 = Some(journal::sha256(&std::fs::read(&path).unwrap()));
        let origin = Origin::new(fs, None, Some(HOST.into()), abs, at);
        let hooks = Hooks {
            autosave: true,
            state: crate::gui::State::parse("clips=2-4,0-1").unwrap(),
            ..Hooks::default()
        };
        let cache = tmp.path().join("cache");
        let copy = tmp.path().join("copy.wav");
        std::fs::copy(&path, &copy).unwrap();
        let outcome = open(origin, &copy, &cache, &hooks, Vec::new()).unwrap();
        assert!(outcome.saved, "{side:?}");
        assert_eq!(order(&path), [1320, 1760, 440]);
        let edits = walk(&cache.join(".edits"));
        assert!(
            edits
                .iter()
                .all(|p| !p.to_string_lossy().contains(".journal-")),
            "{side:?}: {edits:?}"
        );
        let links: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".a.wav.sopen-backup-")
            })
            .collect();
        assert_eq!(links.len(), 1, "{side:?}");
    }
}

/// E16, 07 §10: closing with unsaved edits asks Save · Save As… · Discard · Cancel, Discard's
/// hover naming the kept edits; Save As… opens the modal and the window closes only after a
/// Save As that landed — the edits then are somewhere, and the journal goes.
#[test]
fn the_close_prompt_offers_save_as_and_closes_after_it_lands() {
    for side in sides() {
        let Some(mut ed) = Ed::open(side, Read::Stream, Knobs::default()) else {
            return;
        };
        let folder = ed.tmp.path().join("out");
        std::fs::create_dir(&folder).unwrap();
        ed.win.save_as_folder = Some(folder.clone());
        ed.cut_second();
        ed.drv
            .until(&mut ed.win, 60, |w| w.schedule.status() == Keep::Kept);
        let journals = ed.journals();
        assert_eq!(journals.len(), 1);
        assert!(!crate::gui::Window::close_requested(&mut ed.win));
        // The prompt comes once the newest edits are in the journal (a generation on a worker).
        ed.drv.until(&mut ed.win, 60, |w| w.prompt);
        ed.drv.idle(&mut ed.win, 2);
        let last = |ed: &Ed, label: &str| {
            ed.drv
                .texts()
                .iter()
                .filter(|(_, _, t)| t == label)
                .map(|(r, _, _)| r.center())
                .next_back()
        };
        for button in ["Save", "Save As…", "Discard", "Cancel"] {
            assert!(last(&ed, button).is_some(), "{side:?}: {button}");
        }
        assert!(ed
            .drv
            .find_text(&format!(
                "Save the changes to {HOST}:{} before closing?",
                ed.remote.display()
            ))
            .is_some());
        // Cancel keeps the window and the edits.
        let cancel = last(&ed, "Cancel").unwrap();
        ed.drv.click(&mut ed.win, cancel);
        assert!(!ed.win.prompt && !ed.win.close_ok && ed.win.unsaved());
        // Save As…: the modal; the window closes once it has saved.
        assert!(!crate::gui::Window::close_requested(&mut ed.win));
        // The prompt comes once the newest edits are in the journal (a generation on a worker).
        ed.drv.until(&mut ed.win, 60, |w| w.prompt);
        ed.drv.idle(&mut ed.win, 2);
        let save_as = last(&ed, "Save As…").unwrap();
        ed.drv.click(&mut ed.win, save_as);
        assert!(ed.win.save_as.is_some(), "{side:?}: the modal");
        ed.drv.idle(&mut ed.win, 2);
        ed.drv.key(&mut ed.win, Key::Enter, Modifiers::NONE);
        ed.drv.until(&mut ed.win, 60, |w| w.save_as.is_none());
        ed.drv.idle(&mut ed.win, 2);
        let saved = folder.join("r.edited.wav");
        assert_eq!(
            order(&saved),
            [440, 1320, 1760],
            "{side:?}: the edit, in a new file"
        );
        assert!(ed.win.close_ok, "{side:?}: closes after the Save As landed");
        assert!(ed.log.lines().is_empty(), "{:?}", ed.log.lines());
        // The edits are somewhere: the journal goes with the window.
        let Ed {
            win,
            cache,
            remote,
            tmp,
            ..
        } = ed;
        drop(win);
        assert!(journals_of(&cache, &remote).is_empty(), "{side:?}");
        drop(tmp);
    }
    // Discard removes the kept edits.
    for side in sides() {
        let Some(mut ed) = Ed::open(side, Read::Stream, Knobs::default()) else {
            return;
        };
        ed.cut_second();
        ed.drv
            .until(&mut ed.win, 60, |w| w.schedule.status() == Keep::Kept);
        let dir = ed.journals().pop().unwrap();
        assert!(!crate::gui::Window::close_requested(&mut ed.win));
        // The prompt comes once the newest edits are in the journal (a generation on a worker).
        ed.drv.until(&mut ed.win, 60, |w| w.prompt);
        ed.drv.idle(&mut ed.win, 2);
        ed.drv.click_text(&mut ed.win, "Discard");
        assert!(ed.win.close_ok && ed.win.discard, "{side:?}");
        let Ed {
            win,
            cache,
            remote,
            tmp,
            ..
        } = ed;
        drop(win);
        assert!(journals_of(&cache, &remote).is_empty(), "{side:?}");
        assert!(!dir.exists());
        drop(tmp);
    }
}

/// 07 §7: F7 is in the media window's row and its help line, and opens Save As; Alt+⌘S does
/// too, and is not taken for ⌘S. Save As of a streamed file renders the edit into a new local
/// file and sends nothing to the remote.
#[test]
fn f7_and_alt_command_s_open_save_as_and_save_as_writes_a_local_file() {
    let page = crate::gui::help::for_window(&crate::gui::chrome::merged(
        &[crate::gui::chrome::HELP_KEY, crate::gui::chrome::EXIT_KEY],
        crate::gui::chrome::MEDIA_KEYS,
    ));
    assert!(
        page.lines
            .iter()
            .any(|l| l.keys == "F7" && l.what.starts_with("Save As")),
        "{:?}",
        page.lines
    );
    for side in sides() {
        let Some(mut ed) = Ed::open(side, Read::Stream, Knobs::default()) else {
            return;
        };
        let folder = ed.tmp.path().join("out");
        own_folder(&folder);
        ed.win.save_as_folder = Some(folder.clone());
        ed.drv.as_shell();
        ed.drv.idle(&mut ed.win, 1);
        assert!(ed.drv.find_text("SaveAs ").is_some(), "the row's slot");
        ed.cut_second();
        let alt_cmd = Modifiers {
            alt: true,
            ..command(false)
        };
        ed.drv.key(&mut ed.win, Key::S, alt_cmd);
        assert!(ed.win.save_as.is_some(), "Alt+⌘S opens Save As");
        assert!(ed.win.save.is_none(), "and is not ⌘S");
        ed.drv.key(&mut ed.win, Key::Escape, Modifiers::NONE);
        assert!(ed.win.save_as.is_none());
        ed.drv.key(&mut ed.win, Key::F7, Modifiers::NONE);
        assert!(ed.win.save_as.is_some(), "F7 opens Save As");
        ed.drv.key(&mut ed.win, Key::Enter, Modifiers::NONE);
        ed.drv.until(&mut ed.win, 60, |w| w.save_as.is_none());
        let saved = folder.join("r.edited.wav");
        assert_eq!(order(&saved), [440, 1320, 1760], "{side:?}");
        // Rendered once in sopen's exports folder (none naming a path in the chosen folder),
        // copied in, and the render gone after.
        assert!(ffmpeg::exits::naming(folder.to_str().unwrap()).is_empty());
        let runs = ffmpeg::exits::naming(ed.win.work_root.to_str().unwrap());
        assert_eq!(runs.len(), 1, "{side:?}: {runs:?}");
        assert!(runs[0].success());
        assert!(
            walk(&ed.win.work_root).is_empty(),
            "{:?}",
            walk(&ed.win.work_root)
        );
        assert!(ed.log.lines().is_empty(), "{side:?}: {:?}", ed.log.lines());
        assert!(!ed.win.unsaved_nowhere(), "the edits are somewhere");
        let _ = ed.side;
    }
}

/// 07 §9, E15: a source that changes during the render gets its responses cut by the byte
/// server; ffmpeg may still exit 0, so the cut account fails the render. Nothing is linked or
/// uploaded, the journal keeps the edits, and the window says why. The Source's engine is
/// stepped on a thread of the test's, polling its file every 50 ms: once the render has read a
/// good part of its input, the engine is held until ffmpeg's connection waits for a block, the
/// file is changed in place, and the engine goes on — a handshake, no clock.
#[test]
fn a_stream_changed_during_the_render_uploads_nothing_and_keeps_the_journal() {
    let Some(tools) = tools_or_skip() else { return };
    const MIB: u64 = 1 << 20;
    for side in sides() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("remote");
        std::fs::create_dir(&dir).unwrap();
        let remote = dir.join("r.wav");
        // 600 s: more than the stream's cache holds, so the render reads from the remote.
        super::edit_tests::ff(
            &tools,
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=330:sample_rate=48000:duration=600",
                "-ac",
                "2",
                remote.to_str().unwrap(),
            ],
        );
        let abs = remote.to_str().unwrap().to_string();
        let shown = format!("{HOST}:{abs}");
        let transport = conn(side).into_ranged(&abs).map_err(|r| r.error).unwrap();
        let at = transport.opened();
        let settings = crate::stream::Settings {
            poll_every: Duration::from_millis(50),
            ..crate::stream::Settings::plain()
        };
        let opening = crate::stream::Opening {
            label: shown.clone(),
            began: Instant::now(),
            sniff: 0,
        };
        let (source, mut engine) =
            Source::build(transport, no_reopen(), Arc::new(|| {}), opening, settings);
        let (quit, hold) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        // The engine holds itself once the render has been sent `mark` bytes: it cannot run
        // past it between the test's looks.
        let watched: Arc<Mutex<Option<Arc<ffmpeg::Served>>>> = Arc::default();
        let mark = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
        let engine = {
            let (quit, hold, watched, mark) =
                (quit.clone(), hold.clone(), watched.clone(), mark.clone());
            std::thread::spawn(move || {
                while !quit.load(SeqCst) {
                    let past = watched
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|s| s.server().sent(Kind::Render) >= mark.load(SeqCst));
                    if past {
                        hold.store(true, SeqCst);
                    }
                    if hold.load(SeqCst) || !engine.step(Some(Duration::from_millis(5))) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            })
        };
        let media = Media::stream(source.clone(), "r.wav", &shown).unwrap();
        let log = Log::default();
        let logged = Logged {
            inner: conn(side),
            log: log.clone(),
            knobs: Knobs::default(),
            blind: None,
        };
        let origin = Origin::new(Box::new(logged), None, Some(HOST.into()), abs, at);
        let cache = tmp.path().join("cache");
        let mut win = MediaWindow::new(
            media,
            MediaFile::new(origin, &cache),
            crate::cache::exports_root(&cache),
            Marks::default(),
            Selection::default(),
            Vec::new(),
        );
        win.sink = Sink::Null;
        win.unplanned = true;
        let mut ed = Ed {
            win,
            drv: Driver::new(900.0, 520.0),
            remote,
            log,
            knobs: Knobs::default(),
            served: None,
            cache,
            side,
            tmp,
        };
        ed.quiet();
        ed.cut_second();
        ed.drv.until(&mut ed.win, 120, |w| w.evidence_known());
        let served = ed.win.media.input.served().cloned().expect("a stream");
        mark.store(served.server().sent(Kind::Render) + 8 * MIB, SeqCst);
        *watched.lock().unwrap() = Some(served.clone());
        ed.drv.key(&mut ed.win, Key::S, command(false));
        // A good part of the render's input has gone to ffmpeg, and the engine held itself.
        ed.drv
            .until(&mut ed.win, 120, |w| hold.load(SeqCst) || w.save.is_none());
        assert!(
            ed.win.save.is_some(),
            "{side:?}: the render ended first: {}",
            ed.texts()
        );
        ed.drv.until(&mut ed.win, 60, |w| {
            source.core().pf.waiting_readers() >= 1 || w.save.is_none()
        });
        assert!(ed.win.save.is_some(), "{side:?}: the render ended first");
        // A write in place, in another whole second (SFTP carries no more than that).
        use std::os::unix::fs::FileExt;
        let size = std::fs::metadata(&ed.remote).unwrap().len();
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&ed.remote)
            .unwrap();
        f.write_all_at(&[0xEE; 4096], size - 2 * MIB).unwrap();
        f.set_modified(at.mtime + Duration::from_secs(10)).unwrap();
        drop(f);
        let changed = std::fs::read(&ed.remote).unwrap();
        *watched.lock().unwrap() = None;
        hold.store(false, SeqCst);
        ed.wait_save();
        assert_eq!(
            ed.win.status.text,
            "save failed",
            "{side:?}: {}",
            ed.texts()
        );
        assert!(
            ed.texts().contains("the file changed on the remote")
                && ed.texts().contains("the render was stopped"),
            "{side:?}: {}",
            ed.texts()
        );
        assert!(
            ed.texts().contains("nothing was uploaded"),
            "{}",
            ed.texts()
        );
        assert!(ed.log.lines().is_empty(), "{side:?}: {:?}", ed.log.lines());
        assert_eq!(
            std::fs::read(&ed.remote).unwrap(),
            changed,
            "untouched by sopen"
        );
        assert!(ed.links().is_empty() && ed.temps().is_empty());
        let journals = ed.journals();
        assert_eq!(journals.len(), 1, "{side:?}: the journal stays");
        assert!(!journals[0].join("save").exists(), "no save was begun");
        assert!(ed.win.unsaved() && ed.win.editable(), "the edits stay");
        // The window says where they are kept: the Save's worker kept them first.
        ed.drv
            .until(&mut ed.win, 60, |w| w.schedule.status() == Keep::Kept);
        let kept = ed.win.kept_line().0;
        assert_eq!(
            kept,
            format!(
                "unsaved — kept at {}",
                crate::gui::binary::tilde(&journals[0])
            ),
            "{side:?}"
        );
        drop(ed);
        drop((served, source));
        quit.store(true, SeqCst);
        engine.join().unwrap();
    }
}

/// 07 §3.1, §5.3: a rename that had its reply landed, whatever fails after it (its `lstat`):
/// the Save is saved — never "not saved" — with what failed said; the link is the session's
/// backup and is never removed (it is the original's only name now), the journal is retired,
/// and the next Save makes no new link.
#[test]
fn a_rename_that_replied_but_cannot_be_checked_after_landed_and_keeps_the_link() {
    for side in sides() {
        let Some(mut ed) = Ed::open(side, Read::Copy, Knobs::default()) else {
            return;
        };
        ed.knobs.blind_after_rename.store(true, SeqCst);
        ed.cut_second();
        ed.save();
        assert!(
            ed.win.status.text.starts_with(&format!("saved {HOST}:")),
            "{side:?}: {}",
            ed.texts()
        );
        assert!(
            ed.texts()
                .contains("saved, but what followed the rename failed"),
            "{side:?}: {}",
            ed.texts()
        );
        let links = ed.links();
        assert_eq!(links.len(), 1, "{side:?}: the link is kept");
        assert!(
            !ed.log.verbs().contains(&"remove".to_string()),
            "{:?}",
            ed.log.lines()
        );
        assert!(!ed.win.unknown && !ed.win.unsaved() && ed.win.editable());
        assert!(ed.journals().is_empty(), "{side:?}: retired");
        assert_eq!(order(&ed.remote), [440, 1320, 1760]);
        ed.cut(1.0 / 3.0, 2.0 / 3.0);
        ed.save();
        assert!(ed.win.status.text.starts_with("saved "), "{}", ed.texts());
        assert_eq!(ed.links(), links, "{side:?}: no new link");
        assert_eq!(
            ed.log.verbs(),
            ["link", "create", "rename", "create", "rename"],
            "{:?}",
            ed.log.lines()
        );
    }
}

/// 07 §10: Esc drops a Save still waiting for the journal (a generation on its way holds it) —
/// nothing is sent — and stops one pressed in the middle of its upload before its rename: the
/// upload ends, the temp goes, the file is as it was, and the link goes too.
#[test]
fn esc_drops_a_queued_save_and_stops_one_mid_upload_before_its_rename() {
    for side in sides() {
        // Queued behind the journal.
        let Some(mut ed) = Ed::open(side, Read::Stream, Knobs::default()) else {
            return;
        };
        ed.cut_second();
        let journal = ed.win.journal.clone();
        let held = journal.lock().unwrap();
        ed.drv.key(&mut ed.win, Key::S, command(false));
        ed.drv.idle(&mut ed.win, 3);
        assert!(
            ed.drv.find_text("saving — keeping the edits…").is_some(),
            "{side:?}: the Save waits for the journal"
        );
        ed.drv.key(&mut ed.win, Key::Escape, Modifiers::NONE);
        drop(held);
        ed.wait_save();
        assert_eq!(
            ed.win.status.text, "save cancelled: nothing was sent",
            "{side:?}"
        );
        assert!(ed.log.lines().is_empty(), "{:?}", ed.log.lines());
        assert!(ed.win.unsaved() && ed.win.editable());

        // Mid-upload: one chunk in.
        let gate = Gate::default();
        let knobs = Knobs {
            hold_upload: Some(gate.clone()),
            ..Knobs::default()
        };
        let Some(mut ed) = Ed::open(side, Read::Stream, knobs) else {
            return;
        };
        let before = std::fs::read(&ed.remote).unwrap();
        ed.cut_second();
        ed.drv.key(&mut ed.win, Key::S, command(false));
        ed.drv.until(&mut ed.win, 60, |_| gate.reached());
        assert_eq!(ed.temps().len(), 1, "{side:?}: the upload is under way");
        ed.drv.key(&mut ed.win, Key::Escape, Modifiers::NONE);
        gate.release();
        ed.wait_save();
        assert!(
            ed.win
                .status
                .text
                .starts_with("save cancelled before the rename: the uploaded temporary file"),
            "{side:?}: {}",
            ed.texts()
        );
        assert_eq!(std::fs::read(&ed.remote).unwrap(), before);
        assert!(
            ed.temps().is_empty() && ed.links().is_empty(),
            "{:?}",
            ed.log.lines()
        );
        assert!(!ed.log.verbs().contains(&"rename".to_string()));
        assert!(ed.win.unsaved() && ed.win.editable());
    }
}

/// A journal an earlier session left in the middle of a Save — its record in phase `naming`, and
/// a file at the temp's name sopen cannot prove its own — on the next open.
fn left_in_naming(side: Side) -> Option<(Ed, PathBuf)> {
    let tools = tools_or_skip()?;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("remote");
    std::fs::create_dir(&dir).unwrap();
    let remote = dir.join("r.wav");
    tones(&tools, &remote, true);
    let abs = remote.to_str().unwrap().to_string();
    let media = Media::probe(&remote).unwrap();
    // The baseline as the side's server reports it (SFTP's times are whole seconds).
    let mut at = conn(side).stat(&abs).unwrap();
    let whole = journal::sha256(&std::fs::read(&remote).unwrap());
    at.sha256 = Some(whole);
    let edits = Edits {
        rate: media.rate(),
        source: media.ticks(),
        clips: vec![(0, media.ticks() / 4), (media.ticks() / 2, media.ticks())],
        stereo: false,
        crop: None,
        marks: (None, None),
    };
    let snap = journal::Snapshot {
        baseline: at.fingerprint(),
        seen: journal::Seen::whole(whole),
        edits: edits.text(),
        pastes: Vec::new(),
    };
    let cache = tmp.path().join("cache");
    let temp = dir.join(".r.wav.sopen-0123abcd");
    let mut journal =
        Journal::create(journal::disk(), &cache, HOST, &abs, journal::Kind::Media).unwrap();
    journal.publish(&snap).unwrap();
    journal
        .begin_save(
            &snap,
            journal::Plan::Temp {
                route: journal::Route::Upload,
                target: abs.clone(),
                temp: temp.to_str().unwrap().to_string(),
            },
        )
        .unwrap();
    // Killed there: the record stays, the lock is let go.
    drop(journal);
    std::fs::write(&temp, b"a file sopen cannot prove its own").unwrap();
    Some((
        Ed::on(side, Read::Copy, Knobs::default(), tmp, remote),
        temp,
    ))
}

/// 07 §3.1 and lesson 1: the question about an interrupted save's temp is bound to its record
/// and to the file as shown. Keep it ends the record and leaves the file; Remove it removes the
/// file the question was about; a file put there since the question — another size — is not
/// removed, and the question and the record stay. Restore waits for the answer.
#[test]
fn a_rows_question_removes_only_the_file_it_was_about() {
    for side in sides() {
        // Keep it.
        let Some((mut ed, temp)) = left_in_naming(side) else {
            return;
        };
        assert_eq!(ed.win.rows.len(), 1, "{side:?}");
        let asked = ed
            .drv
            .text_starting_with(&format!("{HOST}:{}: ", temp.display()))
            .expect("the question");
        assert!(
            asked.ends_with(
                "the create's reply was never recorded, so it may not be sopen's — remove it?"
            ),
            "{asked}"
        );
        ed.drv.click_text(&mut ed.win, "Restore");
        ed.drv.idle(&mut ed.win, 2);
        assert!(!ed.win.unsaved(), "{side:?}: Restore waits for the answer");
        ed.drv.click_text(&mut ed.win, "Keep it");
        ed.drv.until(&mut ed.win, 60, |w| w.answering.is_none());
        ed.drv.idle(&mut ed.win, 2);
        assert!(temp.exists(), "{side:?}: kept");
        let journal = ed.journals().pop().unwrap();
        assert!(!journal.join("save").exists(), "the record ended");
        assert!(ed
            .win
            .status
            .text
            .contains("it will not be asked about again"));
        ed.drv.click_text(&mut ed.win, "Restore");
        ed.drv
            .until(&mut ed.win, 60, |w| w.restoring.is_none() && w.unsaved());
        assert_eq!(ed.win.timeline.clips().len(), 2, "restored");

        // Remove it: the file as asked about goes.
        let Some((mut ed, temp)) = left_in_naming(side) else {
            return;
        };
        ed.drv.click_text(&mut ed.win, "Remove it");
        ed.drv.until(&mut ed.win, 60, |w| w.answering.is_none());
        assert!(!temp.exists(), "{side:?}: removed");
        assert!(ed.win.status.text.starts_with("removed "), "{}", ed.texts());
        let journal = ed.journals().pop().unwrap();
        assert!(!journal.join("save").exists(), "the record ended");

        // Another file under the name since the question: not removed, still asked.
        let Some((mut ed, temp)) = left_in_naming(side) else {
            return;
        };
        std::fs::write(
            &temp,
            b"another file, put there after the question was asked",
        )
        .unwrap();
        ed.drv.click_text(&mut ed.win, "Remove it");
        ed.drv.until(&mut ed.win, 60, |w| w.answering.is_none());
        ed.drv.idle(&mut ed.win, 2);
        assert!(temp.exists(), "{side:?}: not removed");
        assert!(
            ed.win
                .status
                .text
                .contains("is not the file the question was about"),
            "{side:?}: {}",
            ed.texts()
        );
        assert!(ed
            .drv
            .text_starting_with(&format!("{HOST}:{}: ", temp.display()))
            .is_some());
        let journal = ed.journals().pop().unwrap();
        assert!(journal.join("save").exists(), "the record stays");
    }
}

/// 07 §3, §4.5, §9 above 256 MiB: the baseline is the first and last MiB (no whole hash), and a
/// same-size, same-time change in the first MiB is refused before anything is rendered; a
/// Save that lands says what it checked; a change of the first MiB between the upload and the
/// rename is refused right before the rename, the temp removed.
#[test]
fn above_256_mib_the_first_and_last_mib_are_checked() {
    let Some(tools) = tools_or_skip() else { return };
    // 1420 s of 48 kHz stereo 16-bit: about 260 MiB, made once; each case takes a copy (a clone
    // of it where the file system makes one). Each edit keeps its first 17 s: the source is what
    // is above 256 MiB, the render stays small.
    let master = tempfile::tempdir().unwrap();
    let made = master.path().join("r.wav");
    super::edit_tests::ff(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1420",
            "-ac",
            "2",
            made.to_str().unwrap(),
        ],
    );
    assert!(std::fs::metadata(&made).unwrap().len() > crate::remote::HASH_CHECK_MAX);
    let make = |dir: &Path| -> PathBuf {
        let remote = dir.join("r.wav");
        std::fs::copy(&made, &remote).unwrap();
        remote
    };
    let poke_first_mib = |remote: &Path| {
        let meta = std::fs::metadata(remote).unwrap();
        use std::os::unix::fs::FileExt;
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(remote)
            .unwrap();
        f.write_all_at(&[0x5A; 16], 4096).unwrap();
        f.set_modified(meta.modified().unwrap()).unwrap();
    };
    for side in sides() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("remote");
        std::fs::create_dir(&dir).unwrap();
        let remote = make(&dir);
        let mut ed = Ed::on_quiet(side, Knobs::default(), tmp, remote);
        ed.cut(0.012, 1.0);
        ed.drv.until(&mut ed.win, 60, |w| w.evidence_known());
        assert!(
            matches!(
                ed.win.pass.as_ref().unwrap().ended(),
                Some(Ok(file::Evidence::Ends(_)))
            ),
            "{side:?}: the first and last MiB"
        );
        poke_first_mib(&ed.remote);
        ed.save();
        assert_eq!(
            ed.win.status.text,
            "not saved: the file changed",
            "{}",
            ed.texts()
        );
        assert!(
            ed.texts()
                .contains("its first or last MiB changed (same size and modification time)"),
            "{side:?}: {}",
            ed.texts()
        );
        assert!(ed.log.lines().is_empty(), "{:?}", ed.log.lines());
    }
    // One whose first MiB changes after its link, before its upload, is refused right before
    // the overwrite (the link goes, nothing is uploaded); one opened again on the file as it is
    // now lands and says what it checked.
    for side in sides() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("remote");
        std::fs::create_dir(&dir).unwrap();
        let remote = make(&dir);
        let mut ed = Ed::on_quiet(side, Knobs::default(), tmp, remote);
        let gate = Gate::default();
        ed.win.file.lock().unwrap().step_hook = Some(gate.hook("linked"));
        ed.cut(0.012, 1.0);
        ed.drv.key(&mut ed.win, Key::S, command(false));
        ed.drv.until(&mut ed.win, 300, |_| gate.reached());
        poke_first_mib(&ed.remote);
        gate.release();
        ed.wait_save();
        assert_eq!(
            ed.win.status.text,
            "not saved: the file changed",
            "{side:?}: {}",
            ed.texts()
        );
        assert!(
            ed.texts().contains("its first or last MiB changed"),
            "{}",
            ed.texts()
        );
        assert_eq!(
            ed.log.verbs(),
            ["link", "remove"],
            "{side:?}: {:?}",
            ed.log.lines()
        );
        assert!(ed.temps().is_empty() && ed.links().is_empty());
        ed.win.file.lock().unwrap().step_hook = None;
        let Ed { tmp, remote, .. } = ed;
        let mut ed = Ed::on_quiet(side, Knobs::default(), tmp, remote);
        ed.cut(0.012, 1.0);
        ed.save();
        assert!(
            ed.win.status.text.starts_with("saved "),
            "{side:?}: {}",
            ed.texts()
        );
        assert!(
            ed.win.notices.contains(&journal::NOTE_ENDS.to_string()),
            "{:?}",
            ed.win.notices
        );
        assert_eq!(ed.log.verbs(), ["link", "create", "rename"]);
        // Above 256 MiB the overwrite reads nothing whole: the first and last MiB are read by
        // `read_at`, not `read_to`.
        assert_eq!(ed.log.read(), 0, "{side:?}");
    }
}

/// 07 §3.1 and lesson 3 on open: a Save whose rename landed with its reply lost, then a window
/// ended as a kill ends it — the next open reconciles the record from fresh reads: it landed,
/// so the journal is retired, the row says `landed (found on reopen)`, nothing is offered and
/// nothing is sent.
#[test]
fn a_lost_reply_is_found_landed_on_reopen() {
    for side in sides() {
        let Some(mut ed) = Ed::open(side, Read::Copy, Knobs::default()) else {
            return;
        };
        ed.knobs.lose_rename.store(true, SeqCst);
        ed.cut_second();
        ed.save();
        assert_eq!(
            ed.win.status.text,
            "save outcome unknown",
            "{side:?}: {}",
            ed.texts()
        );
        assert!(
            ed.journals()[0].join("save").exists(),
            "{side:?}: its record"
        );
        let Ed {
            win, tmp, remote, ..
        } = ed;
        win.crash();
        let again = Ed::on(side, Read::Copy, Knobs::default(), tmp, remote);
        let notes: Vec<&str> = again
            .win
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Note(line) => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(again.win.rows.len(), 1, "{side:?}: {notes:?}");
        assert!(
            notes.len() == 1 && notes[0].contains("landed (found on reopen)"),
            "{side:?}: {notes:?}"
        );
        assert!(
            again.journals().is_empty(),
            "{side:?}: {:?}",
            again.journals()
        );
        assert!(again.log.lines().is_empty(), "{:?}", again.log.lines());
        assert_eq!(order(&again.remote), [440, 1320, 1760]);
    }
}

/// 07 §7: a final render's container is said explicitly when its file's name does not say it:
/// `-f <muxer>`, with the encoders and `+faststart` of that container; a named output keeps its
/// own, without `-f`.
#[test]
fn a_render_into_a_file_without_an_extension_names_its_container() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("v.mp4");
    super::fixtures::clip(&tools, &src, (64, 48));
    let mut media = Media::probe(&src).unwrap();
    media.plan();
    let (kind, ext, plan) = media.plan.clone().unwrap();
    assert_eq!(ext, "mp4");
    let spec = edit_spec(
        &media,
        &Timeline::new(media.rate(), media.ticks()),
        (0, 25),
        None,
    );
    let input = media.input.clone();
    let words = |args: &ffmpeg::EditArgs| -> Vec<String> {
        args.outputs
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    let part = tmp.path().join("part");
    let args = ffmpeg::edit_args_as(
        &[&input],
        &part,
        Some(&ext),
        &spec,
        kind,
        &plan,
        ffmpeg::Encode::Final,
    )
    .unwrap();
    let out = words(&args);
    let n = out.len();
    assert_eq!(
        &out[n - 3..],
        ["-f", "mp4", part.to_str().unwrap()],
        "{out:?}"
    );
    assert!(
        out.windows(2).any(|w| w == ["-movflags", "+faststart"]),
        "{out:?}"
    );
    let named = ffmpeg::edit_args(
        &[&input],
        &tmp.path().join("o.mp4"),
        &spec,
        kind,
        &plan,
        ffmpeg::Encode::Final,
    )
    .unwrap();
    assert!(
        !words(&named).contains(&"-f".to_string()),
        "{:?}",
        words(&named)
    );
    for (ext, muxer) in [
        ("m4a", "ipod"),
        ("wav", "wav"),
        ("mkv", "matroska"),
        ("aac", "adts"),
    ] {
        assert_eq!(ffmpeg::muxer_for(ext), Some(muxer));
    }
    assert_eq!(ffmpeg::muxer_for("txt"), None);
}

/// 07 §7: Save As of a video renders in sopen's exports folder — one ffmpeg run, none naming a
/// path in the chosen folder — and copies the render into the staging `part`: the saved file is
/// the render's bytes (a second of the 2 s clip, video and sound), and the render is gone after.
#[test]
fn save_as_of_a_video_renders_in_sopens_folder_and_copies_it_in() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("out");
    folder_with(&folder, 0o700);
    let exports = tmp.path().join("exports");
    let decision = super::super::binary::Decision::new();
    let (outcome, (out, rendered)) =
        save_half_as(&tools, tmp.path(), &folder, &decision, Box::new(|_| {}));
    let dest = folder.join("v.edited.mp4");
    match outcome {
        save_as::Outcome::Saved { path, .. } => assert_eq!(path, dest),
        other => panic!("{}", other.line()),
    }
    assert!(out.starts_with(&exports), "{}", out.display());
    let runs = ffmpeg::exits::naming(out.to_str().unwrap());
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(runs[0].success());
    assert!(ffmpeg::exits::naming(folder.to_str().unwrap()).is_empty());
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        rendered,
        "the render's bytes"
    );
    assert!(walk(&exports).is_empty(), "{:?}", walk(&exports));
    assert_half(&tools, &dest, "saved");
}

/// A folder of this user's only (0700, whatever the umask).
fn own_folder(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// The journal's current generation's edits in `dir` (read off the disk).
fn kept_edits(dir: &Path) -> String {
    let at = journal::parse_current(&std::fs::read(dir.join("CURRENT")).unwrap()).unwrap();
    let gen = std::fs::read(dir.join(format!("gen-{}", at.n))).unwrap();
    journal::parse_generation(&gen).unwrap().edits
}

/// The journal's disk, whose writes can be held (a slow or stalled disk under the journal) until
/// the test lets them go; it says when one is held.
struct SlowDisk {
    held: Arc<AtomicBool>,
    waiting: Arc<AtomicBool>,
}

impl journal::Fs for SlowDisk {
    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        journal::Disk.create_dir(path)
    }
    fn write_new(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        while self.held.load(SeqCst) {
            self.waiting.store(true, SeqCst);
            std::thread::sleep(Duration::from_millis(2));
        }
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

/// 07 §3, lesson 2: closing writes the newest edits at once, on a worker — even while another
/// worker holds the file (a recovery listing, an answer) — and the prompt asks meanwhile. Any
/// path out but Discard waits for a generation on its way: after a Save As from the prompt
/// lands while a write is held, the window stays, saying `keeping the edits…`, and goes once
/// the write is done — no frame waiting. Discard goes at once; the journal is removed once the
/// held write ends, off the window. Dropped without closing, the window still writes the
/// newest edits.
#[test]
fn closing_writes_the_newest_edits_first_and_goes_only_once_they_are_on_disk() {
    for side in sides() {
        // The prompt at once; the newest edits on disk while a worker holds the file.
        let Some(mut ed) = Ed::open(side, Read::Copy, Knobs::default()) else {
            return;
        };
        let file = ed.win.file.clone();
        let held = file.lock().unwrap();
        ed.cut_second();
        assert!(!crate::gui::Window::close_requested(&mut ed.win));
        assert!(ed.win.prompt, "{side:?}: the prompt asks at once");
        ed.drv.until(&mut ed.win, 60, |w| {
            w.writing.is_none() && !w.write_soon && w.journal_dir.is_some()
        });
        let dir = ed.journals().pop().expect("a journal");
        assert_eq!(
            kept_edits(&dir),
            ed.win.edits().text(),
            "{side:?}: the newest edits"
        );
        drop(held);

        // At once: an edit, then closing, with no pass between — the close starts the newest
        // generation itself, never leaving it to the 250 ms schedule.
        let Some(mut ed) = Ed::open(side, Read::Copy, Knobs::default()) else {
            return;
        };
        ed.win.marks.in_s = Some(1.0);
        ed.win.marks.out_s = Some(2.0);
        let ctx = ed.drv.ctx.clone();
        ed.win.remove(&ctx, false);
        assert!(ed.win.unsaved());
        assert!(!crate::gui::Window::close_requested(&mut ed.win));
        assert!(
            ed.win.write_soon || ed.win.writing.is_some(),
            "{side:?}: closing writes at once"
        );
        drop(ed);

        // A Save As from the prompt lands while the write is held: the window stays until it
        // is done.
        let Some(mut ed) = Ed::open(side, Read::Copy, Knobs::default()) else {
            return;
        };
        let (hold, waiting) = (
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(false)),
        );
        ed.win.journal_fs = Arc::new(SlowDisk {
            held: hold.clone(),
            waiting: waiting.clone(),
        });
        let folder = ed.tmp.path().join("out");
        std::fs::create_dir(&folder).unwrap();
        ed.win.save_as_folder = Some(folder.clone());
        ed.drv.as_shell();
        ed.cut_second();
        assert!(!crate::gui::Window::close_requested(&mut ed.win));
        ed.drv.until(&mut ed.win, 60, |_| waiting.load(SeqCst));
        ed.drv.idle(&mut ed.win, 2);
        let save_as = ed
            .drv
            .texts()
            .iter()
            .filter(|(_, _, t)| t == "Save As…")
            .map(|(r, _, _)| r.center())
            .next_back()
            .unwrap();
        ed.drv.click(&mut ed.win, save_as);
        ed.drv.idle(&mut ed.win, 2);
        ed.drv.key(&mut ed.win, Key::Enter, Modifiers::NONE);
        ed.drv.until(&mut ed.win, 120, |w| w.save_as.is_none());
        ed.drv.idle(&mut ed.win, 3);
        assert!(
            order(&folder.join("r.edited.wav")).len() == 3,
            "{side:?}: saved as"
        );
        assert!(
            !ed.drv.window_closed,
            "{side:?}: it stays while the write is held"
        );
        assert_eq!(ed.win.status.text, "keeping the edits…");
        hold.store(false, SeqCst);
        let give_up = Instant::now() + Duration::from_secs(60);
        while !ed.drv.window_closed {
            assert!(Instant::now() < give_up, "{side:?}: the window never went");
            ed.drv.idle(&mut ed.win, 1);
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(ed);

        // Discard while the write is held: the window goes at once; the journal goes once the
        // write is done.
        let Some(mut ed) = Ed::open(side, Read::Copy, Knobs::default()) else {
            return;
        };
        let (hold, waiting) = (
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(false)),
        );
        ed.win.journal_fs = Arc::new(SlowDisk {
            held: hold.clone(),
            waiting: waiting.clone(),
        });
        ed.drv.as_shell();
        ed.cut_second();
        assert!(!crate::gui::Window::close_requested(&mut ed.win));
        ed.drv.until(&mut ed.win, 60, |_| waiting.load(SeqCst));
        ed.drv.idle(&mut ed.win, 2);
        ed.drv.click_text(&mut ed.win, "Discard");
        ed.drv.idle(&mut ed.win, 2);
        assert!(ed.drv.window_closed, "{side:?}: Discard goes at once");
        let Ed {
            win,
            cache,
            remote,
            tmp,
            ..
        } = ed;
        drop(win);
        hold.store(false, SeqCst);
        let give_up = Instant::now() + Duration::from_secs(60);
        while !journals_of(&cache, &remote).is_empty() {
            assert!(
                Instant::now() < give_up,
                "{side:?}: the discarded journal stayed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(tmp);

        // Dropped at once after an edit, with the file held: written on the way out.
        let Some(mut ed) = Ed::open(side, Read::Copy, Knobs::default()) else {
            return;
        };
        let file = ed.win.file.clone();
        let held = file.lock().unwrap();
        ed.cut_second();
        let edits = ed.win.edits().text();
        let Ed {
            win,
            cache,
            remote,
            tmp,
            ..
        } = ed;
        drop(win);
        drop(held);
        let dirs = journals_of(&cache, &remote);
        assert_eq!(dirs.len(), 1, "{side:?}");
        assert_eq!(kept_edits(&dirs[0]), edits, "{side:?}");
        drop(tmp);
    }
}

// ---------------------------------------------------------------------------------------------
// An earlier Save whose outcome cannot be told; the crop on its own; Restore beside a write of
// the window's own; Save As into a folder others can rename in.

/// A Save whose rename landed with its reply lost, then the file at its name swapped for other
/// bytes, the landed file kept aside: the retry cannot tell how that Save went. Also returned:
/// the file's bytes and modification time before that Save.
fn cut_off_and_swapped(side: Side) -> Option<(Ed, PathBuf, (Vec<u8>, std::time::SystemTime))> {
    let mut ed = Ed::open(side, Read::Copy, Knobs::default())?;
    let before = (
        std::fs::read(&ed.remote).unwrap(),
        std::fs::metadata(&ed.remote).unwrap().modified().unwrap(),
    );
    ed.knobs.lose_rename.store(true, SeqCst);
    ed.cut_second();
    ed.save();
    assert_eq!(
        ed.win.status.text,
        "save outcome unknown",
        "{side:?}: {}",
        ed.texts()
    );
    let aside = ed.tmp.path().join("landed");
    std::fs::rename(&ed.remote, &aside).unwrap();
    std::fs::write(
        &ed.remote,
        b"neither the file before that save nor its render",
    )
    .unwrap();
    let sent = ed.log.lines().len();
    ed.save_briefly();
    assert!(
        ed.texts().contains("a save was cut off"),
        "{side:?}: the retry ran: {}",
        ed.texts()
    );
    assert_eq!(
        ed.log.lines().len(),
        sent,
        "{side:?}: nothing sent: {:?}",
        ed.log.lines()
    );
    Some((ed, aside, before))
}

/// 07 §3.1: a retry that finds an earlier Save of the session neither landed nor not landed
/// says the outcome is unknown and names what to check — never "not saved": the file may hold
/// that Save's render already. Its record stays.
#[test]
fn a_retry_that_cannot_tell_says_the_outcome_is_unknown() {
    for side in sides() {
        let Some((ed, ..)) = cut_off_and_swapped(side) else {
            return;
        };
        let abs = ed.remote.to_str().unwrap();
        assert!(
            !ed.texts().contains("not saved"),
            "{side:?}: {}",
            ed.texts()
        );
        assert_eq!(
            ed.win.status.text,
            "save outcome unknown",
            "{side:?}: {}",
            ed.texts()
        );
        let line = ed
            .win
            .notices
            .iter()
            .find(|n| n.contains("a save was cut off"))
            .unwrap_or_else(|| panic!("{side:?}: {}", ed.texts()));
        assert!(
            line.starts_with("save outcome unknown: a save was cut off; check "),
            "{side:?}: {line}"
        );
        assert!(line.contains(&format!("{HOST}:{abs} and ")), "{line}");
        let journals = ed.journals();
        assert_eq!(journals.len(), 1, "{side:?}");
        assert!(
            journals[0].join("save").exists(),
            "{side:?}: its record stays"
        );
    }
}

/// 07 §3.1: while an earlier Save's outcome cannot be told, its record holds the generation it
/// rendered and no edit is made. The Save that then finds it landed retires that generation
/// only — nothing newer was made; the one that finds the file as it was before renders exactly
/// that generation.
#[test]
fn no_edit_is_made_while_an_earlier_saves_outcome_is_unknown() {
    for side in sides() {
        for landed in [true, false] {
            let Some((mut ed, aside, (bytes, mtime))) = cut_off_and_swapped(side) else {
                return;
            };
            let what = format!("{side:?}, landed {landed}");
            let journal = ed.journals().pop().expect("its journal");
            let kept = kept_edits(&journal);
            assert_eq!(kept, ed.win.edits().text(), "{what}");
            // An edit tried now — a cut of the first piece — is refused.
            let clips = ed.win.timeline.clips().to_vec();
            let (a, b) = (ed.at(0.02), ed.at(0.2));
            ed.drv.drag(&mut ed.win, PointerButton::Primary, a, b);
            ed.drv.key(&mut ed.win, Key::Delete, Modifiers::NONE);
            ed.drv.idle(&mut ed.win, 3);
            assert_eq!(
                ed.win.timeline.clips(),
                &clips[..],
                "{what}: no edit while the outcome is unknown"
            );
            assert_eq!(ed.win.status.text, "save outcome unknown", "{what}");
            assert!(
                ed.texts()
                    .contains("save outcome unknown: a save was cut off; check "),
                "{what}: what to check stays said: {}",
                ed.texts()
            );
            assert_eq!(
                kept_edits(&journal),
                kept,
                "{what}: the record's generation"
            );
            if landed {
                // The landed file back under its name: found landed, nothing sent.
                std::fs::rename(&aside, &ed.remote).unwrap();
                let sent = ed.log.lines().len();
                ed.save_briefly();
                assert!(
                    ed.win
                        .status
                        .text
                        .contains("landed (found now): nothing was written again"),
                    "{what}: {}",
                    ed.texts()
                );
                assert_eq!(ed.log.lines().len(), sent, "{what}: {:?}", ed.log.lines());
            } else {
                // The file as it was before that Save: not landed; the Save renders the record's
                // generation.
                std::fs::write(&ed.remote, &bytes).unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(&ed.remote)
                    .unwrap()
                    .set_modified(mtime)
                    .unwrap();
                ed.save();
                assert!(
                    ed.win.status.text.starts_with("saved "),
                    "{what}: {}",
                    ed.texts()
                );
            }
            assert_eq!(order(&ed.remote), [440, 1320, 1760], "{what}");
            assert!(!ed.win.unsaved() && !ed.win.unknown, "{what}");
            assert!(ed.journals().is_empty(), "{what}: {:?}", ed.journals());
        }
    }
}

/// A video window (a 2 s clip, 64×48) on a "remote" file of a fresh folder.
fn video(side: Side) -> Option<Ed> {
    let tools = tools_or_skip()?;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("remote");
    std::fs::create_dir(&dir).unwrap();
    let remote = dir.join("v.mp4");
    super::fixtures::clip(&tools, &remote, (64, 48));
    Some(Ed::on_video(side, tmp, remote))
}

impl Ed {
    /// The window on a video `remote`, settled: planned, its picture on screen and what an
    /// earlier session left listed.
    fn on_video(side: Side, tmp: tempfile::TempDir, remote: PathBuf) -> Ed {
        let mut ed = Ed::unsettled(side, Read::Copy, Knobs::default(), tmp, remote);
        ed.drv.until(&mut ed.win, 120, |w| {
            w.map.is_some()
                && w.timeline_rect.is_some()
                && w.planning.is_none()
                && !w.unplanned
                && w.recovering.is_none()
                && w.recovery_started
        });
        ed.drv.idle(&mut ed.win, 2);
        ed
    }
}

/// 07 §2: the crop is one of the media window's edits the journal keeps, on its own too: a
/// rectangle drawn on an unedited timeline is kept, a window killed then offers it on the next
/// open, and Restore draws it again.
#[test]
fn a_crop_on_its_own_is_kept_and_restored() {
    for side in sides() {
        let Some(mut ed) = video(side) else {
            return;
        };
        let disp = ed.win.map.as_ref().unwrap().disp;
        let (a, b) = (
            disp.lerp_inside(egui::vec2(0.2, 0.2)),
            disp.lerp_inside(egui::vec2(0.7, 0.8)),
        );
        ed.drv.drag(&mut ed.win, PointerButton::Primary, a, b);
        let crop = ed.win.crop().expect("a rectangle drawn");
        assert!(ed.win.timeline.is_identity(), "{side:?}: the crop alone");
        ed.drv.idle(&mut ed.win, 3);
        assert!(
            ed.win.journal_wants(),
            "{side:?}: a crop alone is an edit to keep"
        );
        ed.drv.until(&mut ed.win, 60, |w| {
            w.journal_text.1 && w.schedule.status() == Keep::Kept
        });
        assert_eq!(ed.journals().len(), 1, "{side:?}");
        let Ed {
            win, tmp, remote, ..
        } = ed;
        win.crash();
        let mut again = Ed::on_video(side, tmp, remote);
        assert_eq!(again.win.rows.len(), 1, "{side:?}: offered");
        assert_eq!(again.journals().len(), 1, "{side:?}: still on disk");
        again.drv.click_text(&mut again.win, "Restore");
        again
            .drv
            .until(&mut again.win, 60, |w| w.restoring.is_none());
        assert_eq!(again.win.crop(), Some(crop), "{side:?}: {}", again.texts());
        // Closing asks about it: Discard or Cancel (Save and Save As have nothing to write).
        assert!(!crate::gui::Window::close_requested(&mut again.win));
        again.drv.idle(&mut again.win, 2);
        assert!(again.win.prompt, "{side:?}: asked");
        assert!(
            again
                .drv
                .text_starting_with("Discard the crop drawn on ")
                .is_some(),
            "{side:?}: the prompt says what it is about"
        );
        assert!(again.drv.find_text("Cancel").is_some());
        again.drv.click_text(&mut again.win, "Discard");
        again.drv.idle(&mut again.win, 2);
        assert!(again.win.crop().is_none(), "{side:?}");
        assert!(crate::gui::Window::close_requested(&mut again.win));
    }
}

/// 07 §2: an edit list with a crop on an otherwise whole timeline is an edit: a journal of it
/// is not one with nothing in it (which the next open removes).
#[test]
fn an_edit_list_with_only_a_crop_is_not_the_file_as_it_is() {
    let whole = file::Edits {
        rate: 100.0,
        source: 400,
        clips: vec![(0, 400)],
        stereo: false,
        crop: None,
        marks: (None, None),
    };
    assert!(whole.is_identity());
    let cropped = file::Edits {
        crop: Some([8, 6, 32, 24]),
        ..whole
    };
    assert!(!cropped.is_identity());
    assert_eq!(cropped.summary(), "1 piece, crop");
    let read = file::Edits::parse(&cropped.text()).unwrap();
    assert!(!read.is_identity(), "{}", cropped.text());
}

/// 07 §10: the crop is an edit: while editing is off (here, an earlier Save's outcome unknown)
/// a drag on the picture draws no rectangle and the one drawn stays — neither Esc nor Clear
/// rect takes it; Esc is then what it is without one.
#[test]
fn the_crop_is_neither_drawn_nor_cleared_while_editing_is_off() {
    for side in sides() {
        let Some(mut ed) = video(side) else {
            return;
        };
        let disp = ed.win.map.as_ref().unwrap().disp;
        let at = |x: f32, y: f32| disp.lerp_inside(egui::vec2(x, y));
        ed.drv.drag(
            &mut ed.win,
            PointerButton::Primary,
            at(0.2, 0.2),
            at(0.7, 0.8),
        );
        let crop = ed.win.crop().expect("a rectangle drawn");
        ed.knobs.lose_rename.store(true, SeqCst);
        ed.cut(0.25, 0.5);
        ed.save();
        assert!(
            ed.win.unknown && !ed.win.editable(),
            "{side:?}: {}",
            ed.texts()
        );
        // Where the picture is now (rows the Save added may have moved it).
        let disp = ed.win.map.as_ref().unwrap().disp;
        let at = |x: f32, y: f32| disp.lerp_inside(egui::vec2(x, y));
        ed.drv.drag(
            &mut ed.win,
            PointerButton::Primary,
            at(0.05, 0.05),
            at(0.5, 0.5),
        );
        assert_eq!(ed.win.crop(), Some(crop), "{side:?}: no drawing");
        ed.drv.key(&mut ed.win, Key::Escape, Modifiers::NONE);
        assert_eq!(ed.win.crop(), Some(crop), "{side:?}: Esc keeps it");
        ed.drv.click_text(&mut ed.win, "Clear rect");
        ed.drv.idle(&mut ed.win, 2);
        assert_eq!(ed.win.crop(), Some(crop), "{side:?}: Clear rect is off");
    }
}

/// A window whose earlier session left a journal (the second second cut), reopened: the row
/// that offers it is listed.
fn with_a_row(side: Side) -> Option<Ed> {
    let mut ed = Ed::open(side, Read::Copy, Knobs::default())?;
    ed.cut_second();
    ed.drv.until(&mut ed.win, 60, |w| {
        w.journal_text.1 && w.schedule.status() == Keep::Kept
    });
    let Ed {
        win, tmp, remote, ..
    } = ed;
    win.crash();
    let again = Ed::on(side, Read::Copy, Knobs::default(), tmp, remote);
    assert_eq!(again.win.rows.len(), 1, "{side:?}: one row");
    Some(again)
}

/// 07 §3, §10: Restore is not taken while a generation of the window's own is on its way — an
/// edit's first, held on a slow disk, the edit undone meanwhile — and the frame never waits for
/// that write. Once it is written, the row is still there.
#[test]
fn restore_is_refused_while_a_generation_is_on_its_way() {
    for side in sides() {
        let hold = Arc::new(AtomicBool::new(true));
        let inside = hold.clone();
        let ran = super::fixtures::supervised(120, move |_| {
            let Some(mut ed) = with_a_row(side) else {
                return;
            };
            let waiting = Arc::new(AtomicBool::new(false));
            ed.win.journal_fs = Arc::new(SlowDisk {
                held: inside.clone(),
                waiting: waiting.clone(),
            });
            ed.cut_second();
            ed.drv.until(&mut ed.win, 60, |_| waiting.load(SeqCst));
            ed.drv.key(&mut ed.win, Key::Z, command(false));
            assert!(!ed.win.unsaved(), "{side:?}: undone");
            assert!(ed.win.writing.is_some(), "{side:?}: still on its way");
            ed.drv.click_text(&mut ed.win, "Restore");
            ed.drv.idle(&mut ed.win, 5);
            assert!(
                ed.win.restoring.is_none() && ed.win.rows.len() == 1,
                "{side:?}: not taken: {}",
                ed.texts()
            );
            inside.store(false, SeqCst);
            ed.drv.until(&mut ed.win, 60, |w| w.writing.is_none());
            assert_eq!(ed.win.rows.len(), 1, "{side:?}");
        });
        hold.store(false, SeqCst);
        if let Err(why) = ran {
            panic!("{side:?}: {why}");
        }
    }
}

/// 07 §3: no edit is made while a Restore is on its way (its journal would land over the one
/// the edit starts, and its clips over the edit).
#[test]
fn no_edit_is_made_while_a_restore_is_on_its_way() {
    for side in sides() {
        let Some(mut ed) = with_a_row(side) else {
            return;
        };
        assert!(ed.win.editable(), "{side:?}");
        ed.win.restore_row(0);
        assert!(ed.win.restoring.is_some(), "{side:?}: on its way");
        assert!(!ed.win.editable(), "{side:?}: no edit meanwhile");
        ed.drv
            .until(&mut ed.win, 60, |w| w.restoring.is_none() && w.unsaved());
        assert!(ed.win.editable(), "{side:?}");
    }
}

/// A render's path and its bytes, as they were before the copy into `part`.
type Rendered = (PathBuf, Vec<u8>);

/// Save As of a 2 s clip's first half into `folder` as `v.edited.mp4`, its work in
/// `tmp/exports`, `look` run between the render and its copy into `part` (given the render's
/// path). Returns the outcome and the render's path and bytes as they were before the copy.
fn save_half_as(
    tools: &Tools,
    tmp: &Path,
    folder: &Path,
    decision: &super::super::binary::Decision,
    look: file::BeforeCopy,
) -> (save_as::Outcome, Rendered) {
    let src = tmp.join("v.mp4");
    super::fixtures::clip(tools, &src, (64, 48));
    let mut media = Media::probe(&src).unwrap();
    media.plan();
    let half = media.ticks() / 2;
    let mut timeline = Timeline::new(media.rate(), media.ticks());
    timeline.set_clips(
        vec![Clip {
            start: 0,
            end: half,
        }],
        View::default(),
    );
    let spec = edit_spec(&media, &timeline, (0, timeline.len()), None);
    let seen: Arc<Mutex<Option<Rendered>>> = Arc::default();
    let looked = seen.clone();
    let mut producer = file::MediaProducer {
        media: Arc::new(media),
        spec,
        work_root: tmp.join("exports"),
        hold: None,
        held: Arc::default(),
        before_copy: Some(Box::new(move |out: &Path| {
            let bytes = std::fs::read(out).unwrap();
            *looked.lock().unwrap() = Some((out.to_path_buf(), bytes));
            look(out);
        })),
    };
    let dest = folder.join("v.edited.mp4");
    let ctl = save_as::Ctl::new(decision, &|_| {});
    let outcome = save_as::write(&dest, save_as::Size::About(1 << 20), &mut producer, &ctl);
    let seen = seen.lock().unwrap().clone().expect("rendered");
    (outcome, seen)
}

/// The half-length render with picture and sound at `path`.
fn assert_half(tools: &Tools, path: &Path, what: &str) {
    let probe = ffmpeg::probe(tools, path).unwrap();
    let duration = probe.duration.unwrap();
    assert!((duration - 1.0).abs() < 0.05, "{what}: {duration}");
    assert!(probe.video.is_some() && probe.has_audio, "{what}");
}

/// A folder made with `mode` (whatever the umask).
fn folder_with(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// The staging folder Save As made in `folder` (`.sopen-<hex>`): the one there is.
fn staging_in(folder: &Path) -> PathBuf {
    let found: Vec<PathBuf> = std::fs::read_dir(folder)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".sopen-")
        })
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    found[0].clone()
}

/// What is in `dir` (names), sorted.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// 07 §7: Save As renders in sopen's own folder and copies the render into the staging `part`
/// through its handle, whatever the chosen folder's mode: a staging folder swapped (by another
/// user, in a folder they can rename entries of) for one whose `part` links to a file of yours
/// is never written through, and the file saved is the render's bytes.
#[test]
fn save_as_never_writes_through_a_swapped_staging_part() {
    let Some(tools) = tools_or_skip() else { return };
    for mode in [0o700, 0o777] {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("out");
        folder_with(&folder, mode);
        let victim = tmp.path().join("victim");
        std::fs::write(&victim, b"not sopen's").unwrap();
        let (target, chosen) = (victim.clone(), folder.clone());
        let decision = super::super::binary::Decision::new();
        let (outcome, (out, rendered)) = save_half_as(
            &tools,
            tmp.path(),
            &folder,
            &decision,
            Box::new(move |_| {
                // The staging folder renamed aside, and under its name a folder whose `part`
                // links to a file of yours.
                let staging = staging_in(&chosen);
                let name = staging.file_name().unwrap().to_string_lossy();
                std::fs::rename(&staging, staging.with_file_name(format!("{name}-aside"))).unwrap();
                std::fs::create_dir(&staging).unwrap();
                std::os::unix::fs::symlink(&target, staging.join("part")).unwrap();
            }),
        );
        let what = format!("mode {mode:o}");
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"not sopen's",
            "{what}: never written through the link"
        );
        assert!(
            out.starts_with(tmp.path().join("exports")),
            "{}",
            out.display()
        );
        let dest = folder.join("v.edited.mp4");
        match outcome {
            save_as::Outcome::Saved { path, .. } => assert_eq!(path, dest),
            other => panic!("{what}: {}", other.line()),
        }
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            rendered,
            "{what}: the render's bytes"
        );
        assert_half(&tools, &dest, &what);
        assert!(names_in(&tmp.path().join("exports")).is_empty(), "{what}");
    }
}

/// 07 §7: a folder of this user's only, under a parent another user can rename entries of: the
/// folder renamed aside after the render, and made again under its name with the staging
/// folder's name and a `part` linking to a file of yours, is never written through — the copy
/// goes through `part`'s handle; the file saved is the render's bytes, in the folder renamed
/// aside (the save reaches it by its handle).
#[test]
fn save_as_under_a_parent_others_can_rename_in_never_writes_through_a_swap() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("shared");
    folder_with(&parent, 0o777);
    let folder = parent.join("mine");
    folder_with(&folder, 0o700);
    let aside = parent.join("mine-aside");
    let victim = tmp.path().join("victim");
    std::fs::write(&victim, b"not sopen's").unwrap();
    let (target, chosen, moved) = (victim.clone(), folder.clone(), aside.clone());
    let decision = super::super::binary::Decision::new();
    let (outcome, (out, rendered)) = save_half_as(
        &tools,
        tmp.path(),
        &folder,
        &decision,
        Box::new(move |_| {
            let name = staging_in(&chosen).file_name().unwrap().to_owned();
            std::fs::rename(&chosen, &moved).unwrap();
            std::fs::create_dir(&chosen).unwrap();
            std::fs::create_dir(chosen.join(&name)).unwrap();
            std::os::unix::fs::symlink(&target, chosen.join(&name).join("part")).unwrap();
        }),
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"not sopen's",
        "never written through the link"
    );
    assert!(
        out.starts_with(tmp.path().join("exports")),
        "{}",
        out.display()
    );
    match outcome {
        save_as::Outcome::Saved { .. } => {}
        other => panic!("{}", other.line()),
    }
    let saved = aside.join("v.edited.mp4");
    assert_eq!(
        std::fs::read(&saved).unwrap(),
        rendered,
        "the render's bytes"
    );
    assert_half(&tools, &saved, "in the folder renamed aside");
    assert!(names_in(&tmp.path().join("exports")).is_empty());
}

/// 07 §7: a copy that is cancelled, or fails (its render gone, as a full disk fails a write),
/// publishes nothing: no file under the name, the staging folder removed, and the render gone
/// with sopen's work folder.
#[test]
fn a_cancelled_or_failed_copy_publishes_nothing_and_leaves_no_render() {
    let Some(tools) = tools_or_skip() else { return };
    for cancel in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("out");
        folder_with(&folder, 0o700);
        let decision = Arc::new(super::super::binary::Decision::new());
        let theirs = decision.clone();
        let (outcome, _) = save_half_as(
            &tools,
            tmp.path(),
            &folder,
            &decision,
            Box::new(move |out| {
                if cancel {
                    assert!(theirs.cancel(), "cancelled before the publish");
                } else {
                    std::fs::remove_file(out).unwrap();
                }
            }),
        );
        let what = format!("cancel {cancel}");
        match (&outcome, cancel) {
            (save_as::Outcome::Cancelled { .. }, true) => {}
            (save_as::Outcome::Failed(why), false) => {
                assert!(why.contains("cannot read"), "{what}: {why}")
            }
            (other, _) => panic!("{what}: {}", other.line()),
        }
        assert!(
            names_in(&folder).is_empty(),
            "{what}: {:?}",
            names_in(&folder)
        );
        assert!(
            names_in(&tmp.path().join("exports")).is_empty(),
            "{what}: {:?}",
            names_in(&tmp.path().join("exports"))
        );
    }
}

/// 07 §7: the copy into `part` stops at a cancel, writing nothing more, and a write that fails
/// (a full disk; here a handle that cannot write) is an error, never a short file called done.
#[test]
fn the_copy_into_part_stops_at_a_cancel_and_fails_on_a_failed_write() {
    let tmp = tempfile::tempdir().unwrap();
    let from = tmp.path().join("render");
    std::fs::write(&from, vec![7u8; 3 << 20]).unwrap();
    let to = tmp.path().join("part");
    let mut part = std::fs::File::create(&to).unwrap();
    let e = file::copy_in(&from, &mut part, &|| true).unwrap_err();
    assert!(e.is::<crate::remote::Cancelled>(), "{e:#}");
    assert_eq!(std::fs::metadata(&to).unwrap().len(), 0);
    let mut read_only = std::fs::File::open(&to).unwrap();
    let e = file::copy_in(&from, &mut read_only, &|| false).unwrap_err();
    assert!(
        format!("{e:#}").contains("cannot write the staging file"),
        "{e:#}"
    );
    file::copy_in(&from, &mut part, &|| false).unwrap();
    assert_eq!(std::fs::read(&to).unwrap(), std::fs::read(&from).unwrap());
}
