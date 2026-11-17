//! The binary-file dialog driven by real input (keys, clicks) through its `ui`, with a stand-in
//! [`Source`] that counts what it is asked to do and a stand-in [`Chooser`].

use super::*;
use crate::gui::testkit::Driver;
use crate::gui::Origin;
use crate::remote::LocalFs;
use eframe::egui::{Modifiers, Pos2};
use std::sync::atomic::AtomicUsize;

const BODY: &[u8] = b"PK\x03\x04 a zip, as far as anyone can tell\x00\x01\x02\xff";

/// The file is a local fixture: hex opens it in place, save copies it. Counts the calls; a
/// save may be held until the test lets it go.
struct Fake {
    file: PathBuf,
    cache: PathBuf,
    opens: Arc<AtomicUsize>,
    saves: Arc<AtomicUsize>,
    hold: Option<Mutex<Receiver<()>>>,
}

impl Source for Fake {
    fn open_hex(&mut self, progress: &mut dyn FnMut(u64, u64)) -> Result<Opened> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        let abs = self.file.to_str().unwrap().to_string();
        let mut fs = LocalFs::new();
        let at = fs.stat(&abs)?;
        progress(at.size, at.size);
        let origin = Origin::new(Box::new(fs), None, None, abs, at);
        Ok(Opened {
            file: HexFile::open(origin, &self.file, None, &self.cache)?,
            notices: Vec::new(),
            fetch_ms: Some(7),
        })
    }

    fn save_to(
        &mut self,
        dest: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> std::result::Result<u64, SaveError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        if let Some(hold) = &self.hold {
            let _ = hold.lock().unwrap().recv();
        }
        copy_to(&self.file, dest, progress)
    }
}

/// The system dialog's answer, given at once.
struct Picks(Option<PathBuf>, Arc<AtomicUsize>);

impl Chooser for Picks {
    fn choose(&mut self, _suggested: &Path) -> Receiver<Option<PathBuf>> {
        self.1.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        tx.send(self.0.clone()).unwrap();
        rx
    }
}

struct Fx {
    _tmp: tempfile::TempDir,
    /// Where saves go (`SOPEN_SAVE_TO` = `<out>/blob.zip`).
    out: PathBuf,
    win: BinaryWindow,
    drv: Driver,
    opens: Arc<AtomicUsize>,
    saves: Arc<AtomicUsize>,
    chooses: Arc<AtomicUsize>,
}

fn dialog_with(pick: Option<PathBuf>, hold: Option<Receiver<()>>) -> Fx {
    dialog_hooked(pick, hold, None)
}

fn dialog_hooked(
    pick: Option<PathBuf>,
    hold: Option<Receiver<()>>,
    choice: Option<crate::gui::Choice>,
) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("src/blob.zip");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, BODY).unwrap();
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let (opens, saves, chooses) = (Arc::default(), Arc::default(), Arc::default());
    let source = Fake {
        file: file.clone(),
        cache: tmp.path().join("cache"),
        opens: Arc::clone(&opens),
        saves: Arc::clone(&saves),
        hold: hold.map(Mutex::new),
    };
    // Tests never save into the real download folder: the path is always given.
    let hooks = Hooks {
        save_to: Some(out.join("blob.zip")),
        choice,
        ..Hooks::default()
    };
    let info = Info {
        title: format!("box:{}", file.display()),
        name: "blob.zip".to_string(),
        size: BODY.len() as u64,
    };
    let win = BinaryWindow::new(
        info,
        Box::new(source),
        Box::new(Picks(pick, Arc::clone(&chooses))),
        &hooks,
    );
    let mut fx = Fx {
        _tmp: tmp,
        out,
        win,
        drv: Driver::new(WINDOW[0], WINDOW[1]),
        opens,
        saves,
        chooses,
    };
    fx.drv.idle(&mut fx.win, 3);
    fx
}

fn dialog() -> Fx {
    dialog_with(None, None)
}

impl Fx {
    fn calls(&self) -> (usize, usize) {
        (
            self.opens.load(Ordering::SeqCst),
            self.saves.load(Ordering::SeqCst),
        )
    }

    fn until_asking(&mut self) {
        self.drv.until(&mut self.win, 20, |w| w.asking());
        self.drv.idle(&mut self.win, 2);
    }

    fn until_saved(&mut self) {
        self.drv.until(&mut self.win, 20, |w| {
            matches!(w.stage, Stage::Saved { .. })
        });
    }

    fn out_files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.out)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

/// What the dialog shows before anything is downloaded, and its default answer: Enter opens
/// the hex editor — the only moment the file is fetched; the same window becomes the hex window.
#[test]
fn enter_opens_the_hex_editor_and_only_then_fetches() {
    let mut fx = dialog();
    let describe = format!("{} B ({} bytes) · ZIP archive", BODY.len(), BODY.len());
    for text in [
        "blob.zip",
        describe.as_str(),
        "Open in hex editor",
        "Save to…",
        "Cancel",
        "Browse…",
    ] {
        assert!(fx.drv.find_text(text).is_some(), "{text:?} not shown");
    }
    fx.drv.idle(&mut fx.win, 20);
    assert_eq!(fx.calls(), (0, 0), "nothing fetched before the choice");
    assert!(fx.win.ready());
    fx.drv.key(&mut fx.win, Key::Enter, Modifiers::NONE);
    fx.drv
        .until(&mut fx.win, 20, |w| matches!(w.stage, Stage::Hex(_)));
    assert_eq!(fx.calls(), (1, 0));
    fx.drv.idle(&mut fx.win, 3);
    assert!(
        fx.drv
            .find_text("50 4B 03 04 20 61 20 7A  69 70 2C 20 61 73 20 66")
            .is_some(),
        "the same window shows the hex grid"
    );
    assert!(fx.win.ready());
    let ended = fx.win.ended.lock().unwrap().clone();
    assert!(ended.hex.is_some() && ended.saved_to.is_none());
    assert_eq!(ended.fetch_ms, Some(7));
    assert!(fx.out_files().is_empty());
}

/// `SOPEN_CHOICE=hex`: the answer is given — the fetch starts on the first pass, with no key,
/// and the window becomes the hex window (ready for a screenshot only then).
#[test]
fn a_prepicked_hex_answer_fetches_at_once_and_switches_the_window() {
    let mut fx = dialog_hooked(None, None, Some(crate::gui::Choice::Hex));
    fx.drv
        .until(&mut fx.win, 20, |w| matches!(w.stage, Stage::Hex(_)));
    assert_eq!(fx.calls(), (1, 0));
    fx.drv.idle(&mut fx.win, 3);
    assert!(fx.win.ready());
    assert!(fx
        .drv
        .find_text("50 4B 03 04 20 61 20 7A  69 70 2C 20 61 73 20 66")
        .is_some());
}

/// Esc, Cancel and closing the window: nothing fetched, nothing written.
#[test]
fn esc_cancel_and_close_download_nothing() {
    let mut fx = dialog();
    fx.drv.key(&mut fx.win, Key::Escape, Modifiers::NONE);
    assert!(fx.win.close_ok);
    let mut fx2 = dialog();
    fx2.drv.click_text(&mut fx2.win, "Cancel");
    assert!(fx2.win.close_ok);
    let mut fx3 = dialog();
    assert!(fx3.win.close_requested());
    for fx in [&fx, &fx2, &fx3] {
        assert_eq!(fx.calls(), (0, 0));
        assert!(fx.out_files().is_empty());
        let ended = fx.win.ended.lock().unwrap().clone();
        assert!(ended.hex.is_none() && ended.saved_to.is_none());
    }
}

/// "Save to…" a free path writes exactly the file's bytes, with the mode any new file there
/// gets (not the cache's owner-only mode); "saved <path>" shows, then the window closes.
#[test]
fn save_to_a_free_path_writes_the_bytes_with_a_new_files_mode() {
    if crate::testutil::rerun_under_umask_022(
        "gui::binary::tests::save_to_a_free_path_writes_the_bytes_with_a_new_files_mode",
    ) {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let mut fx = dialog();
    fx.drv.click_text(&mut fx.win, "Save to…");
    fx.until_saved();
    let dest = fx.out.join("blob.zip");
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    let control = fx.out.join("control");
    File::create(&control).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dest), mode(&control));
    assert_eq!(mode(&dest), 0o644);
    assert_eq!(fx.calls(), (0, 1));
    assert_eq!(
        fx.win.ended.lock().unwrap().saved_to.as_deref(),
        Some(dest.as_path())
    );
    let line = format!("saved {}", tilde(&dest));
    fx.drv.idle(&mut fx.win, 1);
    assert!(fx.drv.find_text(&line).is_some(), "{line}");
    fx.drv.until(&mut fx.win, 5, |w| w.close_ok);
}

/// An existing file is never replaced: the dialog says so whole, puts the next free name in the
/// field, and "Save to…" again saves there.
#[test]
fn save_to_an_existing_path_never_replaces_it_and_offers_the_next_name() {
    let mut fx = dialog();
    let taken = fx.out.join("blob.zip");
    std::fs::write(&taken, b"someone's file").unwrap();
    fx.drv.click_text(&mut fx.win, "Save to…");
    fx.until_asking();
    assert_eq!(std::fs::read(&taken).unwrap(), b"someone's file");
    let note = format!(
        "{} exists — not replaced. Save to… again saves as blob-1.zip",
        tilde(&taken)
    );
    assert!(fx.drv.find_text(&note).is_some(), "said whole: {note}");
    assert_eq!(fx.win.path, tilde(&fx.out.join("blob-1.zip")));
    fx.drv.click_text(&mut fx.win, "Save to…");
    fx.until_saved();
    assert_eq!(std::fs::read(fx.out.join("blob-1.zip")).unwrap(), BODY);
    assert_eq!(std::fs::read(&taken).unwrap(), b"someone's file");
    assert_eq!(fx.out_files(), ["blob-1.zip", "blob.zip"]);
}

/// Enter in the path field saves there; it never opens the hex editor.
#[test]
fn enter_in_the_path_field_saves_and_does_not_open_hex() {
    let mut fx = dialog();
    // The field is the row's room left of Browse….
    let browse = fx.drv.find_text("Browse…").expect("Browse… shown");
    let field = Pos2::new(browse.left() - 60.0, browse.center().y);
    fx.drv.click(&mut fx.win, field);
    assert_eq!(fx.drv.ctx.memory(|m| m.focused()), Some(path_id()));
    fx.drv.key(&mut fx.win, Key::Enter, Modifiers::NONE);
    fx.until_saved();
    assert_eq!(fx.calls(), (0, 1));
    assert_eq!(std::fs::read(fx.out.join("blob.zip")).unwrap(), BODY);
}

/// Browse… asks the system dialog; its Save saves where it says.
#[test]
fn browse_saves_where_the_system_dialog_says() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("picked.bin");
    let mut fx = dialog_with(Some(picked.clone()), None);
    fx.drv.click_text(&mut fx.win, "Browse…");
    fx.until_saved();
    assert_eq!(fx.chooses.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(&picked).unwrap(), BODY);
    assert_eq!(fx.win.path, tilde(&picked));
}

/// No path from the system dialog (cancelled, or none here): one line says the field still
/// works; nothing is saved.
#[test]
fn browse_without_an_answer_says_the_path_field_still_works() {
    let mut fx = dialog();
    fx.drv.click_text(&mut fx.win, "Browse…");
    fx.until_asking();
    assert!(fx.drv.find_text(BROWSE_NONE).is_some());
    assert_eq!(fx.calls(), (0, 0));
    assert!(fx.out_files().is_empty());
}

/// Closing during a download never cuts it: the window says it closes when the transfer is
/// done, and does then; Enter and Esc do nothing meanwhile.
#[test]
fn closing_during_a_download_waits_for_it() {
    let (release, hold) = mpsc::channel();
    let mut fx = dialog_with(None, Some(hold));
    fx.drv.click_text(&mut fx.win, "Save to…");
    assert!(matches!(fx.win.stage, Stage::Moving { .. }));
    assert!(!fx.win.ready(), "no screenshot mid-transfer");
    fx.drv.key(&mut fx.win, Key::Enter, Modifiers::NONE);
    fx.drv.key(&mut fx.win, Key::Escape, Modifiers::NONE);
    assert!(!fx.win.close_ok);
    assert!(!fx.win.close_requested());
    fx.drv.idle(&mut fx.win, 2);
    assert!(fx.drv.find_text(CLOSES_AFTER).is_some());
    release.send(()).unwrap();
    fx.drv.until(&mut fx.win, 20, |w| w.close_ok);
    assert_eq!(std::fs::read(fx.out.join("blob.zip")).unwrap(), BODY);
    assert_eq!(fx.calls(), (0, 1));
}

/// The never-replace rule at the bottom: an existing file or a link (even a dangling one) is
/// "exists", untouched and not written through; a failed fill removes only the file it made.
#[test]
fn save_new_never_replaces_or_writes_through_a_link() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("a.bin");
    std::fs::write(&dest, b"mine").unwrap();
    let fill = |f: &mut File| -> Result<u64> {
        f.write_all(b"new")?;
        Ok(3)
    };
    match save_new(&dest, fill) {
        Err(SaveError::Exists { next }) => assert_eq!(next, tmp.path().join("a-1.bin")),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&dest).unwrap(), b"mine");
    let link = tmp.path().join("link.bin");
    let target = tmp.path().join("nowhere.bin");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(matches!(
        save_new(&link, fill),
        Err(SaveError::Exists { .. })
    ));
    assert!(!target.exists(), "written through the link");
    let fresh = tmp.path().join("b.bin");
    let failing = |f: &mut File| -> Result<u64> {
        f.write_all(b"half")?;
        bail!("the connection dropped")
    };
    match save_new(&fresh, failing) {
        Err(SaveError::Failed(e)) => assert!(format!("{e:#}").contains("dropped"), "{e:#}"),
        other => panic!("{other:?}"),
    }
    assert!(!fresh.exists(), "the half-written file is removed");
    assert_eq!(save_new(&fresh, fill).unwrap(), 3);
    assert_eq!(std::fs::read(&fresh).unwrap(), b"new");
}

/// A download whose source changes during the transfer is not saved (and leaves nothing).
#[test]
fn a_download_of_a_file_that_changes_meanwhile_is_not_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.bin");
    std::fs::write(&src, BODY).unwrap();
    let dest = tmp.path().join("dest.bin");
    let mut once = true;
    let result = download_to(
        &mut LocalFs::new(),
        src.to_str().unwrap(),
        &dest,
        &mut |_, _| {
            if std::mem::take(&mut once) {
                std::fs::write(&src, b"a different, longer body than before").unwrap();
            }
        },
    );
    match result {
        Err(SaveError::Failed(e)) => assert!(format!("{e:#}").contains("changed"), "{e:#}"),
        other => panic!("{other:?}"),
    }
    assert!(!dest.exists());
    // Quiet: saved whole.
    let n = download_to(
        &mut LocalFs::new(),
        src.to_str().unwrap(),
        &dest,
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap().len() as u64, n);
}

#[test]
fn names_paths_and_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.tar.gz");
    std::fs::write(&a, b"").unwrap();
    assert_eq!(next_free(&a), tmp.path().join("a.tar-1.gz"));
    std::fs::write(tmp.path().join("a.tar-1.gz"), b"").unwrap();
    assert_eq!(next_free(&a), tmp.path().join("a.tar-2.gz"));
    assert_eq!(
        next_free(&tmp.path().join("README")),
        tmp.path().join("README-1")
    );
    // The default path: itself when free; else the next free name, said.
    let free = tmp.path().join("new.zip");
    assert_eq!(suggest(free.clone()), (free, None));
    let (path, note) = suggest(a.clone());
    assert_eq!(path, tmp.path().join("a.tar-2.gz"));
    assert!(note.unwrap().ends_with("suggesting a.tar-2.gz"));
    assert_eq!(kind_name("x.ZIP"), Some("ZIP archive"));
    assert_eq!(kind_name("lib.so"), Some("shared library"));
    assert_eq!(kind_name("blob"), None);
    assert_eq!(kind_name("x.unknownext"), None);
    if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
        assert_eq!(tilde(&home.join("Downloads/a.zip")), "~/Downloads/a.zip");
        assert_eq!(untilde("~/Downloads/a.zip"), home.join("Downloads/a.zip"));
        assert_eq!(untilde("~"), home);
    }
    assert_eq!(tilde(Path::new("/tmp/x")), "/tmp/x");
    assert_eq!(untilde("/tmp/x"), PathBuf::from("/tmp/x"));
}
