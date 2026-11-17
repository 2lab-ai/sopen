//! The hex window: the model, the write side, and the window driven by real input events (keys
//! with modifiers, typed text, clicks) through its `ui`.

use super::*;
use crate::gui::testkit::{command, Driver};
use crate::gui::Window;
use crate::remote::{LocalFs, RemoteFs};
use eframe::egui::{Event, Modifiers};
use std::sync::atomic::{AtomicBool, Ordering};

/// The first row spells something; the second is short and has non-printable bytes.
const FIXTURE: &[u8] = b"sopen hex view\x00\xffABC\n\x80";

struct Fx {
    tmp: tempfile::TempDir,
    /// The "remote" file Save writes.
    remote: PathBuf,
    /// The local file the window reads (the cached copy; the remote itself when opened in place).
    base: PathBuf,
    win: HexWindow,
    drv: Driver,
}

fn window_for(file: HexFile) -> HexWindow {
    let doc = HexDoc::new(file.base(), file.size());
    HexWindow::new(
        file,
        doc,
        Cursor::default(),
        Vec::new(),
        Arc::default(),
        Arc::default(),
    )
}

/// `bytes` as a remote file fetched into a cached copy (as the dialog does it): the window reads
/// the copy, Save writes the remote.
fn fetched(name: &str, bytes: &[u8], w: f32, h: f32) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("remote").join(name);
    std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
    std::fs::write(&remote, bytes).unwrap();
    let abs = remote.to_str().unwrap().to_string();
    let base = tmp.path().join("cache/local/copy").join(name);
    let mut fs = LocalFs::new();
    let at = fs.fetch(&abs, &base, &mut |_, _| {}).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let file = HexFile::open(origin, &base, None, &tmp.path().join("cache")).unwrap();
    let mut fx = Fx {
        tmp,
        remote,
        base,
        win: window_for(file),
        drv: Driver::new(w, h),
    };
    fx.drv.idle(&mut fx.win, 2);
    fx
}

/// A local file opened in place: the window reads it, Save replaces it.
fn in_place(path: &Path, tmp: tempfile::TempDir, w: f32, h: f32) -> Fx {
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let file = HexFile::open(origin, path, None, &tmp.path().join("cache")).unwrap();
    let mut fx = Fx {
        remote: path.to_path_buf(),
        base: path.to_path_buf(),
        tmp,
        win: window_for(file),
        drv: Driver::new(w, h),
    };
    fx.drv.idle(&mut fx.win, 2);
    fx
}

impl Fx {
    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.drv.key(&mut self.win, key, modifiers);
    }

    fn typed(&mut self, text: &str) {
        self.drv.frame(
            &mut self.win,
            vec![Event::Text(text.into())],
            Modifiers::NONE,
        );
        self.drv.idle(&mut self.win, 1);
    }

    fn at(&self) -> u64 {
        self.win.cursor.at
    }

    /// Where byte `at` is drawn in `pane` (it must be on screen).
    fn pos(&self, at: u64, pane: Pane) -> Pos2 {
        let l = self.win.layout.expect("drawn");
        let row = at / ROW - self.win.top_row;
        let i = at % ROW;
        let y = l.rows.top() + l.row_h * (row as f32 + 0.5);
        let x = match pane {
            Pane::Hex => l.hex_x + l.cw * (hex_col(i) as f32 + 1.0),
            Pane::Ascii => l.ascii_x + l.cw * (i as f32 + 0.5),
        };
        Pos2::new(x, y)
    }

    /// Passes until the grid's place on screen stops moving.
    fn settle(&mut self) {
        let mut stable = 0;
        for _ in 0..30 {
            let before = self.win.layout;
            self.drv.idle(&mut self.win, 1);
            stable = if self.win.layout == before {
                stable + 1
            } else {
                0
            };
            if stable >= 3 {
                return;
            }
        }
        panic!("the layout never settled");
    }

    /// The save the last input started has finished (a small one can finish within the passes
    /// that delivered the key: "started" is the job running or its result already in).
    fn wait_for_save(&mut self) {
        let text = &self.win.status.text;
        assert!(
            self.win.job.is_some()
                || ["saved ", "not saved", "save failed"]
                    .iter()
                    .any(|s| text.starts_with(s)),
            "no save started: {text}"
        );
        self.drv.until(&mut self.win, 20, |w| w.job.is_none());
        self.drv.idle(&mut self.win, 2);
    }
}

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

#[test]
fn offsets_and_bytes_parse_and_format() {
    assert_eq!(parse_offset("0x1F").unwrap(), 31);
    assert_eq!(parse_offset(" 0Xff ").unwrap(), 255);
    assert_eq!(parse_offset("1234").unwrap(), 1234);
    for bad in ["", "0x", "+5", "-1", "12a", "0xg", "ff"] {
        assert!(parse_offset(bad).is_err(), "{bad}");
    }
    assert_eq!(parse_bytes("00Ff7a").unwrap(), [0, 255, 0x7a]);
    for bad in ["", "f", "0g", "1 2"] {
        assert!(parse_bytes(bad).is_err(), "{bad}");
    }
    assert_eq!(grouped(0), "0");
    assert_eq!(grouped(999), "999");
    assert_eq!(grouped(1_234_567), "1,234,567");
    assert_eq!(offset_digits(0), 4);
    assert_eq!(offset_digits(0x1_0000), 4);
    assert_eq!(offset_digits(0x1_0001), 5);
    assert_eq!(offset_digits(256 << 20), 7);
    assert_eq!(offset_digits(u64::MAX), 16);
    let row: Vec<Option<u8>> = (0..16).map(Some).collect();
    assert_eq!(
        hex_cells(&row),
        "00 01 02 03 04 05 06 07  08 09 0A 0B 0C 0D 0E 0F"
    );
    assert_eq!(hex_cells(&[Some(0xab), None]), "AB ??");
    assert_eq!(ascii_char(Some(b'~')), '~');
    assert_eq!(ascii_char(Some(0x7f)), '.');
    assert_eq!(ascii_char(None), '.');
    // The hex column's grid: two digits and a space per byte, one more space after the 8th.
    assert_eq!(hex_col(7), 21);
    assert_eq!(hex_col(8), 25);
    assert_eq!(hex_cells(&row).len() as u64, hex_col(15) + 2);
}

#[test]
fn the_model_overwrites_nibbles_and_characters_and_undoes_them() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.bin");
    std::fs::write(&path, FIXTURE).unwrap();
    let mut doc = HexDoc::new(Arc::new(File::open(&path).unwrap()), FIXTURE.len() as u64);
    let c = Cursor::default();
    // 's' = 0x73: '4' replaces the high nibble (0x43), the cursor stays for the low one.
    let c = doc.type_char(c, '4').unwrap();
    assert_eq!((doc.byte(0), c.at, c.low), (Some(0x43), 0, true));
    let c = doc.type_char(c, 'a').unwrap();
    assert_eq!((doc.byte(0), c.at, c.low), (Some(0x4a), 1, false));
    assert_eq!(doc.undo.len(), 1, "two digits, one step");
    assert_eq!(doc.type_char(c, 'g'), None, "not a hex digit");
    let c = Cursor {
        pane: Pane::Ascii,
        ..c
    };
    let c = doc.type_char(c, '!').unwrap();
    assert_eq!((doc.byte(1), c.at), (Some(b'!'), 2));
    assert_eq!(doc.type_char(c, 'é'), None, "not ASCII");
    assert_eq!(doc.changed_count(), 2);
    // Writing the original value back is no change.
    assert!(doc.set(2, b'p', c));
    assert_eq!(doc.changed_count(), 2);
    // The file itself is never written.
    assert_eq!(std::fs::read(&path).unwrap(), FIXTURE);
    // At the last byte the cursor stays there.
    let last = Cursor {
        at: FIXTURE.len() as u64 - 1,
        pane: Pane::Ascii,
        low: false,
    };
    assert_eq!(doc.type_char(last, 'x').unwrap().at, last.at);
    // Undo walks back step by step (the cursor with it); redo forward.
    assert_eq!(doc.undo().unwrap().at, last.at);
    assert_eq!(doc.undo().unwrap().at, 1);
    assert_eq!(doc.byte(1), Some(b'o'));
    assert_eq!(doc.undo().unwrap().at, 0);
    assert_eq!(doc.byte(0), Some(b's'), "the whole byte at once");
    assert!(doc.undo().is_none());
    assert_eq!(doc.changed_count(), 0);
    doc.redo();
    assert_eq!(doc.byte(0), Some(0x4a));
    assert_eq!(doc.changed_count(), 1);
    // A new edit drops what could be redone.
    doc.set(5, 0, c);
    assert!(!doc.can_redo());
}

#[test]
fn rows_show_offsets_hex_and_ascii() {
    let mut fx = fetched("view.bin", FIXTURE, 1000.0, 600.0);
    fx.settle();
    for text in [
        "0000",
        "0010",
        "73 6F 70 65 6E 20 68 65  78 20 76 69 65 77 00 FF",
        "sopen hex view..",
        "41 42 43 0A 80",
        "ABC..",
    ] {
        assert!(fx.drv.find_text(text).is_some(), "{text:?} not shown");
    }
    // The ASCII column starts where the hex column's widest row ends, on the same line.
    let hex = fx
        .drv
        .find_text("73 6F 70 65 6E 20 68 65  78 20 76 69 65 77 00 FF")
        .unwrap();
    let ascii = fx.drv.find_text("sopen hex view..").unwrap();
    assert!(ascii.left() > hex.right(), "{hex:?} {ascii:?}");
    assert!((ascii.center().y - hex.center().y).abs() < 0.5);
    // The status bar: offset, byte, size, changes.
    assert!(fx.drv.find_text("0x0 (0) · 0x73 115 's'").is_some());
    assert!(fx.drv.find_text("21 B").is_some());
    assert!(fx.drv.find_text("0 bytes changed").is_some());
    assert_eq!(fx.win.rows_painted, 2);
}

#[test]
fn navigation_keys_clicks_and_go_to_move_the_cursor() {
    let bytes: Vec<u8> = (0..1600u32).map(|i| i as u8).collect();
    let mut fx = fetched("nav.bin", &bytes, 1000.0, 400.0);
    fx.settle();
    let visible = fx.win.visible;
    assert!(visible > 4 && visible < 100, "{visible}");
    let none = Modifiers::NONE;
    fx.key(Key::ArrowRight, none);
    assert_eq!(fx.at(), 1);
    fx.key(Key::ArrowDown, none);
    assert_eq!(fx.at(), 17);
    fx.key(Key::ArrowLeft, none);
    assert_eq!(fx.at(), 16);
    fx.key(Key::ArrowUp, none);
    assert_eq!(fx.at(), 0);
    fx.key(Key::ArrowUp, none);
    assert_eq!(fx.at(), 0, "stays in the file");
    fx.key(Key::End, none);
    assert_eq!(fx.at(), 15, "end of the row");
    fx.key(Key::Home, none);
    assert_eq!(fx.at(), 0, "start of the row");
    fx.key(Key::PageDown, none);
    assert_eq!(fx.at(), visible * 16);
    fx.key(Key::PageUp, none);
    assert_eq!(fx.at(), 0);
    fx.key(Key::End, command(false));
    assert_eq!(fx.at(), 1599, "end of the file");
    assert_eq!(fx.win.top_row, 100 - visible, "scrolled to it");
    fx.key(Key::ArrowRight, none);
    assert_eq!(fx.at(), 1599, "stays in the file");
    fx.key(Key::Home, command(false));
    assert_eq!((fx.at(), fx.win.top_row), (0, 0));
    // Tab switches the column; the cursor stays on its byte.
    fx.key(Key::Tab, none);
    assert_eq!((fx.at(), fx.win.cursor.pane), (0, Pane::Ascii));
    fx.key(Key::Tab, none);
    assert_eq!(fx.win.cursor.pane, Pane::Hex);
    // A click places the cursor on the byte under it, in the column clicked.
    let p = fx.pos(19, Pane::Hex);
    fx.drv.click(&mut fx.win, p);
    assert_eq!((fx.at(), fx.win.cursor.pane), (19, Pane::Hex));
    let p = fx.pos(37, Pane::Ascii);
    fx.drv.click(&mut fx.win, p);
    assert_eq!((fx.at(), fx.win.cursor.pane), (37, Pane::Ascii));
    let p = fx.pos(40, Pane::Hex);
    fx.drv.click(&mut fx.win, p);
    assert_eq!(fx.at(), 40, "second half of a row");
    // Cmd/Ctrl+G: the Go to field takes `0x…` or decimal; Enter goes there.
    fx.key(Key::G, command(false));
    fx.typed("0x400");
    fx.key(Key::Enter, none);
    assert_eq!(fx.at(), 0x400);
    assert!(fx.win.top_row <= 0x40 && 0x40 < fx.win.top_row + visible);
    fx.key(Key::G, command(false));
    fx.typed("33");
    fx.key(Key::Enter, none);
    assert_eq!(fx.at(), 33);
    fx.key(Key::G, command(false));
    fx.typed("0x9999");
    fx.key(Key::Enter, none);
    assert_eq!(fx.at(), 33, "past the end: stays");
    assert!(fx.win.status.error, "{}", fx.win.status.text);
    // The keyboard is the grid's again: arrows move the cursor.
    fx.key(Key::ArrowRight, none);
    assert_eq!(fx.at(), 34);
    // The wheel scrolls rows without moving the cursor.
    let top = fx.win.top_row;
    let over = fx.pos(34, Pane::Hex);
    fx.drv.wheel(&mut fx.win, over, -2.0);
    assert_eq!(fx.win.top_row, top + 6, "two lines = six rows");
    assert_eq!(fx.at(), 34);
}

#[test]
fn typing_overwrites_and_undo_redo_keys_work() {
    let mut fx = fetched("edit.bin", FIXTURE, 1000.0, 600.0);
    fx.typed("4");
    assert!(fx.win.cursor.low);
    fx.typed("1");
    assert_eq!((fx.at(), fx.win.cursor.low), (1, false));
    fx.key(Key::Tab, Modifiers::NONE);
    fx.typed("Z");
    assert_eq!(fx.at(), 2);
    fx.typed("é");
    assert!(fx.win.status.error, "{}", fx.win.status.text);
    fx.settle();
    assert!(
        fx.drv
            .find_text("41 5A 70 65 6E 20 68 65  78 20 76 69 65 77 00 FF")
            .is_some(),
        "changed bytes shown"
    );
    assert!(fx.drv.find_text("AZpen hex view..").is_some());
    assert!(fx.drv.find_text("2 bytes changed").is_some());
    assert!(fx.drv.find_text("unsaved").is_some());
    // The cached copy is never written.
    assert_eq!(std::fs::read(&fx.base).unwrap(), FIXTURE);
    fx.key(Key::Z, command(false));
    assert_eq!(fx.win.doc.byte(1), Some(b'o'));
    assert_eq!((fx.at(), fx.win.cursor.pane), (1, Pane::Ascii));
    fx.key(Key::Z, command(false));
    assert_eq!(fx.win.doc.byte(0), Some(b's'), "both digits in one step");
    assert_eq!(fx.win.doc.changed_count(), 0);
    fx.key(Key::Z, command(true));
    fx.key(Key::Z, command(true));
    assert_eq!(fx.win.doc.byte(0), Some(0x41));
    assert_eq!(fx.win.doc.byte(1), Some(b'Z'));
}

/// Save writes exactly the edited bytes over the remote, after a local backup of the original;
/// the cached copy is not touched.
#[test]
fn save_writes_the_patched_bytes_back_after_a_backup() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    fx.typed("ff");
    fx.key(Key::Tab, Modifiers::NONE);
    fx.key(Key::End, command(false));
    fx.typed("!");
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    assert!(
        fx.win.status.text.starts_with("saved "),
        "{}",
        fx.win.status.text
    );
    let mut want = FIXTURE.to_vec();
    want[0] = 0xff;
    *want.last_mut().unwrap() = b'!';
    assert_eq!(std::fs::read(&fx.remote).unwrap(), want);
    assert_eq!(std::fs::read(&fx.base).unwrap(), FIXTURE, "cache untouched");
    let backups = walk(&crate::cache::backup_root(&fx.tmp.path().join("cache")));
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(std::fs::read(&backups[0]).unwrap(), FIXTURE);
    assert!(!fx.win.doc.dirty());
    // Nothing left in the staging area.
    let staged = walk(&crate::cache::edits_root(&fx.tmp.path().join("cache")));
    assert!(staged.is_empty(), "{staged:?}");
    // A second save goes over our own write and keeps the one backup.
    fx.key(Key::Home, command(false));
    fx.key(Key::Tab, Modifiers::NONE);
    fx.typed("00");
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    want[0] = 0;
    assert_eq!(std::fs::read(&fx.remote).unwrap(), want);
    assert_eq!(
        walk(&crate::cache::backup_root(&fx.tmp.path().join("cache"))).len(),
        1
    );
}

/// The local disk, calling `before_write` the first time anything is created on it (the first
/// write of a Save, before the remote is touched), and `after_rename` right after the first
/// rename over the target lands.
struct FirstWrite {
    fs: LocalFs,
    before_write: Option<Box<dyn FnOnce() + Send>>,
    after_rename: Option<Box<dyn FnOnce() + Send>>,
}

impl RemoteFs for FirstWrite {
    fn stat_opt(&mut self, p: &str) -> Result<Option<crate::remote::Stat>> {
        self.fs.stat_opt(p)
    }
    fn lstat_opt(&mut self, p: &str) -> Result<Option<crate::remote::Stat>> {
        self.fs.lstat_opt(p)
    }
    fn realpath(&mut self, p: &str) -> Result<String> {
        self.fs.realpath(p)
    }
    fn read_head(&mut self, p: &str, max: usize) -> Result<Vec<u8>> {
        self.fs.read_head(p, max)
    }
    fn list_dir(&mut self, p: &str) -> Result<Vec<String>> {
        self.fs.list_dir(p)
    }
    fn read_to(
        &mut self,
        p: &str,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        self.fs.read_to(p, out, progress)
    }
    fn create_from(
        &mut self,
        local: &Path,
        p: &str,
        like: Option<crate::remote::Like>,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        if let Some(hook) = self.before_write.take() {
            hook();
        }
        self.fs.create_from(local, p, like)
    }
    fn set_mode(&mut self, p: &str, mode: u32) -> Result<()> {
        self.fs.set_mode(p, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.fs.rename_over(from, to)?;
        if let Some(hook) = self.after_rename.take() {
            hook();
        }
        Ok(())
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.fs.link_new(from, to)
    }
    fn remove(&mut self, p: &str) -> Result<()> {
        self.fs.remove(p)
    }
}

/// Save order: the backup is complete on disk before the first write to the remote, and a
/// failure to stage leaves nothing (no partial backup) and the remote untouched.
#[test]
fn the_backup_is_complete_before_the_remote_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("a.bin");
    std::fs::write(&remote, FIXTURE).unwrap();
    let abs = remote.to_str().unwrap().to_string();
    let base = tmp.path().join("copy/a.bin");
    let at = LocalFs::new().fetch(&abs, &base, &mut |_, _| {}).unwrap();
    let cache = tmp.path().join("cache");
    let seen: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let (seen2, backups) = (seen.clone(), crate::cache::backup_root(&cache));
    let fs = FirstWrite {
        fs: LocalFs::new(),
        before_write: Some(Box::new(move || {
            let found = walk(&backups);
            *seen2.lock().unwrap() = found.iter().map(|p| std::fs::read(p).unwrap()).collect();
        })),
        after_rename: None,
    };
    let origin = Origin::new(Box::new(fs), None, None, abs.clone(), at);
    let mut file = HexFile::open(origin, &base, None, &cache).unwrap();
    let saved = file.save(&BTreeMap::from([(0u64, 0u8)])).unwrap();
    assert!(matches!(saved, Saved::Written { .. }), "{saved:?}");
    assert_eq!(
        *seen.lock().unwrap(),
        [FIXTURE.to_vec()],
        "the backup, whole, first"
    );

    // Staging cannot start (the edits folder is a file): nothing written anywhere.
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("a.bin");
    std::fs::write(&remote, FIXTURE).unwrap();
    let abs = remote.to_str().unwrap().to_string();
    let base = tmp.path().join("copy/a.bin");
    let at = LocalFs::new().fetch(&abs, &base, &mut |_, _| {}).unwrap();
    let cache = tmp.path().join("cache");
    let origin = Origin::new(Box::new(LocalFs::new()), None, None, abs, at);
    let mut file = HexFile::open(origin, &base, None, &cache).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(crate::cache::edits_root(&cache), b"not a folder").unwrap();
    let err = file.save(&BTreeMap::from([(0u64, 0u8)])).unwrap_err();
    assert!(format!("{err:#}").contains("was not touched"), "{err:#}");
    assert!(walk(&crate::cache::backup_root(&cache)).is_empty());
    assert_eq!(std::fs::read(&remote).unwrap(), FIXTURE);
}

/// A remote changed since it was fetched is refused: the remote keeps the other writer's bytes,
/// the edit is kept in a local file, and the disclosure row names it whole.
#[test]
fn save_refuses_a_changed_remote_and_keeps_the_edit() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    fx.typed("00");
    std::fs::write(&fx.remote, b"another writer").unwrap();
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    fx.settle();
    assert_eq!(std::fs::read(&fx.remote).unwrap(), b"another writer");
    let refused = fx.win.outcome.lock().unwrap().refused.clone().unwrap();
    assert!(refused.contains("changed since it was opened"), "{refused}");
    let kept: Vec<_> = walk(&crate::cache::edits_root(&fx.tmp.path().join("cache")))
        .into_iter()
        .filter(|p| p.to_string_lossy().contains(".conflict-"))
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    let mut want = FIXTURE.to_vec();
    want[0] = 0;
    assert_eq!(std::fs::read(&kept[0]).unwrap(), want);
    assert!(refused.contains(kept[0].to_str().unwrap()), "{refused}");
    assert!(
        fx.drv.find_text(&refused).is_some(),
        "the refusal and where the edit went, shown whole"
    );
    assert!(fx.win.doc.dirty(), "still unsaved");
}

/// Save copy publishes `<stem>.edited<ext>` next to the original — `-1` when that is taken —
/// never replacing anything.
#[test]
fn save_copy_publishes_an_edited_sibling_without_replacing() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    let planted = fx.remote.with_file_name("blob.edited.bin");
    std::fs::write(&planted, b"someone's file").unwrap();
    fx.typed("ab");
    fx.key(Key::S, command(true));
    fx.wait_for_save();
    let copy = fx.remote.with_file_name("blob.edited-1.bin");
    let mut want = FIXTURE.to_vec();
    want[0] = 0xab;
    assert_eq!(std::fs::read(&copy).unwrap(), want);
    assert_eq!(std::fs::read(&planted).unwrap(), b"someone's file");
    assert_eq!(std::fs::read(&fx.remote).unwrap(), FIXTURE);
    assert!(
        fx.win.status.text.ends_with("/blob.edited-1.bin"),
        "{}",
        fx.win.status.text
    );
    // Written somewhere: closing does not ask.
    assert!(fx.win.close_requested());
}

/// A local file is read in place, through the handle opened with it: Save renames a new file
/// over the path, and undoing the edit and saving again gives back the original bytes.
#[test]
fn a_local_file_saves_in_place_and_undo_then_save_restores_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.bin");
    std::fs::write(&path, FIXTURE).unwrap();
    let mut fx = in_place(&path, tmp, 1000.0, 600.0);
    fx.key(Key::ArrowRight, Modifiers::NONE);
    fx.typed("00");
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    let mut want = FIXTURE.to_vec();
    want[1] = 0;
    assert_eq!(std::fs::read(&path).unwrap(), want);
    fx.key(Key::Z, command(false));
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    assert!(
        fx.win.status.text.starts_with("saved "),
        "{}",
        fx.win.status.text
    );
    assert_eq!(std::fs::read(&path).unwrap(), FIXTURE);
}

/// The bytes as opened changing under the window (a local file written in place by someone
/// else) is a refusal before anything is written: no backup of the changed bytes, no staging.
#[test]
fn a_base_changed_in_place_is_refused_before_anything_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.bin");
    std::fs::write(&path, FIXTURE).unwrap();
    let mut fx = in_place(&path, tmp, 1000.0, 600.0);
    fx.typed("00");
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"more").unwrap();
    }
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    fx.settle();
    let refused = fx.win.outcome.lock().unwrap().refused.clone().unwrap();
    assert!(refused.contains("changed since it was opened"), "{refused}");
    // Nothing but the list of the edits: no backup, no stage.
    let cache = fx.tmp.path().join("cache");
    let left = walk(&cache);
    assert_eq!(left.len(), 1, "{left:?}");
    let list = &left[0];
    assert!(
        list.to_string_lossy().contains("local.bin.hexpatch-"),
        "{list:?}"
    );
    assert!(refused.contains(&*list.to_string_lossy()), "{refused}");
    assert!(
        fx.drv.find_text(&refused).is_some(),
        "named whole in the rows"
    );
    assert!(std::fs::read(&path).unwrap().ends_with(b"more"));
}

/// A 256 MiB file (sparse: nothing to read but zeros) opens and paints only the rows on screen,
/// at its start and at its end; the page cache stays small.
#[test]
fn a_large_file_paints_only_the_visible_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("big.img");
    let size = 256u64 << 20;
    File::create(&path).unwrap().set_len(size).unwrap();
    let mut fx = in_place(&path, tmp, 1000.0, 600.0);
    fx.settle();
    let visible = fx.win.visible;
    assert_eq!(fx.win.doc.rows(), size / 16);
    assert!(
        fx.win.rows_painted <= visible + 1,
        "{}",
        fx.win.rows_painted
    );
    fx.key(Key::End, command(false));
    fx.settle();
    assert_eq!(fx.at(), size - 1);
    assert_eq!(fx.win.top_row, size / 16 - visible);
    assert!(
        fx.win.rows_painted <= visible + 1,
        "{}",
        fx.win.rows_painted
    );
    assert!(
        fx.drv.find_text("FFFFFF0").is_some(),
        "the last row's offset"
    );
    assert!(fx.drv.find_text("0xFFFFFFF (268435455) · 0x00 0").is_some());
    // Forty pages down from the start: only the pages those screens show were read.
    fx.key(Key::Home, command(false));
    for _ in 0..40 {
        fx.key(Key::PageDown, Modifiers::NONE);
    }
    assert!(fx.win.doc.resident() <= PAGES * PAGE as usize);
    assert!(fx.win.doc.reads() < 200, "{}", fx.win.doc.reads());
}

/// The bars are one line whatever they say, so the grid never moves when the status changes —
/// at the narrowest window too, where the toolbar still fits on its one row.
#[test]
fn the_bars_stay_one_line_and_the_grid_stays_put() {
    let mut fx = fetched("blob.bin", FIXTURE, 480.0, 320.0);
    fx.settle();
    let quiet = fx.win.layout;
    let long = format!(
        "saved {} · original backed up at {}",
        "/a/rather/long/path".repeat(6),
        "/the/backup/path".repeat(6)
    );
    fx.win.status = Status::info(long.clone());
    fx.typed("ff");
    fx.settle();
    assert_eq!(fx.win.layout, quiet, "the grid moved");
    assert_eq!(fx.drv.rows_of(&long), Some(1), "the status is one line");
    let row = |fx: &Fx, label: &str| {
        fx.drv
            .find_text(label)
            .unwrap_or_else(|| panic!("no {label:?} on screen"))
            .center()
            .y
    };
    assert!((row(&fx, "Go to") - row(&fx, "Save copy")).abs() < 0.5);
    // A running save's spinner does not make the status bar taller either.
    let (_tx, rx) = mpsc::channel();
    fx.win.job = Some(Job {
        kind: JobKind::Save { close_after: false },
        snapshot: fx.win.doc.patches().clone(),
        rx,
    });
    fx.settle();
    assert_eq!(fx.win.layout, quiet, "the grid moved with a save running");
}

#[test]
fn closing_with_unsaved_edits_asks() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    assert!(fx.win.close_requested(), "nothing edited: closes");
    fx.typed("00");
    assert!(!fx.win.close_requested());
    assert!(fx.win.prompt);
    fx.drv.idle(&mut fx.win, 2);
    for label in ["Save", "Discard", "Cancel"] {
        assert!(fx.drv.find_text(label).is_some(), "{label}");
    }
    fx.drv.click_text(&mut fx.win, "Cancel");
    assert!(!fx.win.prompt && !fx.win.close_ok);
    assert!(!fx.win.close_requested());
    fx.drv.idle(&mut fx.win, 2);
    fx.drv.click_text(&mut fx.win, "Discard");
    assert!(fx.win.close_ok);
    assert_eq!(std::fs::read(&fx.remote).unwrap(), FIXTURE);
}

/// A server that cannot replace a file in one step: Save is off with the reason (in the rows
/// and on hover), Save copy still works.
#[test]
fn without_one_step_replace_save_is_off_and_save_copy_works() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.bin");
    std::fs::write(&path, FIXTURE).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = crate::testutil::NoReplace(LocalFs::new());
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let file = HexFile::open(origin, &path, None, &tmp.path().join("cache")).unwrap();
    let mut win = window_for(file);
    let mut drv = Driver::new(1000.0, 600.0);
    drv.idle(&mut win, 3);
    assert!(drv.find_text(CANNOT_REPLACE).is_some(), "said whole");
    drv.frame(&mut win, vec![Event::Text("00".into())], Modifiers::NONE);
    drv.key(&mut win, Key::S, command(false));
    assert!(win.job.is_none(), "Save did not start");
    assert_eq!(win.status.text, CANNOT_REPLACE);
    assert_eq!(std::fs::read(&path).unwrap(), FIXTURE);
    drv.key(&mut win, Key::S, command(true));
    drv.until(&mut win, 20, |w| w.job.is_none());
    let mut want = FIXTURE.to_vec();
    want[0] = 0;
    assert_eq!(
        std::fs::read(path.with_file_name("a.edited.bin")).unwrap(),
        want
    );
}

/// `SOPEN_STATE` patches (each byte an undo step) and places the cursor; `SOPEN_AUTOSAVE`
/// saves them without a window, or reports the refusal (exit 3 in the CLI).
#[test]
fn the_state_hook_and_autosave() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("a.bin");
    std::fs::write(&remote, FIXTURE).unwrap();
    let abs = remote.to_str().unwrap().to_string();
    let open = |state: &str| {
        let mut fs = LocalFs::new();
        let base = tmp.path().join("copy/a.bin");
        let at = fs.fetch(&abs, &base, &mut |_, _| {}).unwrap();
        let origin = Origin::new(Box::new(fs), None, None, abs.clone(), at);
        let file = HexFile::open(origin, &base, None, &tmp.path().join("cache")).unwrap();
        let hooks = Hooks {
            state: crate::gui::State::parse(state).unwrap(),
            autosave: true,
            ..Hooks::default()
        };
        (file, hooks)
    };
    let (file, hooks) = open("at=0x10;patch=1:4142,0x13:ff");
    let mut doc = HexDoc::new(file.base(), file.size());
    let cursor = apply_state(&mut doc, &hooks).unwrap();
    assert_eq!(cursor.at, 16);
    assert_eq!(doc.changed_count(), 3);
    assert_eq!(doc.undo().map(|c| c.at), Some(0x13), "a step per byte");
    assert_eq!(doc.changed_count(), 2);
    let outcome = super::open(file, &hooks, Vec::new()).unwrap();
    assert!(outcome.saved && outcome.refused.is_none());
    let mut want = FIXTURE.to_vec();
    want[1..3].copy_from_slice(b"AB");
    want[0x13] = 0xff;
    assert_eq!(std::fs::read(&remote).unwrap(), want);

    // Past the end: an error, nothing written.
    let (file, hooks) = open("patch=0x14:0000");
    let err = super::open(file, &hooks, Vec::new()).unwrap_err();
    assert!(err.to_string().contains("past the end"), "{err}");
    let (file, hooks) = open("at=21");
    assert!(super::open(file, &hooks, Vec::new()).is_err());

    // Changed after the fetch: refused, the remote keeps the other bytes.
    let (file, hooks) = open("patch=0:00");
    std::fs::write(&remote, b"another writer").unwrap();
    let outcome = super::open(file, &hooks, Vec::new()).unwrap();
    assert!(outcome.conflict);
    let refused = outcome.refused.unwrap();
    assert!(refused.contains("changed since it was opened"), "{refused}");
    assert_eq!(std::fs::read(&remote).unwrap(), b"another writer");
}

/// An empty file opens (nothing to show or edit) and its status says so.
#[test]
fn an_empty_file_opens() {
    let mut fx = fetched("empty.bin", b"", 800.0, 400.0);
    fx.typed("ff");
    fx.key(Key::ArrowRight, Modifiers::NONE);
    fx.settle();
    assert_eq!(fx.win.doc.changed_count(), 0);
    assert!(fx.drv.find_text("empty file").is_some());
    assert!(fx.drv.find_text("empty file — nothing to show").is_some());
    assert_eq!(fx.win.rows_painted, 0);
}

// ---------------------------------------------------------------------------------------------
// The model's history and dirtiness; the page cache's bound.

/// At the undo bound the two digits of a byte are still one step: one undo restores the byte
/// whole (the bound drops the oldest step, never splits a byte).
#[test]
fn two_digits_stay_one_step_at_the_undo_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.bin");
    std::fs::write(&path, FIXTURE).unwrap();
    let mut doc = HexDoc::new(Arc::new(File::open(&path).unwrap()), FIXTURE.len() as u64);
    doc.set_undo_cap(3);
    let c = Cursor::default();
    for at in 0..3 {
        assert!(doc.set(at, b'#', c));
    }
    assert_eq!(doc.undo_len(), 3, "full");
    let c = Cursor { at: 5, ..c };
    let c = doc.type_char(c, 'a').unwrap();
    doc.type_char(c, 'b').unwrap();
    assert_eq!(doc.byte(5), Some(0xab));
    assert_eq!(doc.undo_len(), 3, "the oldest went");
    assert_eq!(doc.undo().unwrap().at, 5);
    assert_eq!(doc.byte(5), Some(FIXTURE[5]), "the whole byte at once");
}

/// Dirty means "differs from what the file holds": an edit undone, or a byte typed back to its
/// value, is clean — before a save and after one (against what the save wrote).
#[test]
fn dirty_follows_content_not_history() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.bin");
    std::fs::write(&path, FIXTURE).unwrap();
    let mut doc = HexDoc::new(Arc::new(File::open(&path).unwrap()), FIXTURE.len() as u64);
    let c = Cursor::default();
    assert!(!doc.dirty());
    doc.set(0, 0, c);
    assert!(doc.dirty() && doc.unsaved());
    doc.undo();
    assert!(
        !doc.dirty() && !doc.unsaved(),
        "undone to the file as opened"
    );
    doc.set(1, 0, c);
    doc.set(1, b'o', c);
    assert!(!doc.dirty(), "typed back to its value");
    // A save of {1: 0x00}: clean; edit + undo is clean again; undo past the save is dirty.
    doc.set(1, 0, c);
    doc.mark_saved(doc.patches().clone());
    assert!(!doc.dirty());
    doc.set(2, 0, c);
    doc.undo();
    assert!(!doc.dirty(), "undone to what was saved");
    doc.undo();
    assert!(
        doc.dirty(),
        "the file holds the saved byte; this is the original"
    );
    // A Save copy of a state: not unsaved while the state is that.
    doc.mark_copied(doc.patches().clone());
    assert!(doc.dirty() && !doc.unsaved());
}

/// The page cache never holds more than its bound: more distinct pages than it holds are read
/// in turn, the bound checked after each; a page that was dropped reads back the same bytes.
#[test]
fn the_page_cache_stays_within_its_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("pages.bin");
    let pages = PAGES as u64 + 8;
    let byte = |at: u64| ((at / PAGE) as u8).wrapping_mul(7) ^ (at % 251) as u8;
    let body: Vec<u8> = (0..pages * PAGE).map(byte).collect();
    std::fs::write(&path, &body).unwrap();
    let mut doc = HexDoc::new(Arc::new(File::open(&path).unwrap()), body.len() as u64);
    let bound = PAGES * PAGE as usize;
    for p in 0..pages {
        let at = p * PAGE + 7;
        assert_eq!(doc.byte(at), Some(byte(at)), "page {p}");
        assert!(doc.resident() <= bound, "page {p}: {} held", doc.resident());
    }
    assert_eq!(doc.reads(), pages, "each page read once");
    // Page 0 was the least recently used: dropped; it reads again, the same bytes.
    assert_eq!(doc.byte(7), Some(byte(7)));
    assert_eq!(doc.byte(PAGE - 1), Some(byte(PAGE - 1)));
    assert_eq!(doc.reads(), pages + 1, "page 0 was read again");
    assert!(doc.resident() <= bound);
}

// ---------------------------------------------------------------------------------------------
// Save: one pass, the changes kept when the bytes as opened change, conflicts at equal size.

/// A file whose bytes the window reads (`base`) and a "remote" Save writes; `in_place` = they
/// are one file (a local file opened in place).
struct Pair {
    tmp: tempfile::TempDir,
    remote: PathBuf,
    base: PathBuf,
}

impl Pair {
    fn new(body: &[u8], in_place: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote/a.bin");
        std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
        std::fs::write(&remote, body).unwrap();
        // A fixed mtime in the past: a write during a test always moves it.
        set_mtime(&remote, 1_600_000_000);
        let base = if in_place {
            remote.clone()
        } else {
            let base = tmp.path().join("copy/a.bin");
            LocalFs::new()
                .fetch(remote.to_str().unwrap(), &base, &mut |_, _| {})
                .unwrap();
            set_mtime(&base, 1_600_000_000);
            base
        };
        Self { tmp, remote, base }
    }

    fn cache(&self) -> PathBuf {
        self.tmp.path().join("cache")
    }

    /// As the window opens it: a fetched copy has the fetch's stat (with its hash); a local
    /// file opened in place a plain stat (no hash).
    fn open(&self) -> HexFile {
        let abs = self.remote.to_str().unwrap().to_string();
        let mut fs = LocalFs::new();
        let mut at = fs.stat(&abs).unwrap();
        if self.base != self.remote {
            let bytes = std::fs::read(&self.base).unwrap();
            use sha2::Digest;
            at.sha256 = Some(sha2::Sha256::digest(&bytes).into());
        }
        let origin = Origin::new(Box::new(fs), None, None, abs, at);
        HexFile::open(origin, &self.base, None, &self.cache()).unwrap()
    }
}

fn set_mtime(path: &Path, secs: u64) {
    let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(at))
        .unwrap();
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

/// A writer in the gap between the pre-pass check and the streaming baseline must not make
/// changed bytes look like the original. Test both Save and Save copy on a cached source.
#[test]
fn a_change_between_pre_pass_observations_is_refused() {
    for copy in [false, true] {
        let pair = Pair::new(FIXTURE, false);
        let mut file = pair.open();
        let base = pair.base.clone();
        file.pre_pass_hook = Some(Box::new(move || {
            File::options()
                .write(true)
                .open(&base)
                .unwrap()
                .write_all_at(&[0xEE], 4)
                .unwrap();
            set_mtime(&base, 1_700_000_000);
        }));
        let patches = BTreeMap::from([(0u64, 0xff)]);
        let message = if copy {
            file.save_copy(&patches)
                .expect_err("changed source must be refused")
                .to_string()
        } else {
            match file.save(&patches).unwrap() {
                Saved::Refused { message, .. } => message,
                other => panic!("changed source must be refused: {other:?}"),
            }
        };
        assert!(message.contains("changed since it was opened"), "{message}");
        assert_eq!(std::fs::read(&pair.remote).unwrap(), FIXTURE);
        assert!(!pair.remote.with_file_name("a.edited.bin").exists());
        let kept = walk(&pair.cache());
        assert_eq!(kept.len(), 1, "only the patch list should remain: {kept:?}");
        assert!(kept[0].to_string_lossy().contains(".hexpatch-"));
        assert!(std::fs::read_to_string(&kept[0])
            .unwrap()
            .contains("0x0: FF"));
    }
}

/// The source written in place in the middle of the save's pass (between two chunks): the
/// save is refused, nothing staged or backed up is left, the remote is untouched, and the
/// changes are listed in a file of their own. Save copy is refused the same way.
#[test]
fn a_source_written_during_the_pass_is_refused_and_only_the_list_is_kept() {
    let body: Vec<u8> = (0..(3 * CHUNK) as u32).map(|i| (i % 253) as u8).collect();
    for in_place in [false, true] {
        let pair = Pair::new(&body, in_place);
        let mut file = pair.open();
        let base = pair.base.clone();
        file.chunk_hook = Some(Box::new(move |done| {
            if done == CHUNK as u64 {
                File::options()
                    .write(true)
                    .open(&base)
                    .unwrap()
                    .write_all_at(&[0xEE], 2 * CHUNK as u64)
                    .unwrap();
            }
        }));
        let patches = BTreeMap::from([(10u64, 0u8)]);
        let Saved::Refused { message, kept } = file.save(&patches).unwrap() else {
            panic!("in_place={in_place}: expected Refused")
        };
        assert!(message.contains("while it was being read"), "{message}");
        let kept = kept.unwrap();
        assert!(
            kept.to_string_lossy().contains("a.bin.hexpatch-"),
            "{kept:?}"
        );
        let left = walk(&pair.cache());
        assert_eq!(
            left,
            std::slice::from_ref(&kept),
            "in_place={in_place}: only the list"
        );
        if !in_place {
            assert_eq!(
                std::fs::read(&pair.remote).unwrap(),
                body,
                "remote untouched"
            );
        }
        // Save copy: refused, listed, nothing published.
        let err = file.save_copy(&patches).unwrap_err().to_string();
        assert!(err.contains("no copy saved"), "{err}");
        assert!(!pair.remote.with_file_name("a.edited.bin").exists());
    }
}

/// Truncating the source mid-pass makes the next read fail; the sparse edits must still be
/// retained as a patch list rather than lost with the partial stage.
#[test]
fn truncation_during_the_pass_keeps_the_patch_list() {
    let pair = Pair::new(&vec![0x41; 2 * CHUNK], false);
    let mut file = pair.open();
    let base = pair.base.clone();
    file.chunk_hook = Some(Box::new(move |done| {
        if done == CHUNK as u64 {
            File::options()
                .write(true)
                .open(&base)
                .unwrap()
                .set_len(0)
                .unwrap();
        }
    }));
    let patches = BTreeMap::from([(0u64, 0xff)]);
    let Saved::Refused {
        kept: Some(kept), ..
    } = file.save(&patches).unwrap()
    else {
        panic!("a changed source must retain its edits");
    };
    assert!(std::fs::read_to_string(&kept).unwrap().contains("0x0: FF"));
    assert_eq!(walk(&pair.cache()), [kept]);
}

/// The list of changes when the bytes as opened changed before the save: a new owner-only file
/// `<name>.hexpatch-<stamp>.txt` among the edits — a header naming the source as opened, then
/// one `offset: bytes` line per run of changed bytes.
#[test]
fn the_list_of_changes_names_the_source_and_each_run() {
    let pair = Pair::new(FIXTURE, true);
    let mut file = pair.open();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&pair.base)
        .unwrap()
        .write_all(b"more")
        .unwrap();
    let patches = BTreeMap::from([(1u64, 0xaa), (2, 0xbb), (10, 0xcc)]);
    let Saved::Refused { message, kept } = file.save(&patches).unwrap() else {
        panic!("expected Refused")
    };
    let kept = kept.unwrap();
    let name = kept.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("a.bin.hexpatch-") && name.ends_with(".txt"),
        "{name}"
    );
    assert!(message.contains(&*kept.to_string_lossy()), "{message}");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&kept).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let text = std::fs::read_to_string(&kept).unwrap();
    assert!(
        text.contains(&format!("# source: {}", pair.base.display())),
        "{text}"
    );
    assert!(
        text.contains("# as opened: 21 bytes, modified 2020-09-13T"),
        "{text}"
    );
    let lines: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(lines, ["0x1: AABB", "0xA: CC"]);
}

/// Conflicts at the same length — what a size check alone misses. The remote rewritten with a
/// different mtime, or with the fetch's own mtime put back (the fetch's hash catches it); a
/// local file written in place (its handle sees it); a local file replaced by another of the
/// same size and mtime (the hash the save's pass takes catches it). Never overwritten.
#[test]
fn same_length_changes_are_refused() {
    let other = b"SOPEN HEX VIEW\x00\xffabc\n\x80"; // FIXTURE's length, other bytes
    assert_eq!(other.len(), FIXTURE.len());
    let patches = BTreeMap::from([(0u64, 0u8)]);
    for case in ["fetched, new mtime", "fetched, same mtime"] {
        let pair = Pair::new(FIXTURE, false);
        let mut file = pair.open();
        let was = mtime(&pair.remote);
        std::fs::write(&pair.remote, other).unwrap();
        if case.ends_with("new mtime") {
            set_mtime(&pair.remote, 1_700_000_000);
        } else {
            File::options()
                .write(true)
                .open(&pair.remote)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(was))
                .unwrap();
        }
        let saved = file.save(&patches).unwrap();
        assert!(matches!(saved, Saved::Refused { .. }), "{case}: {saved:?}");
        assert_eq!(std::fs::read(&pair.remote).unwrap(), other, "{case}");
    }
    // In place, written in place with a new mtime: the handle sees it before anything is made.
    let pair = Pair::new(FIXTURE, true);
    let mut file = pair.open();
    File::options()
        .write(true)
        .open(&pair.remote)
        .unwrap()
        .write_all_at(other, 0)
        .unwrap();
    set_mtime(&pair.remote, 1_700_000_000);
    let Saved::Refused { kept, .. } = file.save(&patches).unwrap() else {
        panic!("in place, rewritten: expected Refused")
    };
    assert!(kept.unwrap().to_string_lossy().contains(".hexpatch-"));
    assert_eq!(std::fs::read(&pair.remote).unwrap(), other);
    // In place, replaced by another file of the same size and mtime before the save: the old
    // file's change time moved (unlinked), so the handle refuses at once.
    let other_file = |pair: &Pair| {
        let swap = pair.remote.with_file_name("swap.bin");
        std::fs::write(&swap, other).unwrap();
        File::options()
            .write(true)
            .open(&swap)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(mtime(&pair.remote)))
            .unwrap();
        swap
    };
    let pair = Pair::new(FIXTURE, true);
    let mut file = pair.open();
    std::fs::rename(other_file(&pair), &pair.remote).unwrap();
    let saved = file.save(&patches).unwrap();
    assert!(matches!(saved, Saved::Refused { .. }), "{saved:?}");
    assert_eq!(
        std::fs::read(&pair.remote).unwrap(),
        other,
        "not overwritten"
    );
    // In place, replaced during the save itself — after the pass and the target's first check,
    // when the remote is first written: same size, same mtime, so only content tells. The
    // overwrite checks the target against the hash the pass took, and refuses.
    let pair = Pair::new(FIXTURE, true);
    let abs = pair.remote.to_str().unwrap().to_string();
    let at = LocalFs::new().stat(&abs).unwrap();
    assert!(at.sha256.is_none(), "a plain stat: no hash from a fetch");
    let (swap, target) = (other_file(&pair), pair.remote.clone());
    let fs = FirstWrite {
        fs: LocalFs::new(),
        before_write: Some(Box::new(move || std::fs::rename(&swap, &target).unwrap())),
        after_rename: None,
    };
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let mut file = HexFile::open(origin, &pair.base, None, &pair.cache()).unwrap();
    let saved = file.save(&patches).unwrap();
    let Saved::Refused { message, kept } = saved else {
        panic!("in place, replaced during the save: expected Refused, got {saved:?}")
    };
    assert!(message.contains("its content changed"), "{message}");
    assert!(kept.unwrap().to_string_lossy().contains(".conflict-"));
    assert_eq!(
        std::fs::read(&pair.remote).unwrap(),
        other,
        "not overwritten"
    );
}

/// A save spanning several chunks: changes on both sides of each chunk boundary and at the last
/// byte land exactly; the backup is the original exactly.
#[test]
fn a_multi_chunk_save_writes_every_byte_exactly() {
    let len = 2 * CHUNK + CHUNK / 2;
    let body: Vec<u8> = (0..len as u32).map(|i| (i % 241) as u8).collect();
    let pair = Pair::new(&body, false);
    let mut file = pair.open();
    let c = CHUNK as u64;
    let at = [0, c - 1, c, c + 1, 2 * c - 1, 2 * c, len as u64 - 1];
    let patches: BTreeMap<u64, u8> = at.iter().map(|&o| (o, !body[o as usize])).collect();
    let Saved::Written { backup, .. } = file.save(&patches).unwrap() else {
        panic!("expected Written")
    };
    let mut want = body.clone();
    for (&o, &b) in &patches {
        want[o as usize] = b;
    }
    assert!(
        std::fs::read(&pair.remote).unwrap() == want,
        "the whole file"
    );
    assert!(std::fs::read(&backup).unwrap() == body, "the whole backup");
}

// ---------------------------------------------------------------------------------------------
// The window: one frame's input against a finished save; the Go to field; the rows.

/// A save (chosen in the close prompt) that lands in the same frame as an edit or an undo:
/// the frame's input counts first — the window asks again rather than closing over it.
/// Without input in that frame it closes.
#[test]
fn a_save_landing_in_the_same_frame_as_an_edit_never_closes_over_it() {
    for input in ["typed", "undo", "none"] {
        let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
        fx.typed("00");
        let before = fx.win.doc.patches().clone();
        let (tx, rx) = mpsc::channel();
        fx.win.job = Some(Job {
            kind: JobKind::Save { close_after: true },
            snapshot: before.clone(),
            rx,
        });
        tx.send(Ok(JobDone::Saved(Saved::Written {
            remote: "box:/blob.bin".to_string(),
            backup: PathBuf::from("/backup/blob.bin"),
            notes: Vec::new(),
        })))
        .unwrap();
        let (events, mods) = match input {
            "typed" => (vec![Event::Text("ff".into())], Modifiers::NONE),
            "undo" => (
                vec![Event::Key {
                    key: Key::Z,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: command(false),
                }],
                command(false),
            ),
            _ => (Vec::new(), Modifiers::NONE),
        };
        fx.drv.frame(&mut fx.win, events, mods);
        assert!(fx.win.job.is_none(), "{input}: the result was taken");
        if input == "none" {
            assert!(fx.win.close_ok, "nothing new: closes");
            continue;
        }
        assert!(!fx.win.close_ok, "{input}: closed over the frame's edit");
        assert!(fx.win.prompt, "{input}: asks again");
        assert_ne!(
            *fx.win.doc.patches(),
            before,
            "{input}: the frame's input is kept"
        );
        assert!(fx.win.doc.unsaved(), "{input}");
    }
}

/// Once closing is decided nothing edits: typing in the frames before the window goes away
/// changes nothing.
#[test]
fn nothing_edits_once_closing_is_decided() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    fx.typed("00");
    assert_eq!(fx.win.doc.changed_count(), 1);
    fx.win.close_ok = true;
    fx.typed("ff");
    assert_eq!(
        fx.win.doc.byte(1),
        Some(FIXTURE[1]),
        "typed after closing was decided"
    );
    fx.key(Key::Z, command(false));
    assert_eq!(
        fx.win.doc.changed_count(),
        1,
        "undone after closing was decided"
    );
    assert_eq!(fx.win.doc.byte(0), Some(0));
}

/// A close asked for during a save that then lands with notes (say, the remote changed right
/// after it) keeps the window open so they can be read — as Save chosen in the close prompt
/// does.
#[test]
fn a_save_that_lands_with_notes_keeps_the_window_open() {
    let note = "saved, but the remote changed right after: another writer".to_string();
    for prompt in [false, true] {
        let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
        fx.typed("00");
        let (tx, rx) = mpsc::channel();
        fx.win.job = Some(Job {
            kind: JobKind::Save {
                close_after: prompt,
            },
            snapshot: fx.win.doc.patches().clone(),
            rx,
        });
        if !prompt {
            assert!(!fx.win.close_requested());
            assert!(fx.win.close_when_done);
        }
        tx.send(Ok(JobDone::Saved(Saved::Written {
            remote: "box:/blob.bin".to_string(),
            backup: PathBuf::from("/backup/blob.bin"),
            notes: vec![note.clone()],
        })))
        .unwrap();
        fx.settle();
        assert!(
            !fx.win.close_ok,
            "prompt={prompt}: closed with notes unread"
        );
        assert!(!fx.win.prompt, "prompt={prompt}: nothing is unsaved");
        assert!(
            fx.drv.find_text(&note).is_some(),
            "prompt={prompt}: shown whole"
        );
        assert!(
            fx.win.status.text.starts_with("not closed"),
            "{}",
            fx.win.status.text
        );
    }
}

/// A close asked for during a Save that was itself chosen in the close prompt is used up by that
/// Save, whatever it reports (notes, or a failure): a later ordinary Save never closes the window.
#[test]
fn a_close_request_does_not_outlive_its_save() {
    let written = |notes: Vec<String>| {
        Ok(JobDone::Saved(Saved::Written {
            remote: "box:/blob.bin".to_string(),
            backup: PathBuf::from("/backup/blob.bin"),
            notes,
        }))
    };
    for first in ["notes", "failure"] {
        let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
        fx.typed("00");
        let (tx, rx) = mpsc::channel();
        fx.win.job = Some(Job {
            kind: JobKind::Save { close_after: true },
            snapshot: fx.win.doc.patches().clone(),
            rx,
        });
        assert!(!fx.win.close_requested(), "{first}: waits for the save");
        tx.send(if first == "notes" {
            written(vec!["saved, but the remote changed right after".to_string()])
        } else {
            Err(anyhow!("the disk is full"))
        })
        .unwrap();
        fx.settle();
        assert!(!fx.win.close_ok, "{first}: the first save keeps it open");
        fx.win.prompt = false;
        // Later, an ordinary Save (not asked to close) lands cleanly.
        fx.typed("11");
        let (tx, rx) = mpsc::channel();
        fx.win.job = Some(Job {
            kind: JobKind::Save { close_after: false },
            snapshot: fx.win.doc.patches().clone(),
            rx,
        });
        tx.send(written(Vec::new())).unwrap();
        fx.settle();
        assert!(
            !fx.win.close_ok,
            "{first}: a stale close request closed the window"
        );
    }
}

/// A save whose worker ends without a result is a visible failure: the edits stay, the window
/// stays usable, and a new Save works — even when the dead worker held the file.
#[test]
fn a_dead_save_worker_is_a_visible_failure_and_the_window_stays_usable() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    fx.typed("00");
    let (tx, rx) = mpsc::channel::<Result<JobDone>>();
    drop(tx);
    fx.win.job = Some(Job {
        kind: JobKind::Save { close_after: false },
        snapshot: fx.win.doc.patches().clone(),
        rx,
    });
    let file = fx.win.file.clone();
    let _ = std::thread::spawn(move || {
        let _held = file.lock().unwrap();
        panic!("the save worker dies holding the file (test)");
    })
    .join();
    assert!(fx.win.file.is_poisoned());
    fx.settle();
    assert!(fx.win.job.is_none());
    assert!(fx.win.status.error, "{}", fx.win.status.text);
    assert!(
        fx.win.notices[0].contains("stopped without a result"),
        "{:?}",
        fx.win.notices
    );
    assert!(fx.win.doc.dirty(), "the edits stay");
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    assert!(
        fx.win.status.text.starts_with("saved "),
        "{}",
        fx.win.status.text
    );
    assert_eq!(std::fs::read(&fx.remote).unwrap()[0], 0);
}

/// Cmd/Ctrl+G and typing in one frame: the typing goes to the Go to field (or nowhere), never
/// into the bytes. A click into the field with typing in the same frame: the same.
#[test]
fn the_go_to_field_owns_the_typing_of_its_frame() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    fx.settle();
    let rev = fx.win.doc.rev();
    assert_eq!(FIXTURE[0] >> 4, 7, "'1' would change the first byte");
    let events = vec![
        Event::Key {
            key: Key::G,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: command(false),
        },
        Event::Text("12".into()),
    ];
    fx.drv.frame(&mut fx.win, events, command(false));
    fx.drv.idle(&mut fx.win, 1);
    assert_eq!(fx.win.doc.rev(), rev, "Cmd+G with typing edited a byte");
    assert_eq!(fx.win.doc.changed_count(), 0);
    assert_eq!(fx.win.goto, "12", "the typing went to the field");
    // Back to the grid; then a click into the empty field with typing in the same frame.
    fx.key(Key::Escape, Modifiers::NONE);
    fx.win.goto.clear();
    fx.drv.idle(&mut fx.win, 2);
    let field = fx.drv.find_text("0x… or decimal").expect("the empty field");
    fx.drv.move_to(&mut fx.win, field.center());
    fx.drv.press(&mut fx.win, egui::PointerButton::Primary);
    let events = vec![
        Event::PointerButton {
            pos: field.center(),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        },
        // Not a digit the first byte already starts with ('s' = 0x73): typing it would change it.
        Event::Text("a".into()),
    ];
    let cursor = fx.win.cursor;
    fx.drv.frame(&mut fx.win, events, Modifiers::NONE);
    fx.drv.idle(&mut fx.win, 1);
    assert_eq!(
        fx.win.doc.rev(),
        rev,
        "a click into the field with typing edited a byte"
    );
    assert_eq!(fx.win.doc.changed_count(), 0);
    assert_eq!(fx.win.cursor, cursor, "the typing moved the cursor");
    // Back to the grid; then the press itself and typing in one frame.
    fx.key(Key::Escape, Modifiers::NONE);
    fx.win.goto.clear();
    fx.drv.idle(&mut fx.win, 2);
    assert_ne!(fx.drv.ctx.memory(|m| m.focused()), Some(goto_id()));
    let events = vec![
        Event::PointerButton {
            pos: field.center(),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        },
        Event::Text("a".into()),
    ];
    fx.drv.frame(&mut fx.win, events, Modifiers::NONE);
    fx.drv.release(&mut fx.win, egui::PointerButton::Primary);
    fx.drv.idle(&mut fx.win, 1);
    assert_eq!(
        fx.win.doc.rev(),
        rev,
        "a press into the field with typing edited a byte"
    );
    assert_eq!(fx.win.cursor, cursor, "the typing moved the cursor");
}

/// The disclosure rows sit below the grid: a report — a long one, wrapping — appears there only
/// once the button is let go, takes rows off the grid's bottom, and never moves the grid's top
/// or any row on it.
#[test]
fn a_report_never_moves_the_rows() {
    let bytes: Vec<u8> = (0..1024u32).map(|i| (i * 7 % 256) as u8).collect();
    let mut fx = fetched("blob.bin", &bytes, 700.0, 500.0);
    fx.settle();
    let before = fx.win.layout.unwrap();
    let visible = fx.win.visible;
    let row1 = hex_cells(&bytes[16..32].iter().map(|&b| Some(b)).collect::<Vec<_>>());
    let row_at = |fx: &Fx| fx.drv.find_text(&row1).expect("row 1 shown").min;
    let row1_at = row_at(&fx);
    let long = format!(
        "{} changed since it was opened (size 1024 → 2048); not saved — your edit is at {}",
        "box:/srv/a/rather/long/path/to/the/file/blob.bin",
        "<cache>/.edits/box/srv/a/rather/long/path/to/the/file/blob.conflict-20261117T000000Z-1a2b3c4d.bin"
    );
    let (tx, rx) = mpsc::channel();
    fx.win.job = Some(Job {
        kind: JobKind::Save { close_after: false },
        snapshot: fx.win.doc.patches().clone(),
        rx,
    });
    let p = fx.pos(40, Pane::Hex);
    fx.drv.move_to(&mut fx.win, p);
    fx.drv.press(&mut fx.win, egui::PointerButton::Primary);
    tx.send(Ok(JobDone::Saved(Saved::Refused {
        message: long.clone(),
        kept: None,
    })))
    .unwrap();
    for pass in 0..4 {
        fx.drv.idle(&mut fx.win, 1);
        assert!(
            fx.drv.find_text(&long).is_none(),
            "{pass}: not while the button is down"
        );
        assert_eq!(fx.win.layout.unwrap(), before, "{pass}");
    }
    fx.drv.release(&mut fx.win, egui::PointerButton::Primary);
    fx.settle();
    assert!(
        fx.drv.find_text(&long).is_some(),
        "shown whole after the release"
    );
    let after = fx.win.layout.unwrap();
    assert_eq!(after.rows.top(), before.rows.top(), "the grid's top moved");
    assert_eq!(
        (after.row_h, after.hex_x, after.ascii_x),
        (before.row_h, before.hex_x, before.ascii_x)
    );
    assert_eq!(row_at(&fx), row1_at, "a row moved");
    assert!(
        after.rows.bottom() < before.rows.bottom(),
        "the rows below it give way"
    );
    assert!(fx.win.visible < visible, "{} vs {visible}", fx.win.visible);
}

/// The cursor is painted in both columns — filled where it types, outlined in the other — and
/// follows navigation, Tab and undo.
#[test]
fn the_cursor_is_painted_in_both_columns() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    let fill = fx.drv.ctx.global_style().visuals.selection.bg_fill;
    let painted = |fx: &Fx, at: u64| {
        let rects = fx.drv.rects();
        let filled = |pane| {
            let p = fx.pos(at, pane);
            rects.iter().any(|(r, f, _)| *f == fill && r.contains(p))
        };
        let outlined = |pane| {
            let p = fx.pos(at, pane);
            rects
                .iter()
                .any(|(r, f, w)| *f != fill && *w > 0.0 && r.contains(p))
        };
        (
            filled(Pane::Hex),
            outlined(Pane::Hex),
            filled(Pane::Ascii),
            outlined(Pane::Ascii),
        )
    };
    fx.key(Key::ArrowDown, Modifiers::NONE);
    fx.key(Key::ArrowRight, Modifiers::NONE);
    fx.key(Key::ArrowRight, Modifiers::NONE);
    assert_eq!(fx.at(), 18);
    assert_eq!(
        painted(&fx, 18),
        (true, false, false, true),
        "hex column types"
    );
    assert_eq!(
        painted(&fx, 0),
        (false, false, false, false),
        "not where it was"
    );
    fx.key(Key::Tab, Modifiers::NONE);
    assert_eq!(
        painted(&fx, 18),
        (false, true, true, false),
        "ASCII column types"
    );
    fx.typed("Z");
    assert_eq!(fx.at(), 19);
    assert!(
        fx.drv.find_text("ABZ..").is_some(),
        "the ASCII column shows it"
    );
    assert!(
        fx.drv.find_text("41 42 5A 0A 80").is_some(),
        "the hex column too"
    );
    fx.key(Key::Z, command(false));
    assert_eq!(fx.at(), 18, "undo puts the cursor back on the byte");
    assert_eq!(painted(&fx, 18), (false, true, true, false));
    assert!(fx.drv.find_text("ABC..").is_some());
    assert!(fx.drv.find_text("41 42 43 0A 80").is_some());
    fx.key(Key::Z, command(true));
    assert_eq!(fx.at(), 18, "redo puts the cursor on the byte");
    assert_eq!(painted(&fx, 18), (false, true, true, false));
    assert!(
        fx.drv.find_text("ABZ..").is_some(),
        "redone in the ASCII column"
    );
    assert!(
        fx.drv.find_text("41 42 5A 0A 80").is_some(),
        "redone in the hex column"
    );
}

// ---------------------------------------------------------------------------------------------
// After a save: what the window reads from, and where nothing may be left.

/// A cached copy changed during an upload, with its mtime put back: the upload cannot change the
/// copy, so its identity is never refreshed after a save — the next Save sees the change and
/// refuses instead of uploading the changed bytes.
#[test]
fn a_cached_copy_changed_during_an_upload_is_refused_by_the_next_save() {
    use sha2::Digest;
    let pair = Pair::new(FIXTURE, false);
    let abs = pair.remote.to_str().unwrap().to_string();
    let (base, was) = (pair.base.clone(), mtime(&pair.base));
    let fs = FirstWrite {
        fs: LocalFs::new(),
        before_write: Some(Box::new(move || {
            let f = File::options().write(true).open(&base).unwrap();
            f.write_all_at(b"X", 5).unwrap();
            f.set_times(std::fs::FileTimes::new().set_modified(was))
                .unwrap();
        })),
        after_rename: None,
    };
    let mut at = LocalFs::new().stat(&abs).unwrap();
    at.sha256 = Some(sha2::Sha256::digest(FIXTURE).into());
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let mut file = HexFile::open(origin, &pair.base, None, &pair.cache()).unwrap();
    let first = file.save(&BTreeMap::from([(0u64, 0u8)])).unwrap();
    assert!(matches!(first, Saved::Written { .. }), "{first:?}");
    let mut want = FIXTURE.to_vec();
    want[0] = 0;
    assert_eq!(std::fs::read(&pair.remote).unwrap(), want);
    assert_eq!(mtime(&pair.base), was, "the change hides behind its mtime");
    let second = file.save(&BTreeMap::from([(0u64, 0u8), (1, 0)])).unwrap();
    assert!(matches!(second, Saved::Refused { .. }), "{second:?}");
    assert_eq!(
        std::fs::read(&pair.remote).unwrap(),
        want,
        "the changed copy's bytes were uploaded"
    );
}

/// A local file saved in place: the rename replaced the file the window read, so the window
/// reads the new one from then on — checked to hold exactly what was saved — and it is the new
/// original: no byte counts as changed, undo still goes back.
#[test]
fn a_local_file_saved_in_place_becomes_the_new_original() {
    use std::os::unix::fs::MetadataExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.bin");
    std::fs::write(&path, FIXTURE).unwrap();
    let mut fx = in_place(&path, tmp, 1000.0, 600.0);
    fx.key(Key::ArrowRight, Modifiers::NONE);
    fx.typed("00");
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    let held = fx.win.file.lock().unwrap().base().metadata().unwrap().ino();
    assert_eq!(
        held,
        std::fs::metadata(&path).unwrap().ino(),
        "reads the new file"
    );
    assert_eq!(
        fx.win.doc.changed_count(),
        0,
        "the saved byte is the original now"
    );
    assert!(!fx.win.doc.dirty() && !fx.win.doc.unsaved());
    assert_eq!(fx.win.doc.byte(1), Some(0));
    fx.settle();
    assert!(fx.drv.find_text("0 bytes changed").is_some());
    // Undo goes back past the save: a change against the new original.
    fx.key(Key::Z, command(false));
    assert_eq!(fx.win.doc.byte(1), Some(b'o'));
    assert_eq!(fx.win.doc.changed_count(), 1);
    fx.key(Key::Z, command(true));
    assert_eq!(fx.win.doc.byte(1), Some(0));
    assert_eq!(fx.win.doc.changed_count(), 0);
}

/// A local file changed by someone else right after sopen's save replaced it: the new file does
/// not hold what was saved, so the window does not take it as the original — it turns
/// read-only, says so whole, and lists the edits in a file of their own.
#[test]
fn a_local_file_changed_right_after_its_save_turns_the_window_read_only() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.bin");
    std::fs::write(&path, FIXTURE).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let target = path.clone();
    let fs = FirstWrite {
        fs: LocalFs::new(),
        before_write: None,
        after_rename: Some(Box::new(move || {
            File::options()
                .write(true)
                .open(&target)
                .unwrap()
                .write_all_at(b"ZZ", 3)
                .unwrap();
        })),
    };
    let at = LocalFs::new().stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let cache = tmp.path().join("cache");
    let file = HexFile::open(origin, &path, None, &cache).unwrap();
    let mut fx = Fx {
        remote: path.clone(),
        base: path.clone(),
        tmp,
        win: window_for(file),
        drv: Driver::new(1000.0, 600.0),
    };
    fx.drv.idle(&mut fx.win, 2);
    fx.typed("00");
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    fx.settle();
    let lists: Vec<PathBuf> = walk(&cache)
        .into_iter()
        .filter(|p| p.to_string_lossy().contains(".hexpatch-"))
        .collect();
    assert_eq!(lists.len(), 1, "{lists:?}");
    assert!(std::fs::read_to_string(&lists[0])
        .unwrap()
        .lines()
        .any(|l| l == "0x0: 00"));
    let rows: Vec<String> = fx
        .win
        .opened
        .iter()
        .chain(&fx.win.notices)
        .cloned()
        .collect();
    let said = rows
        .iter()
        .find(|r| r.contains("read-only"))
        .unwrap_or_else(|| panic!("not said: {rows:?}"))
        .clone();
    assert!(said.contains(&*lists[0].to_string_lossy()), "{said}");
    assert!(fx.drv.find_text(&said).is_some(), "said whole");
    let changed = fx.win.doc.changed_count();
    fx.key(Key::ArrowRight, Modifiers::NONE);
    fx.typed("11");
    assert_eq!(
        fx.win.doc.changed_count(),
        changed,
        "edited a read-only window"
    );
    let on_disk = std::fs::read(&path).unwrap();
    fx.key(Key::S, command(false));
    assert!(fx.win.job.is_none(), "saved a read-only window");
    assert_eq!(std::fs::read(&path).unwrap(), on_disk);
    assert!(
        fx.win.close_requested(),
        "the edits are listed: closing does not ask"
    );
}

/// Any failure after the pass started — here the stat that ends it — leaves no stage and no
/// backup behind.
#[test]
fn a_failure_after_the_pass_leaves_nothing() {
    let pair = Pair::new(FIXTURE, false);
    let mut file = pair.open();
    file.fail_stat_after_pass = true;
    assert!(file.save(&BTreeMap::from([(0u64, 0u8)])).is_err());
    assert!(walk(&pair.cache()).is_empty(), "{:?}", walk(&pair.cache()));
    assert_eq!(std::fs::read(&pair.remote).unwrap(), FIXTURE);
    assert!(file.save_copy(&BTreeMap::from([(0u64, 0u8)])).is_err());
    assert!(walk(&pair.cache()).is_empty(), "{:?}", walk(&pair.cache()));
}

/// Save copy's own pass, with its own writer in the middle: refused, the edits listed, nothing
/// published.
#[test]
fn save_copy_refuses_a_source_written_during_its_own_pass() {
    let body: Vec<u8> = (0..(3 * CHUNK) as u32).map(|i| (i % 251) as u8).collect();
    let pair = Pair::new(&body, false);
    let mut file = pair.open();
    let (base, ran) = (pair.base.clone(), Arc::new(AtomicBool::new(false)));
    let ran2 = ran.clone();
    file.chunk_hook = Some(Box::new(move |done| {
        if done == CHUNK as u64 {
            File::options()
                .write(true)
                .open(&base)
                .unwrap()
                .write_all_at(&[0xEE], 2 * CHUNK as u64)
                .unwrap();
            ran2.store(true, Ordering::SeqCst);
        }
    }));
    let err = file
        .save_copy(&BTreeMap::from([(10u64, 0u8)]))
        .unwrap_err()
        .to_string();
    assert!(
        ran.load(Ordering::SeqCst),
        "the writer ran during Save copy's pass"
    );
    assert!(err.contains("while it was being read"), "{err}");
    let left = walk(&pair.cache());
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].to_string_lossy().contains(".hexpatch-"));
    assert!(err.contains(&*left[0].to_string_lossy()), "{err}");
    assert!(!pair.remote.with_file_name("a.edited.bin").exists());
    assert_eq!(std::fs::read(&pair.remote).unwrap(), body);
}

/// The list of edits is the owner's only (0600) whatever the umask strips from the creating
/// call — here the owner's own write bit.
#[test]
fn the_list_of_edits_is_owner_only_under_any_umask() {
    if crate::testutil::rerun_under_umask_022(
        "gui::hex::tests::the_list_of_edits_is_owner_only_under_any_umask",
    ) {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let pair = Pair::new(FIXTURE, true);
    let file = pair.open();
    crate::cache::private_dir(file.edits.parent().unwrap()).unwrap();
    // Alone in this child process (one test): no other thread creates files meanwhile.
    // SAFETY: umask has no memory effects.
    unsafe { libc::umask(0o277) };
    let kept = file.keep_patches(&BTreeMap::from([(0u64, 1u8)]), "a test");
    unsafe { libc::umask(0o022) };
    let kept = kept.unwrap();
    let mode = std::fs::metadata(&kept).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
