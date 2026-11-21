//! The binary-file dialog driven by real input (keys, clicks) through its `ui` — with a stand-in
//! [`Source`] that counts what it is asked to do, and with the production [`FileSource`] over a
//! counting file layer on the route main takes ([`open_file`]) — plus Save to…'s file rules.

use super::*;
use crate::gui::testkit::{Driver, Meter as Metered};
use crate::remote::{LocalFs, Stat};
use eframe::egui::{Modifiers, Pos2};
use std::os::unix::fs::FileExt;
use std::sync::atomic::AtomicUsize;

const BODY: &[u8] = b"PK\x03\x04 a zip, as far as anyone can tell\x00\x01\x02\xff";

/// Files under `dir`, recursively; an absent `dir` has none, any other error fails the test.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => panic!("cannot list {}: {e}", dir.display()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let p = entry
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
            .path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

/// The file is a local fixture: hex opens it in place, save copies it. Counts the calls; a save
/// may be held until the test lets it go, or panic (a worker that dies).
struct Fake {
    file: PathBuf,
    cache: PathBuf,
    opens: Arc<AtomicUsize>,
    saves: Arc<AtomicUsize>,
    hold: Option<Mutex<Receiver<()>>>,
    panics: bool,
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
        cancel: &Decision,
    ) -> std::result::Result<SavedTo, SaveError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        if let Some(hold) = &self.hold {
            let _ = hold.lock().unwrap().recv();
        }
        if self.panics {
            panic!("the transfer worker dies (test)");
        }
        copy_to(&self.file, dest, progress, cancel)
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

#[derive(Default)]
struct Opts {
    pick: Option<PathBuf>,
    hold: Option<Receiver<()>>,
    choice: Option<Choice>,
    panics: bool,
    local: bool,
}

fn dialog_with(opts: Opts) -> Fx {
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
        hold: opts.hold.map(Mutex::new),
        panics: opts.panics,
    };
    // Tests never save into the real download folder: the path is always given.
    let hooks = Hooks {
        save_to: Some(out.join("blob.zip")),
        choice: opts.choice,
        ..Hooks::default()
    };
    let info = Info {
        title: format!("box:{}", file.display()),
        name: "blob.zip".to_string(),
        size: BODY.len() as u64,
        local: opts.local,
    };
    let win = BinaryWindow::new(
        info,
        Box::new(source),
        Box::new(Picks(opts.pick, Arc::clone(&chooses))),
        &hooks,
    );
    let mut fx = Fx {
        _tmp: tmp,
        out,
        win,
        drv: Driver::new(WINDOW[0], 320.0),
        opens,
        saves,
        chooses,
    };
    fx.drv.idle(&mut fx.win, 3);
    fx
}

fn dialog() -> Fx {
    dialog_with(Opts::default())
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

    /// Focuses the path field (the row's room left of Browse…).
    fn focus_path(&mut self) {
        let browse = self.drv.find_text("Browse…").expect("Browse… shown");
        let field = Pos2::new(browse.left() - 60.0, browse.center().y);
        self.drv.click(&mut self.win, field);
        assert_eq!(self.drv.ctx.memory(|m| m.focused()), Some(path_id()));
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
        hint(false),
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
    let mut fx = dialog_with(Opts {
        choice: Some(Choice::Hex),
        ..Opts::default()
    });
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

/// A file with nothing to download (local, or already in the cache) is not said to be
/// "downloaded until you choose"; one on another machine is.
#[test]
fn the_hint_says_nothing_is_downloaded_only_when_something_would_be() {
    let here = dialog_with(Opts {
        local: true,
        ..Opts::default()
    });
    assert!(here.drv.find_text(hint(true)).is_some());
    assert!(!hint(true).contains("downloaded"));
    assert!(here.drv.find_text(hint(false)).is_none());
    let there = dialog();
    assert!(there.drv.find_text(hint(false)).is_some());
}

/// The window is as tall as its content: it asks for a height that holds the last line and
/// leaves no large empty area below it.
#[test]
fn the_dialog_asks_for_its_contents_height() {
    let fx = dialog();
    let asked = fx.win.asked_height().expect("a height was asked for");
    let last = fx.drv.find_text(hint(false)).expect("the hint line");
    assert!(asked >= last.bottom(), "{asked} < {}", last.bottom());
    assert!(
        asked <= last.bottom() + 30.0,
        "{asked} leaves room below {}",
        last.bottom()
    );
    assert!(asked < 320.0, "{asked}");
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
    let mut fx = dialog();
    fx.drv.click_text(&mut fx.win, "Save to…");
    fx.until_saved();
    let dest = fx.out.join("blob.zip");
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    let control = fx.out.join("control");
    File::create(&control).unwrap();
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
    assert_eq!(
        fx.out_files(),
        ["blob.zip", "control"],
        "no staging file left"
    );
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

/// Enter in the path field saves there; it never opens the hex editor. When that save is
/// refused, the field has the keyboard again: the next Enter saves (to the offered name), it
/// does not start a fetch.
#[test]
fn enter_in_the_path_field_saves_and_a_refusal_keeps_the_field_focused() {
    let mut fx = dialog();
    std::fs::write(fx.out.join("blob.zip"), b"someone's file").unwrap();
    fx.focus_path();
    fx.drv.key(&mut fx.win, Key::Enter, Modifiers::NONE);
    fx.until_asking();
    assert_eq!(fx.calls(), (0, 1));
    assert_eq!(
        fx.drv.ctx.memory(|m| m.focused()),
        Some(path_id()),
        "the field has the keyboard back"
    );
    fx.drv.key(&mut fx.win, Key::Enter, Modifiers::NONE);
    fx.until_saved();
    assert_eq!(fx.calls(), (0, 2), "no hex fetch");
    assert_eq!(std::fs::read(fx.out.join("blob-1.zip")).unwrap(), BODY);
}

/// The path is saved as typed: only an empty field is refused, spaces are part of the name.
#[test]
fn the_path_is_saved_exactly_as_typed() {
    let mut fx = dialog();
    let spaced = fx.out.join(" blob .zip ");
    fx.win.path = spaced.to_str().unwrap().to_string();
    fx.drv.click_text(&mut fx.win, "Save to…");
    fx.until_saved();
    assert_eq!(std::fs::read(&spaced).unwrap(), BODY);
    let mut fx = dialog();
    fx.win.path = "   ".to_string();
    fx.drv.click_text(&mut fx.win, "Save to…");
    fx.drv.idle(&mut fx.win, 2);
    assert!(fx.drv.find_text("type a path to save to").is_some());
    assert_eq!(fx.calls(), (0, 0));
}

/// Browse… asks the system dialog; its Save saves where it says.
#[test]
fn browse_saves_where_the_system_dialog_says() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("picked.bin");
    let mut fx = dialog_with(Opts {
        pick: Some(picked.clone()),
        ..Opts::default()
    });
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

/// Closing during a download cancels it (06 §7.5, replacing 01 §6a's "closing waits for it"):
/// the transfer stops, nothing is saved, and the window closes then; Enter does nothing
/// meanwhile.
#[test]
fn closing_during_a_download_cancels_it() {
    let (release, hold) = mpsc::channel();
    let mut fx = dialog_with(Opts {
        hold: Some(hold),
        ..Opts::default()
    });
    fx.drv.click_text(&mut fx.win, "Save to…");
    assert!(matches!(fx.win.stage, Stage::Moving { .. }));
    assert!(!fx.win.ready(), "no screenshot mid-transfer");
    fx.drv.key(&mut fx.win, Key::Enter, Modifiers::NONE);
    assert!(!fx.win.close_ok);
    assert!(!fx.win.close_requested(), "until the transfer has stopped");
    release.send(()).unwrap();
    fx.drv.until(&mut fx.win, 20, |w| w.close_ok);
    assert!(fx.out_files().is_empty(), "{:?}", fx.out_files());
    assert_eq!(fx.win.ended.lock().unwrap().moved, Moved::Stopped);
    assert_eq!(fx.calls(), (0, 1));
}

/// A transfer whose worker dies is a visible, final failure — not a dialog whose buttons do
/// nothing; Close (or Esc) closes it. A close asked for during that transfer closes it at once.
#[test]
fn a_dead_transfer_worker_is_a_visible_failure() {
    let mut fx = dialog_with(Opts {
        panics: true,
        ..Opts::default()
    });
    fx.drv.click_text(&mut fx.win, "Save to…");
    fx.drv
        .until(&mut fx.win, 20, |w| matches!(w.stage, Stage::Failed(_)));
    fx.drv.idle(&mut fx.win, 3);
    let Stage::Failed(message) = &fx.win.stage else {
        unreachable!()
    };
    let message = message.clone();
    assert!(message.contains("stopped without a result"), "{message}");
    assert!(fx.drv.find_text(&message).is_some(), "shown whole");
    assert!(fx.win.ready());
    fx.drv.click_text(&mut fx.win, "Close");
    assert!(fx.win.close_ok);

    let (release, hold) = mpsc::channel();
    let mut fx = dialog_with(Opts {
        panics: true,
        hold: Some(hold),
        ..Opts::default()
    });
    fx.drv.click_text(&mut fx.win, "Save to…");
    assert!(!fx.win.close_requested(), "waits for the transfer");
    release.send(()).unwrap();
    fx.drv.until(&mut fx.win, 20, |w| w.close_ok);
    assert!(fx.out_files().is_empty());
}

// ---------------------------------------------------------------------------------------------
// Save to…'s file rules.

/// The names directly in `dir`, sorted (folders too, hidden ones too).
fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// The private staging folder a save made in `dir`, if there is one.
fn private_folder(dir: &Path) -> Option<PathBuf> {
    let found: Vec<String> = names(dir)
        .into_iter()
        .filter(|n| n.starts_with(".sopen-"))
        .collect();
    assert!(found.len() <= 1, "{found:?}");
    found.first().map(|n| dir.join(n))
}

fn os_error(code: i32) -> std::io::Error {
    std::io::Error::from_raw_os_error(code)
}

/// A fill that checks what is on disk while it writes: the final name is absent; the only new
/// entry in the folder is the private staging folder `.sopen-<16 hex>` (0700), holding only
/// `part` (0600, one link: the probe's second name is gone) — the very file being written.
fn watching_fill<'a>(dir: &'a Path, dest: &'a Path) -> impl FnOnce(&mut File) -> Result<u64> + 'a {
    move |f: &mut File| {
        use std::os::unix::fs::MetadataExt;
        f.write_all(BODY)?;
        assert!(
            !dest.exists(),
            "bytes under the final name before they are complete"
        );
        let private = private_folder(dir).expect("a private staging folder");
        let pname = private.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(pname.len(), ".sopen-".len() + 16, "{pname}");
        assert!(pname[7..].chars().all(|c| c.is_ascii_hexdigit()), "{pname}");
        let others: Vec<String> = names(dir)
            .into_iter()
            .filter(|n| *n != pname && dest.file_name().unwrap() != n.as_str())
            .collect();
        assert!(others.is_empty(), "{others:?}");
        assert_eq!(mode(&private), 0o700, "a private folder");
        assert_eq!(names(&private), ["part"]);
        let part = std::fs::symlink_metadata(private.join("part")).unwrap();
        assert!(part.file_type().is_file());
        assert_eq!(part.mode() & 0o777, 0o600, "owner-only while it fills");
        assert_eq!(part.nlink(), 1, "the probe's link is gone");
        let ours = f.metadata().unwrap();
        assert_eq!(
            (part.dev(), part.ino()),
            (ours.dev(), ours.ino()),
            "bound to its inode"
        );
        Ok(BODY.len() as u64)
    }
}

/// Bytes go under the final name only complete: `part` fills inside the private folder, then is
/// linked to the name (never over anything) with a new file's mode; the private folder goes. A
/// failed fill leaves nothing of either.
#[test]
fn save_new_publishes_only_complete_bytes() {
    if crate::testutil::rerun_under_umask_022(
        "gui::binary::tests::save_new_publishes_only_complete_bytes",
    ) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("a.bin");
    let saved = save_new(&dest, watching_fill(tmp.path(), &dest)).unwrap();
    assert_eq!(
        saved,
        SavedTo {
            bytes: BODY.len() as u64,
            notes: Vec::new()
        }
    );
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    assert_eq!(mode(&dest), 0o644);
    assert_eq!(names(tmp.path()), ["a.bin"], "the staging folder is gone");

    let fresh = tmp.path().join("b.bin");
    let failing = |f: &mut File| -> Result<u64> {
        f.write_all(b"half")?;
        bail!("the connection dropped")
    };
    match save_new(&fresh, failing) {
        Err(SaveError::Failed(e)) => assert!(format!("{e:#}").contains("dropped"), "{e:#}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(names(tmp.path()), ["a.bin"], "nothing of the failed save");
}

/// The staging name does not grow with the chosen one: a name at the longest a folder takes
/// saves like any other.
#[test]
fn save_new_takes_the_longest_name_a_folder_takes() {
    let tmp = tempfile::tempdir().unwrap();
    let long = format!("{}.bin", "n".repeat(251));
    assert_eq!(long.len(), 255);
    let dest = tmp.path().join(&long);
    save_new(&dest, |f: &mut File| {
        f.write_all(BODY)?;
        Ok(BODY.len() as u64)
    })
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    assert_eq!(names(tmp.path()), [long]);
}

/// The folder is opened once, following a link to a folder: the file lands in the real folder.
#[test]
fn save_new_follows_a_link_to_a_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let real_dir = tmp.path().join("real");
    std::fs::create_dir(&real_dir).unwrap();
    std::os::unix::fs::symlink(&real_dir, tmp.path().join("via")).unwrap();
    save_new(&tmp.path().join("via/a.bin"), |f: &mut File| {
        f.write_all(BODY)?;
        Ok(BODY.len() as u64)
    })
    .unwrap();
    assert_eq!(std::fs::read(real_dir.join("a.bin")).unwrap(), BODY);
    assert_eq!(names(&real_dir), ["a.bin"]);
}

/// The next free name of a name already at the longest a folder takes still fits: the stem
/// gives way to the `-N`, and a save there works.
#[test]
fn the_next_free_name_of_a_longest_name_still_fits() {
    let tmp = tempfile::tempdir().unwrap();
    let long = format!("{}.bin", "n".repeat(251));
    let taken = tmp.path().join(&long);
    std::fs::write(&taken, b"mine").unwrap();
    let next = next_free(&taken);
    let name = next.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.len() <= 255, "{} bytes: {name}", name.len());
    assert!(name.ends_with("-1.bin"), "{name}");
    save_new(&next, |f: &mut File| {
        f.write_all(BODY)?;
        Ok(BODY.len() as u64)
    })
    .unwrap();
    assert_eq!(std::fs::read(&next).unwrap(), BODY);
    // A multi-byte stem is cut on a character boundary.
    let wide = "한".repeat(85);
    assert_eq!(wide.len(), 255);
    let taken = tmp.path().join(&wide);
    std::fs::write(&taken, b"mine").unwrap();
    let next = next_free(&taken);
    let name = next.file_name().unwrap().to_str().unwrap().to_string();
    assert_eq!(name, format!("{}-1", "한".repeat(84)));
    std::fs::write(&next, b"").unwrap();
}

/// A group of ours other than the primary one (root may use any), to see a folder's group pass
/// to a new file; `None` when there is none.
fn another_group() -> Option<u32> {
    // SAFETY: plain id queries; getgroups fills at most the room given.
    unsafe {
        if libc::geteuid() == 0 {
            return Some(4242);
        }
        let primary = libc::getegid();
        let mut groups = vec![0 as libc::gid_t; 256];
        let n = libc::getgroups(groups.len() as libc::c_int, groups.as_mut_ptr());
        groups
            .into_iter()
            .take(n.max(0) as usize)
            .find(|&g| g != primary)
            .map(|g| g as _)
    }
}

/// A setgid folder (a shared folder) keeps its group on what is saved in it: the private
/// staging folder keeps the setgid bit when it is made owner-only, so `part` — and the published
/// file — get the folder's group (Linux follows the bit; BSD and macOS always give the folder's
/// group). Where the folder's group is another group of ours, the file is checked to carry it.
#[test]
fn a_setgid_folders_group_reaches_the_saved_file() {
    use std::os::unix::fs::MetadataExt;
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    let group = another_group();
    if let Some(gid) = group {
        std::os::unix::fs::chown(&shared, None, Some(gid)).unwrap();
    }
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o2775)).unwrap();
    let want_gid = std::fs::metadata(&shared).unwrap().gid();
    let dest = shared.join("a.bin");
    let seen = std::cell::Cell::new(None);
    save_new(&dest, |f: &mut File| {
        f.write_all(BODY)?;
        let private = private_folder(&shared).expect("a private staging folder");
        let mode = std::fs::metadata(&private).unwrap().mode();
        seen.set(Some((mode & 0o7777, f.metadata().unwrap().gid())));
        Ok(BODY.len() as u64)
    })
    .unwrap();
    let (private_mode, part_gid) = seen.get().unwrap();
    assert_eq!(
        private_mode, 0o2700,
        "{private_mode:o}: setgid kept, owner-only"
    );
    if group.is_none() {
        eprintln!("note: no second group of ours here; only the setgid bit was checked");
        return;
    }
    assert_eq!(part_gid, want_gid, "part has the folder's group");
    assert_eq!(std::fs::metadata(&dest).unwrap().gid(), want_gid);
}

/// The folder is opened once: a link to it retargeted while the bytes are on their way changes
/// nothing — the file is published in the folder opened, and the new target is left untouched.
#[test]
fn a_folder_link_retargeted_during_the_save_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (first, second) = (tmp.path().join("first"), tmp.path().join("second"));
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    let via = tmp.path().join("via");
    std::os::unix::fs::symlink(&first, &via).unwrap();
    let fill = |f: &mut File| -> Result<u64> {
        f.write_all(BODY)?;
        std::fs::remove_file(&via)?;
        std::os::unix::fs::symlink(&second, &via)?;
        Ok(BODY.len() as u64)
    };
    save_new(&via.join("a.bin"), fill).unwrap();
    assert_eq!(std::fs::read(first.join("a.bin")).unwrap(), BODY);
    assert_eq!(names(&first), ["a.bin"], "cleanup in the folder opened");
    assert!(names(&second).is_empty(), "the new target was touched");
}

/// Never over, or through, anything: an existing file or a link (even a dangling one) is
/// "exists" before any byte moves; a name taken at the last moment (during the fill, or between
/// the fill and the link) is "exists" too, the other file untouched and the staging folder gone.
#[test]
fn save_new_never_replaces_or_writes_through_a_link() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("a.bin");
    std::fs::write(&dest, b"mine").unwrap();
    let never = |_: &mut File| -> Result<u64> { panic!("filled although the name is taken") };
    match save_new(&dest, never) {
        Err(SaveError::Exists { next }) => assert_eq!(next, tmp.path().join("a-1.bin")),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&dest).unwrap(), b"mine");
    let link = tmp.path().join("link.bin");
    let target = tmp.path().join("nowhere.bin");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(matches!(
        save_new(&link, never),
        Err(SaveError::Exists { .. })
    ));
    assert!(!target.exists(), "written through the link");

    // Taken while the bytes were on their way (by the production link).
    let during = tmp.path().join("during.bin");
    let fill_and_take = |f: &mut File| -> Result<u64> {
        f.write_all(BODY)?;
        std::fs::write(&during, b"someone else's, meanwhile")?;
        Ok(BODY.len() as u64)
    };
    match save_new(&during, fill_and_take) {
        Err(SaveError::Exists { next }) => assert_eq!(next, tmp.path().join("during-1.bin")),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        std::fs::read(&during).unwrap(),
        b"someone else's, meanwhile"
    );

    // Taken between the fill and the link.
    let late = tmp.path().join("late.bin");
    let fill = |f: &mut File| -> Result<u64> {
        f.write_all(BODY)?;
        Ok(BODY.len() as u64)
    };
    let late2 = late.clone();
    let mut someone = move |step: LinkStep, _: &Path| {
        if step == LinkStep::Publish {
            std::fs::write(&late2, b"someone else's, just now")?;
        }
        Ok(())
    };
    match save_new_with(&late, fill, &mut someone) {
        Err(SaveError::Exists { next }) => assert_eq!(next, tmp.path().join("late-1.bin")),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&late).unwrap(), b"someone else's, just now");
    assert_eq!(
        names(tmp.path()),
        ["a.bin", "during.bin", "late.bin", "link.bin"],
        "the staging folder was removed"
    );
}

/// Where publishing cannot work, the save fails before a byte is transferred, saying why: a
/// folder whose filesystem has no hard links (found by linking the still-empty `part` to a
/// second name), a path ending in '/', a folder that does not exist, a file where the folder
/// should be. Nothing is left.
#[test]
fn save_new_fails_before_transferring_where_it_cannot_publish() {
    let tmp = tempfile::tempdir().unwrap();
    let never = |_: &mut File| -> Result<u64> { panic!("transferred although it cannot publish") };
    let codes = [
        libc::ENOTSUP,
        libc::EOPNOTSUPP,
        libc::EPERM,
        libc::EXDEV,
        libc::EMLINK,
    ];
    for code in codes {
        let dest = tmp.path().join("a.bin");
        let mut no_links = |step: LinkStep, _: &Path| match step {
            LinkStep::Probe => Err(os_error(code)),
            LinkStep::Publish => panic!("published after a failed probe"),
        };
        let err = save_new_with(&dest, never, &mut no_links).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("has no hard links"), "{code}: {msg}");
        assert!(msg.contains("choose another folder"), "{code}: {msg}");
        assert!(!msg.contains("an existing file"), "{code}: {msg}");
        assert!(
            names(tmp.path()).is_empty(),
            "{code}: {:?}",
            names(tmp.path())
        );
    }
    // Another probe failure is said as it is.
    let mut broken = |step: LinkStep, _: &Path| match step {
        LinkStep::Probe => Err(os_error(libc::EIO)),
        LinkStep::Publish => panic!("published after a failed probe"),
    };
    let msg = save_new_with(&tmp.path().join("a.bin"), never, &mut broken)
        .unwrap_err()
        .to_string();
    assert!(msg.contains("cannot test whether"), "{msg}");
    assert!(names(tmp.path()).is_empty());

    let typed = format!("{}/newdir/", tmp.path().display());
    let msg = save_new(Path::new(&typed), never).unwrap_err().to_string();
    assert!(msg.contains("ends with '/'"), "{msg}");
    let msg = save_new(&tmp.path().join("missing/a.bin"), never)
        .unwrap_err()
        .to_string();
    assert!(msg.contains("does not exist"), "{msg}");
    std::fs::write(tmp.path().join("file.txt"), b"").unwrap();
    let msg = save_new(&tmp.path().join("file.txt/a.bin"), never)
        .unwrap_err()
        .to_string();
    assert!(msg.contains("is not a folder"), "{msg}");
    assert_eq!(names(tmp.path()), ["file.txt"]);
}

/// A link that fails at publishing for another reason keeps the complete download inside the
/// private folder, never under the chosen name, and the error says why and where.
#[test]
fn a_failed_publish_keeps_the_complete_download_and_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("a.bin");
    let fill = |f: &mut File| -> Result<u64> {
        f.write_all(BODY)?;
        Ok(BODY.len() as u64)
    };
    let mut fails = |step: LinkStep, _: &Path| match step {
        LinkStep::Probe => Ok(()),
        LinkStep::Publish => Err(os_error(libc::EIO)),
    };
    let msg = save_new_with(&dest, fill, &mut fails)
        .unwrap_err()
        .to_string();
    assert!(!dest.exists());
    let part = private_folder(tmp.path()).unwrap().join("part");
    assert_eq!(std::fs::read(&part).unwrap(), BODY);
    assert!(msg.contains(&os_error(libc::EIO).to_string()), "{msg}");
    assert!(msg.contains(&*part.to_string_lossy()), "{msg}");
    assert!(!msg.contains("an existing file"), "{msg}");
}

/// A staging folder that cannot be removed is said, not dropped.
#[test]
fn a_staging_folder_that_cannot_be_removed_is_said() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("a.bin");
    let fill = |f: &mut File| -> Result<u64> {
        f.write_all(BODY)?;
        Ok(BODY.len() as u64)
    };
    let mut seen = Vec::new();
    let mut intrude = |step: LinkStep, private: &Path| {
        if step == LinkStep::Publish {
            // Something sopen did not make is now in its folder: it is left alone.
            std::fs::write(private.join("intruder"), b"")?;
            seen.push(private.to_path_buf());
        }
        Ok(())
    };
    let saved = save_new_with(&dest, fill, &mut intrude).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    assert_eq!(saved.notes.len(), 1, "{:?}", saved.notes);
    assert!(
        saved.notes[0].contains("could not be removed"),
        "{:?}",
        saved.notes
    );
    assert!(saved.notes[0].contains(&*seen[0].to_string_lossy()));
    assert_eq!(
        names(&seen[0]),
        ["intruder"],
        "only sopen's own entries removed"
    );
}

/// A local (or cached) source that changes during the copy is not saved: truncated midway
/// (fewer bytes than its size), or written in place at the same size (its mtime and change
/// time move). Nothing is left.
#[test]
fn copy_to_refuses_a_source_that_changes_mid_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.bin");
    let big: Vec<u8> = (0..3u32 << 20).map(|i| (i % 251) as u8).collect();
    for case in ["truncated", "rewritten"] {
        std::fs::write(&src, &big).unwrap();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        let dest = tmp.path().join(format!("{case}.bin"));
        let mut once = true;
        let result = copy_to(
            &src,
            &dest,
            &mut |_, _| {
                if std::mem::take(&mut once) {
                    let f = File::options().write(true).open(&src).unwrap();
                    if case == "truncated" {
                        f.set_len(1 << 20).unwrap();
                    } else {
                        f.write_all_at(&[0xEE; 4096], 2 << 20).unwrap();
                    }
                }
            },
            &Decision::default(),
        );
        match result {
            Err(SaveError::Failed(e)) => assert!(
                format!("{e:#}").contains("changed while it was copied"),
                "{case}: {e:#}"
            ),
            other => panic!("{case}: {other:?}"),
        }
        assert!(!dest.exists(), "{case}");
    }
    assert_eq!(names(tmp.path()), ["src.bin"], "no staging folder left");
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
        &Decision::default(),
    );
    match result {
        Err(SaveError::Failed(e)) => assert!(format!("{e:#}").contains("changed"), "{e:#}"),
        other => panic!("{other:?}"),
    }
    assert!(!dest.exists());
    assert_eq!(names(tmp.path()), ["src.bin"]);
    // Quiet: saved whole.
    let saved = download_to(
        &mut LocalFs::new(),
        src.to_str().unwrap(),
        &dest,
        &mut |_, _| {},
        &Decision::default(),
    )
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap().len() as u64, saved.bytes);
}

/// 06 §7.5, the one decision: a cancel that comes during the download's final stat — after the
/// last byte, before the publish — still wins. Nothing is under the name, nothing is left of the
/// staging, and the error is the cancel's.
#[test]
fn w8a_a_cancel_during_the_final_remote_stat_publishes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.bin");
    std::fs::write(&src, BODY).unwrap();
    let dest = tmp.path().join("dest.bin");
    let decision = Arc::new(Decision::default());
    let cancel = decision.clone();
    let mut fs = Counting {
        fs: Box::new(LocalFs::new()),
        reads: Arc::default(),
        hold: None,
        refuse: Arc::default(),
        // The stat before the read is the first; the one after it the second.
        at_stat: Some(Arc::new(move |n| {
            if n == 2 {
                assert!(cancel.cancel(), "nothing is published yet");
            }
        })),
        stats: 0,
    };
    let result = download_to(
        &mut fs,
        src.to_str().unwrap(),
        &dest,
        &mut |_, _| {},
        &decision,
    );
    assert_eq!(fs.stats, 2, "the cancel came inside the final stat");
    assert!(
        result.as_ref().is_err_and(save_cancelled),
        "{:?}",
        result.map(|s| s.bytes)
    );
    assert!(!dest.exists(), "nothing is published");
    assert_eq!(names(tmp.path()), ["src.bin"], "no staging folder left");
}

/// The same decision for a local copy: a cancel from the last progress report (the whole file
/// copied, the read at its end) wins, and so does one made before an empty file is copied —
/// which has no chunk to stop between.
#[test]
fn w8a_a_cancel_at_the_end_of_a_local_copy_publishes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.bin");
    std::fs::write(&src, BODY).unwrap();
    let dest = tmp.path().join("dest.bin");
    let decision = Decision::default();
    let result = copy_to(
        &src,
        &dest,
        &mut |done, total| {
            if done == total {
                assert!(decision.cancel());
            }
        },
        &decision,
    );
    assert!(decision.cancelled(), "the last report came");
    assert!(
        result.as_ref().is_err_and(save_cancelled),
        "the last report"
    );
    assert!(!dest.exists());
    let empty = tmp.path().join("empty.bin");
    std::fs::write(&empty, b"").unwrap();
    let decision = Decision::default();
    assert!(decision.cancel());
    let result = copy_to(&empty, &dest, &mut |_, _| {}, &decision);
    assert!(result.as_ref().is_err_and(save_cancelled), "an empty file");
    assert!(!dest.exists());
    assert_eq!(
        names(tmp.path()),
        ["empty.bin", "src.bin"],
        "no staging left"
    );
}

/// The decision is taken right before the complete file gets its name — after the fill, its
/// sync and its mode. A cancel that comes just before that publishes nothing and leaves nothing;
/// one that comes after is refused, and the save is reported as what it is: saved.
#[test]
fn w8a_cancel_and_publish_are_decided_once_right_before_the_link() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest.bin");
    let fill = |file: &mut File| -> Result<u64> {
        file.write_all(BODY)?;
        Ok(BODY.len() as u64)
    };
    // Just before publication: the part is complete, synced, and has its mode.
    let decision = Decision::default();
    let result = save_new_deciding(
        &dest,
        fill,
        &mut |step, staging| {
            if step == LinkStep::Publish {
                let part = std::fs::read(staging.join("part")).unwrap();
                assert_eq!(part, BODY, "the complete file is staged");
                assert!(decision.cancel());
            }
            Ok(())
        },
        &decision,
    );
    assert!(result.as_ref().is_err_and(save_cancelled), "cancel won");
    assert!(!dest.exists(), "nothing is published");
    assert_eq!(names(tmp.path()), Vec::<String>::new(), "no staging left");
    // After it: the publish has won, and the cancel is told so.
    let decision = Decision::default();
    let saved = save_new_deciding(&dest, fill, &mut |_, _| Ok(()), &decision).unwrap();
    assert_eq!(saved.bytes, BODY.len() as u64);
    assert!(!decision.cancel(), "too late: the file has its name");
    assert!(!decision.cancelled());
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    assert_eq!(names(tmp.path()), ["dest.bin"]);
}

/// A save whose publication wins the decision and then loses the name: something takes the
/// destination between the decision and the link.
fn taken_at_the_last_moment(
    dest: &Path,
    decision: &Decision,
) -> std::result::Result<SavedTo, SaveError> {
    let fill = |file: &mut File| -> Result<u64> {
        file.write_all(BODY)?;
        Ok(BODY.len() as u64)
    };
    save_new_deciding(
        dest,
        fill,
        &mut |step, _| {
            if step == LinkStep::Publish {
                std::fs::write(dest, b"someone else's file").unwrap();
            }
            Ok(())
        },
        decision,
    )
}

/// Publication wins the decision, then the link itself fails: the name was taken at the last
/// moment. The save is refused as `Exists` — not as cancelled, not as saved — the file there
/// keeps its contents, the staging is cleaned, and a cancel that comes after is still told it
/// was too late (the decision was the publish's, whatever the link then did).
#[test]
fn w8a_a_publish_that_won_and_then_lost_the_name_is_exists_not_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest.bin");
    let decision = Decision::default();
    let result = taken_at_the_last_moment(&dest, &decision);
    match &result {
        Err(SaveError::Exists { next }) => assert_eq!(next, &tmp.path().join("dest-1.bin")),
        other => panic!("{other:?}"),
    }
    assert!(!result.as_ref().is_err_and(save_cancelled));
    assert_eq!(std::fs::read(&dest).unwrap(), b"someone else's file");
    assert_eq!(names(tmp.path()), ["dest.bin"], "the staging is cleaned");
    assert!(!decision.cancel(), "too late: the publish had won");
    assert!(!decision.cancelled());
}

/// The dialog's own source, for the test above in the window: its save loses the name at the
/// last moment, with the window's own decision.
struct Taken;

impl Source for Taken {
    fn open_hex(&mut self, _: &mut dyn FnMut(u64, u64)) -> Result<Opened> {
        bail!("not in this test")
    }

    fn save_to(
        &mut self,
        dest: &Path,
        _: &mut dyn FnMut(u64, u64),
        cancel: &Decision,
    ) -> std::result::Result<SavedTo, SaveError> {
        taken_at_the_last_moment(dest, cancel)
    }
}

/// The same in the dialog: it says the name exists and was not replaced, and offers the next
/// one — it never says `saved`, and never `cancelled`; nothing is recorded as saved.
#[test]
fn w8a_the_dialog_does_not_say_saved_when_the_name_was_taken_at_the_last_moment() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("blob.zip");
    let hooks = Hooks {
        save_to: Some(dest.clone()),
        ..Hooks::default()
    };
    let info = Info {
        title: "box:/remote/blob.zip".into(),
        name: "blob.zip".into(),
        size: BODY.len() as u64,
        local: false,
    };
    let mut win = BinaryWindow::new(
        info,
        Box::new(Taken),
        Box::new(Picks(None, Arc::default())),
        &hooks,
    );
    let mut drv = Driver::new(WINDOW[0], 320.0);
    drv.idle(&mut win, 3);
    drv.click_text(&mut win, "Save to…");
    drv.until(&mut win, 20, |w| w.asking() && !w.notes.is_empty());
    drv.idle(&mut win, 3);
    let shown: Vec<String> = drv.texts().into_iter().map(|(_, _, text)| text).collect();
    let said = |what: &str| shown.iter().any(|text| text.contains(what));
    assert!(said("exists — not replaced"), "{shown:?}");
    assert!(said("blob-1.zip"), "the next free name: {shown:?}");
    assert!(!said("saved "), "{shown:?}");
    assert!(!said("cancelled"), "{shown:?}");
    assert!(win.ended.lock().unwrap().saved_to.is_none());
    assert_eq!(std::fs::read(&dest).unwrap(), b"someone else's file");
    assert_eq!(names(tmp.path()), ["blob.zip"], "the staging is cleaned");
}

// ---------------------------------------------------------------------------------------------
// The production source and the route main takes.

/// The local disk posing as a remote host, counting full reads (a fetch or a download is one)
/// and head reads.
struct Counting {
    fs: Box<dyn RemoteFs + Send>,
    reads: Arc<AtomicUsize>,
    /// A full read stops here once, at its first response, until the test lets it go.
    hold: Option<Hold>,
    /// Why the file is not opened for a stream, while it is not.
    refuse: Arc<Mutex<Option<String>>>,
    /// Called at every stat, with how many there have been (tests: a cancel inside one).
    at_stat: Option<AtStat>,
    stats: usize,
}

type AtStat = Arc<dyn Fn(usize) + Send + Sync>;

/// Where a held read says it is there, and waits to be let go (at most 20 s: a failed test
/// never leaves a worker hanging).
struct Hold {
    reached: mpsc::Sender<()>,
    go: Receiver<()>,
}

/// What a held read writes through: its first response stops at the gate, after it was taken.
struct HoldWriter<'a> {
    out: &'a mut dyn Write,
    hold: Option<Hold>,
}

impl Write for HoldWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.out.write(buf)?;
        if let Some(hold) = self.hold.take() {
            let _ = hold.reached.send(());
            let _ = hold.go.recv_timeout(Duration::from_secs(20));
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

impl RemoteFs for Counting {
    fn stat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
        self.stats += 1;
        if let Some(at_stat) = &self.at_stat {
            at_stat(self.stats);
        }
        self.fs.stat_opt(p)
    }
    fn lstat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
        self.fs.lstat_opt(p)
    }
    fn realpath(&mut self, p: &str) -> Result<String> {
        self.fs.realpath(p)
    }
    fn read_head(&mut self, p: &str, max: usize) -> Result<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.fs.read_head(p, max)
    }
    fn list_dir(&mut self, p: &str) -> Result<Vec<String>> {
        self.fs.list_dir(p)
    }
    fn list_entries(&mut self, p: &str) -> Result<Vec<crate::remote::Entry>> {
        self.fs.list_entries(p)
    }
    fn read_to(
        &mut self,
        p: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let mut held = HoldWriter {
            out,
            hold: self.hold.take(),
        };
        self.fs.read_to(p, &mut held, progress)
    }
    fn create_from(
        &mut self,
        local: &Path,
        p: &str,
        like: Option<crate::remote::Like>,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        self.fs.create_from(local, p, like)
    }
    fn set_mode(&mut self, p: &str, mode: u32) -> Result<()> {
        self.fs.set_mode(p, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.fs.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.fs.link_new(from, to)
    }
    fn remove(&mut self, p: &str) -> Result<()> {
        self.fs.remove(p)
    }
    /// The connection becomes the stream's transport (06 §2); a failure gives it back.
    fn into_ranged(
        self: Box<Self>,
        path: &str,
    ) -> std::result::Result<Box<dyn crate::remote::Ranged>, crate::remote::Refused> {
        let Counting {
            fs,
            reads,
            hold,
            refuse,
            ..
        } = *self;
        let back = |fs| -> Box<dyn RemoteFs + Send> {
            Box::new(Counting {
                fs,
                reads: Arc::clone(&reads),
                hold: None,
                refuse: Arc::clone(&refuse),
                at_stat: None,
                stats: 0,
            })
        };
        drop(hold);
        let refused = refuse.lock().unwrap().clone();
        if let Some(why) = refused {
            return Err(crate::remote::Refused {
                conn: back(fs),
                error: anyhow!(why),
            });
        }
        fs.into_ranged(path)
            .map_err(|refused| crate::remote::Refused {
                conn: back(refused.conn),
                error: refused.error,
            })
    }
}

struct NoMeter;

impl Meter for NoMeter {
    fn start(&mut self, _: &str) {}
    fn bytes(&mut self, _: u64, _: u64) {}
    fn finish(&mut self) {}
}

/// A file "on host box" (the local disk behind a counting layer), its cache, and the route.
struct Route {
    tmp: tempfile::TempDir,
    abs: String,
    reads: Arc<AtomicUsize>,
    /// Connections opened after the open's own (the `Origin`'s, a stream's retry).
    dials: Arc<AtomicUsize>,
    /// Why the file is not opened for a stream, while it is not.
    refuse: Arc<Mutex<Option<String>>>,
    /// What crossed the route's connections, counted below the `Source` ([`Metered`]): a stream
    /// may ask a first screen's block and 4 MiB more, a whole read may deliver 8 MiB — a route
    /// that reads the owner's 24 GB whole fails at once, not after the file.
    meter: Arc<Metered>,
}

/// What the first screen costs.
const BLOCK: u64 = crate::stream::BLOCK;

impl Route {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("remote/blob.zip");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, BODY).unwrap();
        Self {
            abs: file.to_str().unwrap().to_string(),
            tmp,
            reads: Arc::default(),
            dials: Arc::default(),
            refuse: Arc::default(),
            meter: Metered::new(BLOCK + (4 << 20), 8 << 20),
        }
    }

    /// The route on a file of `size` bytes that holds nothing (sparse: no disk is used).
    fn sized(size: u64) -> Self {
        let route = Self::new();
        let file = std::fs::File::create(&route.abs).unwrap();
        file.set_len(size).unwrap();
        route
    }

    fn dials(&self) -> usize {
        self.dials.load(Ordering::SeqCst)
    }

    fn cache(&self) -> PathBuf {
        self.tmp.path().join("cache")
    }

    fn source(&self) -> FileSource {
        self.source_over(Box::new(LocalFs::new()), None)
    }

    /// The production source over `fs` posing as host `box`; `hold` stops its first full read
    /// at its first response.
    fn source_over(&self, fs: Box<dyn RemoteFs + Send>, hold: Option<Hold>) -> FileSource {
        let fs = Counting {
            fs: self.meter.connection(fs),
            reads: Arc::clone(&self.reads),
            hold,
            refuse: Arc::clone(&self.refuse),
            at_stat: None,
            stats: 0,
        };
        let (reads, dials, refuse, meter) = (
            Arc::clone(&self.reads),
            Arc::clone(&self.dials),
            Arc::clone(&self.refuse),
            Arc::clone(&self.meter),
        );
        let reconnect: crate::gui::Reconnect = Box::new(move || {
            dials.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Counting {
                fs: meter.connection(Box::new(LocalFs::new())),
                reads: Arc::clone(&reads),
                hold: None,
                refuse: Arc::clone(&refuse),
                at_stat: None,
                stats: 0,
            }) as Box<dyn RemoteFs + Send>)
        });
        FileSource::new(
            Box::new(fs),
            Some(reconnect),
            Some("box".to_string()),
            &self.abs,
            &self.cache(),
            None,
        )
    }

    fn info(&self) -> Info {
        Info {
            title: format!("box:{}", self.abs),
            name: "blob.zip".to_string(),
            size: BODY.len() as u64,
            local: false,
        }
    }

    fn hooks(&self) -> Hooks {
        Hooks {
            save_to: Some(self.tmp.path().join("out.zip")),
            ..Hooks::default()
        }
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    /// The route, with `drive` playing the owner in the dialog window.
    fn open(
        &self,
        hooks: &Hooks,
        drive: impl FnOnce(&mut Driver, &mut BinaryWindow),
    ) -> FileOutcome {
        let mut drive = Some(drive);
        open_file(
            self.info(),
            self.source(),
            hooks,
            false,
            true,
            &mut NoMeter,
            Box::new(Picks(None, Arc::default())),
            &mut |mut win| {
                let mut drv = Driver::new(WINDOW[0], 320.0);
                (drive.take().expect("one window"))(&mut drv, &mut win);
                Ok(())
            },
        )
        .unwrap()
    }
}

/// The route main takes for a binary file, with the production source and no answer preset:
/// the dialog asks, nothing is read and nothing reaches the cache before the choice; Esc
/// cancels and still nothing has been.
#[test]
fn the_route_main_takes_downloads_nothing_before_the_choice() {
    let route = Route::new();
    let mut asked = false;
    let out = route.open(&route.hooks(), |drv, win| {
        drv.idle(win, 10);
        assert!(win.asking(), "the dialog asks");
        assert!(drv.find_text("Open in hex editor").is_some());
        assert_eq!(route.reads(), 0, "read before the choice");
        assert!(
            walk(&route.cache()).is_empty(),
            "{:?}",
            walk(&route.cache())
        );
        drv.key(win, Key::Escape, Modifiers::NONE);
        assert!(win.close_ok);
        asked = true;
    });
    assert!(asked);
    assert!(out.saved_to.is_none() && out.action.is_none() && out.fetch_ms.is_none());
    assert_eq!(
        out.cancelled.as_deref(),
        Some("sopen: cancelled; nothing was downloaded")
    );
    assert_eq!(route.reads(), 0);
    assert!(walk(&route.cache()).is_empty());
    assert!(!route.tmp.path().join("out.zip").exists());
}

/// Closed while the hex window was being opened: the open ends, its stream is dropped unused
/// and the window closes. Nothing was fetched (06 §7.5), so nothing is in the cache and the
/// closing line claims no download.
#[test]
fn a_close_while_the_hex_window_opens_closes_without_it() {
    let route = Route::new();
    let out = route.open(&route.hooks(), |drv, win| {
        drv.idle(win, 3);
        let ctx = drv.ctx.clone();
        win.start_hex(&ctx);
        assert!(!win.close_requested(), "waits for the open");
        drv.until(win, 20, |w| w.close_ok);
    });
    assert_eq!(route.reads(), 0, "nothing was read whole");
    assert!(out.action.is_none(), "the hex window did not open");
    assert!(out.fetch_ms.is_none(), "nothing was fetched");
    assert_eq!(out.cancelled.as_deref(), Some("sopen: cancelled"));
    assert!(walk(&route.cache()).is_empty());
}

// ---------------------------------------------------------------------------------------------
// 06 §13 W8a: the routes open a stream.

const FIRST_ROW: &str = "50 4B 03 04 20 61 20 7A  69 70 2C 20 61 73 20 66";

/// 06 §12 V1 at the hex answer — the step the dialog's Enter, `SOPEN_CHOICE=hex` and `--hex`
/// all run: a remote file of the owner's size (24 GB, here sparse) opens through a stream.
/// Nothing of it is read whole or reaches the cache, the open's connection is the stream's and
/// no other is made — whether the server can replace a file is not asked at open (06 §2) — and
/// the first screen costs one block.
#[test]
fn w8a_the_hex_answer_opens_a_stream_and_fetches_one_block() {
    const SIZE: u64 = 24_000_000_000;
    let route = Route::sized(SIZE);
    let mut source = route.source();
    let opened = source.open_hex(&mut |_, _| {}).unwrap();
    let stream = opened
        .file
        .stream()
        .expect("a remote file is viewed through a stream");
    assert_eq!(stream.size(), SIZE);
    assert_eq!(opened.fetch_ms, None, "nothing was fetched to open it");
    assert_eq!(route.reads(), 0, "no whole read");
    assert!(
        opened.file.can_replace(),
        "taken as yes until the first Save"
    );
    assert_eq!(route.dials(), 0, "the Origin connects on its first use");
    let mut win = hex::embedded(
        opened.file,
        &Hooks::default(),
        opened.notices,
        Arc::default(),
        Arc::default(),
    )
    .unwrap();
    let mut drv = Driver::new(760.0, 560.0);
    drv.until(&mut win, 60, |w| w.ready());
    assert!(
        drv.find_text("00 00 00 00 00 00 00 00  00 00 00 00 00 00 00 00")
            .is_some(),
        "the first rows"
    );
    // What crossed the connection, by the fixture's own count below the Source: one read, of
    // block 0, answered once; nothing read whole; no budget passed.
    assert_eq!(
        route.meter.reads_of(&route.abs),
        vec![(0, BLOCK as u32)],
        "one block before the first screen"
    );
    assert_eq!(route.meter.totals(), (BLOCK, 1, 0, 0));
    assert!(!route.meter.tripped());
    assert!(route.meter.heads.load(Ordering::SeqCst) <= 8 << 10);
    // And the product's own statistic says the same.
    assert_eq!(stream.fetched().total, BLOCK);
    assert_eq!(route.reads(), 0, "no whole read");
    assert_eq!(route.dials(), 0, "no second connection for viewing");
    assert!(
        walk(&route.cache()).is_empty(),
        "viewing writes nothing to disk: {:?}",
        walk(&route.cache())
    );
}

/// The standalone route (`sopen host:/file`, the dialog and Enter) and the dialog with its
/// answer preset (`SOPEN_CHOICE=hex`): the window becomes the hex window over a stream — no
/// whole read, nothing in the cache, no fetch time.
#[test]
fn w8a_the_dialog_streams_on_enter_and_on_a_preset_answer() {
    for preset in [false, true] {
        let route = Route::new();
        let hooks = Hooks {
            choice: preset.then_some(Choice::Hex),
            ..route.hooks()
        };
        let out = route.open(&hooks, |drv, win| {
            drv.idle(win, 3);
            if !preset {
                drv.key(win, Key::Enter, Modifiers::NONE);
            }
            drv.until(win, 20, |w| matches!(w.stage, Stage::Hex(_)));
            drv.until(win, 20, |w| w.ready());
            assert!(drv.find_text(FIRST_ROW).is_some(), "the hex grid");
        });
        assert_eq!(route.reads(), 0, "preset {preset}: no whole read");
        // The fixture's count: the 41-byte file crossed once, as one stream reply.
        assert_eq!(
            route.meter.totals(),
            (BODY.len() as u64, 1, 0, 0),
            "preset {preset}"
        );
        assert_eq!(route.dials(), 0, "preset {preset}: one connection");
        assert_eq!(out.action, Some("open"));
        assert_eq!(out.fetch_ms, None, "a streamed open has no fetch time");
        assert!(
            walk(&route.cache()).is_empty(),
            "preset {preset}: {:?}",
            walk(&route.cache())
        );
    }
}

/// `--hex` (06 §7.5) opens a stream; with `SOPEN_AUTOSAVE` its Save runs without a window. The
/// open's connection is the stream's, so the Save is the `Origin`'s first use: it connects
/// then, once, and fetches the whole file for the interim Save (06 §7.1) — the one whole read.
#[test]
fn w8a_the_hex_flag_streams_and_its_save_is_the_origins_first_use() {
    let route = Route::new();
    let hooks = Hooks {
        autosave: true,
        state: crate::gui::State {
            patches: vec![(0, vec![0xAA])],
            ..Default::default()
        },
        ..route.hooks()
    };
    let out = open_file(
        route.info(),
        route.source(),
        &hooks,
        true,
        true,
        &mut NoMeter,
        Box::new(Picks(None, Arc::default())),
        &mut |_| panic!("no window with SOPEN_AUTOSAVE"),
    )
    .unwrap();
    assert_eq!(out.action, Some("save"));
    assert_eq!(out.fetch_ms, None, "a streamed open has no fetch time");
    let now = std::fs::read(&route.abs).unwrap();
    assert_eq!(now[0], 0xAA);
    assert_eq!(now[1..], BODY[1..]);
    assert_eq!(route.dials(), 1, "the Origin connected at its first Save");
    assert_eq!(
        route.reads(),
        2,
        "the interim Save's whole fetch, and the overwrite's re-read of the target (01 §5)"
    );
}

/// The words `ssh` has for a session the server refused, as a failed start carries them
/// (`remote::sftp`'s `a_refused_session_reaches_the_starts_error` shows they get there).
const REFUSED: &str = "cannot start sftp on box: unexpected end of file: ssh said: \
     mux_client_request_session: session request failed: Session open refused by peer";

/// 06 §12 V10 at open, in the dialog: Enter while the hex answer fails as sshd's refusal of a
/// session does. The dialog shows 06 §6's line — not ssh's words — and asks again, as it was:
/// it kept its connection, so Enter once sessions are free opens the stream on it, with no
/// second connection, and nothing was fetched whole meanwhile.
#[test]
fn w8a_v10_a_refused_session_is_said_in_the_dialog_which_recovers() {
    let route = Route::new();
    *route.refuse.lock().unwrap() = Some(REFUSED.into());
    let out = route.open(&route.hooks(), |drv, win| {
        drv.idle(win, 3);
        drv.key(win, Key::Enter, Modifiers::NONE);
        drv.until(win, 20, |w| w.asking() && !w.notes.is_empty());
        drv.idle(win, 3);
        let shown: String = drv
            .texts()
            .iter()
            .map(|(_, _, text)| text.replace('\n', " "))
            .collect::<Vec<_>>()
            .join(" ");
        let shown = shown.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            shown.contains(crate::stream::SESSION_REFUSED),
            "the dialog shows the line: {shown}"
        );
        assert!(!shown.contains("mux_client"), "not ssh's words: {shown}");
        assert!(drv.find_text("Open in hex editor").is_some(), "as it was");
        assert_eq!(route.reads(), 0, "never a quiet whole fetch instead");
        assert_eq!(route.meter.totals(), (0, 0, 0, 0), "nothing crossed");
        // Sessions are free again: the same Enter opens it, on the connection it kept.
        *route.refuse.lock().unwrap() = None;
        drv.key(win, Key::Enter, Modifiers::NONE);
        drv.until(win, 20, |w| matches!(w.stage, Stage::Hex(_)));
        drv.until(win, 20, |w| w.ready());
        assert!(drv.find_text(FIRST_ROW).is_some(), "the hex grid");
    });
    assert_eq!(out.action, Some("open"));
    assert_eq!(route.dials(), 0, "on the connection it kept");
    assert_eq!(route.meter.totals(), (BODY.len() as u64, 1, 0, 0));
}

/// The same refusal under the dialog's other answer: the source keeps its connection, so
/// "Save to…" still downloads.
#[test]
fn w8a_v10_a_refused_hex_open_leaves_save_to_working() {
    let route = Route::new();
    *route.refuse.lock().unwrap() = Some(REFUSED.into());
    let mut source = route.source();
    let err = source
        .open_hex(&mut |_, _| {})
        .err()
        .expect("the session was refused");
    assert_eq!(format!("{err:#}"), crate::stream::SESSION_REFUSED);
    assert!(source.fs.is_some(), "the connection stays with the dialog");
    let dest = route.tmp.path().join("saved.zip");
    source
        .save_to(&dest, &mut |_, _| {}, &Decision::default())
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
}

/// "Any size" includes none: an empty remote file opens through a stream, and the hex window
/// comes ready.
#[test]
fn w8a_an_empty_remote_file_opens_in_the_hex_window() {
    let route = Route::sized(0);
    let opened = route.source().open_hex(&mut |_, _| {}).unwrap();
    assert!(opened.file.stream().is_some());
    let mut win = hex::embedded(
        opened.file,
        &Hooks::default(),
        opened.notices,
        Arc::default(),
        Arc::default(),
    )
    .unwrap();
    let mut drv = Driver::new(760.0, 560.0);
    drv.until(&mut win, 20, |w| w.ready());
    assert_eq!(route.reads(), 0);
}

/// 06 §12 V17: a local file is read in place — no stream — by the same step.
#[test]
fn w8a_v17_a_local_file_gets_no_stream() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("blob.bin");
    std::fs::write(&path, BODY).unwrap();
    let mut source = FileSource::new(
        Box::new(LocalFs::new()),
        None,
        None,
        path.to_str().unwrap(),
        &tmp.path().join("cache"),
        None,
    );
    let opened = source.open_hex(&mut |_, _| {}).unwrap();
    assert!(opened.file.stream().is_none(), "read in place");
    assert!(walk(&tmp.path().join("cache")).is_empty());
}

/// A dialog on a 4 MiB file "on host box" (this machine's SFTP server) whose download stops at
/// its first response until the test lets it go.
fn held_download() -> Option<(Route, FileSource, Receiver<()>, mpsc::Sender<()>)> {
    let server = crate::remote::SftpRemote::local_server()?;
    let route = Route::new();
    std::fs::write(&route.abs, vec![0x5A; 4 << 20]).unwrap();
    let (reached_tx, reached) = mpsc::channel();
    let (go, go_rx) = mpsc::channel();
    let hold = Hold {
        reached: reached_tx,
        go: go_rx,
    };
    let source = route.source_over(Box::new(server), Some(hold));
    Some((route, source, reached, go))
}

/// A cancelled download took its first response and no other: by the fixture's count, the
/// one after the cancel was offered and refused, none came after it, and of the file's 4 MiB
/// at most one response's bytes were written. Nothing reached the cache either.
fn assert_stopped_at_its_first_response(route: &Route) {
    let (_, _, bytes, reads) = route.meter.totals();
    let responses = route.meter.whole_responses.load(Ordering::SeqCst);
    assert_eq!(reads, 1, "one download");
    assert_eq!(responses, 2, "the response after the cancel ended it");
    assert!(
        bytes > 0 && bytes <= u64::from(crate::remote::PIPE_REQUEST),
        "{bytes} bytes taken of {}",
        4 << 20
    );
    assert!(!route.meter.tripped());
    assert!(
        walk(&route.cache()).is_empty(),
        "nothing in the cache: {:?}",
        walk(&route.cache())
    );
}

/// What "Save to…" left beside its target: nothing — no file, no private staging folder.
fn left_in(dir: &Path) -> Vec<String> {
    names(dir)
        .into_iter()
        .filter(|n| n != "remote" && n != "cache")
        .collect()
}

/// 06 §12 V21: Esc during "Save to…" cancels it. The transfer stops between its responses, the
/// part and the private folder are removed, nothing is published, the dialog asks again and
/// says so — and the next "Save to…" saves.
#[test]
fn w8a_v21_esc_cancels_a_save_to_and_leaves_nothing() {
    let Some((route, source, reached, go)) = held_download() else {
        return;
    };
    let hooks = route.hooks();
    let mut win = BinaryWindow::new(
        route.info(),
        Box::new(source),
        Box::new(Picks(None, Arc::default())),
        &hooks,
    );
    let mut drv = Driver::new(WINDOW[0], 320.0);
    drv.idle(&mut win, 3);
    drv.click_text(&mut win, "Save to…");
    assert!(matches!(win.stage, Stage::Moving { .. }));
    reached
        .recv_timeout(Duration::from_secs(10))
        .expect("the download began");
    drv.key(&mut win, Key::Escape, Modifiers::NONE);
    drop(go);
    drv.until(&mut win, 20, |w| !matches!(w.stage, Stage::Moving { .. }));
    assert!(win.asking(), "the dialog asks again");
    drv.idle(&mut win, 2);
    assert!(
        drv.find_text("cancelled; nothing was saved").is_some(),
        "the dialog says so"
    );
    assert_eq!(left_in(route.tmp.path()), Vec::<String>::new());
    assert!(win.ended.lock().unwrap().saved_to.is_none());
    assert_stopped_at_its_first_response(&route);
    // The source came back whole: the next Save to… downloads.
    drv.click_text(&mut win, "Save to…");
    drv.until(&mut win, 20, |w| matches!(w.stage, Stage::Saved { .. }));
    let saved = std::fs::read(route.tmp.path().join("out.zip")).unwrap();
    assert_eq!(saved.len(), 4 << 20);
}

/// 06 §12 V21: closing the window during "Save to…" cancels it the same way — the window does
/// not wait for the rest of the file — and the run's closing line says nothing was saved.
#[test]
fn w8a_v21_closing_cancels_a_save_to_and_leaves_nothing() {
    let Some((route, source, reached, go)) = held_download() else {
        return;
    };
    let hooks = route.hooks();
    let mut go = Some(go);
    let ended = super::dialog(
        route.info(),
        Box::new(source),
        Box::new(Picks(None, Arc::default())),
        &hooks,
        &mut |mut win| {
            let mut drv = Driver::new(WINDOW[0], 320.0);
            drv.idle(&mut win, 3);
            drv.click_text(&mut win, "Save to…");
            reached
                .recv_timeout(Duration::from_secs(10))
                .expect("the download began");
            assert!(!win.close_requested(), "until the transfer has stopped");
            drop(go.take());
            drv.until(&mut win, 20, |w| w.close_ok);
            assert!(win.close_requested());
            Ok(())
        },
    )
    .unwrap();
    assert!(ended.saved_to.is_none() && ended.hex.is_none());
    assert_eq!(
        cancel_line(false, ended.moved),
        "sopen: cancelled; nothing was saved"
    );
    assert_eq!(left_in(route.tmp.path()), Vec::<String>::new());
    assert_eq!(route.reads(), 1, "one download, stopped");
    assert_stopped_at_its_first_response(&route);
}

/// The same route, answered "Save to…": one download to the path, not into the cache. (The hex
/// answer is `w8a_the_dialog_streams_on_enter_and_on_a_preset_answer`.)
#[test]
fn the_route_main_takes_downloads_once_on_save_to() {
    let route = Route::new();
    let out = route.open(&route.hooks(), |drv, win| {
        drv.idle(win, 3);
        drv.click_text(win, "Save to…");
        drv.until(win, 20, |w| matches!(w.stage, Stage::Saved { .. }));
    });
    assert_eq!(route.reads(), 1, "one download");
    let dest = route.tmp.path().join("out.zip");
    assert_eq!(out.saved_to.as_deref(), Some(dest.as_path()));
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    assert!(
        walk(&route.cache()).is_empty(),
        "a download does not touch the cache"
    );
}

/// Opening the hex window can fail after the fetch (here the cached copy is not a regular file):
/// the source keeps the connection and the copy — a retry fails the same way and keeps them
/// again, Save to… then works from the copy (no second download), and a retry opens.
#[test]
fn a_failed_hex_open_keeps_the_source_for_save_and_a_retry() {
    let route = Route::new();
    let mut source = route.source();
    let copy = source.local_path(&mut |_, _| {}).unwrap().0;
    assert_eq!(route.reads(), 1);
    std::fs::remove_file(&copy).unwrap();
    std::fs::create_dir(&copy).unwrap();
    for attempt in ["first", "retry"] {
        let err = source
            .open_hex(&mut |_, _| {})
            .err()
            .expect("not a regular file");
        assert!(
            format!("{err:#}").contains("not a regular file"),
            "{attempt}: {err:#}"
        );
        assert!(
            source.fs.is_some() && source.fetched.is_some(),
            "{attempt}: the source was kept"
        );
    }
    std::fs::remove_dir(&copy).unwrap();
    std::fs::write(&copy, BODY).unwrap();
    let dest = route.tmp.path().join("saved.zip");
    source
        .save_to(&dest, &mut |_, _| {}, &Decision::default())
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    assert_eq!(
        route.reads(),
        1,
        "saved from the copy, not downloaded again"
    );
    let opened = source.open_hex(&mut |_, _| {}).unwrap();
    assert_eq!(opened.file.size(), BODY.len() as u64);
    assert!(source.fs.is_none(), "the connection went to the hex window");
}

/// A local file read in place: a failure to open it (not a regular file just then) keeps the
/// source; once it is a file again, Save to… copies it and a retry opens it.
#[test]
fn a_failed_local_hex_open_keeps_the_source() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("blob.bin");
    let abs = path.to_str().unwrap().to_string();
    std::fs::create_dir(&path).unwrap();
    let mut source = FileSource::new(
        Box::new(LocalFs::new()),
        None,
        None,
        &abs,
        &tmp.path().join("cache"),
        None,
    );
    assert!(source.open_hex(&mut |_, _| {}).is_err());
    assert!(source.fs.is_some());
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, BODY).unwrap();
    let dest = tmp.path().join("copy.bin");
    source
        .save_to(&dest, &mut |_, _| {}, &Decision::default())
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    assert!(source.open_hex(&mut |_, _| {}).is_ok());
    assert!(walk(&tmp.path().join("cache")).is_empty(), "read in place");
}

/// A scripted save on a binary file must say what to open (nothing would answer the dialog).
#[test]
fn autosave_without_an_answer_is_an_error_not_a_window() {
    let route = Route::new();
    let hooks = Hooks {
        autosave: true,
        ..route.hooks()
    };
    let err = open_file(
        route.info(),
        route.source(),
        &hooks,
        false,
        true,
        &mut NoMeter,
        Box::new(Picks(None, Arc::default())),
        &mut |_| panic!("a window was opened"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("need SOPEN_CHOICE=hex"), "{err}");
    assert_eq!(route.reads(), 0);
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
    // The default folder, from the folders the platform names (never the real ones here).
    let (dl, home) = (Path::new("/u/me/Downloads"), Path::new("/u/me"));
    assert_eq!(download_dir_from(Some(dl), Some(home)), dl);
    assert_eq!(download_dir_from(None, Some(home)), home);
    assert_eq!(download_dir_from(None, None), PathBuf::from("."));
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

/// The dialog is a PC Shell dialog box: its title in its frame, the hint on one line, the F-key
/// row at the bottom (under the box's shadow), where F3 closes it as Esc does, downloading
/// nothing.
#[test]
fn the_dialog_is_a_pc_shell_box_and_f3_closes_it() {
    let mut fx = dialog();
    assert!(
        fx.drv.find_text("Binary File").is_some(),
        "titled in its frame"
    );
    assert_eq!(fx.drv.rows_of(hint(false)), Some(1), "the hint is one line");
    assert!(fx.drv.find_text("Exit   ").is_some(), "the F-key row");
    let p = crate::gui::chrome::PALETTE;
    let name = fx.drv.find_text("blob.zip").expect("the name");
    let cyan = fx
        .drv
        .rects()
        .iter()
        .any(|(r, f, _)| *f == p.dialog_back && r.contains(name.center()));
    assert!(cyan, "on the cyan box");
    fx.drv.take_viewport_commands();
    fx.drv.key(&mut fx.win, Key::F3, egui::Modifiers::NONE);
    assert!(fx.drv.closed(), "F3 asks the window to close");
    assert!(fx.win.close_requested(), "and it may: nothing runs");
    assert_eq!(fx.calls(), (0, 0), "nothing was fetched or saved");
}
