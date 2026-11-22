//! The image window's edits as 07 §8 and §10 keep them, driven through its `ui` with real keys,
//! clicks and clipboard events: the journal on the local disk (its operations and pasted
//! pictures, and Restore replaying them on the file as decoded again), the whole-hash baseline
//! a Save leaves, no edit while a Save runs, Save As through the shared modal, and the close
//! prompt's four answers.

use super::*;
use crate::gui::testkit::{command, Driver};
use crate::journal::{self, Seen};
use crate::remote::{LocalFs, RemoteFs};
use eframe::egui::{Event, Key, Modifiers};
use image::Rgb;

const W: u32 = 120;
const H: u32 = 80;

/// Red is x, green is y: any two edits that differ leave different pixels.
fn coordinates() -> DynamicImage {
    DynamicImage::ImageRgb8(ImageBuffer::from_fn(W, H, |x, y| {
        Rgb([(x * 2) as u8, (y * 3) as u8, 7])
    }))
}

pub(super) fn bytes_of(format: ImageFormat) -> Vec<u8> {
    encode(&coordinates(), format, None, None).unwrap().bytes
}

/// The window [`open`] makes, on the file at `path` as it is now, with its cache at `cache`.
pub(super) fn window_on(path: &Path, cache: &Path) -> ImageWindow {
    let bytes = std::fs::read(path).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let file = ImageFile::new(origin, bytes, cache);
    let doc = Doc::decode(file.original()).unwrap();
    ImageWindow::new(file, doc, Selection::default(), DEFAULT_BLOCK, Vec::new())
}

struct Fx {
    tmp: tempfile::TempDir,
    path: PathBuf,
    cache: PathBuf,
    win: ImageWindow,
    drv: Driver,
}

fn open_file(name: &str, bytes: &[u8]) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    let cache = tmp.path().join("cache");
    let win = window_on(&path, &cache);
    let mut fx = Fx {
        tmp,
        path,
        cache,
        win,
        drv: Driver::new(1000.0, 700.0),
    };
    fx.drv.idle(&mut fx.win, 3);
    fx
}

fn open() -> Fx {
    open_file("shot.png", &bytes_of(ImageFormat::Png))
}

impl Fx {
    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.drv.key(&mut self.win, key, modifiers);
    }

    fn select(&mut self, rect: [u32; 4]) {
        self.win.sel.set(rect);
        self.drv.idle(&mut self.win, 1);
    }

    /// The window loses the keyboard (the owner went to another application).
    /// ...and the generation that asks for, once it is in (written on a worker).
    fn blur(&mut self) {
        let lost = vec![Event::WindowFocused(false)];
        self.drv.frame(&mut self.win, lost, Modifiers::NONE);
        self.drv.until(&mut self.win, 10, |w| w.quiet());
        self.drv.idle(&mut self.win, 1);
    }

    /// A new window on the same file and cache, as the next open makes it — the first one gone
    /// without its close prompt (a crash, a kill).
    fn reopen(&mut self) {
        let next = window_on(&self.path, &self.cache);
        drop(std::mem::replace(&mut self.win, next));
        self.drv = Driver::new(1000.0, 700.0);
        self.drv.idle(&mut self.win, 3);
    }

    fn status(&self) -> String {
        self.win.status.text.clone()
    }

    /// The journals of the file on disk (`<file>.journal-*`, none being removed).
    fn journals(&self) -> Vec<PathBuf> {
        journal_dirs(&self.cache, &self.path)
    }

    fn texts(&self) -> Vec<String> {
        self.drv.texts().into_iter().map(|(_, _, t)| t).collect()
    }

    /// A text drawn this pass that starts with `prefix` — whole, or cut to its room with the
    /// rest on hover (the status bar's).
    fn said(&self, prefix: &str) -> Option<String> {
        self.texts().into_iter().find(|t| t.starts_with(prefix))
    }

    /// Passes until `text` is on screen, or fails after a few seconds.
    fn until_shown(&mut self, text: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.drv.find_text(text).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "{text:?} never shown; on screen: {:?}",
                self.texts()
            );
            self.drv.idle(&mut self.win, 1);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Passes until no save, Save copy or encode runs.
    fn until_idle(&mut self) {
        self.drv.until(&mut self.win, 10, |w| w.quiet());
        self.drv.idle(&mut self.win, 2);
    }
}

pub(super) fn journal_dirs(cache: &Path, path: &Path) -> Vec<PathBuf> {
    journal_dirs_of(cache, crate::cache::LOCAL_HOST, path)
}

/// The journals of `path` on `host` (`<file>.journal-*`, none being removed).
pub(super) fn journal_dirs_of(cache: &Path, host: &str, path: &Path) -> Vec<PathBuf> {
    let base = crate::cache::edits_path(cache, host, path.to_str().unwrap());
    let (home, name) = (base.parent().unwrap(), base.file_name().unwrap());
    let prefix = format!("{}.journal-", name.to_string_lossy());
    let mut out: Vec<PathBuf> = std::fs::read_dir(home)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let n = p.file_name().unwrap().to_string_lossy();
                    n.starts_with(&prefix) && !n.ends_with(".gone")
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// The generation `CURRENT` names in the journal `dir`.
pub(super) fn current(dir: &Path) -> journal::Generation {
    let at = journal::parse_current(&std::fs::read(dir.join("CURRENT")).unwrap()).unwrap();
    journal::parse_generation(&std::fs::read(dir.join(format!("gen-{}", at.n))).unwrap()).unwrap()
}

/// The operation each line of a generation's edits names.
pub(super) fn words(gen: &journal::Generation) -> Vec<String> {
    gen.edits
        .lines()
        .map(|l| l.split(' ').next().unwrap_or_default().to_string())
        .collect()
}

/// A 6×4 picture as the clipboard hands one over: opaque blue with a white column.
pub(super) fn clipboard_picture() -> egui::ColorImage {
    let mut rgba = Vec::new();
    for _ in 0..4 {
        for x in 0..6 {
            rgba.extend_from_slice(if x == 2 {
                &[255, 255, 255, 255]
            } else {
                &[0, 0, 255, 255]
            });
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([6, 4], &rgba)
}

pub(super) fn alt_command() -> Modifiers {
    Modifiers::ALT | Modifiers::COMMAND
}

/// 07 §3, §8: every edit — paste of a picture and of text, crop, pixelize, cut — is one line of
/// the journal, the pasted picture a file of its own; focus loss writes it at once, and the
/// status bar says where it is kept. After a crash the next open offers it back; Restore
/// replays the operations on the file as decoded again and shows the same picture, unsaved.
#[test]
fn every_edit_is_kept_in_the_journal_and_restore_replays_it() {
    let mut fx = open();
    assert!(fx.journals().is_empty(), "nothing is kept before an edit");
    fx.select([10, 10, 40, 30]);
    fx.drv.paste_image(&mut fx.win, clipboard_picture());
    fx.select([60, 40, 50, 30]);
    fx.drv.paste_text(&mut fx.win, "ok");
    fx.select([0, 0, 100, 70]);
    fx.key(Key::C, Modifiers::NONE);
    fx.select([5, 5, 30, 20]);
    fx.key(Key::P, Modifiers::NONE);
    fx.select([50, 30, 20, 20]);
    fx.drv.cut(&mut fx.win);
    assert_eq!(fx.win.doc.size(), (100, 70), "cropped");
    fx.blur();
    let dirs = fx.journals();
    assert_eq!(dirs.len(), 1, "one journal for the session: {dirs:?}");
    let gen = current(&dirs[0]);
    assert_eq!(gen.head.kind, journal::Kind::Image);
    assert_eq!(
        words(&gen),
        ["paste", "text", "crop", "pixelize", "cut"],
        "{}",
        gen.edits
    );
    assert_eq!(gen.pastes.len(), 1, "the pasted picture is kept");
    let paste = dirs[0].join(format!("paste-{}.png", crate::cache::hex(&gen.pastes[0])));
    let kept = image::load_from_memory(&std::fs::read(&paste).unwrap()).unwrap();
    assert_eq!(kept.to_rgba8(), rgba_image(&clipboard_picture()));
    let whole = journal::sha256(&std::fs::read(&fx.path).unwrap());
    assert_eq!(gen.head.baseline.sha256, Some(whole));
    assert_eq!(gen.seen, Seen::whole(whole), "the whole file's SHA-256");
    let line = fx
        .said("unsaved — kept at ")
        .unwrap_or_else(|| panic!("the status bar says where: {:?}", fx.texts()));
    assert!(line.len() > "unsaved — kept at ".len(), "{line}");
    let shown = fx.win.doc.img.clone();
    let original = std::fs::read(&fx.path).unwrap();

    fx.reopen();
    fx.until_shown("Restore");
    let row = fx
        .drv
        .text_starting_with("unsaved edits from ")
        .unwrap_or_else(|| panic!("the recovery row: {:?}", fx.texts()));
    assert!(row.contains("5 operations"), "{row}");
    for button in ["Later", "Discard"] {
        assert!(fx.drv.find_text(button).is_some(), "{button}");
    }
    assert!(!fx.win.doc.dirty(), "nothing applied before Restore");
    fx.drv.click_text(&mut fx.win, "Restore");
    fx.drv.idle(&mut fx.win, 2);
    assert_eq!(fx.win.doc.img, shown, "the same picture, replayed");
    assert!(fx.win.doc.unsaved(), "restored edits are unsaved");
    assert!(fx.drv.text_starting_with("unsaved edits from ").is_none());
    assert_eq!(std::fs::read(&fx.path).unwrap(), original, "nothing saved");
    // The restored edits are this session's now: kept in the same journal, against the file
    // as opened this time, and the next edit goes on in it.
    fx.select([0, 0, 8, 8]);
    fx.key(Key::P, Modifiers::NONE);
    fx.blur();
    assert_eq!(fx.journals(), dirs);
    let again = current(&dirs[0]);
    assert!(again.at.n > gen.at.n);
    assert_eq!(again.edits, format!("{}\npixelize 0,0,8,8,16", gen.edits));
    assert_eq!(again.head.baseline.sha256, Some(whole));
    // Undo walks back through the replayed steps.
    fx.key(Key::Z, command(false));
    assert_eq!(fx.win.doc.img, shown);
    fx.key(Key::Z, command(false));
    assert_ne!(fx.win.doc.img, shown);
}

/// 07 §3 Recovery, E10b: Restore checks content, never only metadata. A byte changed with the
/// size and modification time put back replays nothing; the row says why, offers no Restore,
/// and the journal stays. Discard removes it.
#[test]
fn restore_refuses_a_file_whose_content_changed_and_keeps_the_journal() {
    let mut fx = open_file("shot.bmp", &bytes_of(ImageFormat::Bmp));
    fx.select([0, 0, 50, 40]);
    fx.key(Key::C, Modifiers::NONE);
    fx.blur();
    assert_eq!(fx.journals().len(), 1);
    // Another writer: one pixel byte, the size and mtime as they were.
    let mtime = std::fs::metadata(&fx.path).unwrap().modified().unwrap();
    let mut bytes = std::fs::read(&fx.path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&fx.path, &bytes).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&fx.path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    fx.reopen();
    fx.until_shown("Discard");
    let row = fx
        .drv
        .text_starting_with("unsaved edits from ")
        .unwrap_or_else(|| panic!("the recovery row: {:?}", fx.texts()));
    assert!(row.contains("content changed"), "{row}");
    assert!(fx.drv.find_text("Restore").is_none(), "never applied");
    assert!(!fx.win.doc.dirty());
    assert_eq!(fx.win.doc.size(), (W, H));
    assert_eq!(fx.journals().len(), 1, "the journal stays");
    fx.drv.click_text(&mut fx.win, "Discard");
    fx.drv.idle(&mut fx.win, 2);
    assert!(fx.journals().is_empty(), "Discard removes it");
    assert!(fx.drv.text_starting_with("unsaved edits from ").is_none());
}

/// 07 §3: a journal folder that holds nothing but its lock — or not even that: its session
/// ended between the `mkdir` and its first generation — holds no edit. The next open removes it
/// and shows no row.
#[test]
fn a_journal_folder_holding_nothing_is_removed_and_never_offered() {
    for locked in [false, true] {
        let mut fx = open();
        let base = crate::cache::edits_path(
            &fx.cache,
            crate::cache::LOCAL_HOST,
            fx.path.to_str().unwrap(),
        );
        let name = base.file_name().unwrap().to_string_lossy().into_owned();
        let dir = base.with_file_name(format!("{name}.journal-20261122T010203Z-0123abcd"));
        std::fs::create_dir_all(&dir).unwrap();
        if locked {
            std::fs::write(dir.join("lock"), b"").unwrap();
        }
        fx.reopen();
        fx.until_idle();
        assert!(
            fx.said("unsaved edits").is_none() && fx.drv.find_text("Later").is_none(),
            "locked={locked}: offered: {:?}",
            fx.texts()
        );
        let left: Vec<_> = std::fs::read_dir(dir.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert!(left.is_empty(), "locked={locked}: left {left:?}");
    }
}

/// 07 §3: another window's look for earlier journals that takes this window's journal folder
/// between its `mkdir` and its lock (a slow disk holds the writer there) — and removes it,
/// finding nothing in it — never costs this window its journal: its generation lands in one of
/// its own, which the status bar names.
#[test]
fn a_journal_folder_taken_before_its_lock_never_costs_the_window_its_journal() {
    let mut fx = open();
    fx.until_idle();
    let gap = crate::journal::gap::arm(&fx.cache);
    fx.select([0, 0, 50, 40]);
    fx.key(Key::C, Modifiers::NONE);
    // Focus loss writes the generation at once: its writer stops between folder and lock.
    let lost = vec![Event::WindowFocused(false)];
    fx.drv.frame(&mut fx.win, lost, Modifiers::NONE);
    gap.reached();
    let caught = fx.journals();
    assert_eq!(caught.len(), 1, "the folder, before its lock");
    let mut other = window_on(&fx.path, &fx.cache);
    let mut drv = Driver::new(1000.0, 700.0);
    // Its first pass starts the look; then until it is back.
    drv.idle(&mut other, 1);
    drv.until(&mut other, 10, |w| w.quiet());
    drv.idle(&mut other, 1);
    let said: Vec<String> = drv.texts().into_iter().map(|(_, _, t)| t).collect();
    assert!(
        !said.iter().any(|t| t.starts_with("unsaved edits")),
        "offered in the other window: {said:?}"
    );
    assert!(!caught[0].exists(), "the folder holding nothing is removed");
    gap.release();
    fx.until_idle();
    let kept = fx.journals();
    assert_eq!(kept.len(), 1, "{kept:?}; {:?}", fx.texts());
    assert_ne!(kept[0], caught[0], "a journal of its own");
    assert_eq!(words(&current(&kept[0])), ["crop"]);
    assert!(fx.said("unsaved — kept at ").is_some(), "{:?}", fx.texts());
}

/// 07 §3.1's next baseline: once a Save lands the journal goes (nothing is unsaved), and the
/// next edit's journal is against the file as saved — its size and its whole SHA-256 — not the
/// bytes the window first read.
#[test]
fn after_a_save_the_journal_goes_and_the_next_is_against_the_saved_files_whole_hash() {
    let mut fx = open();
    fx.select([0, 0, 90, 60]);
    fx.key(Key::C, Modifiers::NONE);
    fx.blur();
    assert_eq!(fx.journals().len(), 1);
    fx.key(Key::S, command(false));
    fx.until_idle();
    let saved = std::fs::read(&fx.path).unwrap();
    assert_eq!(Doc::decode(&saved).unwrap().size(), (90, 60), "saved");
    assert!(
        fx.journals().is_empty(),
        "nothing unsaved: the journal went"
    );
    fx.select([0, 0, 10, 10]);
    fx.key(Key::P, Modifiers::NONE);
    fx.blur();
    let dirs = fx.journals();
    assert_eq!(dirs.len(), 1);
    let gen = current(&dirs[0]);
    let whole = journal::sha256(&saved);
    assert_eq!(gen.head.baseline.size, saved.len() as u64);
    assert_eq!(
        gen.head.baseline.sha256,
        Some(whole),
        "the saved file's hash"
    );
    assert_eq!(gen.seen, Seen::whole(whole));
    assert_eq!(words(&gen), ["pixelize"], "only what is not in the file");
}

/// `SOPEN_STATE` `restore=1`: once the rows of the journals a killed session left are drawn,
/// the first row's Restore is pressed as a click would, once — the picture is the crop that
/// session kept, and the journal is this window's. Without it the row waits for the owner.
#[test]
fn restore_1_presses_the_first_rows_restore_once_it_is_drawn() {
    let mut fx = open();
    fx.select([0, 0, 70, 50]);
    fx.key(Key::C, Modifiers::NONE);
    fx.blur();
    let kept = fx.win.doc.img.clone();
    fx.reopen();
    assert!(fx.drv.find_text("Restore").is_some(), "the row waits");
    assert_eq!(fx.win.doc.size(), (W, H), "nothing restored unasked");
    let mut next = window_on(&fx.path, &fx.cache);
    next.restore_first = true;
    drop(std::mem::replace(&mut fx.win, next));
    fx.drv = Driver::new(1000.0, 700.0);
    fx.drv
        .until(&mut fx.win, 10, |w| w.listed && w.rows.is_empty());
    fx.drv.idle(&mut fx.win, 2);
    assert!(
        fx.win.doc.img == kept,
        "the picture is the crop that session kept"
    );
    assert!(
        fx.status().starts_with("restored 1 edit"),
        "{}",
        fx.status()
    );
    assert!(!fx.win.restore_first, "pressed once");
    assert_eq!(fx.journals().len(), 1, "this window's now");
}

/// `restore=1` with `SOPEN_KEYS`: the keys are played only after Restore was pressed, so a played
/// Cmd+S saves the restored picture.
#[test]
fn keys_played_after_restore_1_act_on_the_restored_picture() {
    let mut fx = open();
    fx.select([0, 0, 70, 50]);
    fx.key(Key::C, Modifiers::NONE);
    fx.blur();
    let kept = fx.win.doc.img.clone();
    let mut next = window_on(&fx.path, &fx.cache);
    next.restore_first = true;
    next.script = crate::gui::browser::parse_keys("Cmd+S").unwrap().into();
    drop(std::mem::replace(&mut fx.win, next));
    fx.drv = Driver::new(1000.0, 700.0);
    fx.drv.until(&mut fx.win, 20, crate::gui::Window::ready);
    let saved = Doc::decode(&std::fs::read(&fx.path).unwrap()).unwrap();
    assert_eq!(saved.size(), (70, 50), "{}", fx.status());
    assert!(saved.img == kept, "the restored picture was saved");
}

/// 07 §10, E18: while a Save runs, typing, paste, cut, crop, pixelize, undo and redo do nothing,
/// and the status row says why; once it ends, editing resumes.
#[test]
fn no_edit_undo_or_redo_while_a_save_runs() {
    let mut fx = open();
    fx.select([0, 0, 60, 60]);
    fx.key(Key::P, Modifiers::NONE);
    fx.select([0, 0, 30, 30]);
    fx.key(Key::P, Modifiers::NONE);
    fx.key(Key::Z, command(false));
    assert!(
        fx.win.doc.can_undo() && fx.win.doc.can_redo(),
        "something to undo and redo"
    );
    let (tx, rx) = mpsc::channel();
    fx.win.job = Some(Job {
        kind: JobKind::Save { close_after: false },
        rev: fx.win.doc.rev(),
        rx,
    });
    fx.select([10, 10, 20, 20]);
    // What the picture is, and its history, before each input.
    let state = |fx: &Fx| {
        (
            fx.win.doc.rev(),
            fx.win.doc.img.clone(),
            fx.win.doc.undo.len(),
            fx.win.doc.redo.len(),
            fx.win.doc.journal_edits().map(<[Edit]>::len),
        )
    };
    let before = state(&fx);
    type Input = Box<dyn Fn(&mut Fx)>;
    let inputs: [(&str, Input); 9] = [
        ("crop key", Box::new(|fx| fx.key(Key::C, Modifiers::NONE))),
        (
            "pixelize key",
            Box::new(|fx| fx.key(Key::P, Modifiers::NONE)),
        ),
        ("undo", Box::new(|fx| fx.key(Key::Z, command(false)))),
        ("redo", Box::new(|fx| fx.key(Key::Z, command(true)))),
        ("cut", Box::new(|fx| fx.drv.cut(&mut fx.win))),
        (
            "paste text",
            Box::new(|fx| fx.drv.paste_text(&mut fx.win, "x")),
        ),
        (
            "paste picture",
            Box::new(|fx| fx.drv.paste_image(&mut fx.win, clipboard_picture())),
        ),
        (
            "Undo button",
            Box::new(|fx| fx.drv.click_text(&mut fx.win, "Undo")),
        ),
        (
            "Crop button",
            Box::new(|fx| fx.drv.click_text(&mut fx.win, "Crop  C")),
        ),
    ];
    for (name, input) in &inputs {
        input(&mut fx);
        assert!(
            state(&fx) == before,
            "{name} changed the picture or its history"
        );
        assert!(fx.status().starts_with(SAVING), "{name}: {}", fx.status());
    }
    for name in ["Redo", "Pixelize  P"] {
        fx.drv.click_text(&mut fx.win, name);
        assert!(state(&fx) == before, "{name} button");
    }
    // The save ends: editing resumes — the same pastes take effect now (a paste is applied to
    // the picture at once; nothing is staged for later), and undo works again.
    tx.send(Err(anyhow!("the save failed (a test)"))).unwrap();
    fx.until_idle();
    let img = fx.win.doc.img.clone();
    fx.drv.paste_text(&mut fx.win, "x");
    assert!(fx.win.doc.img != img, "the text paste after the save");
    let img = fx.win.doc.img.clone();
    fx.drv.paste_image(&mut fx.win, clipboard_picture());
    assert!(fx.win.doc.img != img, "the picture paste after the save");
    fx.key(Key::Z, command(false));
    assert!(
        fx.win.doc.img == img,
        "undo works again: the picture paste undone"
    );
}

/// Types `path` into the Save As modal's field, over what it holds, and presses Enter.
fn save_as_to(fx: &mut Fx, path: &Path) {
    fx.until_shown("Save shot.png with your edits to a new local file");
    fx.key(Key::A, command(false));
    let text = vec![Event::Text(path.to_str().unwrap().to_string())];
    fx.drv.frame(&mut fx.win, text, Modifiers::NONE);
    fx.drv.idle(&mut fx.win, 1);
    fx.key(Key::Enter, Modifiers::NONE);
}

/// 07 §7, §8: F7 opens Save As, which writes the encoder's output to a new local file; the
/// remote file is untouched, and the edits count as put somewhere (no unsaved changes).
#[test]
fn f7_saves_the_encoders_output_as_a_new_local_file() {
    let mut fx = open();
    fx.select([0, 0, 70, 50]);
    fx.key(Key::C, Modifiers::NONE);
    let original = std::fs::read(&fx.path).unwrap();
    let want = fx.win.doc.encode().unwrap().bytes;
    fx.key(Key::F7, Modifiers::NONE);
    let dest = fx.tmp.path().join("out").join("mine.png");
    std::fs::create_dir(dest.parent().unwrap()).unwrap();
    save_as_to(&mut fx, &dest);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !dest.exists() || fx.win.doc.unsaved() {
        assert!(std::time::Instant::now() < deadline, "never saved");
        fx.drv.idle(&mut fx.win, 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read(&dest).unwrap(), want, "the encoder's output");
    assert_eq!(std::fs::read(&fx.path).unwrap(), original, "untouched");
    assert!(!fx.win.doc.unsaved(), "the edits are somewhere");
    assert!(fx.win.doc.dirty(), "but not in the file");
    assert!(fx.status().starts_with("saved "), "{}", fx.status());
}

/// 07 §7: Save As never writes over anything: a taken name is refused and the field offers the
/// next free one; the file there is untouched and the edits stay unsaved.
#[test]
fn save_as_never_writes_over_an_existing_file() {
    let mut fx = open();
    fx.select([0, 0, 70, 50]);
    fx.key(Key::C, Modifiers::NONE);
    let dest = fx.tmp.path().join("taken.png");
    std::fs::write(&dest, b"someone's file").unwrap();
    fx.key(Key::F7, Modifiers::NONE);
    save_as_to(&mut fx, &dest);
    fx.until_shown("taken.png exists — not replaced. Save again saves as taken-1.png");
    assert_eq!(std::fs::read(&dest).unwrap(), b"someone's file");
    assert!(fx.win.doc.unsaved());
}

/// 07 §10, E16: closing with unsaved edits asks Save · Save As… · Discard · Cancel; Discard's
/// hover names the kept edits it deletes, and Discard removes the journal and closes.
#[test]
fn the_close_prompt_offers_save_as_and_discard_deletes_the_kept_edits() {
    let mut fx = open();
    fx.select([0, 0, 70, 50]);
    fx.key(Key::C, Modifiers::NONE);
    assert!(!crate::gui::Window::close_requested(&mut fx.win));
    fx.drv.until(&mut fx.win, 10, |w| w.quiet());
    fx.drv.idle(&mut fx.win, 2);
    for button in ["Save", "Save As…", "Discard", "Cancel"] {
        assert!(
            fx.drv.find_text(button).is_some(),
            "{button}: {:?}",
            fx.texts()
        );
    }
    let dirs = fx.journals();
    assert_eq!(dirs.len(), 1, "closing asks for the journal at once");
    let discard = fx.drv.find_text("Discard").unwrap();
    fx.drv.move_to(&mut fx.win, discard.center());
    for _ in 0..90 {
        fx.drv.idle(&mut fx.win, 1);
        if fx.said("deletes the kept edits at ").is_some() {
            break;
        }
    }
    assert!(
        fx.said("deletes the kept edits at ").is_some(),
        "{:?}",
        fx.texts()
    );
    fx.drv.click_text(&mut fx.win, "Discard");
    assert!(fx.win.close_ok, "closes");
    assert!(fx.journals().is_empty(), "the kept edits are deleted");
}

/// 07 §10: Save As… in the close prompt opens the modal, and the window closes only once that
/// save has landed. Its writer held: the window stays, the edits unsaved and recoverable from
/// the journal, the destination not there; released: the encoder's output there, and closed.
#[test]
fn save_as_from_the_close_prompt_closes_once_it_lands() {
    let mut fx = open();
    let hold = Arc::new(super::file::Hold::default());
    hold.held.store(true, Ordering::SeqCst);
    fx.win.save_as_hold = Some(hold.clone());
    fx.select([0, 0, 70, 50]);
    fx.key(Key::C, Modifiers::NONE);
    let want = fx.win.doc.encode().unwrap().bytes;
    assert!(!crate::gui::Window::close_requested(&mut fx.win));
    fx.drv.until(&mut fx.win, 10, |w| w.quiet());
    fx.drv.idle(&mut fx.win, 2);
    fx.drv.click_text(&mut fx.win, "Save As…");
    let dest = fx.tmp.path().join("closing.png");
    save_as_to(&mut fx, &dest);
    fx.drv
        .until(&mut fx.win, 10, |_| hold.reached.load(Ordering::SeqCst));
    fx.drv.idle(&mut fx.win, 3);
    assert!(
        !fx.win.close_ok && !fx.drv.closed(),
        "stays while it writes"
    );
    assert!(fx.win.doc.unsaved());
    assert!(!dest.exists());
    let dirs = fx.journals();
    assert_eq!(dirs.len(), 1, "recoverable meanwhile");
    assert_eq!(words(&current(&dirs[0])), ["crop"]);
    hold.held.store(false, Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !fx.win.close_ok {
        assert!(std::time::Instant::now() < deadline, "never closed");
        fx.drv.idle(&mut fx.win, 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(fx.drv.closed(), "closed once it landed");
    assert_eq!(std::fs::read(&dest).unwrap(), want, "the encoder's output");
    fx.drv.idle(&mut fx.win, 2);
    assert!(
        fx.journals().is_empty(),
        "nothing unsaved: the journal went"
    );
}

/// Where the last pass drew `text` lowest: the prompt's button, below the toolbar's of the same
/// name.
fn lowest(drv: &Driver, text: &str) -> Option<Rect> {
    drv.texts()
        .into_iter()
        .filter(|(_, _, t)| t == text)
        .map(|(r, _, _)| r)
        .max_by(|a, b| a.top().total_cmp(&b.top()))
}

/// 07 §10, E16: the close prompt's answers, performed. Save writes the picture and closes; Cancel
/// keeps the window, the edits and the journal, and writes nothing; where Save is off, Save copy
/// writes the edited sibling and closes.
#[test]
fn the_close_prompts_save_cancel_and_save_copy_do_what_they_say() {
    // Save.
    let mut fx = open();
    fx.select([0, 0, 70, 50]);
    fx.key(Key::C, Modifiers::NONE);
    let want = fx.win.doc.encode().unwrap().bytes;
    assert!(!crate::gui::Window::close_requested(&mut fx.win));
    fx.drv.until(&mut fx.win, 10, |w| w.quiet());
    fx.drv.idle(&mut fx.win, 2);
    let save = lowest(&fx.drv, "Save").expect("the prompt's Save (below the toolbar's)");
    fx.drv.click(&mut fx.win, save.center());
    fx.drv.until(&mut fx.win, 10, |w| w.close_ok);
    assert_eq!(std::fs::read(&fx.path).unwrap(), want, "saved");
    assert!(fx.drv.closed(), "closed");
    // Cancel.
    let mut fx = open();
    let original = std::fs::read(&fx.path).unwrap();
    fx.select([0, 0, 70, 50]);
    fx.key(Key::C, Modifiers::NONE);
    let shown = fx.win.doc.img.clone();
    assert!(!crate::gui::Window::close_requested(&mut fx.win));
    fx.drv.until(&mut fx.win, 10, |w| w.quiet());
    fx.drv.idle(&mut fx.win, 2);
    fx.drv.click_text(&mut fx.win, "Cancel");
    fx.drv.idle(&mut fx.win, 3);
    assert!(
        !fx.win.prompt && !fx.win.close_ok && !fx.drv.closed(),
        "the window stays"
    );
    assert_eq!(fx.win.doc.img, shown, "the edits stay");
    assert!(fx.win.doc.unsaved());
    assert_eq!(fx.journals().len(), 1, "the journal stays");
    assert_eq!(
        std::fs::read(&fx.path).unwrap(),
        original,
        "nothing written"
    );
    // Save copy, where Save is off.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("shot.png");
    let bytes = bytes_of(ImageFormat::Png);
    std::fs::write(&path, &bytes).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = crate::testutil::NoReplace(LocalFs::new());
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let file = ImageFile::new(origin, bytes.clone(), &tmp.path().join("cache"));
    let doc = Doc::decode(file.original()).unwrap();
    let mut win = ImageWindow::new(file, doc, Selection::default(), DEFAULT_BLOCK, Vec::new());
    let mut drv = Driver::new(1000.0, 700.0);
    drv.until(&mut win, 10, |w| w.listed && w.quiet());
    win.doc.crop([0, 0, 50, 40]).unwrap();
    let want = win.doc.encode().unwrap().bytes;
    drv.idle(&mut win, 1);
    assert!(!crate::gui::Window::close_requested(&mut win));
    drv.until(&mut win, 10, |w| w.quiet());
    drv.idle(&mut win, 2);
    let copy = lowest(&drv, "Save copy").expect("the prompt's Save copy");
    drv.click(&mut win, copy.center());
    drv.until(&mut win, 10, |w| w.close_ok);
    assert!(drv.closed());
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "the file untouched");
    let copy = tmp.path().join("shot.edited.png");
    assert_eq!(std::fs::read(&copy).unwrap(), want, "the edited sibling");
}

/// 07 §3's `NOT kept on disk (<why>)`: Undo past a Save leaves a picture the journal cannot
/// write as operations on the file as saved; the status bar says so (the close prompt still
/// guards it).
#[test]
fn undo_past_a_save_is_said_to_be_not_kept() {
    let mut fx = open();
    fx.select([0, 0, 90, 60]);
    fx.key(Key::C, Modifiers::NONE);
    fx.key(Key::S, command(false));
    fx.until_idle();
    fx.key(Key::Z, command(false));
    assert!(fx.win.doc.unsaved());
    fx.blur();
    assert!(
        fx.said("unsaved — NOT kept on disk (").is_some(),
        "{:?}",
        fx.texts()
    );
    assert!(!crate::gui::Window::close_requested(&mut fx.win), "asked");
}

/// The window over a stream of the file at `path` (06 §7.3), as a remote file of `host`, with
/// `budget` before the memory line.
fn streaming_on(path: &Path, cache: &Path, budget: u64) -> Streaming {
    let abs = path.to_str().unwrap().to_string();
    let at = LocalFs::new().stat(&abs).unwrap();
    let transport = Box::new(LocalFs::new()).into_ranged(&abs).ok().unwrap();
    let reopen: crate::stream::Reopen = Box::new(|| Err(anyhow!("no second connection")));
    let source = Source::open(transport, reopen, Arc::new(|| {})).unwrap();
    let origin = Origin::new(Box::new(LocalFs::new()), None, Some("host".into()), abs, at);
    Streaming::new(
        Input::Stream(source),
        origin,
        cache,
        &Hooks::default(),
        Vec::new(),
        budget,
    )
    .unwrap()
}

/// Past the memory line: Continue, then the image window once the file is whole.
fn continued(path: &Path, cache: &Path, drv: &mut Driver) -> Streaming {
    let mut win = streaming_on(path, cache, 1000);
    drv.until(&mut win, 20, |w| w.asked);
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    drv.until(&mut win, 20, |w| w.image.is_some());
    drv.idle(&mut win, 3);
    win
}

/// E14 on a small JPEG: the memory line, Continue, crop; the journal's evidence is the hash the
/// streamed read took; after a kill the next open replays the crop on the file read again; Save
/// then backs up the original, uploads the encoded output and retires the journal.
#[test]
fn a_streamed_image_past_the_memory_line_keeps_restores_and_saves_its_crop() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("big.jpg");
    let bytes = bytes_of(ImageFormat::Jpeg);
    std::fs::write(&path, &bytes).unwrap();
    let cache = tmp.path().join("cache");
    let mut drv = Driver::new(1000.0, 700.0);
    let mut win = continued(&path, &cache, &mut drv);
    {
        let image = win.image.as_deref_mut().unwrap();
        image.sel.set([10, 10, 60, 40]);
    }
    drv.idle(&mut win, 1);
    drv.key(&mut win, Key::C, Modifiers::NONE);
    let lost = vec![Event::WindowFocused(false)];
    drv.frame(&mut win, lost, Modifiers::NONE);
    drv.until(&mut win, 10, |w| {
        w.image.as_deref().is_some_and(|i| i.quiet())
    });
    let base = crate::cache::edits_path(&cache, "host", path.to_str().unwrap());
    let dirs = {
        let home = base.parent().unwrap();
        let mut d: Vec<PathBuf> = std::fs::read_dir(home)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains(".journal-"))
            .collect();
        d.sort();
        d
    };
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    let gen = current(&dirs[0]);
    assert_eq!(gen.seen, Seen::whole(journal::sha256(&bytes)));
    assert_eq!(gen.head.host, "host");
    assert_eq!(words(&gen), ["crop"]);
    // Killed: the next open asks, continues, and offers the crop back.
    drop(win);
    let mut drv = Driver::new(1000.0, 700.0);
    let mut win = continued(&path, &cache, &mut drv);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while drv.find_text("Restore").is_none() {
        assert!(std::time::Instant::now() < deadline, "no Restore row");
        drv.idle(&mut win, 1);
    }
    drv.click_text(&mut win, "Restore");
    drv.idle(&mut win, 2);
    assert_eq!(win.image.as_deref().unwrap().doc.size(), (60, 40));
    // Save: the backup of the original first, then the encoded output over the file — read once
    // the Save has ended (it first waits in its queue for the journal, with no job yet).
    let since = win.image.as_deref().unwrap().saves_ended;
    drv.key(&mut win, Key::S, command(false));
    drv.until(&mut win, 20, |w| {
        w.image.as_deref().unwrap().save_ended(since)
    });
    drv.idle(&mut win, 2);
    let image = win.image.as_deref().unwrap();
    let saved = std::fs::read(&path).unwrap();
    assert_eq!(Doc::decode(&saved).unwrap().size(), (60, 40));
    assert_eq!(
        saved,
        image.doc.encode().unwrap().bytes,
        "the encoded output"
    );
    let backups = walk(&crate::cache::backup_root(&cache));
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(std::fs::read(&backups[0]).unwrap(), bytes);
    assert!(
        image.status.text.contains("original backed up at"),
        "{}",
        image.status.text
    );
    assert!(!dirs[0].exists(), "nothing unsaved: the journal went");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// 01 §5 through the window: another writer changed the file since it was read — Save is
/// refused, nothing is written over it, and the edits stay unsaved and kept in the journal.
#[test]
fn a_save_over_a_changed_file_is_refused_and_the_edits_stay_kept() {
    let mut fx = open();
    fx.select([0, 0, 90, 60]);
    fx.key(Key::C, Modifiers::NONE);
    std::fs::write(&fx.path, b"someone else's bytes").unwrap();
    fx.key(Key::S, command(false));
    fx.until_idle();
    assert_eq!(std::fs::read(&fx.path).unwrap(), b"someone else's bytes");
    assert!(fx.win.doc.unsaved());
    assert_eq!(fx.status(), "not saved: the file changed");
    let dirs = fx.journals();
    assert_eq!(dirs.len(), 1, "the edits are kept");
    assert_eq!(words(&current(&dirs[0])), ["crop"]);
}

/// The close prompt where Save is off (no `posix-rename`): Save copy stands in for Save, and all
/// four answers are on screen whole, in a wide window and in the narrowest.
#[test]
fn the_close_prompt_without_replace_offers_save_copy_and_save_as() {
    for width in [1000.0, 480.0] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("shot.png");
        let bytes = bytes_of(ImageFormat::Png);
        std::fs::write(&path, &bytes).unwrap();
        let abs = path.to_str().unwrap().to_string();
        let mut fs = crate::testutil::NoReplace(LocalFs::new());
        let at = fs.stat(&abs).unwrap();
        let origin = Origin::new(Box::new(fs), None, None, abs, at);
        let file = ImageFile::new(origin, bytes, &tmp.path().join("cache"));
        let doc = Doc::decode(file.original()).unwrap();
        let mut win = ImageWindow::new(file, doc, Selection::default(), DEFAULT_BLOCK, Vec::new());
        let mut drv = Driver::new(width, 420.0);
        drv.idle(&mut win, 2);
        win.doc.crop([0, 0, 50, 40]).unwrap();
        drv.idle(&mut win, 1);
        assert!(!crate::gui::Window::close_requested(&mut win));
        drv.idle(&mut win, 3);
        for button in ["Save copy", "Save As…", "Discard", "Cancel"] {
            assert!(drv.find_text(button).is_some(), "{button} at {width}");
        }
        // Save is the toolbar's alone (disabled there): the prompt has none.
        let saves = drv
            .texts()
            .into_iter()
            .filter(|(_, _, t)| t == "Save")
            .count();
        assert_eq!(saves, 1, "Save is off at {width}");
    }
}

/// 07 §3: Restore replays a journal's edits on the file they were made on, and on nothing else.
/// A Save in this session after the listing makes another file of it: the row's Restore goes,
/// and Restore asked for anyway replays nothing. Edits of this session's own keep Restore off
/// until they are undone.
#[test]
fn restore_is_checked_again_after_a_save_and_never_replays_on_another_file() {
    let mut fx = open();
    fx.select([0, 0, 90, 60]);
    fx.key(Key::C, Modifiers::NONE);
    fx.blur();
    fx.reopen();
    fx.until_shown("Restore");
    // An edit of this session: Restore waits for it to be undone.
    fx.select([0, 0, 10, 10]);
    fx.key(Key::P, Modifiers::NONE);
    let edited = fx.win.doc.img.clone();
    fx.drv.click_text(&mut fx.win, "Restore");
    assert_eq!(fx.win.doc.img, edited, "nothing replayed over an edit");
    assert!(
        fx.status().contains("undo your edits first"),
        "{}",
        fx.status()
    );
    // Undone: the picture is the file again; Restore would apply. Saved instead (another
    // crop): the file is another one now.
    fx.key(Key::Z, command(false));
    fx.select([0, 0, 100, 70]);
    fx.key(Key::C, Modifiers::NONE);
    fx.key(Key::S, command(false));
    fx.until_idle();
    assert_eq!(fx.win.doc.size(), (100, 70));
    assert!(fx.drv.find_text("Restore").is_none(), "{:?}", fx.texts());
    let row = fx
        .drv
        .text_starting_with("unsaved edits from ")
        .unwrap_or_else(|| panic!("the row stays: {:?}", fx.texts()));
    assert!(row.contains("content changed"), "{row}");
    let saved = fx.win.doc.img.clone();
    fx.win.restore(0);
    assert_eq!(fx.win.doc.img, saved, "nothing replayed on the saved file");
    assert!(!fx.win.doc.unsaved());
    assert!(fx.status().starts_with("not restored: "), "{}", fx.status());
}
