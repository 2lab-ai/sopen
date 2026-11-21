//! The hex window: the model, the write side, and the window driven by real input events (keys
//! with modifiers, typed text, clicks) through its `ui`.

use super::*;
use crate::gui::chrome::cga;
use crate::gui::testkit::{command, Driver};
use crate::gui::Window;
use crate::remote::{LocalFs, RemoteFs};
use eframe::egui::{Event, Modifiers, Pos2};
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
    // The ASCII column shows code page 437's glyphs, as PC Shell does; an unreadable byte is `?`.
    assert_eq!(ascii_char(Some(b'~')), '~');
    assert_eq!(ascii_char(Some(0x7f)), '⌂');
    assert_eq!(ascii_char(Some(0x01)), '☺');
    assert_eq!(ascii_char(Some(0x00)), ' ');
    assert_eq!(ascii_char(None), '?');
    // Offsets read as PC Shell's File Edit: decimal, then hex in parentheses.
    assert_eq!(offset_text(16, 21), "0016(0010)");
    assert_eq!(offset_text(0, 0), "0000(0000)");
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
        "0000(0000)",
        "0016(0010)",
        "73 6F 70 65 6E 20 68 65  78 20 76 69 65 77 00 FF",
        "sopen hex view \u{A0}",
        "41 42 43 0A 80",
        "ABC◙Ç",
    ] {
        assert!(fx.drv.find_text(text).is_some(), "{text:?} not shown");
    }
    // The ASCII column starts where the hex column's widest row ends, on the same line.
    let hex = fx
        .drv
        .find_text("73 6F 70 65 6E 20 68 65  78 20 76 69 65 77 00 FF")
        .unwrap();
    let ascii = fx.drv.find_text("sopen hex view \u{A0}").unwrap();
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
    assert!(fx.drv.find_text("AZpen hex view \u{A0}").is_some());
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
    fn list_entries(&mut self, p: &str) -> Result<Vec<crate::remote::Entry>> {
        self.fs.list_entries(p)
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
        fx.drv.find_text("268435440(FFFFFF0)").is_some(),
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
    let (cursor, waiting) = apply_state(&mut doc, &hooks).unwrap();
    assert!(waiting.is_empty(), "a file read in place has every byte");
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
        fx.drv.find_text("ABZ◙Ç").is_some(),
        "the ASCII column shows it"
    );
    assert!(
        fx.drv.find_text("41 42 5A 0A 80").is_some(),
        "the hex column too"
    );
    fx.key(Key::Z, command(false));
    assert_eq!(fx.at(), 18, "undo puts the cursor back on the byte");
    assert_eq!(painted(&fx, 18), (false, true, true, false));
    assert!(fx.drv.find_text("ABC◙Ç").is_some());
    assert!(fx.drv.find_text("41 42 43 0A 80").is_some());
    fx.key(Key::Z, command(true));
    assert_eq!(fx.at(), 18, "redo puts the cursor on the byte");
    assert_eq!(painted(&fx, 18), (false, true, true, false));
    assert!(
        fx.drv.find_text("ABZ◙Ç").is_some(),
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
        .chain(&fx.win.read_only)
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

// ---------------------------------------------------------------------------------------------
// Rebinding after a Save in place: the edits are never lost, never silently clean.

/// A local file opened in place, with a second handle on the file as opened (to take its bytes
/// away later) and a Save of the window's edits run as the worker would — before the result is
/// handed to the window.
fn saved_in_place(fs: Box<dyn RemoteFs + Send>, edit: impl FnOnce(&mut Fx)) -> (Fx, File, Saved) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.bin");
    std::fs::write(&path, FIXTURE).unwrap();
    let old = File::options().write(true).open(&path).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let at = LocalFs::new().stat(&abs).unwrap();
    let origin = Origin::new(fs, None, None, abs, at);
    let cache = tmp.path().join("cache");
    let file = HexFile::open(origin, &path, None, &cache).unwrap();
    let mut fx = Fx {
        remote: path.clone(),
        base: path,
        tmp,
        win: window_for(file),
        drv: Driver::new(1000.0, 600.0),
    };
    fx.drv.idle(&mut fx.win, 2);
    edit(&mut fx);
    let snapshot = fx.win.doc.patches().clone();
    let saved = fx.win.file.lock().unwrap().save(&snapshot).unwrap();
    (fx, old, saved)
}

impl Fx {
    /// The save started with the current edits as its snapshot lands now.
    fn lands(&mut self, saved: Saved, snapshot: BTreeMap<u64, u8>) {
        let (tx, rx) = mpsc::channel();
        self.win.job = Some(Job {
            kind: JobKind::Save { close_after: false },
            snapshot,
            rx,
        });
        tx.send(Ok(JobDone::Saved(saved))).unwrap();
        self.settle();
    }

    /// The folder lists of edits go to refuses new files (until [`unblock`]).
    fn block_lists(&self) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let file = self.win.file.lock().unwrap();
        let dir = file.edits.parent().unwrap().to_path_buf();
        drop(file);
        crate::cache::private_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        dir
    }

    fn lists(&self) -> Vec<PathBuf> {
        walk(&self.tmp.path().join("cache"))
            .into_iter()
            .filter(|p| p.to_string_lossy().contains(".hexpatch-"))
            .collect()
    }
}

fn unblock(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Root ignores folder permissions: the tests that block a folder that way skip under it.
fn running_as_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// A change saved in place, undone while the save ran; then the file as opened goes away (its
/// page dropped from the cache, its bytes truncated): after the rebind the undo is still there
/// — the byte shown is the one before the change, unsaved, and closing asks.
#[test]
fn an_undo_during_an_in_place_save_survives_the_rebind() {
    let (mut fx, old, saved) = saved_in_place(Box::new(LocalFs::new()), |fx| {
        fx.key(Key::ArrowRight, Modifiers::NONE);
        fx.typed("00");
    });
    assert!(matches!(saved, Saved::Written { .. }), "{saved:?}");
    let snapshot = BTreeMap::from([(1u64, 0u8)]);
    fx.key(Key::Z, command(false));
    assert_eq!(fx.win.doc.byte(1), Some(b'o'));
    fx.win.doc.pages.held.clear();
    old.set_len(0).unwrap();
    fx.lands(saved, snapshot);
    assert!(fx.win.read_only.is_none(), "{:?}", fx.win.read_only);
    assert_eq!(fx.win.doc.byte(1), Some(b'o'), "the undo vanished");
    assert!(fx.win.doc.unsaved(), "silently clean");
    assert!(!fx.win.close_requested(), "closed over the undo");
    assert!(fx.win.prompt);
    // And it saves: the file gets the byte back.
    fx.win.prompt = false;
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    assert_eq!(std::fs::read(&fx.remote).unwrap(), FIXTURE);
}

/// The new file cannot be read where the window must check an edit against it (truncated right
/// after the save): the rebase is not made — the window turns read-only and the edits are kept
/// in a list, desired bytes as shown.
#[test]
fn a_new_file_that_cannot_be_read_keeps_the_edits_read_only() {
    let (mut fx, _old, saved) = saved_in_place(Box::new(LocalFs::new()), |fx| {
        fx.key(Key::ArrowRight, Modifiers::NONE);
        fx.typed("00");
    });
    let snapshot = fx.win.doc.patches().clone();
    fx.key(Key::Z, command(false));
    File::options()
        .write(true)
        .open(&fx.remote)
        .unwrap()
        .set_len(0)
        .unwrap();
    fx.lands(saved, snapshot);
    let said = fx.win.read_only.clone().expect("read-only");
    let lists = fx.lists();
    assert_eq!(lists.len(), 1, "{lists:?}");
    let text = std::fs::read_to_string(&lists[0]).unwrap();
    assert!(text.lines().any(|l| l == "0x1: 6F"), "{text}");
    assert!(said.contains(&*lists[0].to_string_lossy()), "{said}");
}

/// Read-only after a failed rebind, with newer edits, and the list of edits cannot be written
/// (the edits folder is blocked): closing does not succeed silently — it asks (Discard /
/// Cancel). "Keep edits…" retries the list once the cause is gone; then closing is free.
#[test]
fn read_only_with_unlisted_edits_asks_before_closing_and_retries() {
    if running_as_root() {
        eprintln!("skipping: root ignores folder permissions");
        return;
    }
    let after = Arc::new(Mutex::new(None::<PathBuf>));
    let target = after.clone();
    let fs = FirstWrite {
        fs: LocalFs::new(),
        before_write: None,
        after_rename: Some(Box::new(move || {
            let path = target.lock().unwrap().clone().unwrap();
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .write_all_at(b"ZZ", 3)
                .unwrap();
        })),
    };
    let after2 = after.clone();
    let (mut fx, _old, saved) = saved_in_place(Box::new(fs), move |fx| {
        *after2.lock().unwrap() = Some(fx.remote.clone());
        fx.typed("00");
    });
    let snapshot = fx.win.doc.patches().clone();
    // A newer edit, made while the save ran (the cursor moved on to byte 1).
    fx.typed("11");
    let blocked = fx.block_lists();
    fx.lands(saved, snapshot);
    assert!(fx.win.read_only.is_some());
    assert!(fx.lists().is_empty());
    assert!(!fx.win.close_requested(), "closed with unlisted edits");
    fx.drv.idle(&mut fx.win, 2);
    for label in ["Discard", "Cancel"] {
        assert!(fx.drv.find_text(label).is_some(), "{label}");
    }
    fx.drv.click_text(&mut fx.win, "Cancel");
    assert!(!fx.win.close_ok);
    unblock(&blocked);
    fx.settle();
    fx.drv.click_text(&mut fx.win, "Keep edits…");
    fx.settle();
    let lists = fx.lists();
    assert_eq!(lists.len(), 1, "{lists:?}");
    let text = std::fs::read_to_string(&lists[0]).unwrap();
    assert!(text.lines().any(|l| l == "0x0: 0011"), "{text}");
    assert!(fx.win.close_requested(), "the edits are listed now");
}

/// The list of edits can also be written where the owner chooses, with Save to…'s rules (a new
/// file, never over anything).
#[test]
fn read_only_edits_can_be_listed_at_a_chosen_path() {
    if running_as_root() {
        eprintln!("skipping: root ignores folder permissions");
        return;
    }
    let after = Arc::new(Mutex::new(None::<PathBuf>));
    let target = after.clone();
    let fs = FirstWrite {
        fs: LocalFs::new(),
        before_write: None,
        after_rename: Some(Box::new(move || {
            let path = target.lock().unwrap().clone().unwrap();
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .write_all_at(b"ZZ", 3)
                .unwrap();
        })),
    };
    let after2 = after.clone();
    let (mut fx, _old, saved) = saved_in_place(Box::new(fs), move |fx| {
        *after2.lock().unwrap() = Some(fx.remote.clone());
        fx.typed("00");
    });
    let snapshot = fx.win.doc.patches().clone();
    fx.typed("11");
    let blocked = fx.block_lists();
    fx.lands(saved, snapshot);
    fx.settle();
    // Typed into the path field, as the owner does: click it, select all, type across frames.
    let write = fx.drv.find_text("Write the list there").expect("the row");
    let field = Pos2::new(write.left() - 60.0, write.center().y);
    fx.drv.click(&mut fx.win, field);
    fx.drv.idle(&mut fx.win, 2);
    assert_eq!(
        fx.drv.ctx.memory(|m| m.focused()),
        Some(keep_id()),
        "the grid took the keyboard from the path field"
    );
    fx.key(Key::A, command(false));
    let chosen = fx.tmp.path().join("my-edits.txt");
    let typed = chosen.to_str().unwrap().to_string();
    let (head, tail) = typed.split_at(typed.len() / 2);
    for part in [head, tail] {
        fx.drv
            .frame(&mut fx.win, vec![Event::Text(part.into())], Modifiers::NONE);
        fx.drv.idle(&mut fx.win, 1);
    }
    // Checked before anything is written: a wrong path must not be written anywhere.
    assert_eq!(fx.win.keep_to, typed, "the typing did not reach the field");
    fx.drv.click_text(&mut fx.win, "Write the list there");
    fx.settle();
    let text = std::fs::read_to_string(&chosen).unwrap();
    assert!(text.lines().any(|l| l == "0x0: 0011"), "{text}");
    assert!(fx.win.close_requested());
    unblock(&blocked);
}

/// Where the list was written, with what that save had to say (a staging folder it could not
/// remove, …), is shown whole in the rows.
#[test]
fn the_kept_line_carries_the_saves_notes() {
    if running_as_root() {
        eprintln!("skipping: root ignores folder permissions");
        return;
    }
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    fx.typed("00");
    // sopen's own place for kept edits refuses, so the row to write the list elsewhere shows.
    let blocked = fx.block_lists();
    fx.win.turn_read_only("a test");
    let chosen = fx.tmp.path().join("list.txt");
    fx.win.keep_to = chosen.to_str().unwrap().to_string();
    let private = Arc::new(Mutex::new(None::<PathBuf>));
    let seen = private.clone();
    fx.win.keep_link = Some(Box::new(move |step, dir| {
        if step == crate::gui::binary::LinkStep::Publish {
            // Something sopen did not make is now in its folder: the folder stays.
            std::fs::write(dir.join("intruder"), b"")?;
            *seen.lock().unwrap() = Some(dir.to_path_buf());
        }
        Ok(())
    }));
    fx.settle();
    fx.drv.click_text(&mut fx.win, "Write the list there");
    fx.settle();
    assert!(std::fs::read_to_string(&chosen)
        .unwrap()
        .contains("0x0: 00"));
    let line = fx.win.read_only.clone().unwrap();
    assert!(line.contains(&*chosen.to_string_lossy()), "{line}");
    assert!(line.contains("could not be removed"), "{line}");
    let dir = private.lock().unwrap().clone().unwrap();
    assert!(line.contains(&*dir.to_string_lossy()), "{line}");
    assert!(fx.drv.find_text(&line).is_some(), "shown whole");
    unblock(&blocked);
}

/// A Save copy's remembered state does not survive a change of the file under it: file `ab`,
/// byte 0 → X and Save copy (the copy is `Xb`), then `ZY` saved in place (the window re-binds to
/// `ZY`), then byte 0 → X: the window shows `XY`, which is written nowhere — closing asks.
#[test]
fn a_save_copy_does_not_count_after_the_file_under_it_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.bin");
    std::fs::write(&path, b"ab").unwrap();
    let mut fx = in_place(&path, tmp, 1000.0, 600.0);
    fx.key(Key::Tab, Modifiers::NONE);
    fx.typed("X");
    fx.key(Key::S, command(true));
    fx.wait_for_save();
    assert_eq!(
        std::fs::read(path.with_file_name("local.edited.bin")).unwrap(),
        b"Xb"
    );
    fx.key(Key::Home, command(false));
    fx.typed("ZY");
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    assert_eq!(std::fs::read(&path).unwrap(), b"ZY");
    assert_eq!(fx.win.doc.changed_count(), 0, "re-bound onto ZY");
    fx.key(Key::Home, command(false));
    fx.typed("X");
    assert_eq!(
        (fx.win.doc.byte(0), fx.win.doc.byte(1)),
        (Some(b'X'), Some(b'Y'))
    );
    assert!(fx.win.doc.unsaved(), "XY is written nowhere");
    assert!(!fx.win.close_requested(), "closed without asking");
    assert!(fx.win.prompt);
}

/// Many bytes touched, then undone: clean — and asking again does not read the file again
/// (the answer is kept until something changes).
#[test]
fn a_large_touched_then_undone_document_is_clean_and_cheap_to_ask() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("big.bin");
    let body: Vec<u8> = (0..(1u32 << 20)).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &body).unwrap();
    let mut doc = HexDoc::new(Arc::new(File::open(&path).unwrap()), body.len() as u64);
    let c = Cursor::default();
    let n = 50_000u64;
    for i in 0..n {
        let at = i * 17;
        assert!(doc.set(at, !body[at as usize], c));
    }
    assert!(doc.dirty() && doc.unsaved());
    for _ in 0..n {
        doc.undo();
    }
    assert_eq!(doc.patches().len(), n as usize, "absolute entries stay");
    assert!(!doc.dirty() && !doc.unsaved());
    assert_eq!(doc.changed_count(), 0);
    let reads = doc.reads();
    for _ in 0..100 {
        assert!(!doc.dirty() && !doc.unsaved());
    }
    assert_eq!(doc.reads(), reads, "asked again: read again");
    doc.redo();
    assert!(doc.dirty(), "a change after the answer was kept");
}

// ---------------------------------------------------------------------------------------------
// The PC Shell look: File Edit's offsets and F-keys.

/// Offsets read decimal then hex, each column as wide as the file's last offset needs.
#[test]
fn offsets_read_decimal_then_hex_and_widen_with_the_file() {
    assert_eq!(offset_text(0, 21), "0000(0000)");
    assert_eq!(offset_text(240, 4096), "0240(00F0)");
    assert_eq!(decimal_digits(10_000), 4);
    assert_eq!(decimal_digits(10_001), 5);
    assert_eq!(offset_text(16, 10_001), "00016(0010)");
    let big = 0x1_0000_0001;
    assert_eq!(offset_text(0, big), "0000000000(000000000)");
    assert_eq!(offset_text(big - 1, big), "4294967296(100000000)");
    // Every row of a file has the same width, down to the last.
    for size in [1, 4096, 1 << 20, big, u64::MAX] {
        let first = offset_text(0, size).len();
        assert_eq!(offset_text(size - 1, size).len(), first, "{size}");
    }
}

/// On its own the window's bottom row is PC Shell's F-key row with what works here: F3 Exit,
/// F5 Save, F6 Goto, F8 Asc/hx — each the same as the key it stands for.
#[test]
fn the_f_keys_do_what_their_labels_say() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    for label in ["Exit   ", "Save   ", "Goto   ", "Asc/hx "] {
        assert!(fx.drv.find_text(label).is_some(), "{label:?} in the row");
    }
    // F8: the typing column, as Tab.
    assert_eq!(fx.win.cursor.pane, Pane::Hex);
    fx.key(Key::F8, Modifiers::NONE);
    assert_eq!(fx.win.cursor.pane, Pane::Ascii);
    fx.key(Key::F8, Modifiers::NONE);
    assert_eq!(fx.win.cursor.pane, Pane::Hex);
    // F6: the Go to field takes the typing, as Cmd/Ctrl+G.
    fx.key(Key::F6, Modifiers::NONE);
    assert_eq!(fx.drv.ctx.memory(|m| m.focused()), Some(goto_id()));
    fx.typed("16");
    assert_eq!(fx.win.goto, "16");
    assert_eq!(fx.win.doc.changed_count(), 0, "no byte was typed");
    fx.key(Key::Enter, Modifiers::NONE);
    assert_eq!(fx.at(), 16);
    // F5: Save, as Cmd/Ctrl+S — one save.
    fx.typed("7A");
    assert!(fx.win.doc.dirty());
    let starts = fx.win.save_starts;
    fx.key(Key::F5, Modifiers::NONE);
    assert_eq!(fx.win.save_starts, starts + 1, "F5 starts exactly one save");
    fx.wait_for_save();
    assert!(
        fx.win.status.text.starts_with("saved "),
        "{}",
        fx.win.status.text
    );
    assert_eq!(std::fs::read(&fx.remote).unwrap()[16], 0x7A);
    // F3: closing is asked for, as Cmd/Ctrl+W (nothing unsaved: it closes).
    fx.drv.as_shell();
    fx.key(Key::F3, Modifiers::NONE);
    assert!(fx.drv.window_closed, "F3 closes the window");
}

/// The browser's part, played by a host: it runs the hex window's pass as hosted
/// ([`crate::gui::host_pass`]) and reads F3 after it, as the browser does.
struct Host {
    win: HexWindow,
    /// The hosted window asked to close (it must not, for F3: that is the host's).
    asked: bool,
    /// F3 was still there for the host after the window's pass.
    f3_left: bool,
}

impl Window for Host {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let ((), asked) = crate::gui::host_pass(&ctx, || self.win.ui(ui));
        self.asked |= asked;
        self.f3_left |= ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F3));
    }

    fn ready(&self) -> bool {
        true
    }
}

/// Hosted in another window, the row is the host's: the window draws no F-key row of its own,
/// does not take F3 (the host gets it, untouched) and asks nothing; the host lists the window's
/// own keys.
#[test]
fn a_hosted_window_leaves_the_row_and_f3_to_its_host() {
    let Fx {
        tmp: _tmp,
        win,
        mut drv,
        ..
    } = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    // On its own it has the row (the control: the check below can see one).
    assert!(drv.find_text("Exit   ").is_some());
    let mut host = Host {
        win,
        asked: false,
        f3_left: false,
    };
    drv.idle(&mut host, 2);
    for label in ["Exit   ", "Save   ", "Goto   ", "Asc/hx "] {
        assert!(
            drv.find_text(label).is_none(),
            "{label:?}: no row of its own"
        );
    }
    drv.take_viewport_commands();
    drv.key(&mut host, Key::F3, Modifiers::NONE);
    assert!(host.f3_left, "F3 is the host's");
    assert!(!host.asked, "the hosted window asked nothing");
    assert!(!drv.closed(), "nor did it close the window around it");
    assert_eq!(
        Window::keys(&host.win),
        HEX_KEYS,
        "the host lists the window's own keys"
    );
}

/// F3 on its own goes through the real close: with an edit the shell's close is refused and
/// the unsaved-changes prompt comes up; Discard closes. Without an edit it closes at once.
#[test]
fn f3_on_a_dirty_window_brings_up_the_prompt_and_discard_closes() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    fx.drv.as_shell();
    fx.typed("41");
    assert!(fx.win.doc.unsaved());
    fx.key(Key::F3, Modifiers::NONE);
    assert_eq!(fx.drv.refused_closes, 1, "an edit: the close is refused");
    assert!(!fx.drv.window_closed);
    assert!(fx.drv.find_text("Unsaved changes").is_some(), "the prompt");
    fx.drv.click_text(&mut fx.win, "Discard");
    fx.drv.idle(&mut fx.win, 1);
    assert!(fx.drv.window_closed, "Discard closes it");
    assert_eq!(
        std::fs::read(&fx.remote).unwrap(),
        FIXTURE,
        "nothing written"
    );
    let mut clean = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    clean.drv.as_shell();
    clean.key(Key::F3, Modifiers::NONE);
    assert_eq!(
        (clean.drv.refused_closes, clean.drv.window_closed),
        (0, true)
    );
}

/// F5, F6 and F8 act only where their keys would: F5 saves nothing while the Go to field has
/// the typing, while a save runs, while the prompt is up or while the window is read-only; F6
/// leaves a focused field and the prompt alone; F8 moves no column while either is there.
#[test]
fn f5_f6_f8_respect_a_focused_field_the_prompt_a_save_and_read_only() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    fx.typed("7A");
    // The Go to field has the typing.
    fx.key(Key::G, command(false));
    fx.typed("12");
    assert_eq!(fx.drv.ctx.memory(|m| m.focused()), Some(goto_id()));
    fx.key(Key::F5, Modifiers::NONE);
    assert!(fx.win.job.is_none(), "F5 in the field saves nothing");
    assert!(
        !fx.win.status.text.starts_with("saved"),
        "{}",
        fx.win.status.text
    );
    fx.key(Key::F6, Modifiers::NONE);
    assert_eq!(fx.win.goto, "12", "F6 leaves what is typed in the field");
    let pane = fx.win.cursor.pane;
    fx.key(Key::F8, Modifiers::NONE);
    assert_eq!(fx.win.cursor.pane, pane, "F8 in the field moves no column");
    assert_eq!(fx.win.goto, "12");
    fx.key(Key::Escape, Modifiers::NONE);
    fx.drv.ctx.memory_mut(|m| m.surrender_focus(goto_id()));
    fx.settle();
    // A save running: no second one starts.
    let (_tx, rx) = mpsc::channel();
    fx.win.job = Some(Job {
        kind: JobKind::Save { close_after: false },
        snapshot: fx.win.doc.patches().clone(),
        rx,
    });
    let starts = fx.win.save_starts;
    fx.key(Key::F5, Modifiers::NONE);
    fx.key(Key::S, command(false));
    assert_eq!(fx.win.save_starts, starts, "no save starts while one runs");
    fx.win.job = None;
    // The prompt is up.
    assert!(!fx.win.close_requested());
    assert!(fx.win.prompt);
    fx.drv.idle(&mut fx.win, 2);
    let pane = fx.win.cursor.pane;
    for key in [Key::F5, Key::F6, Key::F8] {
        fx.key(key, Modifiers::NONE);
    }
    assert!(fx.win.job.is_none(), "F5 under the prompt saves nothing");
    assert_ne!(
        fx.drv.ctx.memory(|m| m.focused()),
        Some(goto_id()),
        "F6 under the prompt"
    );
    assert_eq!(fx.win.cursor.pane, pane, "F8 under the prompt");
    fx.drv.click_text(&mut fx.win, "Cancel");
    // Read-only.
    fx.win.turn_read_only("the file changed");
    fx.key(Key::F5, Modifiers::NONE);
    assert!(fx.win.job.is_none(), "F5 read-only saves nothing");
    assert_eq!(
        std::fs::read(&fx.remote).unwrap(),
        FIXTURE,
        "nothing written"
    );
}

/// `SOPEN_STATE` `close=ask` (the prompt's receipt): with an edit, the first pass brings up the
/// unsaved-changes prompt in the dialog look — black on cyan, its title in its frame.
#[test]
fn close_ask_brings_up_the_prompt_in_the_dialog_look() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    fx.typed("41");
    fx.win.ask_close = true;
    fx.drv.idle(&mut fx.win, 3);
    assert!(fx.win.prompt);
    let p = chrome::PALETTE;
    assert_eq!(
        fx.drv.colors_of("Unsaved changes"),
        Some(vec![p.dialog_text])
    );
    let discard = fx.drv.find_text("Discard").expect("the prompt's buttons");
    let cyan = fx.drv.rects().iter().any(|(r, f, _)| {
        *f == p.dialog_back && r.contains(discard.left_top() - egui::vec2(4.0, 4.0))
    });
    assert!(cyan, "the prompt is cyan");
    assert!(!fx.win.ask_close, "asked once");
    // Its text is black on cyan; a button's label keeps its state's colour, so the one under
    // the pointer (inverted: cyan on black) never goes black on black.
    let message = fx.drv.text_starting_with("Save the changes").unwrap();
    assert_eq!(
        fx.drv.painted_colors_of(&message),
        Some(vec![p.dialog_text])
    );
    assert_eq!(fx.drv.painted_colors_of("Cancel"), Some(vec![cga::BLACK]));
    let under = |fx: &Fx, at: Pos2| {
        fx.drv
            .rects()
            .iter()
            .rev()
            .find(|(r, _, _)| r.contains(at))
            .map(|(_, f, _)| *f)
    };
    let at = discard.center();
    fx.drv.move_to(&mut fx.win, at);
    fx.drv.idle(&mut fx.win, 2);
    let label = fx.drv.painted_colors_of("Discard").unwrap();
    let fill = under(&fx, at).expect("the button's fill");
    assert_eq!(fill, cga::BLACK, "hovered: inverted");
    assert_eq!(label, vec![cga::CYAN], "hovered: cyan on black");
}

/// 04 §3: a viewer's own F-key acts with no modifier at all — Shift+F5 or Alt+F5 saves nothing,
/// Shift+F6 opens no Go to, Shift+F8 moves no column (egui's `consume_key` ignores an extra Shift
/// or Alt) — while the bare keys still act.
#[test]
fn the_f_keys_act_only_without_modifiers() {
    let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
    fx.typed("7A");
    assert!(fx.win.doc.dirty());
    let starts = fx.win.save_starts;
    let pane = fx.win.cursor.pane;
    for mods in [Modifiers::SHIFT, Modifiers::ALT] {
        for key in [Key::F5, Key::F6, Key::F8] {
            fx.key(key, mods);
        }
    }
    assert_eq!(fx.win.save_starts, starts, "no save");
    assert_ne!(
        fx.drv.ctx.memory(|m| m.focused()),
        Some(goto_id()),
        "no Go to"
    );
    assert_eq!(fx.win.cursor.pane, pane, "no column");
    fx.key(Key::F8, Modifiers::NONE);
    assert_ne!(fx.win.cursor.pane, pane, "F8 alone moves the column");
    fx.key(Key::F5, Modifiers::NONE);
    assert_eq!(fx.win.save_starts, starts + 1, "F5 alone saves");
    fx.wait_for_save();
}

/// A window whose pass says whether a pressed Cmd/Ctrl+R is still in the input after it.
struct KeepsR<'a> {
    win: &'a mut HexWindow,
    left: Option<bool>,
}

impl Window for KeepsR<'_> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.win.ui(ui);
        let pressed = |e: &Event| {
            matches!(
                e,
                Event::Key {
                    key: Key::R,
                    pressed: true,
                    ..
                }
            )
        };
        let (any, left) = ui.ctx().input(|i| {
            let left = i.events.iter().any(pressed);
            (left || i.modifiers.command, left)
        });
        if any {
            self.left = Some(self.left.unwrap_or(false) || left);
        }
    }

    fn ready(&self) -> bool {
        self.win.ready()
    }
}

/// Whether a Cmd/Ctrl+R pressed in `win` is still there for whoever reads keys after it.
fn cmd_r_is_left(win: &mut HexWindow, drv: &mut Driver) -> bool {
    let mut probe = KeepsR { win, left: None };
    drv.key(&mut probe, Key::R, command(false));
    probe.left.expect("the key was delivered")
}

/// Cmd/Ctrl+R is the file browser's too: a hex window without a stream (a local file, a fetched
/// copy — every one before the routes change) leaves the key alone.
#[test]
fn cmd_r_is_left_alone_without_a_stream() {
    let mut fx = fetched("a.bin", FIXTURE, 760.0, 560.0);
    let status = fx.win.status.text.clone();
    assert!(cmd_r_is_left(&mut fx.win, &mut fx.drv), "the key was taken");
    assert_eq!(fx.win.status.text, status);
    assert!(fx.win.reloading.is_none());
}

/// `SOPEN_STATE` `at=`: the cursor's row is wholly on screen once the window has settled — the
/// first pass lays the grid out taller than it stays (the rows below it have no height yet).
#[test]
fn a_cursor_from_the_state_is_wholly_on_screen() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("deep.bin");
    std::fs::write(&path, vec![0x5a; 1 << 16]).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let file = HexFile::open(origin, &path, None, &tmp.path().join("cache")).unwrap();
    let hooks = Hooks {
        state: crate::gui::State::parse("at=0x8000").unwrap(),
        ..Hooks::default()
    };
    let mut win = embedded(file, &hooks, Vec::new(), Arc::default(), Arc::default()).unwrap();
    let mut drv = Driver::new(760.0, 560.0);
    drv.idle(&mut win, 4);
    let row = win.cursor.at / ROW;
    assert!(
        win.top_row <= row && row < win.top_row + win.visible,
        "row {row} with rows {}..{} on screen",
        win.top_row,
        win.top_row + win.visible
    );
    // Scrolling away from the cursor afterwards is not undone.
    let over = win.layout.unwrap().rows.center();
    drv.wheel(&mut win, over, 30.0);
    drv.idle(&mut win, 2);
    assert!(
        win.top_row + win.visible <= row,
        "the wheel was followed back"
    );
}

// ---------------------------------------------------------------------------------------------
// Over a stream (06 §7.1; §12 V1–V3, V11): the window on a Source over the Fake, its engine
// stepped by hand on simulated time, so every bound is in bytes and requests, never in seconds.

mod stream {
    use super::*;
    use crate::gui::{Hooks, State};
    use crate::remote::{
        fake, pattern, Asked, Cancelled, Change, FakeControl, FakeFile, Fault, Link, SftpRemote,
        Stat,
    };
    use crate::stream::{Engine, Opening, Settings, Source, State as Flow, BLOCK};
    use eframe::egui::PointerButton;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    /// The owner's file (06 §12 V1).
    const MODEL: u64 = 24_000_000_000;
    /// 20 GiB (06 §12 V3).
    const FAR: u64 = 0x5_0000_0000;
    /// No poll within a test that does not ask for one.
    const NEVER: Duration = Duration::from_secs(1_000_000);

    /// The `Origin`'s connection, counted: viewing must never use it (06 §2: it connects on
    /// first use, which is a Save), and a Save's whole fetch is counted apart.
    #[derive(Default, Clone)]
    struct Counts {
        calls: Arc<AtomicUsize>,
        fetches: Arc<AtomicUsize>,
        /// A fetch waits here until it is cancelled (a fetch slow enough to be stopped).
        hold: Arc<AtomicBool>,
        /// A fetch waits at its first progress report, its `.part` begun, until it is cancelled.
        midway: Arc<AtomicBool>,
        /// A fetch fails once it is asked for (the link dropped).
        fail: Arc<AtomicBool>,
    }

    struct Counting(LocalFs, Counts);

    impl Counting {
        fn hit(&mut self) -> &mut LocalFs {
            self.1.calls.fetch_add(1, Ordering::SeqCst);
            &mut self.0
        }
    }

    impl RemoteFs for Counting {
        fn stat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
            self.hit().stat_opt(p)
        }
        fn lstat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
            self.hit().lstat_opt(p)
        }
        fn realpath(&mut self, p: &str) -> Result<String> {
            self.hit().realpath(p)
        }
        fn read_head(&mut self, p: &str, max: usize) -> Result<Vec<u8>> {
            self.hit().read_head(p, max)
        }
        fn list_dir(&mut self, p: &str) -> Result<Vec<String>> {
            self.hit().list_dir(p)
        }
        fn list_entries(&mut self, p: &str) -> Result<Vec<crate::remote::Entry>> {
            self.hit().list_entries(p)
        }
        fn read_to(
            &mut self,
            p: &str,
            out: &mut dyn std::io::Write,
            progress: &mut dyn FnMut(u64),
        ) -> Result<u64> {
            self.hit().read_to(p, out, progress)
        }
        fn create_from(
            &mut self,
            local: &Path,
            p: &str,
            like: Option<crate::remote::Like>,
        ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
            self.hit().create_from(local, p, like)
        }
        fn set_mode(&mut self, p: &str, mode: u32) -> Result<()> {
            self.hit().set_mode(p, mode)
        }
        fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
            self.hit().rename_over(from, to)
        }
        fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
            self.hit().link_new(from, to)
        }
        fn remove(&mut self, p: &str) -> Result<()> {
            self.hit().remove(p)
        }
        fn can_replace(&self) -> bool {
            self.1.calls.fetch_add(1, Ordering::SeqCst);
            true
        }
        fn fetch_cancellable(
            &mut self,
            remote: &str,
            local: &Path,
            progress: &mut dyn FnMut(u64, u64),
            cancel: &AtomicBool,
        ) -> Result<Stat> {
            self.1.fetches.fetch_add(1, Ordering::SeqCst);
            while self.1.hold.load(Ordering::SeqCst) {
                if cancel.load(Ordering::SeqCst) {
                    return Err(anyhow!(Cancelled));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            if self.1.fail.load(Ordering::SeqCst) {
                bail!("the link dropped");
            }
            let midway = self.1.midway.clone();
            let mut report = |done: u64, total: u64| {
                progress(done, total);
                while midway.load(Ordering::SeqCst) && done > 0 && !cancel.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(2));
                }
            };
            self.hit()
                .fetch_cancellable(remote, local, &mut report, cancel)
        }
    }

    /// The hex window on a Source over the Fake.
    struct Sx {
        tmp: tempfile::TempDir,
        source: Source,
        engine: Engine,
        control: FakeControl,
        counts: Counts,
        /// The stats lines (06 §11), by their event.
        lines: Arc<Mutex<Vec<String>>>,
        /// What the stream wakes, and how often it did.
        repaint: Repaint,
        wakes: Arc<AtomicUsize>,
        win: HexWindow,
        drv: Driver,
    }

    fn sx_with(file: FakeFile, link: Link, poll_every: Duration, hooks: &Hooks) -> Result<Sx> {
        let tmp = tempfile::tempdir().unwrap();
        let at = Stat {
            size: file.size,
            mtime: file.mtime,
            ..LocalFs::new().stat(tmp.path().to_str().unwrap()).unwrap()
        };
        let (transport, control) = fake("/m.bin", file, link);
        let base = transport.now() - control.elapsed();
        let (clock, again) = (control.clone(), control.clone());
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let repaint = Repaint::default();
        let wakes = Arc::new(AtomicUsize::new(0));
        let settings = Settings {
            poll_every,
            stats: Some(Arc::new(move |line: &str| {
                sink.lock().unwrap().push(line.to_string())
            })),
            throttle: None,
            clock: Arc::new(move || base + clock.elapsed()),
        };
        let opening = Opening {
            label: "host:/m.bin".into(),
            began: Instant::now(),
            sniff: 0,
        };
        let (source, engine) = Source::build(
            transport,
            Box::new(move || again.open()),
            {
                let (wakes, wake) = (wakes.clone(), repaint.waker());
                Arc::new(move || {
                    wakes.fetch_add(1, Ordering::SeqCst);
                    wake();
                })
            },
            opening,
            settings,
        );
        let counts = Counts::default();
        let origin = Origin::new(
            Box::new(Counting(LocalFs::new(), counts.clone())),
            None,
            Some("host".into()),
            "/m.bin".into(),
            at,
        );
        let cache = tmp.path().join("cache");
        let file = HexFile::streamed(origin, source.clone(), repaint.clone(), &cache);
        let win = embedded(file, hooks, Vec::new(), Arc::default(), Arc::default())?;
        Ok(Sx {
            tmp,
            source,
            engine,
            control,
            counts,
            lines,
            repaint,
            wakes,
            win,
            drv: Driver::new(760.0, 560.0),
        })
    }

    fn sx(size: u64) -> Sx {
        sx_with(
            FakeFile::pattern(size),
            Link::gate(),
            NEVER,
            &Hooks::default(),
        )
        .unwrap()
    }

    /// A row of 16 bytes none of which is here yet.
    fn pending_row() -> String {
        let mut out = String::new();
        for i in 0..16 {
            if i > 0 {
                out.push_str(if i == 8 { "  " } else { " " });
            }
            out.push_str("··");
        }
        out
    }

    /// The row at `off` as the hex column shows the pattern's bytes.
    fn pattern_row(off: u64) -> String {
        let bytes: Vec<Option<u8>> = pattern(off, 16).into_iter().map(Some).collect();
        hex_cells(&bytes)
    }

    impl Sx {
        fn frames(&mut self, n: usize) {
            self.drv.idle(&mut self.win, n);
        }

        fn out(&mut self) -> usize {
            self.engine.transport().map_or(0, |r| r.pending())
        }

        /// The link works for one step: what is queued goes, one completion comes.
        fn turn(&mut self) {
            self.engine.pump();
            if self.out() > 0 {
                self.engine.step(None);
            }
        }

        /// Exactly one completion (a wake left over from a read ends a wait early).
        fn one(&mut self) {
            for _ in 0..1000 {
                if self.engine.step(None) {
                    return;
                }
            }
            panic!("no completion came");
        }

        /// Frames and link steps until nothing is on its way and nothing more is asked.
        fn settle(&mut self) {
            let mut quiet = 0;
            for _ in 0..100_000 {
                self.frames(1);
                self.engine.pump();
                if self.out() == 0 {
                    // Longer than the scroll bar's thumb rests before its place is asked for.
                    quiet += 1;
                    if quiet >= 10 {
                        return;
                    }
                } else {
                    quiet = 0;
                    self.engine.step(None);
                }
            }
            panic!("the stream did not settle");
        }

        /// `d` of simulated time, the engine stepped through it.
        fn run_for(&mut self, d: Duration) {
            let until = self.control.elapsed() + d;
            for _ in 0..1_000_000 {
                let left = until.saturating_sub(self.control.elapsed());
                if left.is_zero() {
                    return;
                }
                self.engine.step(Some(left));
            }
            panic!("simulated time did not pass");
        }

        fn key(&mut self, key: Key, modifiers: Modifiers) {
            self.drv.key(&mut self.win, key, modifiers);
        }

        fn typed(&mut self, text: &str) {
            self.drv.frame(
                &mut self.win,
                vec![Event::Text(text.into())],
                Modifiers::NONE,
            );
            self.frames(1);
        }

        fn goto(&mut self, at: u64) {
            self.key(Key::G, command(false));
            self.typed(&format!("0x{at:X}"));
            self.key(Key::Enter, Modifiers::NONE);
        }

        fn sent(&self) -> usize {
            self.control.sent().len()
        }

        /// The events of the stats lines so far, with the place of an `input`.
        fn events(&self) -> Vec<String> {
            let lines = self.lines.lock().unwrap();
            lines
                .iter()
                .map(|l| {
                    let event = l.split(' ').nth(1).unwrap_or_default();
                    match l.rsplit_once(" place=") {
                        Some((_, place)) => format!("{event} {place}"),
                        None => event.to_string(),
                    }
                })
                .collect()
        }

        /// Bytes asked and not yet answered.
        fn in_flight(&self) -> u64 {
            self.source.core().pf.adm.in_flight()
        }

        fn fetched(&self) -> u64 {
            self.source.fetched().total
        }

        fn cap(&self) -> u64 {
            self.source.core().pf.adm.cap()
        }

        fn status_now(&mut self) -> Status {
            let ctx = self.drv.ctx.clone();
            self.win.status_text(&ctx)
        }

        fn status(&mut self) -> String {
            self.status_now().text
        }

        fn shows(&self, text: &str) -> bool {
            self.drv.find_text(text).is_some()
        }

        /// How many texts the last pass drew that read exactly `text`, cut by a clip or not.
        fn drawn(&self, text: &str) -> usize {
            let all = self.drv.galleys();
            all.iter().filter(|(_, g)| g.text() == text).count()
        }

        /// Bytes of reads asked and not yet answered (status requests left out).
        fn reads_out(&self) -> u64 {
            self.in_flight() / BLOCK * BLOCK
        }

        /// The offset column's text for the row at `off`.
        fn offset(&self, off: u64) -> String {
            offset_text(off, self.win.doc.size())
        }

        /// The scroll bar: its x, and the y of the thumb's place at `frac` of the file.
        fn bar(&self, frac: f32) -> Pos2 {
            let l = self.win.layout.expect("drawn");
            let cell = crate::gui::chrome::CELL;
            let (top, bottom) = (l.rows.top() - 2.0 * l.row_h, l.rows.bottom());
            let room = (bottom - top - 3.0 * cell.y).max(1.0);
            Pos2::new(
                l.rows.right() + cell.x / 2.0,
                top + 1.5 * cell.y + frac * room,
            )
        }

        /// The reads sent from request `from` on, as block numbers.
        fn blocks_read(&self, from: usize) -> Vec<u64> {
            self.control.sent()[from..]
                .iter()
                .filter_map(|s| match s.asked {
                    Asked::Read { off, .. } => Some(off / BLOCK),
                    _ => None,
                })
                .collect()
        }
    }

    /// V1: the owner's 24 GB file. The window is there before any byte — its frame, its offsets,
    /// its rows as pending — and the first screen costs one 64 KiB block; then nothing more is
    /// sent, the `Origin`'s connection is never used and nothing is created in the cache.
    #[test]
    fn v1_the_first_screen_of_a_24_gb_file_costs_one_block() {
        let mut sx = sx(MODEL);
        sx.frames(2);
        assert_eq!(sx.sent(), 0, "the frame asks; the engine sends");
        for row in 0..3 {
            let off = sx.offset(row * ROW);
            assert!(sx.shows(&off), "row {row}'s offset is drawn");
        }
        assert!(sx.shows(&pending_row()), "the rows show as pending");
        // The second row (the cursor is in the first): every cell in the skin's dim colour.
        let rows = sx.drv.galleys();
        let row: Vec<_> = rows
            .iter()
            .filter(|(_, g)| g.text() == pending_row())
            .collect();
        assert!(row.len() >= 2, "{} pending rows", row.len());
        let cells: Vec<_> = row[1]
            .1
            .job
            .sections
            .iter()
            .step_by(2)
            .map(|s| s.format.color)
            .collect();
        assert_eq!(cells, vec![cga::CYAN; 16], "pending cells are dim");
        assert_eq!(sx.status(), "loading 0x0");
        assert!(!sx.win.ready(), "not shot before the screen's bytes came");
        // Every row painted — the last, cut by the frame, included — has its offset and is
        // pending; none is blank.
        let painted = sx.win.rows_painted;
        assert!(painted >= 20, "{painted} rows");
        assert_eq!(sx.drawn(&pending_row()), painted as usize, "pending rows");
        for row in 0..painted {
            assert_eq!(sx.drawn(&sx.offset(row * ROW)), 1, "row {row}'s offset");
        }
        sx.settle();
        // And once the block is here, every one of them shows the file's bytes at its offset.
        assert_eq!(sx.win.rows_painted, painted);
        for row in 0..painted {
            assert_eq!(sx.drawn(&sx.offset(row * ROW)), 1, "row {row}'s offset");
            assert_eq!(sx.drawn(&pattern_row(row * ROW)), 1, "row {row}'s bytes");
            let ascii: String = pattern(row * ROW, 16)
                .into_iter()
                .map(|b| ascii_char(Some(b)))
                .collect();
            assert!(sx.drawn(&ascii) >= 1, "row {row}'s ASCII column");
        }
        assert_eq!(sx.drawn(&pending_row()), 0);
        for row in 0..3 {
            assert!(sx.shows(&pattern_row(row * ROW)), "row {row}'s bytes");
        }
        assert!(!sx.shows(&pending_row()));
        assert_eq!(sx.status(), "");
        assert!(sx.win.ready());
        let fetched = sx.source.fetched();
        assert_eq!(fetched.total, BLOCK, "one 64 KiB block");
        let sent = sx.sent();
        // No input for 2 s and more: nothing more is sent.
        for _ in 0..5 {
            sx.run_for(Duration::from_secs(1));
            sx.frames(2);
            sx.engine.pump();
        }
        assert_eq!(sx.sent(), sent, "nothing more is sent");
        assert_eq!(sx.fetched(), BLOCK);
        assert_eq!(
            sx.counts.calls.load(Ordering::SeqCst),
            0,
            "viewing used the Origin's connection"
        );
        assert_eq!(sx.counts.fetches.load(Ordering::SeqCst), 0, "a whole fetch");
        let made = walk(sx.tmp.path());
        assert!(made.is_empty(), "viewing wrote to disk: {made:?}");
    }

    /// `SOPEN_STATE` `loading=1` (06 §11): the window is ready to be shot at its first frame
    /// with rows, those still loading as they are; without it, once they came.
    #[test]
    fn loading_1_shoots_the_rows_as_they_are() {
        assert!(State::parse("loading=1").unwrap().loading);
        assert!(!State::parse("at=1").unwrap().loading);
        assert!(State::parse("loading=yes").is_err());
        let hooks = Hooks {
            state: State::parse("loading=1").unwrap(),
            ..Hooks::default()
        };
        let mut sx = sx_with(FakeFile::pattern(MODEL), Link::gate(), NEVER, &hooks).unwrap();
        sx.frames(2);
        assert!(sx.shows(&pending_row()));
        assert!(
            sx.win.ready(),
            "loading=1: shot with its rows still loading"
        );
        let mut plain = sx_with(
            FakeFile::pattern(MODEL),
            Link::gate(),
            NEVER,
            &Hooks::default(),
        )
        .unwrap();
        plain.frames(2);
        assert!(!plain.win.ready());
        plain.settle();
        assert!(plain.win.ready());
    }

    /// V2: PgDn held, the scroll bar dragged to the middle and back, PgUp held. No key waits
    /// for a row, and what was fetched stays within what was shown plus the readahead of each
    /// run and the margin and cap of each jump.
    #[test]
    fn v2_scrolling_a_24_gb_file_stays_bounded() {
        let mut sx = sx(MODEL);
        sx.settle();
        let page = sx.win.visible * ROW;
        for _ in 0..400 {
            sx.key(Key::PageDown, Modifiers::NONE);
            sx.turn();
        }
        assert_eq!(
            sx.win.cursor.at,
            400 * page,
            "a key was lost to a loading row"
        );
        sx.settle();
        assert!(sx.shows(&pattern_row(sx.win.top_row * ROW)), "after PgDn");
        assert!(!sx.shows(&pending_row()));
        let (top, middle) = (sx.bar(0.0), sx.bar(0.5));
        let from = sx.bar(sx.win.top_row as f32 / (MODEL / ROW) as f32);
        sx.drv.move_to(&mut sx.win, from);
        sx.drv.press(&mut sx.win, PointerButton::Primary);
        for k in 1..=8 {
            let at = from + (middle - from) * (k as f32 / 8.0);
            sx.drv.move_to(&mut sx.win, at);
            sx.turn();
        }
        sx.settle();
        assert!(
            sx.win.top_row > MODEL / ROW / 3,
            "the drag went to the middle"
        );
        assert!(
            sx.shows(&pattern_row(sx.win.top_row * ROW)),
            "in the middle"
        );
        for k in 1..=8 {
            let at = middle + (top - middle) * (k as f32 / 8.0);
            sx.drv.move_to(&mut sx.win, at);
            sx.turn();
        }
        sx.drv.release(&mut sx.win, PointerButton::Primary);
        sx.settle();
        assert_eq!(sx.win.top_row, 0, "and back to the top");
        assert!(sx.shows(&pattern_row(0)), "at the top");
        sx.key(Key::End, command(false));
        for _ in 0..150 {
            sx.key(Key::PageUp, Modifiers::NONE);
            sx.turn();
        }
        sx.settle();
        assert_eq!(sx.win.cursor.at, MODEL - 1 - 150 * page);
        assert!(sx.shows(&pattern_row(sx.win.top_row * ROW)), "after PgUp");
        assert!(!sx.shows(&pending_row()));
        let stats = sx.source.stats();
        let bound = stats.delivered + stats.runs * MIB + stats.jumps * (128 * KIB + sx.cap());
        assert!(
            stats.fetched.total <= bound,
            "fetched {} of {bound} allowed: {stats:?}",
            stats.fetched.total
        );
        // The inequality cannot see a viewer that reads more than it shows (what it reads
        // counts as delivered), so the script's own cost is bounded too: the 550 screens it
        // showed, and the readahead of the one run down.
        let shown = 552 * page;
        assert!(
            stats.delivered <= 2 * shown + 64 * KIB,
            "read {} to show {shown}",
            stats.delivered
        );
        assert!(
            stats.fetched.total <= 3 * MIB,
            "fetched {}",
            stats.fetched.total
        );
        assert!(sx.win.doc.resident() <= PAGES * PAGE as usize);
        assert_eq!(sx.counts.calls.load(Ordering::SeqCst), 0);
        assert!(
            walk(sx.tmp.path()).is_empty(),
            "the cache directory changed"
        );
    }

    /// V3: Cmd/Ctrl+G to 20 GiB. The place shows at once as loading, costs its block and its
    /// margin, and what still arrives for the old place is within the cap.
    #[test]
    fn v3_a_far_jump_costs_its_block_and_its_margin() {
        let mut sx = sx(MODEL);
        sx.settle();
        // Scrolling first: readahead is running when the jump comes.
        for _ in 0..40 {
            sx.key(Key::PageDown, Modifiers::NONE);
            sx.turn();
        }
        // Reads for the old place are out when the jump comes: keys go on with the link
        // stalled until some are.
        for pass in 0.. {
            assert!(pass < 400, "no read of the old place was left out");
            sx.key(Key::PageDown, Modifiers::NONE);
            sx.engine.pump();
            if sx.reads_out() >= 2 * BLOCK {
                break;
            }
        }
        let (sent, cap, before) = (sx.sent(), sx.cap(), sx.fetched());
        let old = sx.reads_out();
        assert!(old >= 2 * BLOCK, "{old} bytes of the old place out");
        sx.goto(FAR);
        assert_eq!(sx.win.cursor.at, FAR);
        assert_eq!(sx.sent(), sent, "nothing is sent by a frame");
        let off = sx.offset(FAR);
        assert!(sx.shows(&off), "the place is on screen before its bytes");
        assert!(sx.shows(&pending_row()));
        // The cursor's row came on screen at the bottom: the rows above it load too.
        let top = sx.win.top_row * ROW;
        assert!(top < FAR && FAR - top < 64 * ROW);
        assert_eq!(sx.status(), format!("loading 0x{top:X}"));
        // What was out for the old place when the jump came is at most the cap.
        assert!(old <= cap, "{old} out for the old place, cap {cap}");
        sx.settle();
        assert!(sx.shows(&pattern_row(FAR)), "the bytes at 20 GiB");
        // It all arrived, and with the new place's two blocks it is everything that came since
        // the jump: nothing more was sent for the old place (the list of reads below).
        assert_eq!(sx.fetched() - before, old + 2 * BLOCK, "{old} was out");
        assert_eq!(sx.status(), format!("at 0x{FAR:X} ({FAR})"));
        // Everything sent since the jump is the new place's: its block and the one before it
        // (its margin, and the rows above the cursor) — 128 KiB.
        let asked = sx.blocks_read(sent);
        assert_eq!(asked, [FAR / BLOCK - 1, FAR / BLOCK], "{asked:?}");
        // From a quiet link the bound is exact: the block, and at a block's start the one
        // before it for the margin.
        let before = sx.fetched();
        sx.goto(3 * FAR / 5 + 0x8000);
        sx.settle();
        assert_eq!(sx.fetched() - before, BLOCK, "the block alone");
        let before = sx.fetched();
        sx.goto(2 * FAR / 5);
        sx.settle();
        assert_eq!(sx.fetched() - before, 2 * BLOCK, "the block and its margin");
    }

    /// 06 §7.1: a page that has not arrived is Pending, not empty. The rows whose bytes are here
    /// stay as they are beside the pending ones, the frame and the offsets are drawn, and every
    /// key still moves — with the link stalled the whole time.
    #[test]
    fn pending_rows_never_blank_the_screen_or_drop_a_key() {
        let mut sx = sx(MODEL);
        sx.settle();
        // The last rows of block 0, from the cache: nothing is sent for them.
        sx.goto(BLOCK - PAGE);
        sx.frames(2);
        let sent = sx.sent();
        assert!(sx.shows(&pattern_row(BLOCK - PAGE)), "block 0 is cached");
        // Down until the screen straddles into block 1, which is not here. The link is stalled:
        // the engine is never stepped from here on.
        let rows = sx.win.visible;
        for _ in 0..rows + PAGE / ROW - 8 {
            sx.key(Key::ArrowDown, Modifiers::NONE);
        }
        assert!(sx.win.top_row * ROW < BLOCK && (sx.win.top_row + rows) * ROW > BLOCK);
        let top = sx.win.top_row * ROW;
        assert!(sx.shows(&pattern_row(top)), "a row that is here stays");
        assert!(sx.shows(&pattern_row(BLOCK - ROW)), "up to the last one");
        let (first, second) = (sx.offset(BLOCK), sx.offset(BLOCK + ROW));
        assert!(
            sx.shows(&first) && sx.shows(&second),
            "offsets of rows on their way"
        );
        assert!(sx.shows(&pending_row()), "which show as pending");
        assert!(sx.shows("File Edit"), "the frame");
        assert_eq!(sx.status(), format!("loading 0x{BLOCK:X}"));
        assert_eq!(sx.sent(), sent, "the link is stalled");
        // Keys: the cursor moves through rows that are not here, and far away.
        let at = sx.win.cursor.at;
        sx.key(Key::ArrowDown, Modifiers::NONE);
        sx.key(Key::ArrowRight, Modifiers::NONE);
        assert_eq!(sx.win.cursor.at, at + ROW + 1);
        sx.key(Key::PageDown, Modifiers::NONE);
        assert_eq!(sx.win.cursor.at, at + ROW + 1 + rows * ROW);
        sx.key(Key::End, command(false));
        assert_eq!(sx.win.cursor.at, MODEL - 1);
        let last = sx.offset((MODEL - 1) / ROW * ROW);
        assert!(sx.shows(&last), "the end of the file, loading");
        sx.goto(FAR);
        assert_eq!(sx.win.cursor.at, FAR);
        // Back at the top: its rows were kept, and show without the link.
        sx.key(Key::Home, command(false));
        assert_eq!(sx.win.cursor.at, 0);
        assert!(
            sx.shows(&pattern_row(0)),
            "rows read before are still shown"
        );
        assert!(!sx.shows(&pending_row()));
        // The link comes back: the place asked for last fills in.
        sx.goto(FAR);
        sx.settle();
        assert!(sx.shows(&pattern_row(FAR)));
    }

    /// A demand pending over 1 s names its size, over 10 s the status row says the remote is
    /// slow (06 §5) — and the rows are still the pending ones.
    #[test]
    fn a_slow_demand_says_so_on_the_status_row() {
        let link = Link {
            latency: Duration::from_secs(12),
            ..Link::gate()
        };
        let mut sx = sx_with(FakeFile::pattern(MODEL), link, NEVER, &Hooks::default()).unwrap();
        sx.frames(2);
        sx.engine.pump();
        assert_eq!(sx.status(), "loading 0x0");
        sx.run_for(Duration::from_millis(1500));
        sx.frames(1);
        assert_eq!(sx.status(), "loading 0x0 (64 KiB)");
        sx.run_for(Duration::from_secs(9));
        sx.frames(1);
        assert_eq!(sx.status(), "the remote is slow");
        assert!(sx.shows(&pending_row()));
        sx.settle();
        assert!(sx.shows(&pattern_row(0)));
        assert_eq!(sx.status(), "");
    }

    /// A read that failed is the status row's text, not an empty page: the rows read before
    /// stay, and the next key retries (06 §7.1, §5).
    #[test]
    fn a_failed_read_is_said_and_retried() {
        let mut sx = sx(MODEL);
        sx.settle();
        sx.control.fault(sx.sent(), Fault::Lost);
        sx.goto(FAR);
        sx.settle();
        let Flow::Failed(reason) = sx.source.state() else {
            panic!("{:?}", sx.source.state())
        };
        // Said whole, in the rows below the grid, for as long as the stream is failed.
        assert_eq!(sx.win.stream_warning(), Some(reason.clone()));
        assert_eq!(sx.drawn(&reason), 1, "the reason is drawn whole");
        // The warning's row comes off the grid's bottom, and the cursor's row stays on screen.
        let off = sx.offset(FAR);
        assert!(sx.shows(&off), "the screen stays where it is");
        assert_eq!(sx.win.doc.cell(FAR), Cell::Unreadable);
        // What was read before still shows.
        sx.key(Key::Home, command(false));
        assert!(sx.shows(&pattern_row(0)));
        // A second later a key retries: the connection is back, and the rows come.
        let mut idle = sx.control.open().unwrap();
        assert!(idle.next(Some(Duration::from_millis(1100))).is_none());
        sx.goto(FAR);
        sx.settle();
        assert_eq!(sx.source.state(), Flow::Live);
        assert!(sx.shows(&pattern_row(FAR)), "not held as an empty page");
        assert!(!sx.status_now().error);
        assert_eq!(
            sx.win.stream_warning(),
            None,
            "the warning goes with the state"
        );
    }

    /// V11: a key on a byte whose page is Pending changes nothing and says so; once the bytes
    /// are here the same keys land.
    #[test]
    fn v11_a_key_on_a_pending_byte_changes_nothing() {
        let mut sx = sx(MODEL);
        sx.settle();
        sx.goto(FAR);
        let cursor = sx.win.cursor;
        sx.typed("A");
        assert!(
            sx.win.doc.patches().is_empty(),
            "typed over a byte not here"
        );
        assert_eq!(sx.win.cursor, cursor, "the cursor did not move on");
        assert_eq!(sx.win.doc.undo_len(), 0);
        let status = sx.status_now();
        assert_eq!(status.text, format!("0x{FAR:X} is still loading"));
        assert!(!sx.win.doc.dirty());
        sx.settle();
        sx.typed("A");
        sx.typed("B");
        assert_eq!(sx.win.doc.byte(FAR), Some(0xAB));
        assert_eq!(sx.win.cursor.at, FAR + 1);
        assert!(sx.win.doc.dirty());
        assert_eq!(
            sx.status(),
            format!("at 0x{FAR:X} ({FAR})"),
            "said no longer"
        );
    }

    /// The bytes of a document over a stream, each set once its page is here.
    fn set_when_here(sx: &mut Sx, doc: &mut HexDoc, at: u64, value: u8) {
        for _ in 0..100_000 {
            doc.view(at, 1, Ask::Read);
            if doc.set(at, value, Cursor::default()) {
                return;
            }
            sx.turn();
        }
        panic!("the byte at {at} never came");
    }

    /// 06 §5 through the window (W8a): told it is held without being drawn
    /// (`Window::set_shown(false)`: a browser preview behind a viewer), the hex window's stream
    /// does not poll — nothing is asked of the remote; shown again, it polls every 2 s.
    #[test]
    fn a_window_held_without_being_drawn_does_not_poll() {
        let mut sx = sx_with(
            FakeFile::pattern(MODEL),
            Link::gate(),
            Duration::from_secs(2),
            &Hooks::default(),
        )
        .unwrap();
        sx.settle();
        assert!(sx.win.ready());
        Window::set_shown(&mut sx.win, false);
        let sent = sx.sent();
        sx.run_for(Duration::from_secs(9));
        assert_eq!(sx.sent(), sent, "a hidden window asks nothing");
        Window::set_shown(&mut sx.win, true);
        sx.run_for(Duration::from_secs(5));
        let asked: Vec<Asked> = sx.control.sent()[sent..].iter().map(|s| s.asked).collect();
        assert!(asked.len() >= 4, "it polls again: {asked:?}");
        assert!(
            asked
                .iter()
                .all(|a| matches!(a, Asked::Fstat | Asked::StatPath)),
            "{asked:?}"
        );
    }

    /// V11: `changed`, `changed_count`, `dirty` and `unsaved` with 1 000 patches send no
    /// request and ask the stream for nothing — each patch holds the byte it replaced.
    #[test]
    fn v11_a_thousand_patches_are_compared_without_a_request() {
        let mut sx = sx_with(
            FakeFile::pattern(MODEL),
            Link::gate(),
            Duration::from_secs(2),
            &Hooks::default(),
        )
        .unwrap();
        let file = sx.win.file.clone();
        let mut doc = HexDoc::of(&file.lock().unwrap());
        // Far more pages than the page cache holds: most are dropped again.
        let place = |i: u64| i * 3 * PAGE + 5;
        for i in 0..1000 {
            let at = place(i);
            let was = pattern(at, 1)[0];
            set_when_here(&mut sx, &mut doc, at, !was);
        }
        for _ in 0..1_000 {
            sx.engine.pump();
            if sx.out() == 0 {
                break;
            }
            sx.engine.step(None);
        }
        let (sent, asks) = (sx.sent(), doc.asks());
        assert!(doc.dirty() && doc.unsaved());
        assert_eq!(doc.changed_count(), 1000);
        for i in 0..1000 {
            assert!(doc.changed(place(i)));
        }
        doc.mark_copied(doc.patches().clone());
        assert!(doc.dirty() && !doc.unsaved());
        for _ in 0..1000 {
            doc.undo();
        }
        assert!(!doc.dirty() && !doc.unsaved());
        assert_eq!(doc.changed_count(), 0);
        for i in 0..1000 {
            assert!(!doc.changed(place(i)));
        }
        sx.engine.pump();
        assert_eq!(doc.asks(), asks, "the stream was asked for a patch's byte");
        assert_eq!(sx.sent(), sent, "a request was sent");
        // And where the file can no longer be read at all, the answers are the same: the
        // remote changes, and the stream reads no more.
        sx.control.change(Change::Truncate(MODEL - 1));
        sx.run_for(Duration::from_secs(5));
        assert_eq!(sx.source.state(), Flow::Changed);
        doc.redo();
        assert!(doc.dirty());
        assert_eq!(doc.changed_count(), 1);
        doc.undo();
        assert!(!doc.dirty() && !doc.unsaved());
        assert_eq!(
            doc.changed_count(),
            0,
            "an unreadable byte counted as changed"
        );
    }

    /// A patch keeps the byte it replaced (06 §7.1): the edit, its undo and what counts as
    /// changed never need the file again — here it is gone from the page cache and the stream
    /// has failed.
    #[test]
    fn a_patch_keeps_the_byte_it_replaced() {
        let mut sx = sx(MODEL);
        sx.settle();
        let was = pattern(0x21, 1)[0];
        sx.goto(0x21);
        sx.typed("F");
        sx.typed("F");
        assert_eq!(sx.win.doc.byte(0x21), Some(0xFF));
        // Typed back by hand in the ASCII column of another byte: an entry equal to the file.
        sx.goto(0x30);
        let same = pattern(0x30, 1)[0];
        assert!(sx.win.doc.set(0x30, same ^ 1, Cursor::default()));
        assert!(sx.win.doc.set(0x30, same, Cursor::default()));
        // Far away, through more pages than are held; then the connection is lost for good.
        for i in 1..=(PAGES as u64 + 4) {
            sx.goto(FAR + i * PAGE);
            sx.settle();
        }
        sx.control.fault(sx.sent(), Fault::Lost);
        sx.goto(2 * FAR / 5);
        sx.settle();
        assert!(matches!(sx.source.state(), Flow::Failed(_)));
        let asks = sx.win.doc.asks();
        assert!(sx.win.doc.changed(0x21));
        assert!(!sx.win.doc.changed(0x30), "typed back: no change");
        assert_eq!(sx.win.doc.changed_count(), 1);
        assert!(sx.win.doc.dirty());
        assert_eq!(sx.win.doc.asks(), asks, "the file was read for a patch");
        // Undo puts the kept byte back.
        while sx.win.doc.can_undo() {
            sx.win.doc.undo();
        }
        assert_eq!(sx.win.doc.byte(0x21), Some(was));
        assert!(!sx.win.doc.changed(0x21));
        assert_eq!(sx.win.doc.changed_count(), 0);
        assert!(!sx.win.doc.dirty() && !sx.win.doc.unsaved());
        assert!(
            sx.win.close_requested(),
            "nothing unsaved: closes without asking"
        );
    }

    /// V11: a `SOPEN_STATE` patch waits for its bytes, in order, each an undo step; the window
    /// is not ready — and `close=ask` does not ask — before they landed.
    #[test]
    fn v11_a_state_patch_lands_once_its_bytes_arrive() {
        let hooks = Hooks {
            state: State::parse(&format!(
                "at=0x{FAR:X};patch=0x{FAR:X}:DEAD,0x10:7f;close=ask"
            ))
            .unwrap(),
            ..Hooks::default()
        };
        let mut sx = sx_with(FakeFile::pattern(MODEL), Link::gate(), NEVER, &hooks).unwrap();
        assert_eq!(sx.win.cursor.at, FAR);
        sx.frames(3);
        assert!(
            sx.win.doc.patches().is_empty(),
            "patched before its bytes came"
        );
        assert!(!sx.win.ready() && !sx.win.prompt);
        // Whenever it says it is ready, the screen has its rows.
        for _ in 0..200 {
            sx.frames(1);
            assert!(
                !sx.win.ready() || !sx.shows(&pending_row()),
                "shot too early"
            );
            sx.turn();
        }
        sx.settle();
        assert_eq!(sx.win.doc.byte(FAR), Some(0xDE));
        assert_eq!(sx.win.doc.byte(FAR + 1), Some(0xAD));
        assert_eq!(sx.win.doc.byte(0x10), Some(0x7F));
        assert_eq!(sx.win.doc.undo_len(), 3, "each byte an undo step");
        assert_eq!(sx.win.doc.changed_count(), 3);
        assert!(sx.win.ready());
        assert!(sx.win.prompt, "close=ask asks once the patches are in");
        // Ready means the screen too: the cursor's rows are there, with the patch in them.
        let mut row: Vec<Option<u8>> = pattern(FAR, 16).into_iter().map(Some).collect();
        (row[0], row[1]) = (Some(0xDE), Some(0xAD));
        // (Drawn under the close prompt, which the kit's `find_text` does not look through.)
        let drawn = sx.drv.galleys();
        assert!(
            drawn.iter().any(|(_, g)| g.text() == hex_cells(&row)),
            "the patched row is drawn"
        );
        let line = sx.win.cursor_line();
        assert!(
            line.starts_with(&format!("0x{FAR:X} ({FAR}) · 0xDE 222")),
            "{line}"
        );
        // A patch past the end is refused at once, as before.
        let hooks = Hooks {
            state: State::parse(&format!("patch=0x{:X}:0000", MODEL - 1)).unwrap(),
            ..Hooks::default()
        };
        let err = sx_with(FakeFile::pattern(MODEL), Link::gate(), NEVER, &hooks)
            .err()
            .expect("past the end");
        assert!(format!("{err:#}").contains("runs past the end"), "{err:#}");
    }

    /// 06 §7.1: a scroll-bar drag fetches only where the thumb rests — nothing for a place it
    /// passes, the place once it has rested 100 ms, and at once when it is let go.
    #[test]
    fn a_scroll_bar_drag_fetches_only_where_the_thumb_rests() {
        let mut sx = sx(MODEL);
        sx.settle();
        let sent = sx.sent();
        let at = sx.bar(0.1);
        sx.drv.move_to(&mut sx.win, at);
        sx.drv.press(&mut sx.win, PointerButton::Primary);
        sx.engine.pump();
        let mut passed = vec![sx.win.top_row * ROW / BLOCK];
        for k in 2..=6 {
            let at = sx.bar(k as f32 / 10.0);
            sx.drv.move_to(&mut sx.win, at);
            sx.engine.pump();
            assert_eq!(sx.sent(), sent, "a place the thumb passed was asked for");
            passed.push(sx.win.top_row * ROW / BLOCK);
        }
        let rest = passed.pop().unwrap();
        let off = sx.offset(sx.win.top_row * ROW);
        assert!(sx.shows(&off), "the rows under the thumb are drawn");
        assert!(sx.shows(&pending_row()), "as pending");
        // Four passes (under 100 ms of the window's clock): still nothing.
        for _ in 0..4 {
            sx.frames(1);
            sx.engine.pump();
        }
        assert_eq!(sx.sent(), sent, "asked before the thumb rested 100 ms");
        sx.frames(4);
        sx.engine.pump();
        let asked = sx.blocks_read(sent);
        assert_eq!(asked.first(), Some(&rest), "where it rests: {asked:?}");
        assert!(asked.iter().all(|b| !passed.contains(b)), "{asked:?}");
        sx.settle();
        assert!(sx.shows(&pattern_row(sx.win.top_row * ROW)));
        // Moved again and let go: asked at once.
        let sent = sx.sent();
        let at = sx.bar(0.8);
        sx.drv.move_to(&mut sx.win, at);
        sx.engine.pump();
        assert_eq!(sx.sent(), sent);
        sx.drv.release(&mut sx.win, PointerButton::Primary);
        sx.frames(1);
        sx.engine.pump();
        let block = sx.win.top_row * ROW / BLOCK;
        assert_eq!(sx.blocks_read(sent).first(), Some(&block));
        sx.settle();
        assert!(sx.shows(&pattern_row(sx.win.top_row * ROW)));
    }

    /// One reader, one read a frame: scrolling on with the wheel leaves the cursor behind, and
    /// its byte — long gone from the page cache — is never asked for again. Every read of the
    /// screen runs on from the one before: no jump.
    #[test]
    fn the_cursor_left_behind_is_not_read_again() {
        let mut sx = sx(MODEL);
        sx.settle();
        let over = sx.win.layout.unwrap().rows.center();
        for _ in 0..PAGES + 8 {
            // 84 lines a turn: 252 rows, a little under a page.
            sx.drv.wheel(&mut sx.win, over, -84.0);
            sx.settle();
        }
        assert_eq!(sx.win.cursor.at, 0);
        assert!(sx.win.top_row * ROW > PAGES as u64 * PAGE);
        let asks = sx.win.doc.asks();
        sx.frames(5);
        assert!(sx.win.cursor_line().starts_with("0x0 (0)"));
        assert_eq!(sx.win.doc.asks(), asks, "an idle frame read");
        assert_eq!(sx.source.stats().jumps, 0, "{:?}", sx.source.stats());
        // Typing there is a deliberate read of that place: the byte, then the edit.
        sx.typed("F");
        sx.settle();
        sx.typed("F");
        sx.typed("F");
        assert_eq!(sx.win.doc.byte(0), Some(0xFF));
    }

    /// The cursor stays on screen when the grid gets shorter. A warning row appears with the
    /// cursor on the last row: the grid scrolls down by just the rows it lost. With the cursor
    /// in the middle the grid does not move. The same when the window is made smaller.
    #[test]
    fn a_shorter_grid_never_hides_the_cursor() {
        let changed = |sx: &mut Sx| {
            sx.control.change(Change::Write {
                off: 0x20_0000,
                bytes: vec![0xEE; 16],
                same_second: false,
            });
            sx.run_for(Duration::from_secs(5));
            sx.frames(3);
            assert_eq!(sx.source.state(), Flow::Changed);
            assert!(sx.win.stream_warning().is_some());
        };
        let open = || {
            let mut sx = sx_with(
                FakeFile::pattern(4 * MIB),
                Link::gate(),
                Duration::from_secs(2),
                &Hooks::default(),
            )
            .unwrap();
            sx.settle();
            sx.goto(0x1000);
            sx.settle();
            sx
        };
        // On the last row.
        let mut sx = open();
        let (top, rows, row) = (sx.win.top_row, sx.win.visible, 0x1000 / ROW);
        assert_eq!(row, top + rows - 1, "the cursor is on the last whole row");
        changed(&mut sx);
        let lost = rows - sx.win.visible;
        assert!(lost >= 1, "the warning took no row");
        assert_eq!(
            sx.win.top_row,
            top + lost,
            "scrolled by the rows lost, no more"
        );
        assert_eq!(
            row,
            sx.win.top_row + sx.win.visible - 1,
            "still the last whole row"
        );
        assert!(
            sx.shows(&pattern_row(0x1000)),
            "the cursor's row is wholly on screen"
        );
        assert!(sx.shows(&sx.offset(0x1000)));
        // A smaller window: again by the rows lost.
        let (top, rows) = (sx.win.top_row, sx.win.visible);
        sx.drv.resize(760.0, 400.0);
        sx.frames(3);
        let lost = rows - sx.win.visible;
        assert!(lost >= 3, "{lost} rows lost");
        assert_eq!(sx.win.top_row, top + lost);
        assert!(sx.shows(&pattern_row(0x1000)), "after the window shrank");
        // A narrow window: the warning with its reload hint wraps onto more rows and lies
        // wholly inside its clip and the window — seen, not only drawn — and the cursor's row
        // is still on screen above it.
        const CHANGED: &str = "the file changed on the remote — Cmd/Ctrl+R reloads";
        assert_eq!(
            sx.drv.rows_of(CHANGED),
            Some(1),
            "one row at the full width"
        );
        sx.drv.resize(400.0, 400.0);
        sx.frames(4);
        let seen = sx
            .drv
            .find_text(CHANGED)
            .expect("the warning is cut or out of sight");
        assert!(seen.left() >= 0.0 && seen.right() <= 400.0, "{seen:?}");
        let rows = sx.drv.rows_of(CHANGED).unwrap();
        assert!(rows >= 2, "{rows} row at 400 points");
        let row = 0x1000 / ROW;
        assert!(
            (sx.win.top_row..sx.win.top_row + sx.win.visible).contains(&row),
            "the cursor's row under the wrapped warning"
        );
        assert!(sx.shows(&sx.offset(0x1000)));
        // In the middle of the screen: the grid's top does not move.
        let mut sx = open();
        for _ in 0..8 {
            sx.key(Key::ArrowUp, Modifiers::NONE);
        }
        let (top, rows) = (sx.win.top_row, sx.win.visible);
        changed(&mut sx);
        assert!(sx.win.visible < rows);
        assert_eq!(
            sx.win.top_row, top,
            "the grid moved with the cursor in view"
        );
        // Scrolled away from the cursor by hand: it is not pulled back.
        let mut sx = open();
        let over = sx.win.layout.unwrap().rows.center();
        sx.drv.wheel(&mut sx.win, over, 30.0);
        sx.frames(2);
        let top = sx.win.top_row;
        assert!(
            top + sx.win.visible <= 0x1000 / ROW,
            "the cursor is below the screen"
        );
        changed(&mut sx);
        assert_eq!(sx.win.top_row, top, "a cursor off screen was followed");
    }

    /// Cmd/Ctrl+R (06 §5): after the file changed on the remote the window says so, keeps its
    /// rows, and a reload shows the new version at the same offset. With unsaved edits it is
    /// refused: they were made over the old bytes.
    #[test]
    fn a_changed_file_is_said_and_reloaded() {
        let mut sx = sx_with(
            FakeFile::pattern(4 * MIB),
            Link::gate(),
            Duration::from_secs(2),
            &Hooks::default(),
        )
        .unwrap();
        sx.settle();
        sx.goto(0x1000);
        sx.settle();
        sx.control.change(Change::Write {
            off: 0x1000,
            bytes: vec![0xEE; 16],
            same_second: false,
        });
        sx.run_for(Duration::from_secs(5));
        sx.frames(2);
        assert_eq!(sx.source.state(), Flow::Changed);
        // The warning and its reload hint, whole, in the rows below the grid.
        const CHANGED: &str = "the file changed on the remote — Cmd/Ctrl+R reloads";
        assert_eq!(sx.win.stream_warning().as_deref(), Some(CHANGED));
        assert_eq!(sx.drawn(CHANGED), 1, "drawn whole");
        // Its row comes off the grid's bottom, and the cursor's row — the last — stays on
        // screen with its bytes.
        assert!(sx.shows(&pattern_row(0x1000)), "the screen it had stays");
        assert_eq!(sx.win.doc.byte(0x1000), Some(pattern(0x1000, 1)[0]));
        // An edit made over the old bytes: the reload is refused, the edit stays.
        let other = format!("{:02X}", !pattern(0x1000, 1)[0]);
        sx.typed(&other[..1]);
        sx.typed(&other[1..]);
        assert!(sx.win.doc.dirty());
        sx.key(Key::R, command(false));
        assert_eq!(sx.source.state(), Flow::Changed);
        // The window's own error is on the status row and the stream's warning stays where it
        // is, whole — now, and at every later pass while the file stays changed.
        for _ in 0..3 {
            let status = sx.status_now();
            assert!(status.error, "{}", status.text);
            assert!(status.text.starts_with("not reloaded"), "{}", status.text);
            assert_eq!(sx.drawn(CHANGED), 1, "the warning and its reload hint");
            sx.frames(2);
        }
        // Nor does any later line of the window's take it off.
        sx.goto(0x30_0000);
        sx.typed("A");
        assert_eq!(sx.drawn(CHANGED), 1);
        sx.goto(0x1000);
        sx.key(Key::Z, command(false));
        assert!(!sx.win.doc.dirty());
        assert!(
            !cmd_r_is_left(&mut sx.win, &mut sx.drv),
            "over a stream the key is the window's"
        );
        assert!(!sx.win.ready(), "not shot while it reloads");
        // The reload waits on its own thread for the stream, which this test steps.
        let started = Instant::now();
        while sx.source.core().reloads_queued() == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "no reload asked"
            );
            std::thread::yield_now();
        }
        let rev = sx.win.doc.rev();
        sx.typed("F");
        assert_eq!(sx.win.doc.rev(), rev, "edited while it reloads");
        assert_eq!(sx.status(), "reloading…");
        sx.settle();
        let ctx = sx.drv.ctx.clone();
        sx.drv.until(&mut sx.win, 60, |w| w.reloading.is_none());
        sx.win.status_text(&ctx);
        sx.settle();
        assert_eq!(sx.source.state(), Flow::Live);
        assert_eq!(sx.win.cursor.at, 0x1000, "the offset is kept");
        let row: Vec<Option<u8>> = vec![Some(0xEE); 16];
        assert!(sx.shows(&hex_cells(&row)), "the new version's bytes");
        assert_eq!(sx.status(), "reloaded");
        assert!(!sx.win.doc.can_undo(), "steps over the old bytes are gone");
    }

    /// The Source is opened before the window exists: its waker repaints the window from the
    /// window's first pass on, so a row fills in without a key. And what no arrival wakes — the
    /// status row's change after 1 s and 10 s — is asked for by the frame that shows it.
    #[test]
    fn a_block_that_arrives_repaints_the_window() {
        let mut sx = sx(MODEL);
        sx.frames(3);
        // Asked of a context of its own, where nothing else asks for a repaint.
        let later = |sx: &mut Sx| -> Vec<Duration> {
            let ctx = egui::Context::default();
            let asked = Arc::new(Mutex::new(Vec::new()));
            let seen = asked.clone();
            ctx.set_request_repaint_callback(move |info| seen.lock().unwrap().push(info.delay));
            sx.win.status_text(&ctx);
            let asked = asked.lock().unwrap().clone();
            asked
        };
        let waits = later(&mut sx);
        assert!(
            matches!(waits[..], [d] if d > Duration::from_millis(900) && d <= Duration::from_millis(1010)),
            "painted again when `loading 0x…` gains its size: {waits:?}"
        );
        // What the stream wakes repaints a window once one is bound to it — and this window
        // bound itself on its first pass.
        let (lone, ctx) = (Repaint::default(), egui::Context::default());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        ctx.set_request_repaint_callback(move |info| seen.lock().unwrap().push(info.delay));
        (lone.waker())();
        assert_eq!(
            *asked.lock().unwrap(),
            [],
            "no window yet: nothing to repaint"
        );
        lone.bind(&ctx);
        (lone.waker())();
        assert_eq!(
            *asked.lock().unwrap(),
            [Duration::ZERO],
            "a repaint at once"
        );
        assert!(sx.repaint.bound_to(&sx.drv.ctx), "the window's own context");
        // The read's reply, then its fence: the block is shown only after that, and that is
        // when the stream wakes the window. No frame runs here.
        let wakes = |sx: &Sx| sx.wakes.load(Ordering::SeqCst);
        assert_eq!(wakes(&sx), 0);
        sx.engine.pump();
        sx.one();
        assert_eq!(wakes(&sx), 0, "woken before the block was validated");
        sx.one();
        assert_eq!(
            wakes(&sx),
            1,
            "the block arrived and the window was not woken"
        );
        sx.frames(1);
        assert!(sx.shows(&pattern_row(0)));
        // A slow place: after a second the line names its size, and the next change is at 10 s.
        sx.control.set_link(|l| l.latency = Duration::from_secs(12));
        sx.goto(FAR);
        sx.engine.pump();
        sx.run_for(Duration::from_millis(1500));
        let waits = later(&mut sx);
        assert!(
            matches!(waits[..], [d] if d > Duration::from_secs(8) && d < Duration::from_secs(9)),
            "painted again when the remote is slow: {waits:?}"
        );
        sx.run_for(Duration::from_secs(9));
        assert_eq!(later(&mut sx), [], "nothing more to wait for");
    }

    /// The stats lines' events (06 §11): `first` at the first frame that draws bytes from the
    /// stream — not one with pending rows only — then `input` with its place at a jump and
    /// `ready` at the first frame showing that place's bytes.
    #[test]
    fn the_window_marks_first_input_and_ready() {
        let mut sx = sx(MODEL);
        sx.frames(3);
        assert_eq!(sx.events(), ["open"], "pending rows are not content");
        sx.settle();
        assert_eq!(sx.events(), ["open", "first"]);
        sx.frames(5);
        assert_eq!(sx.events(), ["open", "first"], "once");
        sx.goto(FAR);
        sx.frames(2);
        let input = format!("input {FAR:#x}");
        assert_eq!(sx.events(), ["open", "first", input.as_str()]);
        sx.settle();
        assert_eq!(sx.events(), ["open", "first", input.as_str(), "ready"]);
        // A key that stays on the screen shown is not an input.
        sx.key(Key::ArrowRight, Modifiers::NONE);
        sx.settle();
        assert_eq!(sx.events().len(), 4);
        // Nor one that goes back to rows still held.
        sx.key(Key::Home, command(false));
        sx.settle();
        assert_eq!(sx.events().len(), 4);
        sx.goto(FAR / 2);
        sx.settle();
        let input = format!("input {:#x}", FAR / 2);
        assert_eq!(sx.events()[4..], [input.as_str(), "ready"]);
    }

    /// A jump made before the one before it has arrived is an `input` of its own, and the
    /// `ready` that follows is its; a jump away to rows already held ends the wait with no
    /// `ready` for a place that was never shown. A frame that changes nothing marks nothing.
    #[test]
    fn each_jump_is_its_own_input() {
        let mut sx = sx(MODEL);
        sx.settle();
        let (a, b) = (FAR, FAR / 2);
        sx.goto(a);
        sx.frames(3);
        sx.goto(b);
        sx.frames(3);
        let (to_a, to_b) = (format!("input {a:#x}"), format!("input {b:#x}"));
        assert_eq!(sx.events()[2..], [to_a.as_str(), to_b.as_str()]);
        sx.settle();
        assert_eq!(sx.events()[2..], [to_a.as_str(), to_b.as_str(), "ready"]);
        sx.frames(5);
        assert_eq!(sx.events().len(), 5, "a frame at rest marks nothing");
        // To a place not here, then back to rows that are: no `ready` — A never showed.
        let c = FAR / 4;
        sx.goto(c);
        sx.frames(2);
        sx.goto(b);
        sx.settle();
        let to_c = format!("input {c:#x}");
        assert_eq!(sx.events()[5..], [to_c.as_str()], "{:?}", sx.events());
    }

    /// 06 §4: the latest place wins. A far screen asked for while the cap is full is not sent;
    /// the view then goes back to rows it holds, which reads nothing — and the far place's
    /// demand is withdrawn, so it never goes out when the link has room again.
    #[test]
    fn a_demand_left_behind_is_never_sent() {
        let mut sx = sx(MODEL);
        sx.settle();
        // Fill the cap: scroll on with the link stalled until no block more may go.
        for pass in 0.. {
            assert!(pass < 2000, "the cap never filled");
            sx.key(Key::PageDown, Modifiers::NONE);
            sx.engine.pump();
            if !sx.source.core().pf.adm.admits(BLOCK) {
                break;
            }
        }
        sx.goto(FAR);
        sx.frames(2);
        sx.engine.pump();
        let far = [FAR / BLOCK - 1, FAR / BLOCK];
        let asked = sx.blocks_read(0);
        assert!(
            !asked.iter().any(|b| far.contains(b)),
            "sent over a full cap"
        );
        assert!(sx.shows(&pending_row()), "the far place is waited for");
        // Back to the top, whose rows are held: nothing is read for it.
        let asks = sx.win.doc.asks();
        sx.key(Key::Home, command(false));
        sx.frames(2);
        assert!(sx.shows(&pattern_row(0)) && !sx.shows(&pending_row()));
        assert_eq!(sx.win.doc.asks(), asks, "a held screen was read");
        // The link has room again: what the old scroll had out arrives, and nothing is asked
        // for the place left behind.
        sx.settle();
        let asked = sx.blocks_read(0);
        assert!(
            !asked.iter().any(|b| far.contains(b)),
            "the abandoned place was fetched: {asked:?}"
        );
        // The same for a place the scroll bar's thumb passes on from.
        sx.goto(FAR);
        sx.frames(2);
        let at = sx.bar(0.3);
        sx.drv.move_to(&mut sx.win, at);
        sx.drv.press(&mut sx.win, PointerButton::Primary);
        sx.engine.pump();
        let asked = sx.blocks_read(0);
        assert!(
            !asked.iter().any(|b| far.contains(b)),
            "the place the thumb left was fetched: {asked:?}"
        );
        sx.drv.release(&mut sx.win, PointerButton::Primary);
    }

    /// Every read the window asks of its stream, with its place and length.
    struct Spy {
        inner: Box<dyn Feed>,
        log: Arc<Mutex<Vec<(u64, usize)>>>,
    }

    impl crate::stream::Bytes for Spy {
        fn len(&self) -> u64 {
            self.inner.len()
        }
        fn try_read(
            &mut self,
            off: u64,
            len: usize,
        ) -> std::task::Poll<crate::stream::Result<bytes::Bytes>> {
            self.log.lock().unwrap().push((off, len));
            self.inner.try_read(off, len)
        }
        fn read_at(
            &mut self,
            off: u64,
            buf: &mut [u8],
            cancel: &AtomicBool,
        ) -> crate::stream::Result<usize> {
            self.inner.read_at(off, buf, cancel)
        }
    }

    impl Feed for Spy {
        fn waiting(&self) -> Option<crate::stream::Waiting> {
            self.inner.waiting()
        }
        fn renew(&mut self) {
            self.inner.renew()
        }
    }

    /// V2's `delivered` is a union, so it cannot see the same bytes read twice. Here every read
    /// is seen: a frame asks its stream at most once, for whole pages of the screen it shows —
    /// never more than the screen's pages, and never for a page it holds.
    #[test]
    fn a_frame_reads_one_span_of_the_screens_pages() {
        let mut sx = sx(MODEL);
        let log = Arc::new(Mutex::new(Vec::new()));
        let null = Arc::new(File::open("/dev/null").unwrap());
        let inner = std::mem::replace(&mut sx.win.doc.pages.feed, Box::new(InPlace(null)));
        sx.win.doc.pages.feed = Box::new(Spy {
            inner,
            log: log.clone(),
        });
        sx.settle();
        let over = sx.win.layout.unwrap().rows.center();
        sx.drv.move_to(&mut sx.win, over);
        let mut read = std::collections::HashSet::new();
        let mut seen = log.lock().unwrap().len();
        for pass in 0..600 {
            // Down by a few rows, by a screen, or not at all; the link answers now and then.
            let lines = [-2.0, 0.0, -11.0, 0.0, 0.0, -40.0][pass % 6];
            let wheel = Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::Vec2::new(0.0, lines),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
                modifiers: Modifiers::NONE,
            };
            sx.drv.frame(&mut sx.win, vec![wheel], Modifiers::NONE);
            if pass % 3 == 0 {
                sx.turn();
            }
            let all = log.lock().unwrap().clone();
            let asked: Vec<(u64, usize)> =
                all[seen..].iter().copied().filter(|r| r.1 > 0).collect();
            seen = all.len();
            assert!(asked.len() <= 1, "pass {pass}: {asked:?}");
            let Some(&(off, len)) = asked.first() else {
                continue;
            };
            let (top, rows) = (sx.win.top_row * ROW, sx.win.rows_painted * ROW);
            let (first, last) = (top / PAGE, (top + rows - 1) / PAGE);
            assert_eq!(off % PAGE, 0, "pass {pass}");
            assert_eq!(len as u64 % PAGE, 0, "pass {pass}");
            let pages = off / PAGE..(off + len as u64) / PAGE;
            assert!(
                pages.start >= first && pages.end <= last + 1,
                "pass {pass}: pages {pages:?} for a screen on {first}..={last}"
            );
            // A page that came is not asked for again while it is held.
            if sx.win.doc.pending_at().is_none() {
                for page in pages {
                    assert!(read.insert(page), "pass {pass}: page {page} read twice");
                }
            }
        }
        assert!(read.len() > 20, "{} pages read", read.len());
    }

    // ---- the interim Save (06 §7.1, until 07's lands): on a real file, the engine running ----

    /// The hex window on a Source over a real local file, the `Origin` on the same file.
    struct Live {
        tmp: tempfile::TempDir,
        remote: PathBuf,
        source: Source,
        counts: Counts,
        win: HexWindow,
        drv: Driver,
    }

    /// `body` as a "remote" file, a stream open on it and the hex file over that stream.
    fn live_file(body: &[u8]) -> (tempfile::TempDir, PathBuf, Source, Counts, HexFile) {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote/model.bin");
        std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
        std::fs::write(&remote, body).unwrap();
        let abs = remote.to_str().unwrap().to_string();
        let transport = Box::new(LocalFs::new()).into_ranged(&abs).unwrap();
        let again = abs.clone();
        let repaint = Repaint::default();
        let source = Source::open(
            transport,
            Box::new(move || {
                Box::new(LocalFs::new())
                    .into_ranged(&again)
                    .map_err(|refused| refused.error)
            }),
            repaint.waker(),
        )
        .unwrap();
        let counts = Counts::default();
        let at = LocalFs::new().stat(&abs).unwrap();
        let origin = Origin::new(
            Box::new(Counting(LocalFs::new(), counts.clone())),
            None,
            Some("host".into()),
            abs,
            at,
        );
        let cache = tmp.path().join("cache");
        let file = HexFile::streamed(origin, source.clone(), repaint, &cache);
        (tmp, remote, source, counts, file)
    }

    fn live(body: &[u8]) -> Live {
        let (tmp, remote, source, counts, file) = live_file(body);
        let win = embedded(
            file,
            &Hooks::default(),
            Vec::new(),
            Arc::default(),
            Arc::default(),
        )
        .unwrap();
        let mut live = Live {
            tmp,
            remote,
            source,
            counts,
            win,
            drv: Driver::new(760.0, 560.0),
        };
        live.drv.until(&mut live.win, 60, |w| w.ready());
        live
    }

    /// The folder part of a path.
    fn parent_of(path: &str) -> &str {
        path.rsplit_once('/').map_or("", |(dir, _)| dir)
    }

    impl Live {
        /// Every file under the test's folder, relative to it, sorted.
        fn inventory(&self) -> Vec<String> {
            let root = self.tmp.path();
            let mut all: Vec<String> = walk(root)
                .into_iter()
                .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().into_owned())
                .collect();
            all.sort();
            all
        }

        fn typed(&mut self, text: &str) {
            for c in text.chars() {
                self.drv.frame(
                    &mut self.win,
                    vec![Event::Text(c.to_string())],
                    Modifiers::NONE,
                );
            }
            self.drv.idle(&mut self.win, 1);
        }

        fn save(&mut self) {
            self.drv.key(&mut self.win, Key::S, command(false));
            self.drv.until(&mut self.win, 60, |w| w.job.is_none());
            self.drv.until(&mut self.win, 60, |w| w.ready());
        }

        fn fetches(&self) -> usize {
            self.counts.fetches.load(Ordering::SeqCst)
        }

        fn backups(&self) -> Vec<PathBuf> {
            walk(&self.tmp.path().join("cache/.backup"))
        }
    }

    fn body() -> Vec<u8> {
        (0..200_000u32).map(|i| (i % 251) as u8).collect()
    }

    /// Save of a streamed file: only now is the whole file fetched, once, into the cache; the
    /// Save runs on that copy (backup, conflict-checked overwrite); the Source is reloaded onto
    /// the saved file and stays `Live`; a second Save fetches nothing more.
    #[test]
    fn the_interim_save_fetches_once_saves_and_reloads() {
        let body = body();
        let mut fx = live(&body);
        assert_eq!(fx.counts.calls.load(Ordering::SeqCst), 0, "viewing");
        assert!(walk(&fx.tmp.path().join("cache")).is_empty(), "viewing");
        fx.typed("AB");
        assert_eq!(fx.fetches(), 0, "an edit fetches nothing");
        fx.save();
        assert!(
            fx.win.status.text.starts_with("saved "),
            "{}",
            fx.win.status.text
        );
        assert_eq!(fx.fetches(), 1, "the one whole fetch");
        let mut want = body.clone();
        want[0] = 0xAB;
        assert_eq!(std::fs::read(&fx.remote).unwrap(), want);
        let backups = fx.backups();
        assert_eq!(backups.len(), 1, "{backups:?}");
        assert_eq!(std::fs::read(&backups[0]).unwrap(), body);
        // The view is the saved file's, and nothing counts as changed against it.
        assert_eq!(fx.source.state(), Flow::Live);
        fx.drv.until(&mut fx.win, 60, |w| w.ready());
        assert_eq!(
            fx.win.doc.original(0),
            Some(0xAB),
            "read from the saved file"
        );
        assert_eq!(fx.win.doc.byte(0), Some(0xAB));
        assert!(!fx.win.doc.dirty() && !fx.win.doc.unsaved());
        assert!(fx.win.close_requested());
        // A second Save: no second fetch, and both edits are in the file.
        fx.drv.key(&mut fx.win, Key::ArrowRight, Modifiers::NONE);
        fx.typed("CD");
        assert!(fx.win.doc.dirty());
        fx.save();
        assert_eq!(fx.fetches(), 1, "fetched again");
        want[2] = 0xCD;
        assert_eq!(std::fs::read(&fx.remote).unwrap(), want);
        assert_eq!(fx.backups().len(), 1, "one backup a session");
        assert_eq!(fx.source.state(), Flow::Live);
        assert!(!fx.win.doc.dirty());
    }

    /// The whole fetch must be the file the stream opened (06 §7.1): another size, or another
    /// mtime, is refused as a change — nothing written, the edits listed. Each alone is enough.
    #[test]
    fn the_interim_save_refuses_a_file_that_changed_since_it_was_opened() {
        let body = body();
        let mut longer = body.clone();
        longer.extend_from_slice(b"appended by someone else");
        let mut rewritten = body.clone();
        rewritten[100] ^= 0xff;
        let later = Duration::from_secs(5);
        for (other, moved, why) in [
            (longer, Duration::ZERO, "bytes then,"),
            (rewritten, later, "its modification time changed"),
        ] {
            let mut fx = live(&body);
            fx.typed("AB");
            let opened = std::fs::metadata(&fx.remote).unwrap().modified().unwrap();
            std::fs::write(&fx.remote, &other).unwrap();
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&fx.remote)
                .unwrap();
            file.set_modified(opened + moved).unwrap();
            drop(file);
            fx.drv.key(&mut fx.win, Key::S, command(false));
            fx.drv.until(&mut fx.win, 60, |w| w.job.is_none());
            fx.drv.idle(&mut fx.win, 2);
            assert_eq!(fx.win.status.text, "not saved: the file changed", "{why}");
            assert!(
                std::fs::read(&fx.remote).unwrap() == other,
                "{why}: written over"
            );
            assert!(fx.backups().is_empty(), "{why}");
            let notice = fx.win.notices.join("\n");
            assert!(
                notice.contains("changed since it was opened") && notice.contains(why),
                "{why}: {notice}"
            );
            let lists: Vec<PathBuf> = walk(fx.tmp.path())
                .into_iter()
                .filter(|p| p.to_string_lossy().contains(".hexpatch-"))
                .collect();
            assert_eq!(lists.len(), 1, "{why}: {lists:?}");
            let list = std::fs::read_to_string(&lists[0]).unwrap();
            assert!(list.ends_with("0x0: AB\n"), "{why}: {list}");
            assert!(fx.win.doc.dirty(), "{why}: the edit stays in the window");
            // Everything on disk, by name: the remote file, the fetched copy with its record
            // and lock (the cache's, as any fetch leaves them), and the list of the edits.
            // No `.part`, no staged copy, nothing beside the remote.
            let list = lists[0].file_name().unwrap().to_string_lossy().into_owned();
            let abs = fx
                .remote
                .to_str()
                .unwrap()
                .trim_start_matches('/')
                .to_string();
            let mut want = vec![
                "remote/model.bin".to_string(),
                format!("cache/host/{abs}"),
                format!("cache/.meta/record/host/{abs}"),
                format!("cache/.meta/lock/host/{abs}"),
                format!("cache/.edits/host/{}/{list}", parent_of(&abs)),
            ];
            want.sort();
            assert_eq!(fx.inventory(), want, "{why}");
        }
    }

    /// The whole fetch says its progress on the status row and Esc stops it: nothing is
    /// written and the edit stays. Closing during it stops it too, and then asks.
    #[test]
    fn the_interim_fetch_is_stopped_by_esc_and_by_closing() {
        let body = body();
        let mut fx = live(&body);
        fx.typed("AB");
        fx.counts.hold.store(true, Ordering::SeqCst);
        fx.drv.key(&mut fx.win, Key::S, command(false));
        fx.drv.until(&mut fx.win, 60, |w| {
            w.fetching.active.load(Ordering::SeqCst)
        });
        fx.drv.idle(&mut fx.win, 1);
        let status = fx.win.status_text(&fx.drv.ctx.clone()).text;
        assert!(
            status.starts_with("fetching the file to save it") && status.ends_with("Esc stops"),
            "{status}"
        );
        fx.drv.key(&mut fx.win, Key::Escape, Modifiers::NONE);
        fx.drv.until(&mut fx.win, 60, |w| w.job.is_none());
        fx.drv.idle(&mut fx.win, 1);
        assert_eq!(fx.win.status.text, "stopped: nothing was written");
        assert_eq!(std::fs::read(&fx.remote).unwrap(), body);
        assert!(fx.win.doc.dirty() && fx.backups().is_empty());
        // Nothing is left of the stopped fetch but the claim's lock: no copy, no `.part`, no
        // staged file, no list.
        let abs = fx
            .remote
            .to_str()
            .unwrap()
            .trim_start_matches('/')
            .to_string();
        let mut stopped = vec![
            "remote/model.bin".to_string(),
            format!("cache/.meta/lock/host/{abs}"),
        ];
        stopped.sort();
        assert_eq!(fx.inventory(), stopped, "after Esc");
        // Closing while it fetches: it is stopped, not waited for, and the edit is asked about.
        fx.drv.key(&mut fx.win, Key::S, command(false));
        fx.drv.until(&mut fx.win, 60, |w| {
            w.fetching.active.load(Ordering::SeqCst)
        });
        assert!(!fx.win.close_requested(), "closed with an edit unsaved");
        fx.drv.until(&mut fx.win, 60, |w| w.job.is_none());
        fx.drv.idle(&mut fx.win, 1);
        assert!(fx.win.prompt, "the unsaved edit is asked about");
        assert_eq!(std::fs::read(&fx.remote).unwrap(), body);
        assert_eq!(fx.fetches(), 2);
        assert_eq!(fx.inventory(), stopped, "after closing");
    }

    /// A fetch that is really under way when it is stopped — bytes read, its `.part` being
    /// filled — leaves no `.part` and no copy either.
    #[test]
    fn a_fetch_stopped_midway_leaves_no_part_file() {
        let body: Vec<u8> = (0..(8u32 << 20)).map(|i| (i % 251) as u8).collect();
        let mut fx = live(&body);
        fx.typed("AB");
        fx.counts.midway.store(true, Ordering::SeqCst);
        fx.drv.key(&mut fx.win, Key::S, command(false));
        fx.drv.until(&mut fx.win, 60, |w| {
            w.fetching.active.load(Ordering::SeqCst) && w.fetching.done.load(Ordering::SeqCst) > 0
        });
        let parts = |fx: &Live| -> usize {
            let all = fx.inventory();
            all.iter().filter(|p| p.contains(".part-")).count()
        };
        assert_eq!(
            parts(&fx),
            1,
            "the fetch is under way: {:?}",
            fx.inventory()
        );
        fx.drv.key(&mut fx.win, Key::Escape, Modifiers::NONE);
        fx.drv.until(&mut fx.win, 60, |w| w.job.is_none());
        fx.drv.idle(&mut fx.win, 1);
        assert_eq!(fx.win.status.text, "stopped: nothing was written");
        assert!(std::fs::read(&fx.remote).unwrap() == body, "the remote");
        let abs = fx
            .remote
            .to_str()
            .unwrap()
            .trim_start_matches('/')
            .to_string();
        let mut want = vec![
            "remote/model.bin".to_string(),
            format!("cache/.meta/lock/host/{abs}"),
        ];
        want.sort();
        assert_eq!(fx.inventory(), want);
    }

    /// The same when the fetch itself fails, and when it is stopped: the claim set the earlier
    /// copy aside before a byte was asked for, so its place is said whatever the fetch did.
    #[test]
    fn a_failed_or_stopped_fetch_still_says_where_an_earlier_copy_went() {
        for stopped in [false, true] {
            let body = body();
            let mut fx = live(&body);
            fx.typed("AB");
            let cache = fx.tmp.path().join("cache");
            let cached = crate::cache::cache_path(&cache, "host", fx.remote.to_str().unwrap());
            crate::cache::private_dir(cached.parent().unwrap()).unwrap();
            std::fs::write(&cached, b"an earlier session's edits").unwrap();
            let flag = if stopped {
                &fx.counts.hold
            } else {
                &fx.counts.fail
            };
            flag.store(true, Ordering::SeqCst);
            fx.drv.key(&mut fx.win, Key::S, command(false));
            if stopped {
                fx.drv.until(&mut fx.win, 60, |w| {
                    w.fetching.active.load(Ordering::SeqCst)
                });
                fx.drv.key(&mut fx.win, Key::Escape, Modifiers::NONE);
            }
            fx.drv.until(&mut fx.win, 60, |w| w.job.is_none());
            fx.drv.idle(&mut fx.win, 2);
            let want = if stopped {
                "stopped: nothing was written"
            } else {
                "save failed"
            };
            assert_eq!(fx.win.status.text, want);
            assert!(std::fs::read(&fx.remote).unwrap() == body, "the remote");
            let said = fx.win.opened.join("\n");
            let kept: Vec<String> = fx
                .inventory()
                .into_iter()
                .filter(|p| {
                    std::fs::read(fx.tmp.path().join(p)).unwrap() == b"an earlier session's edits"
                })
                .collect();
            assert_eq!(kept.len(), 1, "{want}: {kept:?}");
            assert!(
                said.contains("kept it at ") && said.contains(&kept[0]),
                "{want}: the earlier copy's place is not said: {said:?}"
            );
            // Said once, however often the Save is tried again.
            fx.drv.key(&mut fx.win, Key::S, command(false));
            if stopped {
                fx.drv.until(&mut fx.win, 60, |w| {
                    w.fetching.active.load(Ordering::SeqCst)
                });
                fx.drv.key(&mut fx.win, Key::Escape, Modifiers::NONE);
            }
            fx.drv.until(&mut fx.win, 60, |w| w.job.is_none());
            fx.drv.idle(&mut fx.win, 2);
            let again = fx.win.opened.join("\n");
            assert_eq!(again.matches("kept it at ").count(), 1, "{want}: {again}");
        }
    }

    /// What the whole fetch set aside is said even when the Save is then refused — the
    /// earlier copy's place reaches the owner whatever the fingerprint check says.
    #[test]
    fn a_refused_fetch_still_says_where_an_earlier_copy_went() {
        let body = body();
        let mut fx = live(&body);
        fx.typed("AB");
        // An earlier session's copy, with no record of what it held, where the fetch goes.
        let cache = fx.tmp.path().join("cache");
        let cached = crate::cache::cache_path(&cache, "host", fx.remote.to_str().unwrap());
        crate::cache::private_dir(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, b"an earlier session's edits").unwrap();
        let mut longer = body.clone();
        longer.extend_from_slice(b"appended by someone else");
        std::fs::write(&fx.remote, &longer).unwrap();
        fx.drv.key(&mut fx.win, Key::S, command(false));
        fx.drv.until(&mut fx.win, 60, |w| w.job.is_none());
        fx.drv.idle(&mut fx.win, 2);
        assert_eq!(fx.win.status.text, "not saved: the file changed");
        let said = fx.win.opened.join("\n");
        assert!(
            said.contains("kept it at "),
            "the earlier copy's place: {said:?}"
        );
        let kept: Vec<String> = fx
            .inventory()
            .into_iter()
            .filter(|p| {
                std::fs::read(fx.tmp.path().join(p)).unwrap() == b"an earlier session's edits"
            })
            .collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(said.contains(&kept[0]), "{said} names {kept:?}");
        let refusal = fx.win.notices.join("\n");
        assert!(refusal.contains("changed since it was opened"), "{refusal}");
    }

    /// Save copy of a streamed file runs on the same one fetch and writes a new sibling.
    #[test]
    fn the_interim_save_copy_writes_a_sibling() {
        let body = body();
        let mut fx = live(&body);
        fx.typed("AB");
        fx.drv.key(&mut fx.win, Key::S, command(true));
        fx.drv.until(&mut fx.win, 60, |w| w.job.is_none());
        fx.drv.idle(&mut fx.win, 2);
        let copy = fx.remote.with_file_name("model.edited.bin");
        let mut want = body.clone();
        want[0] = 0xAB;
        assert_eq!(std::fs::read(&copy).unwrap(), want);
        assert_eq!(std::fs::read(&fx.remote).unwrap(), body, "the original");
        assert_eq!(fx.fetches(), 1);
        assert_eq!(fx.source.state(), Flow::Live);
        assert!(fx.win.doc.dirty() && !fx.win.doc.unsaved());
    }

    /// After Cmd/Ctrl+R the file read is another version: the next Save fetches that one and
    /// backs it up first — the backup of the version before does not stand in for it.
    #[test]
    fn a_save_after_a_reload_backs_up_the_reloaded_version() {
        let body = body();
        let mut fx = live(&body);
        fx.typed("AB");
        fx.save();
        assert_eq!((fx.fetches(), fx.backups().len()), (1, 1));
        // Someone else rewrites the file; the owner reloads.
        let mut other: Vec<u8> = body.iter().map(|b| b ^ 0x55).collect();
        other.extend_from_slice(b"and more");
        std::fs::write(&fx.remote, &other).unwrap();
        fx.drv.key(&mut fx.win, Key::R, command(false));
        fx.drv
            .until(&mut fx.win, 60, |w| w.reloading.is_none() && w.ready());
        assert_eq!(fx.win.status.text, "reloaded");
        assert_eq!(
            fx.win.doc.size(),
            other.len() as u64,
            "the new version's size"
        );
        assert_eq!(fx.win.doc.byte(1), Some(other[1]));
        assert!(!fx.win.doc.dirty() && fx.win.doc.patches().is_empty());
        // An edit over the reloaded version, saved.
        assert_eq!(fx.win.cursor.at, 1);
        fx.typed("CD");
        fx.save();
        assert!(
            fx.win.status.text.starts_with("saved "),
            "{} {:?}",
            fx.win.status.text,
            fx.win.notices
        );
        assert_eq!(
            fx.fetches(),
            2,
            "the reloaded version was fetched for the Save"
        );
        // (The cursor moved on to byte 1 when the first edit was typed.)
        let mut want = other.clone();
        want[1] = 0xCD;
        assert!(
            std::fs::read(&fx.remote).unwrap() == want,
            "the saved bytes"
        );
        let backups: Vec<Vec<u8>> = fx
            .backups()
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        assert_eq!(backups.len(), 2, "no backup of the reloaded version");
        assert!(backups.contains(&body) && backups.contains(&other));
    }

    /// `SOPEN_AUTOSAVE` over a stream, no window: a state patch far from the start reads its
    /// page (and only that), then the interim Save fetches, backs up and writes.
    #[test]
    fn autosave_over_a_stream_patches_and_saves() {
        let body = body();
        let (tmp, remote, source, counts, file) = live_file(&body);
        let hooks = Hooks {
            state: State::parse("patch=0x20000:BEEF,3:00").unwrap(),
            autosave: true,
            ..Hooks::default()
        };
        let outcome = open(file, &hooks, Vec::new()).unwrap();
        assert!(outcome.saved && outcome.refused.is_none());
        let mut want = body.clone();
        (want[0x20000], want[0x20001], want[3]) = (0xBE, 0xEF, 0x00);
        assert_eq!(std::fs::read(&remote).unwrap(), want);
        let backups = walk(&tmp.path().join("cache/.backup"));
        assert_eq!(backups.len(), 1, "{backups:?}");
        assert_eq!(std::fs::read(&backups[0]).unwrap(), body);
        assert_eq!(counts.fetches.load(Ordering::SeqCst), 1);
        // The patches read their own pages through the stream: two blocks, not the file.
        let stats = source.stats();
        assert_eq!(
            stats.delivered,
            2 * PAGE,
            "the two patches' pages and nothing else through the stream: {stats:?}"
        );
        // A patch past the end, and one with nothing to change: no save, nothing fetched.
        let (_tmp, remote, _source, counts, file) = live_file(&body);
        let same = format!("patch=0:{:02X}", body[0]);
        let hooks = Hooks {
            state: State::parse(&same).unwrap(),
            autosave: true,
            ..Hooks::default()
        };
        let outcome = open(file, &hooks, Vec::new()).unwrap();
        assert!(!outcome.saved);
        assert_eq!(counts.fetches.load(Ordering::SeqCst), 0, "nothing to save");
        assert_eq!(std::fs::read(&remote).unwrap(), body);
    }

    /// Over OpenSSH's own `sftp-server` (06 §13), a session for the stream and one for the
    /// `Origin`: the first rows come through SFTP reads, and the interim Save fetches, checks
    /// the fetch against what the stream opened (both stats carry SFTP's whole seconds), backs
    /// up, writes and reloads the stream onto the saved file.
    #[test]
    fn the_window_streams_and_saves_over_a_real_sftp_server() {
        let (Some(view), Some(mut write)) =
            (SftpRemote::local_server(), SftpRemote::local_server())
        else {
            return;
        };
        let body = body();
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote/model.bin");
        std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
        std::fs::write(&remote, &body).unwrap();
        let abs = remote.to_str().unwrap().to_string();
        let at = write.stat(&abs).unwrap();
        let transport = Box::new(view)
            .into_ranged(&abs)
            .map_err(|refused| refused.error)
            .unwrap();
        let repaint = Repaint::default();
        let source = Source::open(
            transport,
            Box::new(|| Err(anyhow!("this test has no second connection"))),
            repaint.waker(),
        )
        .unwrap();
        let origin = Origin::new(Box::new(write), None, Some("host".into()), abs, at);
        let cache = tmp.path().join("cache");
        let file = HexFile::streamed(origin, source.clone(), repaint, &cache);
        let hooks = Hooks::default();
        let mut win = embedded(file, &hooks, Vec::new(), Arc::default(), Arc::default()).unwrap();
        let mut drv = Driver::new(760.0, 560.0);
        drv.until(&mut win, 60, |w| w.ready());
        let row: Vec<Option<u8>> = body[..16].iter().copied().map(Some).collect();
        assert!(drv.find_text(&hex_cells(&row)).is_some(), "the first row");
        assert_eq!(
            source.fetched().total,
            BLOCK,
            "one block for the first screen"
        );
        assert!(walk(&cache).is_empty(), "viewing wrote to disk");
        for c in ["A", "B"] {
            drv.frame(&mut win, vec![Event::Text(c.into())], Modifiers::NONE);
        }
        drv.key(&mut win, Key::S, command(false));
        drv.until(&mut win, 60, |w| w.job.is_none());
        drv.until(&mut win, 60, |w| w.ready());
        assert!(
            win.status.text.starts_with("saved "),
            "{} {:?}",
            win.status.text,
            win.notices
        );
        let mut want = body.clone();
        want[0] = 0xAB;
        assert!(std::fs::read(&remote).unwrap() == want, "the saved bytes");
        let backups = walk(&cache.join(".backup"));
        assert_eq!(backups.len(), 1, "{backups:?}");
        assert!(std::fs::read(&backups[0]).unwrap() == body, "the backup");
        assert_eq!(source.state(), Flow::Live, "sopen's own write is no change");
        assert_eq!(win.doc.original(0), Some(0xAB), "read from the saved file");
        assert!(!win.doc.dirty() && win.close_requested());
    }
}
