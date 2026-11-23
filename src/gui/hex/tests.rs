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

/// The edits of the current generation of the journal at `dir` (their lines) and its evidence
/// (page → SHA-256), read back through `CURRENT` as a reopen reads them.
fn kept_in(dir: &Path) -> (Vec<String>, Vec<(u64, crate::journal::Sha)>) {
    let current =
        crate::journal::parse_current(&std::fs::read(dir.join("CURRENT")).unwrap()).unwrap();
    let gen = std::fs::read(dir.join(format!("gen-{}", current.n))).unwrap();
    let gen = crate::journal::parse_generation(&gen).unwrap();
    assert_eq!(gen.at, current, "CURRENT names this generation");
    let edits = gen
        .edits
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(str::to_string)
        .collect();
    let seen = match gen.seen {
        crate::journal::Seen::Known(ev) => ev.pages.into_iter().collect(),
        crate::journal::Seen::Pending => panic!("no evidence"),
    };
    (edits, seen)
}

/// The SHA-256 of the 4 KiB page at `page` of `path` (shorter at the end).
fn page_sha(path: &Path, page: u64) -> crate::journal::Sha {
    let bytes = std::fs::read(path).unwrap();
    let end = (page as usize + PAGE as usize).min(bytes.len());
    crate::journal::sha256(&bytes[page as usize..end])
}

/// The journal folders under `cache`.
fn journal_dirs_in(cache: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = walk(&cache.join(".edits"))
        .into_iter()
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .filter(|d| d.to_string_lossy().contains(".journal-"))
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
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
    assert!(fx
        .drv
        .text_starting_with("unsaved — ")
        .is_some_and(|t| t.starts_with("unsaved — kept") || t == "unsaved — keeping…"));
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
    // Nothing but the list of the edits and the journal that keeps them (07 §3): no backup, no
    // stage.
    let cache = fx.tmp.path().join("cache");
    let (journal, left): (Vec<PathBuf>, Vec<PathBuf>) = walk(&cache)
        .into_iter()
        .partition(|p| p.to_string_lossy().contains("local.bin.journal-"));
    let mut kept: Vec<String> = journal
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    kept.sort();
    assert_eq!(
        kept,
        ["CURRENT", "gen-1", "lock"],
        "the journal: {journal:?}"
    );
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

/// After a Save of a local file in place, an undo is an edit of the saved file like any: its
/// page's evidence is taken again and the journal keeps it (never `NOT kept`).
#[test]
fn an_undo_after_a_save_in_place_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.bin");
    std::fs::write(&path, FIXTURE).unwrap();
    let cache = tmp.path().join("cache");
    let mut fx = in_place(&path, tmp, 1000.0, 600.0);
    fx.typed("00");
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    assert!(!fx.win.doc.dirty());
    let saved = std::fs::read(&path).unwrap();
    assert_eq!(saved[0], 0x00);
    fx.key(Key::Z, command(false));
    assert!(fx.win.doc.unsaved());
    fx.drv
        .until(&mut fx.win, 20, |w| w.schedule.status() != Keep::Keeping);
    assert_eq!(fx.win.schedule.status(), Keep::Kept);
    let (line, _, failed) = fx.win.kept_line();
    assert!(!failed && line.starts_with("unsaved — kept at "), "{line}");
    // What is kept: the undo's byte against the saved file, and the saved page as evidence.
    let dirs = journal_dirs_in(&cache);
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    let (edits, seen) = kept_in(&dirs[0]);
    assert_eq!(edits, ["0x0: 73 was 00"]);
    assert_eq!(seen, [(0, crate::journal::sha256(&saved))]);
    // It restores against the saved file: a kill, a new window on it, Restore.
    let abs = path.to_str().unwrap().to_string();
    let at = LocalFs::new().stat(&abs).unwrap();
    let origin = Origin::new(Box::new(LocalFs::new()), None, None, abs, at);
    let again = window_for(HexFile::open(origin, &path, None, &cache).unwrap());
    std::mem::replace(&mut fx.win, again).crash();
    fx.drv = Driver::new(1000.0, 600.0);
    fx.drv.until(&mut fx.win, 20, |w| w.ready());
    assert_eq!(fx.win.rows.len(), 1, "one journal to restore");
    fx.win.row_action(0, RowAction::Restore);
    fx.drv.idle(&mut fx.win, 2);
    assert_eq!(fx.win.doc.byte(0), Some(b's'), "the undo, restored");
    assert!(fx.win.doc.unsaved());
}

/// 07 §11: the edits of one window are at most 100 000 changed offsets (with the undo steps,
/// at most 16 MiB of memory). Past the bound a byte at a new offset is refused — the status row
/// says to save first — while a byte already edited can still change. A Save in place makes
/// room again.
#[test]
fn edits_past_the_bound_are_refused_until_a_save() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("big.bin");
    std::fs::write(&path, vec![0u8; 120_000]).unwrap();
    let mut fx = in_place(&path, tmp, 1000.0, 600.0);
    for at in 0..100_000u64 {
        assert!(fx.win.doc.set(at, 0xEE, Cursor::default()), "0x{at:X}");
    }
    assert_eq!(fx.win.doc.patches().len(), 100_000);
    // At the bound: a new offset is refused, an edited one still changes.
    assert!(
        !fx.win.doc.set(100_000, 0xEE, Cursor::default()),
        "past the bound"
    );
    assert!(fx.win.doc.set(5, 0x11, Cursor::default()), "an edited byte");
    // The window says so when a key is refused there.
    fx.win.place(110_000, Pane::Hex);
    fx.typed("AB");
    assert_eq!(fx.win.doc.byte(110_000), Some(0));
    assert_eq!(fx.win.status.text, EDITS_FULL);
    assert!(fx.win.status.error);
    // A Save in place makes room: the bytes saved are the file's now — every edited byte,
    // offset 5's last value included, and nothing past the bound.
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    let file = std::fs::read(&path).unwrap();
    for (at, &b) in file.iter().enumerate().take(100_000) {
        let want = if at == 5 { 0x11 } else { 0xEE };
        assert_eq!(b, want, "0x{at:X}");
    }
    assert_eq!((file[100_000], file[110_000]), (0, 0), "past the bound");
    assert_eq!(
        fx.win.doc.patches().len(),
        0,
        "the saved bytes are the file's"
    );
    fx.win.place(110_000, Pane::Hex);
    fx.typed("AB");
    assert_eq!(fx.win.doc.byte(110_000), Some(0xAB));
    assert_eq!(fx.win.doc.patches().len(), 1);
}

/// What the edits keep besides the entries — each edited offset's file byte and each edited
/// page's evidence — is bounded too: offsets nothing names any more go. Two ways to leave them
/// behind, a thousand times each: edits undone and then replaced by new ones (their redo steps
/// cleared), and a byte typed and typed back while the undo history drops its oldest steps.
#[test]
fn what_the_edits_keep_stays_bounded() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("pages.bin");
    std::fs::write(&path, vec![0u8; 2100 * PAGE as usize]).unwrap();
    let mut fx = in_place(&path, tmp, 1000.0, 600.0);
    let doc = &mut fx.win.doc;
    doc.set_edit_cap(2);
    let c = Cursor::default();
    for k in 0..1000u64 {
        let (a, b) = (2 * k * PAGE, (2 * k + 1) * PAGE);
        assert!(doc.set(a, 0xAA, c) && doc.set(b, 0xBB, c), "{k}");
        assert!(doc.undo().is_some() && doc.undo().is_some(), "{k}");
        let (bytes, pages) = doc.kept_len();
        assert!(
            bytes <= 100 && pages <= 100,
            "{k}: {bytes} bytes, {pages} pages kept"
        );
    }
    assert!(doc.patches().is_empty() && !doc.unsaved());
    doc.set_undo_cap(4);
    for k in 0..1000u64 {
        let at = (k % 2100) * PAGE + 7;
        assert!(doc.set(at, 0x11, c) && doc.set(at, 0x00, c), "{k}");
        let (bytes, pages) = doc.kept_len();
        assert!(
            bytes <= 100 && pages <= 100,
            "{k}: {bytes} bytes, {pages} pages kept"
        );
    }
    // What is still named is still right: undo goes back through the last steps.
    assert_eq!(doc.undo_len(), 4);
    while doc.undo().is_some() {}
    assert!(doc.patches().is_empty() && !doc.unsaved());
}

/// 07 §11's bound holds through the history too (bound 2 here): undo and redo put a byte in
/// as an edit does — counted, refused past the bound with the status line, the step left on its
/// stack — and a byte back at the file's own value is no entry at all. The sequence: edit A and
/// B, Save; undo B; edit C; undo C; undo A — the bytes and the bound checked at every step.
#[test]
fn undo_and_redo_hold_the_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("bound.bin");
    std::fs::write(&path, [0u8; 32]).unwrap();
    let mut fx = in_place(&path, tmp, 1000.0, 600.0);
    fx.win.doc.set_edit_cap(2);
    let (a, b, c) = (0u64, 1u64, 2u64);
    let step = |fx: &mut Fx, what: &str, bytes: [u8; 3], entries: usize| {
        let shown: Vec<u8> = [a, b, c]
            .iter()
            .map(|&at| fx.win.doc.byte(at).unwrap())
            .collect();
        assert_eq!(shown, bytes, "{what}: the bytes");
        assert_eq!(fx.win.doc.patches().len(), entries, "{what}: the entries");
        assert!(fx.win.doc.patches().len() <= 2, "{what}: past the bound");
    };
    fx.typed("AA");
    fx.typed("BB");
    step(&mut fx, "edit A, B", [0xAA, 0xBB, 0], 2);
    fx.key(Key::S, command(false));
    fx.wait_for_save();
    assert_eq!(&std::fs::read(&path).unwrap()[..3], [0xAA, 0xBB, 0]);
    step(&mut fx, "saved", [0xAA, 0xBB, 0], 0);
    fx.key(Key::Z, command(false));
    step(&mut fx, "undo B", [0xAA, 0, 0], 1);
    fx.win.place(c, Pane::Hex);
    fx.typed("CC");
    step(&mut fx, "edit C", [0xAA, 0, 0xCC], 2);
    fx.key(Key::Z, command(false));
    step(
        &mut fx,
        "undo C: back at the file's byte, no entry",
        [0xAA, 0, 0],
        1,
    );
    fx.key(Key::Z, command(false));
    step(&mut fx, "undo A", [0, 0, 0], 2);
    assert!(fx.win.doc.dirty() && fx.win.doc.unsaved());
    // Full: a redo that would add an entry is refused, said, and left to do.
    fx.win.doc.set_edit_cap(1);
    let redo = fx.win.doc.can_redo();
    fx.key(Key::Z, command(true));
    step(
        &mut fx,
        "redo A at bound 1: back at the file's byte, no entry",
        [0xAA, 0, 0],
        1,
    );
    fx.key(Key::Z, command(true));
    step(&mut fx, "redo C at bound 1: refused", [0xAA, 0, 0], 1);
    assert!(redo && fx.win.doc.can_redo(), "the step stays");
    assert_eq!(fx.win.status.text, EDITS_FULL);
    assert!(fx.win.status.error);
    // An undo that would add an entry is refused the same way.
    fx.win.doc.set_edit_cap(2);
    fx.key(Key::Z, command(true));
    step(&mut fx, "redo C at bound 2", [0xAA, 0, 0xCC], 2);
    fx.win.doc.set_edit_cap(1);
    fx.key(Key::Z, command(false));
    step(
        &mut fx,
        "undo C at bound 1: back at the file's byte",
        [0xAA, 0, 0],
        1,
    );
    fx.key(Key::Z, command(false));
    step(&mut fx, "undo A at bound 1: refused", [0xAA, 0, 0], 1);
    assert!(fx.win.doc.can_undo(), "the step stays");
    assert_eq!(fx.win.status.text, EDITS_FULL);
    // Room again: the same undo goes.
    fx.win.doc.set_edit_cap(2);
    fx.key(Key::Z, command(false));
    step(&mut fx, "undo A at bound 2", [0, 0, 0], 2);
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
        kept: None,
        changes: BTreeMap::new(),
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

/// 03 §6.3's reason, word for word (03:578).
const NO_PLACE: &str = "a file at a revision has no place to be saved";

/// Every file and folder under `dir`, with each file's bytes: what a window that writes
/// nothing leaves exactly as it was.
fn files_under(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut walk = vec![dir.to_path_buf()];
    while let Some(at) = walk.pop() {
        for entry in std::fs::read_dir(&at).unwrap().flatten() {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk.push(path.clone());
                out.insert(path, None);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                out.insert(path, Some(bytes));
            }
        }
    }
    out
}

/// Hovers the control labelled `label` highest in the window (the toolbar's, not the F-key
/// row's) until `tip` shows; the pointer is taken to `away` first, so no earlier tip is up.
fn hover_says<W: Window>(drv: &mut Driver, win: &mut W, label: &str, tip: &str, away: Pos2) {
    drv.move_to(win, away);
    for _ in 0..90 {
        if drv.find_text(tip).is_none() {
            break;
        }
        drv.idle(win, 1);
    }
    assert!(drv.find_text(tip).is_none(), "{tip:?} up before the hover");
    let at = drv
        .texts()
        .into_iter()
        .filter(|(_, _, t)| t == label)
        .map(|(r, _, _)| r)
        .min_by(|a, b| a.top().total_cmp(&b.top()))
        .unwrap_or_else(|| panic!("no {label:?} on screen"));
    drv.move_to(win, at.center());
    for _ in 0..90 {
        drv.idle(win, 1);
        if drv.find_text(tip).is_some() {
            return;
        }
    }
    let shown: Vec<String> = drv.texts().into_iter().map(|(_, _, t)| t).collect();
    panic!("{label:?} never said {tip:?} on hover: {shown:?}");
}

/// G-T2 (03:1161) at the hex window: told before its first pass that it shows a file at a
/// revision (`set_read_only(true)`, 03 §6.3), it is a viewer still — the cursor moves, Go to
/// goes — but no byte typed in either column, no undo or redo edits; Save, Save copy and Save
/// As are off with 03:578's reason on hover, and none of their keys (Cmd/Ctrl+S,
/// Shift+Cmd/Ctrl+S, Alt+Cmd/Ctrl+S, F5, F7) or buttons starts one: each says why. An earlier
/// session's journal of the same file is neither looked for nor touched; it closes without
/// asking; and gone, it leaves the folder and the cache exactly as they were.
#[test]
fn a_file_at_a_revision_moves_and_goes_to_but_edits_and_saves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("rev.bin");
    std::fs::write(&path, FIXTURE).unwrap();
    let cache = tmp.path().join("cache");
    // An earlier session left a journal of the file: an edit kept, then a kill.
    let mut fx = in_place(&path, tmp, 1000.0, 600.0);
    fx.typed("00");
    fx.drv
        .until(&mut fx.win, 20, |w| w.schedule.status() == Keep::Kept);
    assert_eq!(journal_dirs_in(&cache).len(), 1, "the earlier session's");
    let abs = path.to_str().unwrap().to_string();
    let at = LocalFs::new().stat(&abs).unwrap();
    let origin = Origin::new(Box::new(LocalFs::new()), None, None, abs, at);
    let at_revision = window_for(HexFile::open(origin, &path, None, &cache).unwrap());
    std::mem::replace(&mut fx.win, at_revision).crash();
    let before = files_under(fx.tmp.path());
    fx.win.set_read_only(true);
    fx.drv = Driver::new(1000.0, 600.0);
    fx.drv.until(&mut fx.win, 20, |w| w.ready());
    fx.drv.idle(&mut fx.win, 2);
    assert!(
        fx.win.rows.is_empty() && fx.win.recovering.is_none(),
        "no earlier session's journal is looked for"
    );
    assert!(fx.drv.find_text("Restore").is_none());
    assert!(
        fx.win.painted && fx.win.rows_painted > 0,
        "the rows are shown"
    );
    // A viewer still: the cursor moves, Go to goes.
    let none = Modifiers::NONE;
    fx.key(Key::ArrowRight, none);
    fx.key(Key::ArrowDown, none);
    assert_eq!(fx.at(), 17);
    fx.key(Key::G, command(false));
    fx.typed("0x5");
    fx.key(Key::Enter, none);
    assert_eq!(fx.at(), 5);
    // No edit: digits, a character in the ASCII column, undo, redo.
    fx.win.status = Status::default();
    fx.typed("41");
    assert!(
        fx.win.status.text.ends_with(NO_PLACE),
        "{}",
        fx.win.status.text
    );
    fx.key(Key::Tab, none);
    fx.typed("Z");
    fx.key(Key::Z, command(false));
    fx.key(Key::Z, command(true));
    assert_eq!(fx.win.doc.changed_count(), 0, "nothing edited");
    assert!(!fx.win.doc.unsaved() && fx.win.doc.undo_len() == 0);
    // No save, whichever key or button; each key says why.
    for (key, mods) in [
        (Key::S, command(false)),
        (Key::S, command(true)),
        (Key::S, Modifiers::ALT | Modifiers::COMMAND),
        (Key::F5, none),
        (Key::F7, none),
    ] {
        fx.win.status = Status::default();
        fx.key(key, mods);
        assert!(
            fx.win.job.is_none() && fx.win.save_as.is_none() && fx.win.save_starts == 0,
            "{key:?} {mods:?} started a save"
        );
        assert!(
            fx.win.status.text.ends_with(NO_PLACE),
            "{key:?} {mods:?}: {}",
            fx.win.status.text
        );
    }
    let away = Pos2::new(500.0, 400.0);
    for label in ["Save", "Save copy", "Save As…"] {
        hover_says(&mut fx.drv, &mut fx.win, label, NO_PLACE, away);
    }
    fx.drv.click_text(&mut fx.win, "Save copy");
    fx.drv.click_text(&mut fx.win, "Save As…");
    assert!(fx.win.job.is_none() && fx.win.save_as.is_none() && fx.win.save_starts == 0);
    fx.drv.idle(&mut fx.win, 30);
    assert!(fx.win.journal_dir.is_none(), "nothing kept");
    assert!(fx.win.close_requested(), "nothing to ask");
    let Fx { tmp, win, .. } = fx;
    drop(win);
    assert_eq!(
        files_under(tmp.path()),
        before,
        "nothing written, nothing removed"
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

/// A save (chosen in the close prompt) that lands in the same frame as an edit or an undo: no
/// edit is taken while a Save runs (07 §10), so the frame's input changes nothing, the status
/// row says editing resumes after the save, and the window closes over nothing unsaved.
#[test]
fn a_save_landing_in_the_same_frame_as_an_edit_takes_no_edit() {
    for input in ["typed", "undo", "none"] {
        let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
        fx.typed("00");
        let before = fx.win.doc.patches().clone();
        let (tx, rx) = mpsc::channel();
        fx.win.job = Some(Job {
            kind: JobKind::Save { close_after: true },
            snapshot: before.clone(),
            kept: None,
            changes: BTreeMap::new(),
            rx,
        });
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
        // While it runs: the input does nothing, and the status row says why.
        fx.drv.frame(&mut fx.win, events.clone(), mods);
        assert_eq!(
            *fx.win.doc.patches(),
            before,
            "{input}: an edit during a save"
        );
        if input != "none" {
            let status = fx.win.status_text(&fx.drv.ctx.clone()).text;
            assert_eq!(status, EDITING_RESUMES, "{input}");
        }
        // It lands in a frame with the same input: still nothing edits, and it closes.
        tx.send(Ok(JobDone::Saved(Saved::Written {
            remote: "box:/blob.bin".to_string(),
            backup: PathBuf::from("/backup/blob.bin"),
            notes: Vec::new(),
        })))
        .unwrap();
        fx.drv.frame(&mut fx.win, events, mods);
        assert!(fx.win.job.is_none(), "{input}: the result was taken");
        assert_eq!(*fx.win.doc.patches(), before, "{input}: the landing frame");
        assert!(fx.win.close_ok, "{input}: nothing unsaved: closes");
        assert!(!fx.win.doc.unsaved(), "{input}");
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
            kept: None,
            changes: BTreeMap::new(),
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
            kept: None,
            changes: BTreeMap::new(),
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
            kept: None,
            changes: BTreeMap::new(),
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
        kept: None,
        changes: BTreeMap::new(),
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
        kept: None,
        changes: BTreeMap::new(),
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
            kept: None,
            changes: BTreeMap::new(),
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

/// A change saved in place: while its result has not landed, an undo is refused (07 §10, no
/// edit while a Save runs); then the file as opened goes away (its page dropped from the cache,
/// its bytes truncated) and the save lands: the window shows the saved byte. An undo now is an
/// edit of the saved file — the byte before the change, unsaved, and closing asks; Save puts it
/// back.
#[test]
fn an_undo_across_an_in_place_save_is_an_edit_of_the_saved_file() {
    let (mut fx, old, saved) = saved_in_place(Box::new(LocalFs::new()), |fx| {
        fx.key(Key::ArrowRight, Modifiers::NONE);
        fx.typed("00");
    });
    assert!(matches!(saved, Saved::Written { .. }), "{saved:?}");
    let snapshot = BTreeMap::from([(1u64, 0u8)]);
    let (tx, rx) = mpsc::channel();
    fx.win.job = Some(Job {
        kind: JobKind::Save { close_after: false },
        snapshot,
        kept: None,
        changes: BTreeMap::new(),
        rx,
    });
    fx.key(Key::Z, command(false));
    assert_eq!(
        fx.win.doc.byte(1),
        Some(0),
        "an undo while the save's result is due"
    );
    assert_eq!(
        fx.win.status_text(&fx.drv.ctx.clone()).text,
        EDITING_RESUMES
    );
    fx.win.doc.pages.held.clear();
    old.set_len(0).unwrap();
    tx.send(Ok(JobDone::Saved(saved))).unwrap();
    fx.settle();
    assert!(fx.win.read_only.is_none(), "{:?}", fx.win.read_only);
    assert_eq!(fx.win.doc.byte(1), Some(0), "the saved byte");
    assert!(!fx.win.doc.unsaved());
    fx.key(Key::Z, command(false));
    assert_eq!(fx.win.doc.byte(1), Some(b'o'), "the undo");
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
    assert!(text.lines().any(|l| l == "0x1: 00"), "{text}");
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
    // Each entry is back at the file's byte (its page here): it goes, and the bound is free.
    assert_eq!(doc.patches().len(), 0, "entries at the file's bytes stay");
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

/// Go to is the field's until it is left, whether or not the platform says the window has the
/// keyboard: an offset typed after F6 or Cmd/Ctrl+G never lands on bytes. Here the window is
/// without the keyboard throughout (a receipt window that came up behind another one).
#[test]
fn go_to_takes_the_typing_in_a_window_without_focus() {
    for (opener, modifiers) in [(Key::F6, Modifiers::NONE), (Key::G, command(false))] {
        let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
        fx.drv.focused = false;
        fx.key(opener, modifiers);
        fx.drv.idle(&mut fx.win, 2);
        fx.typed("0x10");
        assert_eq!(fx.win.doc.changed_count(), 0, "no byte was typed");
        assert_eq!(fx.win.goto, "0x10", "the typing went to the field");
        fx.key(Key::Enter, Modifiers::NONE);
        assert_eq!(fx.at(), 0x10);
        assert_eq!(fx.win.doc.changed_count(), 0);
    }
}

/// …and with a real keyboard: Go to opened, the owner away in another application for a pass,
/// back, the offset, Enter.
#[test]
fn go_to_is_still_open_after_the_window_lost_focus() {
    for (opener, modifiers) in [(Key::F6, Modifiers::NONE), (Key::G, command(false))] {
        let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
        fx.key(opener, modifiers);
        fx.drv.focused = false;
        fx.drv.idle(&mut fx.win, 1);
        fx.drv.focused = true;
        fx.typed("0x10");
        assert_eq!(fx.win.doc.changed_count(), 0, "no byte was typed");
        assert_eq!(fx.win.goto, "0x10", "the typing went to the field");
        fx.key(Key::Enter, Modifiers::NONE);
        assert_eq!(fx.at(), 0x10);
        assert_eq!(fx.win.doc.changed_count(), 0);
    }
}

/// After that round trip Go to is left as ever — by Esc, or by a click on the grid — and the
/// grid has the typing again: a byte typed then lands at the cursor, not in the field.
#[test]
fn leaving_go_to_after_the_window_lost_focus_gives_the_grid_the_typing() {
    for click in [false, true] {
        let mut fx = fetched("blob.bin", FIXTURE, 1000.0, 600.0);
        fx.key(Key::F6, Modifiers::NONE);
        fx.drv.focused = false;
        fx.drv.idle(&mut fx.win, 1);
        fx.drv.focused = true;
        fx.drv.idle(&mut fx.win, 1);
        assert_eq!(
            fx.drv.ctx.memory(|m| m.focused()),
            Some(goto_id()),
            "still open"
        );
        let at = if click {
            let at = fx.pos(3, Pane::Hex);
            fx.drv.click(&mut fx.win, at);
            3
        } else {
            fx.key(Key::Escape, Modifiers::NONE);
            0
        };
        fx.drv.idle(&mut fx.win, 1);
        assert_ne!(
            fx.drv.ctx.memory(|m| m.focused()),
            Some(goto_id()),
            "click={click}: Go to was left"
        );
        assert_eq!(fx.at(), at, "click={click}");
        fx.typed("7A");
        assert_eq!(fx.win.goto, "", "click={click}: nothing went to the field");
        assert_eq!(fx.win.doc.cell(at), Cell::Byte(0x7A), "click={click}");
        assert_eq!(fx.win.doc.changed_count(), 1, "click={click}");
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
        kept: None,
        changes: BTreeMap::new(),
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
    use std::time::{Duration, Instant, UNIX_EPOCH};

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

    // ---- 07's Save over a stream (07 §4–§7, §10, §12): a real file, the engine running ----

    /// What the `Origin`'s connection moved of the file (07 §12: "at the client's read and
    /// write payloads"), on every path that carries file bytes — exact where the bytes are
    /// seen, and conservative (the most it can be) where they are not:
    /// - reads: exact as they are handed over (a streamed whole read cut off part-way counts
    ///   what it handed over); a positioned read that fails counts the length it asked for;
    /// - writes: the bytes each one sends, counted before its reply — a failed or short write
    ///   counts its whole length;
    /// - an upload (`create_from`) cannot be watched below the trait: it counts the whole local
    ///   file; `whole` counts the calls that move a whole file, so a test can say there were
    ///   none;
    /// - apart, what the server's copy moved, by its own progress — no request carries it.
    ///
    /// It can say it has no `copy-data`.
    #[derive(Clone, Default)]
    struct Moved {
        read: Arc<std::sync::atomic::AtomicU64>,
        written: Arc<std::sync::atomic::AtomicU64>,
        copied: Arc<std::sync::atomic::AtomicU64>,
        no_copy: Arc<AtomicBool>,
        /// Bytes the window's journal wrote to the local disk ([`Writes`]); and that disk full,
        /// or holding a `save` record's removal.
        disk: Arc<std::sync::atomic::AtomicU64>,
        disk_full: Arc<AtomicBool>,
        hold_record: Arc<AtomicBool>,
        /// Every removal fails (a server that refuses it); every rename too.
        fail_remove: Arc<AtomicBool>,
        fail_rename: Arc<AtomicBool>,
        /// Calls that move a whole file — an upload (`create_from`), a whole read (`read_to`,
        /// `read_head`) — whatever they moved: a streamed Save makes none.
        whole: Arc<std::sync::atomic::AtomicU64>,
    }

    impl Moved {
        fn now(&self) -> (u64, u64, u64) {
            (
                self.read.load(Ordering::SeqCst),
                self.written.load(Ordering::SeqCst),
                self.copied.load(Ordering::SeqCst),
            )
        }
    }

    struct Moving {
        inner: Box<dyn patch::PatchFs + Send>,
        moved: Moved,
    }

    impl RemoteFs for Moving {
        fn stat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
            self.inner.stat_opt(p)
        }
        fn lstat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
            self.inner.lstat_opt(p)
        }
        fn realpath(&mut self, p: &str) -> Result<String> {
            self.inner.realpath(p)
        }
        fn read_head(&mut self, p: &str, max: usize) -> Result<Vec<u8>> {
            self.moved.whole.fetch_add(1, Ordering::SeqCst);
            let head = self.inner.read_head(p, max)?;
            self.moved
                .read
                .fetch_add(head.len() as u64, Ordering::SeqCst);
            Ok(head)
        }
        fn list_dir(&mut self, p: &str) -> Result<Vec<String>> {
            self.inner.list_dir(p)
        }
        fn list_entries(&mut self, p: &str) -> Result<Vec<crate::remote::Entry>> {
            self.inner.list_entries(p)
        }
        fn free_space(&mut self, p: &str) -> Option<u64> {
            self.inner.free_space(p)
        }
        fn read_to(
            &mut self,
            p: &str,
            out: &mut dyn std::io::Write,
            progress: &mut dyn FnMut(u64),
        ) -> Result<u64> {
            /// Counts what passes through, as it passes.
            struct Through<'a>(&'a mut dyn std::io::Write, &'a std::sync::atomic::AtomicU64);
            impl std::io::Write for Through<'_> {
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    self.1.fetch_add(buf.len() as u64, Ordering::SeqCst);
                    self.0.write(buf)
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    self.0.flush()
                }
            }
            self.moved.whole.fetch_add(1, Ordering::SeqCst);
            let read = self.moved.read.clone();
            self.inner.read_to(p, &mut Through(out, &read), progress)
        }
        fn create_from(
            &mut self,
            local: &Path,
            p: &str,
            like: Option<crate::remote::Like>,
        ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
            self.moved.whole.fetch_add(1, Ordering::SeqCst);
            let n = std::fs::metadata(local).map_or(0, |m| m.len());
            self.moved.written.fetch_add(n, Ordering::SeqCst);
            self.inner.create_from(local, p, like)
        }
        fn set_mode(&mut self, p: &str, mode: u32) -> Result<()> {
            self.inner.set_mode(p, mode)
        }
        fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
            if self.moved.fail_rename.load(Ordering::SeqCst) {
                bail!("rename {from}: permission denied");
            }
            self.inner.rename_over(from, to)
        }
        fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
            self.inner.link_new(from, to)
        }
        fn can_link(&self) -> bool {
            self.inner.can_link()
        }
        fn can_replace(&self) -> bool {
            self.inner.can_replace()
        }
        fn remove(&mut self, p: &str) -> Result<()> {
            if self.moved.fail_remove.load(Ordering::SeqCst) {
                bail!("remove {p}: permission denied");
            }
            self.inner.remove(p)
        }
        fn patcher(&mut self) -> Option<&mut dyn patch::PatchFs> {
            Some(self)
        }
    }

    impl patch::PatchFs for Moving {
        fn can_copy(&self) -> bool {
            !self.moved.no_copy.load(Ordering::SeqCst) && self.inner.can_copy()
        }
        fn open(&mut self, path: &str, how: patch::OpenFor) -> Result<patch::Handle> {
            self.inner.open(path, how)
        }
        fn create_temp(
            &mut self,
            path: &str,
        ) -> std::result::Result<patch::Handle, crate::remote::CreateError> {
            self.inner.create_temp(path)
        }
        fn fstat(&mut self, file: &mut patch::Handle) -> Result<Stat> {
            self.inner.fstat(file)
        }
        fn read_at(
            &mut self,
            file: &mut patch::Handle,
            offset: u64,
            len: usize,
        ) -> Result<Vec<u8>> {
            // A read that fails hands nothing over; it counts what it asked for, the most it can
            // have moved.
            let got = self.inner.read_at(file, offset, len);
            let n = got.as_ref().map_or(len, Vec::len);
            self.moved.read.fetch_add(n as u64, Ordering::SeqCst);
            got
        }
        fn read_through(
            &mut self,
            src: &mut patch::Handle,
            size: u64,
            dst: Option<&mut patch::Handle>,
            each: &mut patch::Each<'_>,
        ) -> Result<u64> {
            let moved = self.moved.clone();
            self.inner.read_through(src, size, dst, &mut |at, chunk| {
                moved.read.fetch_add(chunk.len() as u64, Ordering::SeqCst);
                let out = each(at, chunk)?;
                if let Some(out) = &out {
                    moved.written.fetch_add(out.len() as u64, Ordering::SeqCst);
                }
                Ok(out)
            })
        }
        fn write_at(&mut self, file: &mut patch::Handle, offset: u64, bytes: &[u8]) -> Result<()> {
            self.moved
                .written
                .fetch_add(bytes.len() as u64, Ordering::SeqCst);
            self.inner.write_at(file, offset, bytes)
        }
        fn set_owner(&mut self, file: &mut patch::Handle, uid: u32, gid: u32) -> Result<()> {
            self.inner.set_owner(file, uid, gid)
        }
        fn set_handle_mode(&mut self, file: &mut patch::Handle, mode: u32) -> Result<()> {
            self.inner.set_handle_mode(file, mode)
        }
        fn can_fsync(&self) -> bool {
            self.inner.can_fsync()
        }
        fn fsync(&mut self, file: &mut patch::Handle) -> Result<()> {
            self.inner.fsync(file)
        }
        fn copy(
            &mut self,
            src: &mut patch::Handle,
            dst: &mut patch::Handle,
            size: u64,
            ctl: &mut patch::CopyCtl<'_>,
        ) -> Result<()> {
            // What the server copied, by the copy's own reports.
            let copied = self.moved.copied.clone();
            let start = copied.load(Ordering::SeqCst);
            let progress = &mut *ctl.progress;
            let mut heard = |e: patch::CopyEvent| {
                if let patch::CopyEvent::Copied(done) = e {
                    copied.store(start + done, Ordering::SeqCst);
                }
                progress(e);
            };
            let mut ours = patch::CopyCtl {
                pace: ctl.pace.clone(),
                clock: &mut *ctl.clock,
                cancel: ctl.cancel,
                progress: &mut heard,
            };
            self.inner.copy(src, dst, size, &mut ours)
        }
        fn close(&mut self, file: patch::Handle) {
            self.inner.close(file)
        }
    }

    /// The local disk under the journal, counting every byte it writes (07 §12 E1: "local disk
    /// written"), files later removed included. It can refuse every write (a full disk), and
    /// hold the removal of a `save` record until let go.
    struct Writes {
        bytes: Arc<std::sync::atomic::AtomicU64>,
        full: Arc<AtomicBool>,
        hold_record: Arc<AtomicBool>,
    }

    impl crate::journal::Fs for Writes {
        fn create_dir(&self, path: &Path) -> std::io::Result<()> {
            crate::journal::Disk.create_dir(path)
        }
        fn write_new(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
            if self.full.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("no space left on device"));
            }
            self.bytes.fetch_add(bytes.len() as u64, Ordering::SeqCst);
            crate::journal::Disk.write_new(path, bytes)
        }
        fn sync_file(&self, path: &Path) -> std::io::Result<()> {
            crate::journal::Disk.sync_file(path)
        }
        fn sync_dir(&self, path: &Path) -> std::io::Result<()> {
            crate::journal::Disk.sync_dir(path)
        }
        fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            crate::journal::Disk.rename(from, to)
        }
        fn remove_file(&self, path: &Path) -> std::io::Result<()> {
            while path.file_name().is_some_and(|n| n == "save")
                && self.hold_record.load(Ordering::SeqCst)
            {
                std::thread::sleep(Duration::from_millis(2));
            }
            crate::journal::Disk.remove_file(path)
        }
        fn remove_dir(&self, path: &Path) -> std::io::Result<()> {
            crate::journal::Disk.remove_dir(path)
        }
        fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
            crate::journal::Disk.read(path)
        }
        fn list(&self, dir: &Path) -> std::io::Result<Vec<String>> {
            crate::journal::Disk.list(dir)
        }
        fn node(&self, path: &Path, follow: bool) -> std::io::Result<Option<crate::journal::Node>> {
            crate::journal::Disk.node(path, follow)
        }
        fn lock(&self, path: &Path, create: bool) -> std::io::Result<Option<crate::journal::Held>> {
            crate::journal::Disk.lock(path, create)
        }
    }

    /// A file above 256 MiB: a sparse file a MiB over the content-check limit (07 §12 E1, scaled).
    const BIG: u64 = crate::remote::HASH_CHECK_MAX + MIB;
    /// Where E1's 4 bytes go: inside a page that holds bytes of its own.
    const E1_AT: u64 = 200 * MIB + 0xC35;
    /// The modification time every test file is given, and "put back" to.
    const OLD: u64 = 1_700_000_000;

    /// Distinct bytes in the big file: at its start, across the edited page, far from it, and
    /// at its end.
    fn seeds() -> Vec<(u64, Vec<u8>)> {
        [
            0,
            E1_AT / PAGE * PAGE,
            E1_AT + PAGE,
            130 * MIB + 5,
            BIG - 100,
        ]
        .into_iter()
        .enumerate()
        .map(|(i, at)| {
            let bytes = (0..100u8).map(|b| b.wrapping_mul(7).wrapping_add(i as u8 * 31 + 1));
            (at, bytes.collect())
        })
        .collect()
    }

    fn set_mtime(path: &Path, secs: u64) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    /// Writes `bytes` at `at` of `path` and puts its modification time back (another writer).
    fn write_back(path: &Path, at: u64, bytes: &[u8]) {
        use std::os::unix::fs::FileExt;
        let mtime = std::fs::metadata(path).unwrap().modified().unwrap();
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.write_all_at(bytes, at).unwrap();
        f.set_modified(mtime).unwrap();
    }

    fn bytes_at(path: &Path, at: u64, len: usize) -> Vec<u8> {
        use std::os::unix::fs::FileExt;
        let mut buf = vec![0u8; len];
        let f = std::fs::File::open(path).unwrap();
        f.read_exact_at(&mut buf, at).unwrap();
        buf
    }

    /// A whole file's SHA-256 — by the system's `shasum` / `sha256sum` for a big file (a test
    /// build hashes slowly), else here.
    fn file_sha(path: &Path) -> String {
        if std::fs::metadata(path).unwrap().len() > 16 * MIB {
            for (tool, args) in [("sha256sum", &[][..]), ("shasum", &["-a", "256"][..])] {
                if let Ok(out) = std::process::Command::new(tool)
                    .args(args)
                    .arg(path)
                    .output()
                {
                    let text = String::from_utf8_lossy(&out.stdout).into_owned();
                    let digest = text.split_whitespace().next().unwrap_or_default();
                    if out.status.success() && digest.len() == 64 {
                        return digest.to_string();
                    }
                }
            }
        }
        use sha2::Digest;
        crate::cache::hex(&sha2::Sha256::digest(std::fs::read(path).unwrap()))
    }

    /// The "remote" file: `body`, or a sparse [`BIG`] file holding [`seeds`]; mode 0640, mtime
    /// [`OLD`].
    fn remote_file(tmp: &Path, body: Option<&[u8]>) -> PathBuf {
        let remote = tmp.join("remote/model.bin");
        std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
        match body {
            Some(body) => std::fs::write(&remote, body).unwrap(),
            None => {
                std::fs::File::create(&remote)
                    .unwrap()
                    .set_len(BIG)
                    .unwrap();
                for (at, bytes) in seeds() {
                    write_back(&remote, at, &bytes);
                }
            }
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&remote, std::fs::Permissions::from_mode(0o640)).unwrap();
        set_mtime(&remote, OLD);
        remote
    }

    /// Which server a test runs against: the local disk, or OpenSSH's `sftp-server`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Side {
        Local,
        Sftp,
    }

    /// The sides there are here (`sftp-server` is required under `SOPEN_REQUIRE_SFTP_SERVER`).
    fn sides() -> Vec<Side> {
        let mut sides = vec![Side::Local];
        if SftpRemote::local_server().is_some() {
            sides.push(Side::Sftp);
        }
        sides
    }

    /// A connection to the file's host on `side`.
    fn connection(side: Side) -> Box<dyn patch::PatchFs + Send> {
        match side {
            Side::Local => Box::new(LocalFs::new()),
            Side::Sftp => Box::new(SftpRemote::local_server().expect("checked by sides()")),
        }
    }

    /// The hex window on a stream over the "remote" file, and what its `Origin` moved. (The
    /// window goes first: what it does as it goes is in the test's folder.)
    struct Ed {
        win: HexWindow,
        drv: Driver,
        source: Source,
        moved: Moved,
        remote: PathBuf,
        side: Side,
        tmp: tempfile::TempDir,
    }

    /// A streamed window on `remote` over `tmp`'s cache, on `side`; its `Origin` is `origin`,
    /// or a counted connection.
    fn window_on(
        tmp: &Path,
        remote: &Path,
        side: Side,
        origin: Option<Origin>,
        moved: &Moved,
    ) -> (Source, HexWindow) {
        let abs = remote.to_str().unwrap().to_string();
        let view: Box<dyn RemoteFs + Send> = match side {
            Side::Local => Box::new(LocalFs::new()),
            Side::Sftp => Box::new(SftpRemote::local_server().unwrap()),
        };
        let transport = view.into_ranged(&abs).map_err(|r| r.error).unwrap();
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
        let origin = origin.unwrap_or_else(|| {
            let mut fs = Moving {
                inner: connection(side),
                moved: moved.clone(),
            };
            let at = fs.stat(&abs).unwrap();
            Origin::new(Box::new(fs), None, Some("host".into()), abs, at)
        });
        let mut file = HexFile::streamed(origin, source.clone(), repaint, &tmp.join("cache"));
        file.journal_fs = Arc::new(Writes {
            bytes: moved.disk.clone(),
            full: moved.disk_full.clone(),
            hold_record: moved.hold_record.clone(),
        });
        file.pace = Some(Box::new(patch::Pace::sized(
            64 * KIB,
            16 * KIB,
            MIB,
            2 * MIB,
        )));
        let win = embedded(
            file,
            &Hooks::default(),
            Vec::new(),
            Arc::default(),
            Arc::default(),
        )
        .unwrap();
        (source, win)
    }

    /// A streamed local file of `remote`, as [`window_on`] makes it, with no window.
    fn file_on(tmp: &Path, remote: &Path, moved: &Moved) -> HexFile {
        let abs = remote.to_str().unwrap().to_string();
        let view: Box<dyn RemoteFs + Send> = Box::new(LocalFs::new());
        let transport = view.into_ranged(&abs).map_err(|r| r.error).unwrap();
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
        let mut fs = Moving {
            inner: connection(Side::Local),
            moved: moved.clone(),
        };
        let at = fs.stat(&abs).unwrap();
        let origin = Origin::new(Box::new(fs), None, Some("host".into()), abs, at);
        HexFile::streamed(origin, source, repaint, &tmp.join("cache"))
    }

    fn ed_side(body: Option<&[u8]>, side: Side) -> Ed {
        let tmp = tempfile::tempdir().unwrap();
        let remote = remote_file(tmp.path(), body);
        let moved = Moved::default();
        let (source, win) = window_on(tmp.path(), &remote, side, None, &moved);
        let mut ed = Ed {
            tmp,
            remote,
            side,
            source,
            moved,
            win,
            drv: Driver::new(760.0, 560.0),
        };
        ed.ready();
        ed
    }

    fn ed(body: &[u8]) -> Ed {
        ed_side(Some(body), Side::Local)
    }

    fn body() -> Vec<u8> {
        (0..300 * 1024u32).map(|i| (i % 251) as u8).collect()
    }

    impl Ed {
        fn abs(&self) -> String {
            self.remote.to_str().unwrap().to_string()
        }

        fn shown(&self) -> String {
            format!("host:{}", self.abs())
        }

        fn ready(&mut self) {
            self.drv.until(&mut self.win, 60, |w| w.ready());
        }

        fn frames(&mut self, n: usize) {
            self.drv.idle(&mut self.win, n);
        }

        fn key(&mut self, key: Key, modifiers: Modifiers) {
            self.drv.key(&mut self.win, key, modifiers);
        }

        fn typed(&mut self, text: &str) {
            for c in text.chars() {
                self.drv.frame(
                    &mut self.win,
                    vec![Event::Text(c.to_string())],
                    Modifiers::NONE,
                );
            }
            self.frames(1);
        }

        /// The cursor on `at`, its page here.
        fn goto(&mut self, at: u64) {
            self.win.place(at, Pane::Hex);
            self.frames(2);
            self.drv.until(&mut self.win, 60, |w| {
                w.doc.pending_at().is_none() && w.painted
            });
            let deadline = Instant::now() + Duration::from_secs(30);
            while self.win.doc.cell(at) == Cell::Pending {
                assert!(Instant::now() < deadline, "0x{at:X} never came");
                self.frames(1);
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Until the latest edit is in a durable generation (`kept at`).
        fn kept(&mut self) {
            self.drv
                .until(&mut self.win, 20, |w| w.schedule.status() == Keep::Kept);
        }

        /// Cmd/Ctrl+S, the first-Save question answered, until it has reported.
        fn save(&mut self) {
            self.key(Key::S, command(false));
            if self.win.confirm.is_some() {
                self.frames(1);
                self.key(Key::Enter, Modifiers::NONE);
            }
            self.drv.until(&mut self.win, 120, |w| w.job.is_none());
            self.frames(2);
        }

        /// `.sopen-` names in the remote's folder (temps).
        fn temps(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.remote.parent().unwrap())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains(".sopen-"))
                .collect();
            names.sort();
            names
        }

        /// The journal folders under the cache.
        fn journals(&self) -> Vec<PathBuf> {
            let mut dirs: Vec<PathBuf> = walk(&self.tmp.path().join("cache/.edits"))
                .into_iter()
                .filter_map(|p| p.parent().map(Path::to_path_buf))
                .filter(|d| d.to_string_lossy().contains(".journal-"))
                .collect();
            dirs.sort();
            dirs.dedup();
            dirs
        }

        fn undo_files(&self) -> Vec<PathBuf> {
            walk(&self.tmp.path().join("cache/.backup"))
                .into_iter()
                .filter(|p| p.to_string_lossy().ends_with(".undo.txt"))
                .collect()
        }

        /// Bytes on the local disk under the cache.
        fn local_bytes(&self) -> u64 {
            walk(&self.tmp.path().join("cache"))
                .iter()
                .map(|p| std::fs::metadata(p).map_or(0, |m| m.len()))
                .sum()
        }

        fn hook(&mut self, hook: impl FnMut(patch::Step) -> patch::At + Send + 'static) {
            self.win.file.lock().unwrap().step_hook = Some(Box::new(hook));
        }

        /// The window ends as a kill ends it, and a new one opens on the same file and cache,
        /// its recovery rows listed.
        fn reopen(&mut self) {
            // New counters; a server that refuses removals still refuses them.
            let moved = Moved {
                fail_remove: self.moved.fail_remove.clone(),
                ..Moved::default()
            };
            let (source, win) = window_on(self.tmp.path(), &self.remote, self.side, None, &moved);
            let old = std::mem::replace(&mut self.win, win);
            old.crash();
            self.source = source;
            self.moved = moved;
            self.drv = Driver::new(760.0, 560.0);
            self.ready();
            self.frames(3);
        }

        /// The recovery rows' lines, a journal's question about a temp as a line of its own.
        fn rows(&self) -> Vec<String> {
            let mut out = Vec::new();
            for r in &self.win.rows {
                match r {
                    Row::Edits { line, temp, .. } | Row::Kept { line, temp, .. } => {
                        out.push(line.clone());
                        out.extend(temp.iter().map(|t| t.line.clone()));
                    }
                    Row::Undo { line, .. } | Row::Temp { line, .. } | Row::Note(line) => {
                        out.push(line.clone())
                    }
                }
            }
            out
        }

        /// Presses the button `action` of the row whose line (or question) contains `has`.
        fn press_row(&mut self, has: &str, action: RowAction) {
            let i = self
                .win
                .rows
                .iter()
                .position(|r| match r {
                    Row::Edits { line, temp, .. } | Row::Kept { line, temp, .. } => {
                        line.contains(has) || temp.as_ref().is_some_and(|t| t.line.contains(has))
                    }
                    Row::Undo { line, .. } | Row::Temp { line, .. } | Row::Note(line) => {
                        line.contains(has)
                    }
                })
                .unwrap_or_else(|| panic!("no row with {has:?}: {:?}", self.rows()));
            self.win.row_action(i, action);
            self.drv.until(&mut self.win, 20, |w| w.removing.is_none());
            self.frames(2);
        }

        fn status(&mut self) -> String {
            let ctx = self.drv.ctx.clone();
            self.win.status_text(&ctx).text
        }
    }

    // ---- E1, E2: a 4-byte patch to a file above 256 MiB, the view working meanwhile ----

    /// 07 E1 at a scaled size, on the local disk and over OpenSSH's `sftp-server`: a 4-byte
    /// patch to a sparse file a MiB over 256 MiB. The Save's own requests read the one page back
    /// and write the 4 bytes — the copy is the server's — after asking once what its checks
    /// cannot see. The view reloads and fetches only its screen again; the local disk holds the
    /// journal and the undo file, under 64 KiB; no temp is left; the window says `saved … ·
    /// backup <undo file>` with what was checked; the journal is gone; writing the undo file
    /// back gives the original SHA-256.
    #[test]
    fn e1_a_four_byte_patch_moves_one_page_and_the_patch() {
        for side in sides() {
            let mut ed = ed_side(None, side);
            let original = file_sha(&ed.remote);
            ed.goto(E1_AT);
            ed.typed("DEADBEEF");
            assert_eq!(ed.win.doc.changed_count(), 4, "{side:?}");
            ed.kept();
            assert_eq!(ed.journals().len(), 1, "{side:?}: the edits are kept");
            assert_eq!(ed.moved.now(), (0, 0, 0), "{side:?}: an edit sends nothing");
            // The first Save of a file above 256 MiB asks first.
            ed.key(Key::S, command(false));
            ed.frames(2);
            assert!(ed.win.confirm.is_some(), "{side:?}: no question");
            let asked = ed
                .drv
                .text_starting_with(&format!("Save to {} (", ed.shown()))
                .unwrap_or_default();
            assert!(
                asked.contains("A program writing to the file while it saves can lose that write"),
                "{asked}"
            );
            assert_eq!(
                ed.moved.now(),
                (0, 0, 0),
                "{side:?}: nothing before the answer"
            );
            let fetched = ed.source.fetched().total;
            // At every step of the Save: what is on the local disk besides the journal (which
            // counts its own writes) — the undo file, a staging copy if there were one.
            let cache = ed.tmp.path().join("cache");
            let seen = Arc::new(Mutex::new((
                0u64,
                std::collections::BTreeSet::<String>::new(),
            )));
            let sampled = seen.clone();
            ed.hook(move |_| {
                let files: Vec<PathBuf> = walk(&cache)
                    .into_iter()
                    .filter(|p| !p.starts_with(cache.join(".edits")))
                    .collect();
                let bytes = files
                    .iter()
                    .map(|p| std::fs::metadata(p).map_or(0, |m| m.len()))
                    .sum::<u64>();
                let mut seen = sampled.lock().unwrap();
                seen.0 = seen.0.max(bytes);
                seen.1.extend(
                    files
                        .iter()
                        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned()),
                );
                patch::At::Go
            });
            ed.key(Key::Enter, Modifiers::NONE);
            ed.drv.until(&mut ed.win, 120, |w| w.job.is_none());
            ed.ready();
            let (read, written, copied) = ed.moved.now();
            assert_eq!(
                ed.moved.whole.load(Ordering::SeqCst),
                0,
                "{side:?}: an upload or a whole read"
            );
            assert_eq!(
                (read, written),
                (PAGE, 4),
                "{side:?}: the page check and the patch, nothing else"
            );
            assert_eq!(copied, BIG, "{side:?}: the copy is the server's");
            assert_eq!(bytes_at(&ed.remote, E1_AT, 4), [0xDE, 0xAD, 0xBE, 0xEF]);
            for (at, bytes) in seeds() {
                let len = if at / PAGE * PAGE == E1_AT / PAGE * PAGE {
                    bytes.len().min((E1_AT - at) as usize)
                } else {
                    bytes.len()
                };
                assert_eq!(
                    bytes_at(&ed.remote, at, len),
                    bytes[..len],
                    "{side:?}: 0x{at:X}"
                );
            }
            assert_eq!(ed.temps(), Vec::<String>::new(), "{side:?}: a temp is left");
            let status = ed.win.status.text.clone();
            let undo = ed.undo_files();
            assert_eq!(undo.len(), 1, "{side:?}: {undo:?}");
            assert_eq!(
                status,
                format!(
                    "saved {} · backup {} · checked size, modification time and the 4 KiB under \
                     your edits, not the whole content",
                    ed.shown(),
                    undo[0].display()
                ),
                "{side:?}"
            );
            assert!(ed.journals().is_empty(), "{side:?}: the journal stays");
            // Local disk written, throughout, counted as written — files removed since
            // included: every journal file (generations, CURRENT, the save record), and every
            // undo-file publication (its text, through a temp). Those are the two local writers
            // a streamed Save has.
            let journal = ed.moved.disk.load(Ordering::SeqCst);
            let undone = patch::undo_bytes_under(&ed.tmp.path().join("cache"));
            let undo_bytes = std::fs::metadata(&undo[0]).unwrap().len();
            assert!(journal > 0, "{side:?}: the journal was written");
            assert_eq!(undone, undo_bytes, "{side:?}: the undo file, written once");
            assert!(
                journal + undone <= 64 * KIB,
                "{side:?}: {journal} journal + {undone} undo bytes written"
            );
            // Apart: what the cache held at each step of the Save besides the journal — the
            // undo file alone, never a staging copy or anything else.
            let (most, names) = seen.lock().unwrap().clone();
            let undo_name = undo[0].file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(
                names,
                std::collections::BTreeSet::from([undo_name]),
                "{side:?}: on the disk"
            );
            assert_eq!(most, undo_bytes, "{side:?}");
            // What is left once nothing is unsaved: the undo file alone.
            assert_eq!(
                ed.local_bytes(),
                undo_bytes,
                "{side:?}: the cache after the save"
            );
            // The view reloaded and read its screen again, and the saved bytes are the file's.
            let again = ed.source.fetched().total - fetched;
            assert!(
                again <= 128 * KIB,
                "{side:?}: {again} bytes fetched after the save"
            );
            assert!(!ed.win.doc.dirty() && !ed.win.doc.unsaved(), "{side:?}");
            assert_eq!(ed.win.doc.original(E1_AT), Some(0xDE), "{side:?}");
            assert!(ed.win.close_requested(), "{side:?}: nothing unsaved");
            // The undo file written back gives the original.
            let text = std::fs::read_to_string(&undo[0]).unwrap();
            for run in patch::UndoFile::parse(&text).unwrap().runs {
                write_back(&ed.remote, run.offset, &run.bytes);
            }
            assert_eq!(file_sha(&ed.remote), original, "{side:?}");
        }
    }

    /// 07 E2, on both servers: a Save is held inside its copy (at its second copy request, on
    /// the `Origin`'s session) while the view reads far places it has never read, over its own
    /// stream: each read completes while the copy is still held — a handshake, not a clock; the
    /// only time is a generous watchdog against a deadlock. (The time each read takes on a real
    /// link is the live receipt's.)
    #[test]
    fn e2_the_view_keeps_reading_while_a_save_copies() {
        for side in sides() {
            let mut ed = ed_side(None, side);
            ed.goto(E1_AT);
            ed.typed("01");
            let (at_copy, go) = (
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            );
            let (reached, release) = (at_copy.clone(), go.clone());
            ed.hook(move |step| {
                if step == patch::Step::Copy(1) {
                    reached.store(true, Ordering::SeqCst);
                    while !release.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
                patch::At::Go
            });
            ed.key(Key::S, command(false));
            ed.key(Key::Enter, Modifiers::NONE);
            ed.drv
                .until(&mut ed.win, 30, |_| at_copy.load(Ordering::SeqCst));
            let status = ed.status();
            assert!(
                status.starts_with("saving — copying on host "),
                "{side:?}: {status}"
            );
            for place in [50 * MIB, 130 * MIB, BIG - 4096] {
                assert_eq!(
                    ed.win.doc.cell(place),
                    Cell::Pending,
                    "{side:?}: read before"
                );
                // `goto` waits for the byte, with a 30 s watchdog.
                ed.goto(place);
                assert_eq!(
                    ed.win.doc.cell(place),
                    Cell::Byte(bytes_at(&ed.remote, place, 1)[0]),
                    "{side:?}: 0x{place:X}"
                );
                assert!(
                    ed.win.job.is_some() && !go.load(Ordering::SeqCst),
                    "{side:?}: the copy was let go"
                );
            }
            go.store(true, Ordering::SeqCst);
            ed.drv.until(&mut ed.win, 120, |w| w.job.is_none());
            assert!(
                ed.win.status.text.starts_with("saved "),
                "{side:?}: {}",
                ed.win.status.text
            );
        }
    }

    // ---- E21: the `Origin` refused at Save ----

    /// 07 E21 (06 V10, its second moment): with unsaved edits, Save while sshd refuses the
    /// `Origin`'s session fails with 06 §6's line; the edits stay in the window and in the
    /// journal; nothing was sent to the remote. Once sessions are free the same Save lands.
    #[test]
    fn e21_the_origin_refused_at_save_says_the_line_and_the_next_save_lands() {
        let tmp = tempfile::tempdir().unwrap();
        let body = body();
        let remote = remote_file(tmp.path(), Some(&body));
        let abs = remote.to_str().unwrap().to_string();
        let moved = Moved::default();
        let refuse = Arc::new(AtomicBool::new(true));
        let dials = Arc::new(AtomicUsize::new(0));
        let (r, m, d) = (refuse.clone(), moved.clone(), dials.clone());
        let at = LocalFs::new().stat(&abs).unwrap();
        let origin = Origin::placeholder(
            Some(Box::new(move || {
                d.fetch_add(1, Ordering::SeqCst);
                if r.load(Ordering::SeqCst) {
                    bail!(
                        "cannot start sftp on host: ssh said: mux_client_request_session: session \
                         request failed: Session open refused by peer"
                    );
                }
                Ok(Box::new(Moving {
                    inner: Box::new(LocalFs::new()),
                    moved: m.clone(),
                }) as Box<dyn RemoteFs + Send>)
            })),
            "host".into(),
            abs.clone(),
            at,
            crate::gui::Placeholder::YES,
        );
        let (source, win) = window_on(tmp.path(), &remote, Side::Local, Some(origin), &moved);
        let mut ed = Ed {
            tmp,
            remote,
            side: Side::Local,
            source,
            moved,
            win,
            drv: Driver::new(760.0, 560.0),
        };
        ed.ready();
        ed.typed("AB");
        ed.save();
        assert_eq!(ed.win.status.text, "save failed");
        let said = ed.win.notices.join("\n");
        assert!(said.contains(crate::stream::SESSION_REFUSED), "{said}");
        assert!(!said.contains("mux_client"), "not ssh's words: {said}");
        assert_eq!(dials.load(Ordering::SeqCst), 1);
        // Nothing sent: no request moved a byte, no temp, the file as it was.
        assert_eq!(ed.moved.now(), (0, 0, 0));
        assert!(ed.temps().is_empty());
        assert!(std::fs::read(&ed.remote).unwrap() == body);
        // The edits stay, in the window and in the journal.
        assert!(ed.win.doc.unsaved() && ed.win.doc.byte(0) == Some(0xAB));
        let journals = ed.journals();
        assert_eq!(journals.len(), 1, "{journals:?}");
        let current = std::fs::read(journals[0].join("CURRENT")).unwrap();
        let at = crate::journal::parse_current(&current).unwrap();
        let gen = std::fs::read(journals[0].join(format!("gen-{}", at.n))).unwrap();
        let gen = crate::journal::parse_generation(&gen).unwrap();
        assert!(gen.edits.contains("0x0: AB was 00"), "{}", gen.edits);
        // The refusal ends: the next Save connects and lands.
        refuse.store(false, Ordering::SeqCst);
        ed.save();
        assert!(
            ed.win.status.text.starts_with("saved "),
            "{}",
            ed.win.status.text
        );
        assert_eq!(std::fs::read(&ed.remote).unwrap()[0], 0xAB);
        assert_eq!(dials.load(Ordering::SeqCst), 2);
        assert!(ed.journals().is_empty());
    }

    // ---- E10: a crash, and what the next open offers ----

    /// 07 E10: an edit is `keeping…` until its generation is durable, then `kept at
    /// <journal>`. A kill then leaves the journal; the next open offers Restore, and Restore
    /// shows the same changed bytes, marked, the remote untouched. Discard removes the journal.
    #[test]
    fn e10_after_a_kill_the_next_open_restores_the_edits() {
        let body = body();
        let mut ed = ed(&body);
        ed.goto(0x1234);
        ed.typed("AB");
        let (line, _, _) = ed.win.kept_line();
        assert_eq!(line, "unsaved — keeping…");
        ed.kept();
        let dir = ed.journals().pop().expect("a journal");
        let (line, _, _) = ed.win.kept_line();
        assert_eq!(
            line,
            format!("unsaved — kept at {}", crate::gui::binary::tilde(&dir))
        );
        ed.reopen();
        assert!(std::fs::read(&ed.remote).unwrap() == body, "the remote");
        let rows = ed.rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            rows[0].starts_with("unsaved edits from ")
                && rows[0].contains("(1 byte changed) kept at "),
            "{rows:?}"
        );
        assert!(ed.drv.find_text("Restore").is_some(), "its button");
        ed.press_row("unsaved edits", RowAction::Restore);
        ed.goto(0x1234);
        ed.ready();
        assert_eq!(ed.win.doc.byte(0x1234), Some(0xAB));
        assert!(ed.win.doc.changed(0x1234), "marked as changed");
        assert_eq!(ed.win.doc.changed_count(), 1);
        assert!(ed.win.doc.unsaved());
        assert_eq!(ed.win.status.text, "restored 1 byte");
        // The window holds the journal now: a kill and a reopen offer it again; Discard there
        // removes it.
        ed.kept();
        ed.reopen();
        assert_eq!(ed.rows().len(), 1, "{:?}", ed.rows());
        ed.press_row("unsaved edits", RowAction::Discard);
        assert!(ed.journals().is_empty(), "{:?}", ed.journals());
        assert!(!dir.exists());
        assert!(std::fs::read(&ed.remote).unwrap() == body, "the remote");
    }

    /// 07 E10, a kill right after a second edit: Restore gives the last durable generation — the
    /// first edit at least, never a torn one.
    #[test]
    fn e10_a_kill_before_the_second_edit_is_kept_restores_the_first() {
        let mut ed = ed(&body());
        ed.typed("AB");
        ed.kept();
        ed.typed("CD");
        ed.reopen();
        let rows = ed.rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        ed.press_row("unsaved edits", RowAction::Restore);
        ed.ready();
        assert_eq!(ed.win.doc.byte(0), Some(0xAB), "the first edit");
        let second = ed.win.doc.byte(1);
        assert!(
            second == Some(0xCD) || second == Some(body()[1]),
            "{second:?}"
        );
    }

    /// 07 §3: closing with nothing unsaved removes the journal; Discard in the prompt removes it
    /// with the window; closing with edits written nowhere keeps it for the next open.
    #[test]
    fn closing_keeps_or_removes_the_journal() {
        // Undone back to the file: nothing unsaved, the journal goes with the window.
        let mut ed = ed(&body());
        ed.typed("AB");
        ed.kept();
        ed.key(Key::Z, command(false));
        assert!(!ed.win.doc.unsaved());
        assert!(ed.win.close_requested());
        let tmp = std::mem::replace(
            &mut ed.win,
            window_on(ed.tmp.path(), &ed.remote, Side::Local, None, &ed.moved).1,
        );
        drop(tmp);
        assert!(
            ed.journals().is_empty(),
            "nothing unsaved: {:?}",
            ed.journals()
        );
        // Discard: the prompt's hover names the journal, and it goes.
        let mut ed = ed_side(Some(&body()), Side::Local);
        ed.typed("AB");
        ed.kept();
        let dir = ed.journals().pop().unwrap();
        assert!(!ed.win.close_requested());
        ed.frames(2);
        ed.drv.click_text(&mut ed.win, "Discard");
        assert!(ed.win.discard && ed.win.close_ok);
        let win = std::mem::replace(
            &mut ed.win,
            window_on(ed.tmp.path(), &ed.remote, Side::Local, None, &ed.moved).1,
        );
        drop(win);
        assert!(!dir.exists(), "Discard keeps the journal");
        // Closing without answering (the process ending): kept, the last edit in — written at
        // the close, before its 250 ms were up.
        let mut ed = ed_side(Some(&body()), Side::Local);
        ed.typed("AB");
        assert_eq!(ed.win.schedule.status(), Keep::Keeping, "not kept yet");
        let win = std::mem::replace(
            &mut ed.win,
            window_on(ed.tmp.path(), &ed.remote, Side::Local, None, &ed.moved).1,
        );
        drop(win);
        let dirs = ed.journals();
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        let (edits, seen) = kept_in(&dirs[0]);
        assert_eq!(edits, ["0x0: AB was 00"], "the edit kept");
        assert_eq!(
            seen,
            [(0, page_sha(&ed.remote, 0))],
            "its evidence: page 0 as shown"
        );
    }

    // ---- E11: the undo file ----

    /// 07 E11: the undo file a Save left is offered on the next open; Load as edits makes the
    /// bytes it holds the window's edits, and Save then writes them back: the remote's SHA-256
    /// is the original's.
    #[test]
    fn e11_load_as_edits_and_save_give_the_original_back() {
        let body = body();
        let mut ed = ed(&body);
        ed.goto(0x1000 + 5);
        ed.typed("DEADBEEF");
        ed.save();
        assert!(
            ed.win.status.text.starts_with("saved "),
            "{}",
            ed.win.status.text
        );
        assert_ne!(std::fs::read(&ed.remote).unwrap(), body);
        ed.reopen();
        let rows = ed.rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            rows[0].starts_with("bytes replaced by the save at ")
                && rows[0].contains(" (4 bytes) kept at ")
                && rows[0].ends_with(".undo.txt"),
            "{rows:?}"
        );
        assert!(ed.drv.find_text("Load as edits").is_some());
        ed.press_row("bytes replaced", RowAction::Load);
        ed.goto(0x1000 + 5);
        ed.ready();
        assert_eq!(ed.win.doc.changed_count(), 4);
        for i in 0..4u64 {
            assert_eq!(ed.win.doc.byte(0x1005 + i), Some(body[0x1005 + i as usize]));
        }
        ed.save();
        assert!(
            ed.win.status.text.starts_with("saved "),
            "{}",
            ed.win.status.text
        );
        assert!(
            std::fs::read(&ed.remote).unwrap() == body,
            "the original's bytes"
        );
    }

    /// 07 E11, second part: above 256 MiB another writer's change made before the copy, away
    /// from the edits, with the mtime put back, is kept by the Save (§4.5's first row). Load as
    /// edits and Save then revert only sopen's 4 bytes: the other change stays.
    #[test]
    fn e11_the_undo_file_reverts_only_sopens_bytes() {
        let mut ed = ed_side(None, Side::Local);
        let before = bytes_at(&ed.remote, E1_AT, 4);
        ed.goto(E1_AT);
        ed.typed("DEADBEEF");
        // Another writer, far from the edited page, the mtime put back.
        write_back(&ed.remote, 130 * MIB + 5, b"ZZ");
        ed.save();
        assert!(
            ed.win.status.text.starts_with("saved "),
            "{}",
            ed.win.status.text
        );
        assert_eq!(
            bytes_at(&ed.remote, 130 * MIB + 5, 2),
            b"ZZ",
            "kept by the copy"
        );
        ed.reopen();
        ed.press_row("bytes replaced", RowAction::Load);
        ed.goto(E1_AT);
        ed.ready();
        ed.save();
        assert!(
            ed.win.status.text.starts_with("saved "),
            "{}",
            ed.win.status.text
        );
        assert_eq!(
            bytes_at(&ed.remote, E1_AT, 4),
            before,
            "sopen's bytes reverted"
        );
        assert_eq!(
            bytes_at(&ed.remote, 130 * MIB + 5, 2),
            b"ZZ",
            "the other change stays"
        );
    }

    // ---- E12, E13: Save As ----

    /// The Save As modal of `ed` into `folder`, opened by Alt+Cmd/Ctrl+S.
    fn save_as_into(ed: &mut Ed, folder: &Path) {
        ed.win.save_as_folder = Some(folder.to_path_buf());
        ed.key(Key::S, command(false) | Modifiers::ALT);
        ed.frames(2);
        assert!(ed.win.save_as.is_some(), "no Save As");
    }

    /// `folder`'s entries, names only, sorted (a staging folder would show).
    fn entries(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// 07 E12 at a scaled size (a file above 256 MiB): Alt+Cmd/Ctrl+S opens Save As — never
    /// Save — and its Save writes the remote read fresh with the patch applied; the file
    /// appears only when complete, with the source's size and its bytes patched. The edits are
    /// somewhere now: closing does not ask. A name taken meanwhile is refused with the next
    /// free `-N`; F7 opens it too.
    #[test]
    fn e12_save_as_writes_the_patched_file_complete() {
        let mut ed = ed_side(None, Side::Local);
        ed.goto(E1_AT);
        ed.typed("DEADBEEF");
        let out = ed.tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        // Held mid-transfer: the destination is not there yet, only the staging folder.
        let (held, release) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        ed.win.save_as_held = held.clone();
        ed.win.test_hold = Some(Duration::from_secs(600));
        ed.win.test_release = Some(release.clone());
        save_as_into(&mut ed, &out);
        assert_eq!(ed.win.save_starts, 0, "not Save");
        ed.key(Key::Enter, Modifiers::NONE);
        ed.drv
            .until(&mut ed.win, 120, |_| held.load(Ordering::SeqCst));
        let staged = entries(&out);
        assert_eq!(staged.len(), 1, "{staged:?}");
        assert!(
            !out.join("model.edited.bin").exists(),
            "there before it is complete"
        );
        assert!(ed.win.save_as.as_ref().is_some_and(|(m, ..)| m.busy()));
        release.store(true, Ordering::SeqCst);
        ed.win.test_hold = None;
        ed.drv.until(&mut ed.win, 120, |w| w.save_as.is_none());
        assert_eq!(entries(&out), ["model.edited.bin"]);
        let saved = out.join("model.edited.bin");
        assert_eq!(std::fs::metadata(&saved).unwrap().len(), BIG);
        // The same bytes as the remote with the patch applied.
        let want = ed.tmp.path().join("want.bin");
        std::fs::copy(&ed.remote, &want).unwrap();
        write_back(&want, E1_AT, &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(file_sha(&saved), file_sha(&want));
        assert!(
            ed.win.status.text.starts_with("saved "),
            "{}",
            ed.win.status.text
        );
        assert!(
            ed.win.doc.dirty() && !ed.win.doc.unsaved(),
            "counts as Save copy does"
        );
        assert_eq!(ed.moved.now().1, 0, "nothing written to the remote");
        // F7 opens it again; a name taken after it opened is refused with the next free -N.
        ed.win.save_as_folder = Some(out.clone());
        ed.key(Key::F7, Modifiers::NONE);
        ed.frames(2);
        assert!(ed.win.save_as.is_some(), "F7");
        let field = out.join("model.edited-1.bin");
        std::fs::write(&field, b"someone else's").unwrap();
        ed.key(Key::Enter, Modifiers::NONE);
        ed.drv.until(&mut ed.win, 60, |w| {
            w.save_as.as_ref().is_some_and(|(m, ..)| !m.busy())
        });
        ed.frames(2);
        let said: Vec<String> = ed
            .drv
            .texts()
            .into_iter()
            .map(|(_, _, t)| t)
            .filter(|t| t.contains("exists"))
            .collect();
        assert_eq!(
            said,
            ["model.edited-1.bin exists — not replaced. Save again saves as model.edited-2.bin"],
            "the refusal"
        );
        assert_eq!(std::fs::read(&field).unwrap(), b"someone else's");
        assert_eq!(
            entries(&out),
            ["model.edited-1.bin", "model.edited.bin"],
            "no staging left"
        );
        ed.key(Key::Escape, Modifiers::NONE);
        ed.frames(2);
        assert!(
            ed.win.save_as.is_none(),
            "Esc with no save running closes it"
        );
    }

    /// Every name the Save As field could suggest taken (`-1` to `-9999` beside the name
    /// itself): it suggests none — the field keeps the name and the modal says why — and a Save
    /// there is refused with those words, the file there untouched and nothing left beside it.
    #[test]
    fn save_as_with_every_name_taken_says_so_and_saves_nothing() {
        let mut ed = ed_side(Some(&body()), Side::Local);
        ed.typed("AB");
        let out = ed.tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(out.join("model.edited.bin"), b"the owner's file").unwrap();
        for n in 1..=9999 {
            std::fs::write(out.join(format!("model.edited-{n}.bin")), b"").unwrap();
        }
        let taken = format!(
            "every name from model.edited-1.bin to model.edited-9999.bin is taken in {}",
            crate::gui::binary::tilde(&out)
        );
        save_as_into(&mut ed, &out);
        let said = |ed: &Ed| -> Vec<String> {
            ed.drv
                .texts()
                .into_iter()
                .map(|(_, _, t)| t)
                .filter(|t| t.contains("every name from"))
                .collect()
        };
        assert_eq!(said(&ed), std::slice::from_ref(&taken), "the modal's note");
        ed.key(Key::Enter, Modifiers::NONE);
        ed.drv.until(&mut ed.win, 60, |w| {
            w.save_as.as_ref().is_some_and(|(m, ..)| !m.busy())
        });
        ed.frames(2);
        let refused = said(&ed);
        assert!(
            refused.len() == 1 && refused[0].ends_with(&taken) && refused[0].contains("exists"),
            "{refused:?}"
        );
        assert_eq!(
            std::fs::read(out.join("model.edited.bin")).unwrap(),
            b"the owner's file"
        );
        assert_eq!(entries(&out).len(), 10_000, "something left beside them");
        assert!(ed.win.doc.unsaved(), "the edits are saved nowhere");
        ed.key(Key::Escape, Modifiers::NONE);
    }

    /// 07 E12: Esc while Save As transfers leaves nothing in the folder (no `part`, no staging),
    /// and the edits stay unsaved.
    #[test]
    fn e12_esc_mid_transfer_leaves_nothing() {
        let mut ed = ed(&body());
        ed.typed("AB");
        let out = ed.tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        // A hold in the fill, so Esc comes mid-transfer.
        let held = Arc::new(AtomicBool::new(false));
        ed.win.save_as_held = held.clone();
        ed.win.test_hold = Some(Duration::from_secs(20));
        save_as_into(&mut ed, &out);
        ed.key(Key::Enter, Modifiers::NONE);
        ed.drv
            .until(&mut ed.win, 30, |_| held.load(Ordering::SeqCst));
        let staged = entries(&out);
        assert_eq!(
            staged.len(),
            1,
            "the staging folder while it runs: {staged:?}"
        );
        ed.key(Key::Escape, Modifiers::NONE);
        ed.drv.until(&mut ed.win, 30, |w| {
            w.save_as.as_ref().is_some_and(|(m, ..)| !m.busy())
        });
        ed.frames(2);
        assert!(ed
            .drv
            .find_text(crate::gui::binary::SAVE_CANCELLED)
            .is_some());
        assert!(entries(&out).is_empty(), "{:?}", entries(&out));
        assert!(ed.win.doc.unsaved(), "the edits stay unsaved");
    }

    /// 07 E13: the source changed during the transfer (its modification time moved): nothing
    /// is saved, the staging folder is removed, and the reason is shown.
    #[test]
    fn e13_a_source_changed_mid_transfer_saves_nothing() {
        let mut ed = ed(&body());
        ed.typed("AB");
        let out = ed.tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let (held, release) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        ed.win.save_as_held = held.clone();
        ed.win.test_hold = Some(Duration::from_secs(600));
        ed.win.test_release = Some(release.clone());
        save_as_into(&mut ed, &out);
        ed.key(Key::Enter, Modifiers::NONE);
        ed.drv
            .until(&mut ed.win, 30, |_| held.load(Ordering::SeqCst));
        // Another writer, mid-transfer; then the transfer goes on.
        write_back(&ed.remote, 7, b"x");
        set_mtime(&ed.remote, OLD + 60);
        release.store(true, Ordering::SeqCst);
        ed.drv.until(&mut ed.win, 60, |w| {
            w.save_as.as_ref().is_some_and(|(m, ..)| !m.busy())
        });
        ed.frames(2);
        let said = ed
            .drv
            .text_starting_with(&format!("{} changed while it was read (", ed.shown()))
            .unwrap_or_default();
        assert!(said.ends_with("; nothing was saved"), "{said:?}");
        assert!(entries(&out).is_empty(), "{:?}", entries(&out));
        assert!(ed.win.doc.unsaved());
    }

    // ---- E16: the close prompt ----

    /// 07 E16: closing with unsaved edits asks Save · Save As… · Discard · Cancel, each doing
    /// what §10 says; Save copy stands in for Save where Save is off.
    #[test]
    fn e16_the_close_prompt_saves_saves_as_discards_or_cancels() {
        let buttons = |ed: &Ed| -> Vec<&str> {
            ["Save", "Save copy", "Save As…", "Discard", "Cancel"]
                .into_iter()
                .filter(|b| {
                    ed.drv.texts().iter().filter(|(_, _, t)| t == b).count()
                        > usize::from(*b == "Save" || *b == "Save copy" || *b == "Save As…")
                })
                .collect()
        };
        // Cancel: the window stays, the edit too.
        let mut ed = ed(&body());
        ed.typed("AB");
        assert!(!ed.win.close_requested());
        ed.frames(2);
        assert!(ed
            .drv
            .find_text(&format!(
                "Save the changes to {} before closing?",
                ed.shown()
            ))
            .is_some());
        assert_eq!(buttons(&ed), ["Save", "Save As…", "Discard", "Cancel"]);
        ed.drv.click_text(&mut ed.win, "Cancel");
        assert!(!ed.win.prompt && !ed.win.close_ok && ed.win.doc.unsaved());
        // Save: it saves, then closes.
        assert!(!ed.win.close_requested());
        ed.frames(2);
        let save = ed
            .drv
            .texts()
            .iter()
            .filter(|(_, _, t)| t == "Save")
            .map(|(r, _, _)| r.center())
            .next_back()
            .unwrap();
        ed.drv.click(&mut ed.win, save);
        ed.drv.until(&mut ed.win, 60, |w| w.job.is_none());
        ed.frames(2);
        assert!(ed.win.close_ok, "{}", ed.win.status.text);
        assert_eq!(std::fs::read(&ed.remote).unwrap()[0], 0xAB);
        // Save As…: the modal, and the window closes once it has saved.
        let mut ed = ed_side(Some(&body()), Side::Local);
        ed.typed("CD");
        let out = ed.tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        ed.win.save_as_folder = Some(out.clone());
        assert!(!ed.win.close_requested());
        ed.frames(2);
        let prompt_save_as = ed
            .drv
            .texts()
            .iter()
            .filter(|(_, _, t)| t == "Save As…")
            .map(|(r, _, _)| r.center())
            .next_back()
            .unwrap();
        ed.drv.click(&mut ed.win, prompt_save_as);
        assert!(ed.win.save_as.is_some());
        ed.frames(2);
        ed.key(Key::Enter, Modifiers::NONE);
        ed.drv.until(&mut ed.win, 60, |w| w.save_as.is_none());
        assert!(ed.win.close_ok, "closes after the Save As landed");
        assert_eq!(
            std::fs::read(out.join("model.edited.bin")).unwrap()[0],
            0xCD
        );
        // Where Save is off, Save copy stands in, and does what it says: the sibling is written,
        // the original is not, and the window closes.
        let body = body();
        let mut ed = ed_side(Some(&body), Side::Local);
        ed.typed("EF");
        ed.win.can_replace = false;
        assert!(!ed.win.close_requested());
        ed.frames(2);
        assert_eq!(buttons(&ed), ["Save copy", "Save As…", "Discard", "Cancel"]);
        let copy = ed
            .drv
            .texts()
            .iter()
            .filter(|(_, _, t)| t == "Save copy")
            .map(|(r, _, _)| r.center())
            .next_back()
            .unwrap();
        ed.drv.click(&mut ed.win, copy);
        ed.drv.until(&mut ed.win, 60, |w| w.job.is_none());
        ed.frames(2);
        assert!(ed.win.close_ok, "{}", ed.win.status.text);
        let sibling = ed.remote.with_file_name("model.edited.bin");
        assert_eq!(std::fs::read(&sibling).unwrap()[0], 0xEF);
        assert!(std::fs::read(&ed.remote).unwrap() == body, "the original");
        // Discard, with the edits kept on disk: the hover names the journal; closing is
        // allowed; the journal goes with the window; the remote is as it was.
        let mut ed = ed_side(Some(&body), Side::Local);
        ed.typed("12");
        ed.kept();
        let dirs = ed.journals();
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        assert_eq!(kept_in(&dirs[0]).0, ["0x0: 12 was 00"]);
        assert!(!ed.win.close_requested());
        ed.frames(2);
        ed.drv.click_text(&mut ed.win, "Discard");
        assert!(
            ed.win.close_ok && ed.win.close_requested(),
            "closing is allowed"
        );
        let win = std::mem::replace(
            &mut ed.win,
            window_on(ed.tmp.path(), &ed.remote, Side::Local, None, &ed.moved).1,
        );
        drop(win);
        assert!(!dirs[0].exists(), "the kept edits are gone");
        assert!(ed.journals().is_empty());
        assert!(std::fs::read(&ed.remote).unwrap() == body, "the remote");
    }

    // ---- E18: a Save cut off, reconciled on the next open ----

    /// The window's Save stopped at `step` as a kill stops it.
    fn kill_at(ed: &mut Ed, step: patch::Step) {
        ed.hook(move |s| {
            if s == step {
                patch::At::Stop
            } else {
                patch::At::Go
            }
        });
        ed.save();
        let said = ed.win.notices.join("\n");
        assert!(said.contains("stopped before"), "{step:?}: {said}");
    }

    /// The `save` record of the one journal under `ed`'s cache, as a reopen reads it.
    fn save_record(ed: &Ed) -> crate::journal::SaveIntent {
        let dirs = ed.journals();
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        crate::journal::parse_save(&std::fs::read(dirs[0].join("save")).unwrap()).unwrap()
    }

    /// 07 E18 and T8, on both servers: the server copy stopped before the create, before its
    /// first copy request, and right before the rename. First what the stop left, before any
    /// reopen: the phase the record made durable — `naming` (no temp yet), `created` (the temp
    /// made, empty, as recorded), `renaming` (staged and renaming are recorded back to back; the
    /// temp whole, the patch in it) — and the temp the record names. Then the reopen: the file
    /// untouched, a temp provably this Save's removed, the edits offered and restored, and a
    /// Save lands.
    #[test]
    fn e18_a_save_cut_off_before_the_rename_offers_the_edits_and_removes_its_own_temp() {
        use crate::journal::Phase;
        for side in sides() {
            for step in [
                patch::Step::Create,
                patch::Step::Copy(0),
                patch::Step::Rename,
            ] {
                let body = body();
                let mut ed = ed_side(Some(&body), side);
                ed.typed("AB");
                kill_at(&mut ed, step);
                let tag = format!("{side:?} {step:?}");
                let record = save_record(&ed);
                let (phase, temp) = match &record.phase {
                    Phase::Naming { temp } => ("naming", temp.clone()),
                    Phase::Created { temp, .. } => ("created", temp.clone()),
                    Phase::Staged { temp, .. } => ("staged", temp.clone()),
                    Phase::Renaming { temp, .. } => ("renaming", temp.clone()),
                    Phase::Writing { .. } => ("writing", String::new()),
                };
                let temp = PathBuf::from(temp);
                let mut patched = body.clone();
                patched[0] = 0xAB;
                match step {
                    patch::Step::Create => {
                        assert_eq!(phase, "naming", "{tag}");
                        assert!(!temp.exists(), "{tag}: the create was never sent");
                    }
                    patch::Step::Copy(_) => {
                        assert_eq!(phase, "created", "{tag}");
                        assert_eq!(std::fs::read(&temp).unwrap(), b"", "{tag}: made, empty");
                    }
                    _ => {
                        assert_eq!(phase, "renaming", "{tag}");
                        assert!(
                            std::fs::read(&temp).unwrap() == patched,
                            "{tag}: staged whole"
                        );
                    }
                }
                assert!(
                    std::fs::read(&ed.remote).unwrap() == body,
                    "{tag}: the target"
                );
                ed.reopen();
                assert!(std::fs::read(&ed.remote).unwrap() == body, "{tag}");
                assert!(!temp.exists(), "{tag}: {:?}", ed.temps());
                assert!(ed.temps().is_empty(), "{tag}: {:?}", ed.temps());
                let rows = ed.rows();
                // From step 7 on the undo file was made durable: it is offered too.
                let undo = usize::from(step == patch::Step::Rename);
                assert_eq!(rows.len(), 1 + undo, "{tag}: {rows:?}");
                assert!(
                    rows[0].starts_with("unsaved edits from ")
                        && rows[0].contains("did not land; your edits are kept"),
                    "{tag}: {rows:?}"
                );
                ed.press_row("unsaved edits", RowAction::Restore);
                ed.ready();
                assert_eq!(ed.win.doc.byte(0), Some(0xAB), "{tag}");
                ed.save();
                assert!(
                    ed.win.status.text.starts_with("saved "),
                    "{tag}: {}",
                    ed.win.status.text
                );
                assert!(std::fs::read(&ed.remote).unwrap() == patched, "{tag}");
            }
        }
    }

    /// 07 E2, live — the read times the unit test leaves out: a real Save of a file above
    /// 256 MiB on `SOPEN_TEST_SFTP_HOST` (its server copy on the `Origin`'s session) while the
    /// view goes to places it has never read, each timed from the jump to the byte on screen,
    /// frames included; each read completes while the copy is still under way. Prints a line per
    /// read and the Save's result. The file is changed by one byte (at 0x1000).
    #[test]
    #[ignore = "live: needs SOPEN_TEST_SFTP_HOST=<ssh alias> and SOPEN_TEST_SFTP_FILE=<abs path \
                of a file above 256 MiB there>"]
    fn live_e2_reads_while_a_real_save_copies() {
        let (Ok(host), Ok(abs)) = (
            std::env::var("SOPEN_TEST_SFTP_HOST"),
            std::env::var("SOPEN_TEST_SFTP_FILE"),
        ) else {
            eprintln!("skipping: SOPEN_TEST_SFTP_HOST or SOPEN_TEST_SFTP_FILE is not set");
            return;
        };
        let spec = crate::target::RemoteSpec {
            user: None,
            host: host.clone(),
            path: abs.clone(),
        };
        let state = crate::cache::dirs().unwrap().state;
        let connect = {
            let (spec, state) = (spec.clone(), state.clone());
            move || SftpRemote::connect(&spec, &state)
        };
        let tmp = tempfile::tempdir().unwrap();
        let view: Box<dyn RemoteFs + Send> = Box::new(connect().unwrap());
        let transport = view.into_ranged(&abs).map_err(|r| r.error).unwrap();
        let repaint = Repaint::default();
        let (again, reconnect) = (abs.clone(), connect.clone());
        let source = Source::open(
            transport,
            Box::new(move || {
                let fs: Box<dyn RemoteFs + Send> = Box::new(reconnect()?);
                fs.into_ranged(&again).map_err(|refused| refused.error)
            }),
            repaint.waker(),
        )
        .unwrap();
        let moved = Moved::default();
        let mut fs = Moving {
            inner: Box::new(connect().unwrap()),
            moved: moved.clone(),
        };
        let at = fs.stat(&abs).unwrap();
        let size = at.size;
        assert!(
            size > crate::remote::HASH_CHECK_MAX,
            "{size} bytes: too small"
        );
        let origin = Origin::new(Box::new(fs), None, Some(host.clone()), abs.clone(), at);
        let file = HexFile::streamed(origin, source.clone(), repaint, &tmp.path().join("cache"));
        let win = embedded(
            file,
            &Hooks::default(),
            Vec::new(),
            Arc::default(),
            Arc::default(),
        )
        .unwrap();
        let mut ed = Ed {
            win,
            drv: Driver::new(760.0, 560.0),
            source,
            moved,
            remote: PathBuf::from(&abs),
            side: Side::Sftp,
            tmp,
        };
        ed.ready();
        ed.goto(0x1000);
        // A byte other than the one there (a run before may have saved one).
        let byte = match ed.win.doc.cell(0x1000) {
            Cell::Byte(b) => !b,
            other => panic!("{other:?}"),
        };
        ed.typed(&format!("{byte:02X}"));
        let began = Instant::now();
        ed.key(Key::S, command(false));
        ed.frames(2);
        ed.key(Key::Enter, Modifiers::NONE);
        // The Save's phases as the status row says them, until its copy is under way; and the
        // longest frame.
        let deadline = Instant::now() + Duration::from_secs(120);
        let (mut said, mut longest) = (String::new(), Duration::ZERO);
        loop {
            let now = ed.status();
            if now != said {
                println!("{:>8.3} s  {now}", began.elapsed().as_secs_f64());
                said = now;
            }
            if said.starts_with("saving — copying on ") {
                break;
            }
            assert!(
                ed.win.job.is_some(),
                "the save ended before its copy was seen"
            );
            assert!(Instant::now() < deadline, "no copy: {said}");
            let frame = Instant::now();
            ed.frames(1);
            longest = longest.max(frame.elapsed());
        }
        println!("the longest frame so far: {longest:?}");
        // Places spread over the file, none read before; each counted read is a byte on screen
        // that came while the status row said the copy was under way, before and after it.
        let copying = |ed: &mut Ed| ed.status().starts_with("saving — copying on ");
        let local = std::path::Path::new(&abs).exists();
        let mut n = 0;
        for i in 1..=12u64 {
            let place = size / 13 * i / PAGE * PAGE + 0x800;
            if !copying(&mut ed) {
                break;
            }
            assert_eq!(ed.win.doc.cell(place), Cell::Pending, "read before");
            let start = Instant::now();
            ed.win.place(place, Pane::Hex);
            while ed.win.doc.cell(place) == Cell::Pending {
                assert!(
                    start.elapsed() < Duration::from_secs(30),
                    "0x{place:X} never came"
                );
                ed.frames(1);
            }
            let took = start.elapsed();
            let during = copying(&mut ed);
            let Cell::Byte(b) = ed.win.doc.cell(place) else {
                panic!("0x{place:X}: {:?}", ed.win.doc.cell(place));
            };
            if local {
                assert_eq!(b, bytes_at(&ed.remote, place, 1)[0], "0x{place:X}");
            }
            println!(
                "read 0x{place:X}: {took:?}, 0x{b:02X}, still in the copy: {during} ({})",
                ed.status()
            );
            n += usize::from(during);
        }
        ed.drv.until(&mut ed.win, 100_000, |w| w.job.is_none());
        println!(
            "save done after {:?}: {} — {n} reads completed during the copy",
            began.elapsed(),
            ed.win.status.text
        );
        assert!(n > 0, "no read happened during the copy");
        assert!(
            ed.win.status.text.starts_with("saved "),
            "{}",
            ed.win.status.text
        );
        // The edited byte, read back from the remote over a new session.
        let mut check = connect().unwrap();
        let head = check.read_head(&abs, 0x1001).unwrap();
        assert_eq!(head[0x1000], byte, "the saved byte on the remote");
        println!("0x1000 on the remote: 0x{:02X}", head[0x1000]);
    }

    /// 07 §3 Recovery: the lock retry waits only for this file's own journals — a folder named
    /// exactly as a journal of it, whose generation (when it can be read) is of this host and
    /// path. Beside this file's one journal: another file's whose name starts the same, one
    /// being removed, and a whole journal written for another path under a name of this file's.
    /// None of them is listed, and recovery does not wait out the retry for them.
    #[test]
    fn the_lock_retry_counts_only_this_files_journals() {
        let mut ed = ed(&body());
        ed.typed("AB");
        ed.kept();
        let ours = ed.journals().pop().unwrap();
        let idle = window_on(ed.tmp.path(), &ed.remote, Side::Local, None, &ed.moved).1;
        std::mem::replace(&mut ed.win, idle).crash();
        let cache = ed.tmp.path().join("cache");
        let abs = ed.abs();
        let base = crate::cache::edits_path(&cache, "host", &abs);
        let (home, name) = (base.parent().unwrap(), base.file_name().unwrap());
        let name = name.to_string_lossy();
        std::fs::create_dir(home.join(format!(
            "{name}.journal-notes.txt.journal-20261122T010203Z-0123abcd"
        )))
        .unwrap();
        std::fs::create_dir(home.join(format!("{name}.journal-20261122T010203Z-0123abce.gone")))
            .unwrap();
        let mut other = Journal::create(
            journal::disk(),
            &cache,
            "host",
            "/data/other.bin",
            journal::Kind::Hex,
        )
        .unwrap();
        other
            .publish(&journal::Snapshot {
                baseline: crate::remote::Fingerprint {
                    size: 1,
                    mtime: UNIX_EPOCH,
                    sha256: None,
                },
                seen: journal::Seen::pages(BTreeMap::new()),
                edits: file::edits_text(&BTreeMap::new()),
                pastes: Vec::new(),
            })
            .unwrap();
        let theirs = other.dir().to_path_buf();
        drop(other);
        std::fs::rename(
            &theirs,
            home.join(format!("{name}.journal-20261122T010203Z-89abcdef")),
        )
        .unwrap();
        assert_eq!(
            crate::journal::this_files_journals(&cache, "host", &abs),
            vec![ours.clone()]
        );
        // Our journal's lock is free before the look. A child another test forks holds a copy
        // of every descriptor until its exec, so a lock let go a moment ago can look taken for
        // a while — what the retry is for (07 §3 Recovery). Waited out here: a retry below can
        // only be for a journal that is not this file's. The look's own lock is let go by
        // `unlock`, not by closing: a child forked while it is held keeps its copy of the
        // descriptor, and the lock with it, until its exec — unlocking releases it for both.
        let freed = Instant::now();
        loop {
            use std::os::unix::fs::OpenOptionsExt;
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(ours.join("lock"))
                .unwrap();
            match lock.try_lock() {
                Ok(()) => {
                    lock.unlock().unwrap();
                    break;
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(e)) => panic!("our journal's lock: {e}"),
            }
            assert!(
                freed.elapsed() < Duration::from_secs(30),
                "our journal stays locked"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // A file of its own, not in a window: nothing else holds a journal of it.
        let file = file_on(ed.tmp.path(), &ed.remote, &ed.moved);
        let retries = file.retries.clone();
        let file = Mutex::new(file);
        let began = Instant::now();
        let rows = file::recover(&file);
        let took = began.elapsed();
        let lines: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Edits { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains(&*ours.file_name().unwrap().to_string_lossy()));
        assert_eq!(retries.load(Ordering::SeqCst), 0, "listed again for them");
        // A watchdog only (a deadlock would never come back).
        assert!(took < Duration::from_secs(30), "{took:?}");
    }

    // ---- review round 1: the journal's generations, the temp questions, Restore ----

    /// Opens a held gate when dropped: a test failing while it holds one fails, not hangs.
    struct Opens(Arc<AtomicBool>);

    impl Drop for Opens {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }

    /// P8: a generation held on its way to the journal (slow or stalled storage) when Save is
    /// pressed: the frame does not wait for it — the window keeps drawing, the Save shows as
    /// under way and edits are refused — and once it is let go the Save runs, after it, and
    /// leaves no journal. (A watchdog lets it go after 10 s, so a frame that waits fails rather
    /// than hangs.)
    #[test]
    fn a_save_does_not_wait_on_the_frame_for_a_generation() {
        for side in sides() {
            let body = body();
            let mut ed = ed_side(Some(&body), side);
            let gate = Arc::new(AtomicBool::new(true));
            let _open = Opens(gate.clone());
            ed.win.generation_gate = Some(gate.clone());
            ed.typed("AB");
            ed.drv.until(&mut ed.win, 60, |w| w.writing.is_some());
            let (done, watched) = mpsc::channel::<()>();
            let fired = Arc::new(AtomicBool::new(false));
            let watchdog = {
                let (gate, fired) = (gate.clone(), fired.clone());
                std::thread::spawn(move || {
                    if watched.recv_timeout(Duration::from_secs(10)).is_err() {
                        fired.store(true, Ordering::SeqCst);
                        gate.store(false, Ordering::SeqCst);
                    }
                })
            };
            ed.key(Key::S, command(false));
            ed.frames(3);
            assert!(!fired.load(Ordering::SeqCst), "{side:?}: the frame waited");
            assert!(
                gate.load(Ordering::SeqCst),
                "{side:?}: let go before the test did"
            );
            assert!(ed.win.job.is_some(), "{side:?}: no Save under way");
            let patches = ed.win.doc.patches().clone();
            ed.typed("CD");
            assert_eq!(
                *ed.win.doc.patches(),
                patches,
                "{side:?}: an edit while it waits"
            );
            assert_eq!(ed.status(), EDITING_RESUMES, "{side:?}");
            assert_eq!(
                bytes_at(&ed.remote, 0, 1),
                [body[0]],
                "{side:?}: saved early"
            );
            let _ = done.send(());
            watchdog.join().unwrap();
            gate.store(false, Ordering::SeqCst);
            ed.drv.until(&mut ed.win, 120, |w| w.job.is_none());
            ed.frames(4);
            assert!(
                ed.win.status.text.starts_with("saved "),
                "{side:?}: {}",
                ed.win.status.text
            );
            assert_eq!(bytes_at(&ed.remote, 0, 1), [0xAB], "{side:?}");
            assert!(ed.journals().is_empty(), "{side:?}: {:?}", ed.journals());
            assert!(ed.win.journal_dir.is_none(), "{side:?}: a journal named");
            assert!(!ed.win.doc.unsaved(), "{side:?}");
        }
    }

    /// P8, Esc: a Save still waiting for a generation on its way (stalled storage) is stopped
    /// by Esc — nothing read, written or copied over the connection, nothing written to the
    /// journal, no temp, the remote as it was; the edits unsaved and not said to have failed to
    /// be kept. Once the storage answers, the next generation keeps them.
    #[test]
    fn esc_stops_a_save_waiting_for_a_generation() {
        let body = body();
        let mut ed = ed(&body);
        let gate = Arc::new(AtomicBool::new(true));
        let _open = Opens(gate.clone());
        ed.win.generation_gate = Some(gate.clone());
        ed.typed("AB");
        ed.drv.until(&mut ed.win, 60, |w| w.writing.is_some());
        let counted = |ed: &Ed| {
            (
                ed.moved.now(),
                ed.moved.whole.load(Ordering::SeqCst),
                ed.moved.disk.load(Ordering::SeqCst),
            )
        };
        let before = counted(&ed);
        ed.key(Key::S, command(false));
        ed.frames(2);
        assert!(ed.win.job.is_some());
        ed.key(Key::Escape, Modifiers::NONE);
        ed.drv.until(&mut ed.win, 200, |w| w.job.is_none());
        assert_eq!(ed.win.status.text, STOPPED);
        assert!(ed.win.doc.unsaved());
        assert_ne!(
            ed.win.schedule.status(),
            Keep::NotKept("the journal could not be written".into()),
            "a failure said"
        );
        assert!(std::fs::read(&ed.remote).unwrap() == body, "sent");
        assert!(ed.temps().is_empty());
        // Nothing moved over the connection (read, written, copied, whole-file calls) and
        // nothing written to the journal between Save and Esc.
        assert_eq!(counted(&ed), before, "the Save did something");
        // The storage answers: the edits are kept.
        gate.store(false, Ordering::SeqCst);
        ed.kept();
        assert_eq!(kept_in(&ed.journals()[0]).0, ["0x0: AB was 00"]);
    }

    /// P8 for a local file's Save (in place): its worker, not the frame, waits for the
    /// generation on its way and then keeps the edits; the window goes on drawing and refuses
    /// edits meanwhile; then the Save lands and nothing is left in the journal.
    #[test]
    fn a_local_save_does_not_wait_on_the_frame_either() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("local.bin");
        std::fs::write(&path, FIXTURE).unwrap();
        let cache = tmp.path().join("cache");
        let mut fx = super::in_place(&path, tmp, 1000.0, 600.0);
        let gate = Arc::new(AtomicBool::new(true));
        let _open = Opens(gate.clone());
        fx.win.generation_gate = Some(gate.clone());
        let waited = Arc::new(AtomicBool::new(false));
        let seen = waited.clone();
        fx.win.before_drain = Some(Box::new(move || seen.store(true, Ordering::SeqCst)));
        fx.typed("00");
        fx.drv.until(&mut fx.win, 60, |w| w.writing.is_some());
        let (done, watched) = mpsc::channel::<()>();
        let fired = Arc::new(AtomicBool::new(false));
        let watchdog = {
            let (gate, fired) = (gate.clone(), fired.clone());
            std::thread::spawn(move || {
                if watched.recv_timeout(Duration::from_secs(10)).is_err() {
                    fired.store(true, Ordering::SeqCst);
                    gate.store(false, Ordering::SeqCst);
                }
            })
        };
        fx.key(Key::S, command(false));
        fx.drv.idle(&mut fx.win, 3);
        assert!(!fired.load(Ordering::SeqCst), "the frame waited");
        assert!(fx.win.job.is_some(), "no Save under way");
        let patches = fx.win.doc.patches().clone();
        fx.typed("11");
        assert_eq!(*fx.win.doc.patches(), patches, "an edit while it waits");
        assert_eq!(std::fs::read(&path).unwrap(), FIXTURE, "saved early");
        let _ = done.send(());
        watchdog.join().unwrap();
        gate.store(false, Ordering::SeqCst);
        fx.wait_for_save();
        assert!(
            waited.load(Ordering::SeqCst),
            "the Save did not wait for it"
        );
        assert!(
            fx.win.status.text.starts_with("saved "),
            "{}",
            fx.win.status.text
        );
        assert_eq!(std::fs::read(&path).unwrap()[0], 0x00);
        assert!(
            journal_dirs_in(&cache).is_empty(),
            "{:?}",
            journal_dirs_in(&cache)
        );
        assert!(fx.win.journal_dir.is_none());
    }

    /// A local file's Save that does not land, its start unable to keep the edits (the disk
    /// under the journal refuses the write): the window says they are not kept, and why.
    #[test]
    fn a_local_save_that_could_not_keep_the_edits_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("local.bin");
        std::fs::write(&path, FIXTURE).unwrap();
        let mut fx = super::in_place(&path, tmp, 1000.0, 600.0);
        fx.win.journal_fs = Arc::new(Writes {
            bytes: Arc::default(),
            full: Arc::new(AtomicBool::new(true)),
            hold_record: Arc::default(),
        });
        fx.typed("00");
        // Another writer, so the Save is refused.
        write_back(&path, 5, b"x");
        set_mtime(&path, OLD + 60);
        fx.key(Key::S, command(false));
        fx.wait_for_save();
        assert!(
            !fx.win.status.text.starts_with("saved "),
            "{}",
            fx.win.status.text
        );
        assert!(fx.win.doc.unsaved());
        let (line, _, failed) = fx.win.kept_line();
        assert!(failed && line.contains("no space left on device"), "{line}");
    }

    /// A generation due when Save is pressed, not yet at the journal (held before it takes it):
    /// the Save's start waits for it — it lands first — and none begun before the Save writes
    /// after it. Once the Save has landed nothing is left: no journal on disk, none named in the
    /// window, nothing unsaved.
    #[test]
    fn a_generation_on_its_way_at_a_save_cannot_bring_the_journal_back() {
        for side in sides() {
            let body = body();
            let mut ed = ed_side(Some(&body), side);
            let gate = Arc::new(AtomicBool::new(true));
            let _open = Opens(gate.clone());
            ed.win.generation_gate = Some(gate.clone());
            let waited = Arc::new(AtomicBool::new(false));
            let (open, seen) = (gate.clone(), waited.clone());
            ed.win.before_drain = Some(Box::new(move || {
                seen.store(true, Ordering::SeqCst);
                open.store(false, Ordering::SeqCst);
            }));
            ed.typed("AB");
            ed.drv.until(&mut ed.win, 60, |w| w.writing.is_some());
            assert!(
                ed.journals().is_empty(),
                "{side:?}: held before the journal"
            );
            ed.save();
            // Whatever the Save did with it, the generation is let go now.
            gate.store(false, Ordering::SeqCst);
            ed.drv.until(&mut ed.win, 60, |w| w.writing.is_none());
            ed.frames(4);
            assert!(
                ed.win.status.text.starts_with("saved "),
                "{side:?}: {}",
                ed.win.status.text
            );
            assert!(
                waited.load(Ordering::SeqCst),
                "{side:?}: the Save did not wait for it"
            );
            assert_eq!(bytes_at(&ed.remote, 0, 1), [0xAB], "{side:?}");
            assert!(ed.journals().is_empty(), "{side:?}: {:?}", ed.journals());
            assert!(ed.win.journal_dir.is_none(), "{side:?}: a journal named");
            assert!(
                ed.win.journal.lock().unwrap().is_none(),
                "{side:?}: a journal held"
            );
            assert!(!ed.win.doc.unsaved(), "{side:?}");
        }
    }

    /// The second guard alone: a generation begun before the journal was retired (here, held
    /// at the journal while `saved_journal` retires it) writes nothing when it goes on — no
    /// journal made, none named.
    #[test]
    fn a_generation_begun_before_the_journal_was_retired_writes_nothing() {
        let mut ed = ed(&body());
        let gate = Arc::new(AtomicBool::new(true));
        let _open = Opens(gate.clone());
        ed.win.generation_gate = Some(gate.clone());
        ed.typed("AB");
        ed.drv.until(&mut ed.win, 60, |w| w.writing.is_some());
        // Nothing unsaved now, and the journal retired (as after a Save that landed).
        ed.key(Key::Z, command(false));
        assert!(!ed.win.doc.unsaved());
        ed.win.saved_journal();
        gate.store(false, Ordering::SeqCst);
        ed.drv.until(&mut ed.win, 60, |w| w.writing.is_none());
        ed.frames(2);
        assert!(ed.journals().is_empty(), "{:?}", ed.journals());
        assert!(ed.win.journal_dir.is_none());
    }

    /// A Save of this session cut off — its copy got no reply, and its temp could not be removed
    /// — leaving a temp that sopen cannot prove its own, its record pending. Returns the temp
    /// and what it holds.
    fn cut_off_leaving_a_temp(ed: &mut Ed) -> (PathBuf, Vec<u8>) {
        ed.moved.fail_remove.store(true, Ordering::SeqCst);
        ed.hook(|s| {
            if s == patch::Step::Copy(1) {
                patch::At::LoseReply
            } else {
                patch::At::Go
            }
        });
        ed.save();
        ed.win.file.lock().unwrap().step_hook = None;
        ed.moved.fail_remove.store(false, Ordering::SeqCst);
        let temps = ed.temps();
        assert_eq!(temps.len(), 1, "{temps:?}");
        let temp = ed.remote.parent().unwrap().join(&temps[0]);
        let grown = std::fs::read(&temp).unwrap();
        assert!(!grown.is_empty(), "the copy began");
        let _ = save_record(ed);
        (temp, grown)
    }

    /// The live question's line, if one is asked.
    fn live_question(ed: &Ed) -> Option<String> {
        ed.win.left.as_ref().map(|l| l.line.clone())
    }

    /// After [`cut_off_leaving_a_temp`] the next Save in the same window reconciles it:
    /// nothing is written, the record stays, and the window asks Remove it / Keep it — as a
    /// reopen does — and Save waits for the answer. Keep it ends the record and leaves the file;
    /// Remove it removes it; then the Save lands. On both servers, each answer.
    #[test]
    fn the_next_save_asks_about_a_temp_it_cannot_prove_its_own() {
        for side in sides() {
            for keep in [true, false] {
                let tag = format!("{side:?} keep={keep}");
                let body = body();
                let mut ed = ed_side(Some(&body), side);
                ed.typed("AB");
                let (temp, grown) = cut_off_leaving_a_temp(&mut ed);
                // The next Save: nothing written, the record kept, the question asked.
                let copied = ed.moved.now().2;
                ed.save();
                assert!(ed.win.status.error, "{tag}: {}", ed.win.status.text);
                assert_eq!(ed.moved.now().2, copied, "{tag}: copied again");
                assert!(
                    std::fs::read(&ed.remote).unwrap() == body,
                    "{tag}: the target"
                );
                assert_eq!(std::fs::read(&temp).unwrap(), grown, "{tag}: the temp");
                let _ = save_record(&ed);
                let line = live_question(&ed);
                assert!(
                    line.as_deref().is_some_and(
                        |l| l.contains("sopen could not verify") && l.ends_with("— remove it?")
                    ),
                    "{tag}: {line:?}"
                );
                ed.frames(2);
                assert!(ed.drv.find_text("Remove it").is_some(), "{tag}: no button");
                // Save waits for the answer.
                ed.key(Key::S, command(false));
                ed.frames(2);
                assert!(ed.win.job.is_none(), "{tag}: saved before the answer");
                ed.drv
                    .click_text(&mut ed.win, if keep { "Keep it" } else { "Remove it" });
                ed.drv.until(&mut ed.win, 30, |w| {
                    w.answering.is_none() && w.left.is_none()
                });
                assert_eq!(temp.exists(), keep, "{tag}");
                assert!(
                    !ed.journals()[0].join("save").exists(),
                    "{tag}: the record outlived the answer"
                );
                ed.save();
                assert!(
                    ed.win.status.text.starts_with("saved "),
                    "{tag}: {}",
                    ed.win.status.text
                );
                assert_eq!(bytes_at(&ed.remote, 0, 1), [0xAB], "{tag}");
                if keep {
                    assert_eq!(
                        std::fs::read(&temp).unwrap(),
                        grown,
                        "{tag}: kept as it was"
                    );
                }
            }
        }
    }

    /// An edit made after a cut-off Save, and Save pressed at once (before its generation was
    /// due): the Save's start keeps it in the journal, beside the cut-off one's record — so the
    /// window's "kept" is true even when that Save then only asks its question.
    #[test]
    fn an_edit_after_a_cut_off_save_is_kept_by_the_next_save() {
        for side in sides() {
            let mut ed = ed_side(Some(&body()), side);
            ed.typed("AB");
            let _ = cut_off_leaving_a_temp(&mut ed);
            // The ordinary writer is held on its way, so what is on disk when Save is pressed
            // is the generation from before the edit; it writes nothing after the Save starts.
            let gate = Arc::new(AtomicBool::new(true));
            let _open = Opens(gate.clone());
            ed.win.generation_gate = Some(gate.clone());
            ed.win.place(0x20, Pane::Hex);
            ed.typed("CD");
            ed.drv.until(&mut ed.win, 60, |w| w.writing.is_some());
            assert_eq!(
                kept_in(&ed.journals()[0]).0,
                ["0x0: AB was 00"],
                "{side:?}: on disk at the Save's start"
            );
            let open = gate.clone();
            ed.win.before_drain = Some(Box::new(move || open.store(false, Ordering::SeqCst)));
            ed.save();
            assert!(live_question(&ed).is_some(), "{side:?}: no question");
            let (line, _, failed) = ed.win.kept_line();
            assert!(
                !failed && line.starts_with("unsaved — kept at "),
                "{side:?}: {line}"
            );
            let dirs = ed.journals();
            let (edits, _) = kept_in(&dirs[0]);
            let was = bytes_at(&ed.remote, 0x20, 1)[0];
            assert_eq!(
                edits,
                [
                    "0x0: AB was 00".to_string(),
                    format!("0x20: CD was {was:02X}")
                ],
                "{side:?}: what is kept"
            );
            let _ = save_record(&ed);
        }
    }

    /// The window closed while its question is open — after Discard, or with nothing unsaved:
    /// the record stays (the edits do not), and the next open asks again, the temp untouched.
    #[test]
    fn closing_with_the_question_open_keeps_it_for_the_next_open() {
        for side in sides() {
            for discard in [true, false] {
                let tag = format!("{side:?} discard={discard}");
                let body = body();
                let mut ed = ed_side(Some(&body), side);
                ed.typed("AB");
                let (temp, grown) = cut_off_leaving_a_temp(&mut ed);
                ed.save();
                assert!(live_question(&ed).is_some(), "{tag}");
                if discard {
                    assert!(!ed.win.close_requested(), "{tag}");
                    ed.frames(2);
                    ed.drv.click_text(&mut ed.win, "Discard");
                    assert!(ed.win.discard && ed.win.close_ok, "{tag}");
                } else {
                    ed.key(Key::Z, command(false));
                    assert!(!ed.win.doc.unsaved(), "{tag}");
                    assert!(ed.win.close_requested(), "{tag}");
                }
                let win = std::mem::replace(
                    &mut ed.win,
                    window_on(ed.tmp.path(), &ed.remote, side, None, &ed.moved).1,
                );
                drop(win);
                ed.reopen();
                let rows = ed.rows();
                assert!(
                    rows.iter().any(|l| l.contains("sopen could not verify")),
                    "{tag}: {rows:?}"
                );
                assert!(
                    rows.iter().all(|l| !l.starts_with("unsaved edits from ")),
                    "{tag}: edits offered: {rows:?}"
                );
                assert_eq!(std::fs::read(&temp).unwrap(), grown, "{tag}: the temp");
                assert!(std::fs::read(&ed.remote).unwrap() == body, "{tag}");
                ed.press_row("could not verify", RowAction::Keep);
                ed.press_row("record is kept", RowAction::Discard);
                ed.reopen();
                assert!(ed.rows().is_empty(), "{tag}: {:?}", ed.rows());
                assert!(temp.exists(), "{tag}: kept");
            }
        }
    }

    /// 07 §3: a journal folder that holds nothing but its lock — or not even that: its session
    /// ended between the `mkdir` and its first generation — holds no edit. The next open removes
    /// it, shows no row and does not list again waiting for it.
    #[test]
    fn a_journal_folder_holding_nothing_is_removed_and_never_offered() {
        for locked in [false, true] {
            let body = body();
            let mut ed = ed(&body);
            let base = crate::cache::edits_path(&ed.tmp.path().join("cache"), "host", &ed.abs());
            let name = base.file_name().unwrap().to_string_lossy().into_owned();
            let dir = base.with_file_name(format!("{name}.journal-20261122T010203Z-0123abcd"));
            std::fs::create_dir_all(&dir).unwrap();
            if locked {
                std::fs::write(dir.join("lock"), b"").unwrap();
            }
            ed.reopen();
            assert!(ed.rows().is_empty(), "locked={locked}: {:?}", ed.rows());
            let left: Vec<_> = std::fs::read_dir(dir.parent().unwrap())
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect();
            assert!(left.is_empty(), "locked={locked}: left {left:?}");
            let retries = ed.win.file.lock().unwrap().retries.load(Ordering::SeqCst);
            assert_eq!(retries, 0, "locked={locked}: listed again for it");
        }
    }

    /// 07 §3, §3.1: a journal folder holding a Save's record and no generation is not one that
    /// holds nothing — the next open still asks about the interrupted save's file, and the
    /// folder stays.
    #[test]
    fn a_journal_holding_only_a_save_record_still_asks_about_the_temp() {
        let body = body();
        let mut ed = ed(&body);
        ed.typed("AB");
        let (temp, grown) = cut_off_leaving_a_temp(&mut ed);
        // The window ends as a kill ends it; then its generations go, its record stays.
        let next = window_on(ed.tmp.path(), &ed.remote, ed.side, None, &ed.moved).1;
        std::mem::replace(&mut ed.win, next).crash();
        let dirs = ed.journals();
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        for entry in std::fs::read_dir(&dirs[0]).unwrap().flatten() {
            let name = entry.file_name();
            if name != "lock" && name != "save" {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
        ed.reopen();
        let rows = ed.rows();
        assert!(
            rows.iter().any(|l| l.contains("sopen could not verify")),
            "{rows:?}"
        );
        assert!(dirs[0].join("save").exists(), "the record stays");
        assert_eq!(std::fs::read(&temp).unwrap(), grown, "the temp untouched");
    }

    /// Closing (Discard) while the answer Remove it is on its way — held as it removes the
    /// record, the journal in its hands: the window's end waits for it, so the discarded edits
    /// go with the journal and are never offered again.
    #[test]
    fn closing_while_an_answer_is_on_its_way_waits_for_it() {
        let body = body();
        let mut ed = ed_side(Some(&body), Side::Local);
        ed.typed("AB");
        let (temp, _) = cut_off_leaving_a_temp(&mut ed);
        ed.save();
        assert!(live_question(&ed).is_some());
        let hold = ed.moved.hold_record.clone();
        let _open = Opens(hold.clone());
        hold.store(true, Ordering::SeqCst);
        let open = hold.clone();
        ed.win.before_drain = Some(Box::new(move || open.store(false, Ordering::SeqCst)));
        ed.frames(2);
        ed.drv.click_text(&mut ed.win, "Remove it");
        let deadline = Instant::now() + Duration::from_secs(30);
        while temp.exists() {
            assert!(Instant::now() < deadline, "never removed");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(ed.win.answering.is_some(), "the answer is done already");
        assert!(!ed.win.close_requested());
        ed.frames(2);
        ed.drv.click_text(&mut ed.win, "Discard");
        assert!(ed.win.discard && ed.win.close_ok);
        let win = std::mem::replace(
            &mut ed.win,
            window_on(ed.tmp.path(), &ed.remote, Side::Local, None, &ed.moved).1,
        );
        drop(win);
        // Let go here too (a window's end that did not wait never let it go).
        hold.store(false, Ordering::SeqCst);
        ed.reopen();
        assert!(ed.rows().is_empty(), "{:?}", ed.rows());
        assert!(ed.journals().is_empty(), "{:?}", ed.journals());
        assert!(std::fs::read(&ed.remote).unwrap() == body);
    }

    /// A Save whose start cannot keep the edits (the local disk refuses the journal's write):
    /// nothing is sent, and the window does not say the edits are kept — even with an earlier
    /// generation in the journal, which does not hold the newest edit.
    #[test]
    fn a_save_that_could_not_keep_the_edits_does_not_say_kept() {
        let body = body();
        let mut ed = ed(&body);
        ed.typed("AB");
        ed.kept();
        ed.win.place(0x20, Pane::Hex);
        ed.typed("CD");
        ed.moved.disk_full.store(true, Ordering::SeqCst);
        ed.save();
        assert_eq!(ed.win.status.text, "save failed");
        let said = ed.win.notices.join("\n");
        assert!(said.contains("could not be kept in a journal"), "{said}");
        let (line, _, failed) = ed.win.kept_line();
        assert!(
            failed
                && line.starts_with("unsaved — NOT kept on disk")
                && line.contains("no space left on device"),
            "{line}"
        );
        assert_eq!(
            kept_in(&ed.journals()[0]).0,
            ["0x0: AB was 00"],
            "the journal"
        );
        assert!(std::fs::read(&ed.remote).unwrap() == body, "sent");
        ed.moved.disk_full.store(false, Ordering::SeqCst);
    }

    /// P9: Remove it removes only the file the question was about. Between the question and
    /// the answer another file takes the temp's name: in the live window and in an open's row,
    /// Remove it removes nothing — the other file stays, byte for byte, the record stays, the
    /// question stays — and says why. Keep it then ends the record and leaves that file.
    #[test]
    fn remove_it_removes_only_the_file_it_asked_about() {
        for side in sides() {
            for live in [true, false] {
                let tag = format!("{side:?} live={live}");
                let mut ed = ed_side(Some(&body()), side);
                ed.typed("AB");
                let temp = if live {
                    let (temp, _) = cut_off_leaving_a_temp(&mut ed);
                    ed.save();
                    assert!(live_question(&ed).is_some(), "{tag}");
                    temp
                } else {
                    kill_at(&mut ed, patch::Step::Copy(1));
                    ed.reopen();
                    let temps = ed.temps();
                    assert_eq!(temps.len(), 1, "{tag}: {temps:?}");
                    ed.remote.parent().unwrap().join(&temps[0])
                };
                // Another program's file under the temp's name, after the question was asked.
                std::fs::remove_file(&temp).unwrap();
                std::fs::write(&temp, b"another program's file").unwrap();
                if live {
                    ed.frames(2);
                    ed.drv.click_text(&mut ed.win, "Remove it");
                    ed.drv.until(&mut ed.win, 30, |w| w.answering.is_none());
                    assert!(live_question(&ed).is_some(), "{tag}: the question went");
                } else {
                    ed.press_row("could not verify", RowAction::Remove);
                    assert!(
                        ed.rows().iter().any(|l| l.ends_with("— remove it?")),
                        "{tag}: {:?}",
                        ed.rows()
                    );
                }
                assert_eq!(
                    std::fs::read(&temp).unwrap(),
                    b"another program's file",
                    "{tag}: removed"
                );
                let _ = save_record(&ed);
                let status = ed.win.status.text.clone();
                assert!(
                    status.contains("is not the file the question was about")
                        && status.ends_with("— not removed"),
                    "{tag}: {status}"
                );
                if live {
                    ed.frames(2);
                    ed.drv.click_text(&mut ed.win, "Keep it");
                    ed.drv.until(&mut ed.win, 30, |w| {
                        w.answering.is_none() && w.left.is_none()
                    });
                } else {
                    ed.press_row("could not verify", RowAction::Keep);
                }
                assert!(
                    !ed.journals()[0].join("save").exists(),
                    "{tag}: the record stays"
                );
                assert_eq!(
                    std::fs::read(&temp).unwrap(),
                    b"another program's file",
                    "{tag}"
                );
            }
        }
    }

    /// P9 in the live window, a temp the record proved the Save's from `staged` on: a Save whose
    /// rename was refused, and whose own temp could not be removed then nor at the next Save's
    /// start, leaves the question "could not be removed". A file put there since with every
    /// attribute equal but other bytes is not that temp: Remove it leaves it, the record and the
    /// question — the same check as an open's row.
    #[test]
    fn remove_it_in_the_window_checks_a_proven_temp_as_the_record_does() {
        for side in sides() {
            let mut ed = ed_side(Some(&body()), side);
            ed.typed("AB");
            ed.moved.fail_rename.store(true, Ordering::SeqCst);
            ed.moved.fail_remove.store(true, Ordering::SeqCst);
            ed.save();
            ed.moved.fail_rename.store(false, Ordering::SeqCst);
            let temps = ed.temps();
            assert_eq!(temps.len(), 1, "{side:?}: {temps:?}");
            let temp = ed.remote.parent().unwrap().join(&temps[0]);
            assert!(
                matches!(save_record(&ed).phase, journal::Phase::Renaming { .. }),
                "{side:?}"
            );
            // The next Save: its own temp, still not removable — asked.
            ed.save();
            let line = live_question(&ed);
            assert!(
                line.as_deref()
                    .is_some_and(|l| l.contains("could not be removed")),
                "{side:?}: {line:?}"
            );
            ed.moved.fail_remove.store(false, Ordering::SeqCst);
            let before = std::fs::metadata(&temp).unwrap();
            let mut bytes = std::fs::read(&temp).unwrap();
            bytes[0x10] ^= 0xFF;
            let mut f = std::fs::OpenOptions::new().write(true).open(&temp).unwrap();
            std::io::Write::write_all(&mut f, &bytes).unwrap();
            f.set_modified(before.modified().unwrap()).unwrap();
            drop(f);
            let after = std::fs::metadata(&temp).unwrap();
            assert_eq!(
                (after.len(), after.modified().unwrap(), after.permissions()),
                (
                    before.len(),
                    before.modified().unwrap(),
                    before.permissions()
                ),
                "{side:?}"
            );
            ed.frames(2);
            ed.drv.click_text(&mut ed.win, "Remove it");
            ed.drv.until(&mut ed.win, 30, |w| w.answering.is_none());
            assert_eq!(std::fs::read(&temp).unwrap(), bytes, "{side:?}: removed");
            let status = ed.win.status.text.clone();
            assert!(
                status.contains("is not the file the question was about")
                    && status.ends_with("— not removed"),
                "{side:?}: {status}"
            );
            assert!(live_question(&ed).is_some(), "{side:?}: the question went");
            let _ = save_record(&ed);
        }
    }

    /// P9, a temp the record proved the Save's (staged, its contents' evidence recorded) whose
    /// removal failed on open: before Remove it removes it, the record is reconciled again. A
    /// file put there since with every attribute equal — size, modification time, owner, mode —
    /// but other bytes is not that temp any more: it stays, and so do the record and the
    /// question.
    #[test]
    fn remove_it_checks_a_proven_temp_as_the_record_does() {
        for side in sides() {
            let mut ed = ed_side(Some(&body()), side);
            ed.typed("AB");
            kill_at(&mut ed, patch::Step::Rename);
            let temps = ed.temps();
            assert_eq!(temps.len(), 1, "{side:?}: {temps:?}");
            let temp = ed.remote.parent().unwrap().join(&temps[0]);
            ed.moved.fail_remove.store(true, Ordering::SeqCst);
            ed.reopen();
            ed.moved.fail_remove.store(false, Ordering::SeqCst);
            assert!(
                ed.rows()
                    .iter()
                    .any(|l| l.contains("could not be removed") && l.ends_with("— remove it?")),
                "{side:?}: {:?}",
                ed.rows()
            );
            // Other bytes, every attribute as it was.
            let before = std::fs::metadata(&temp).unwrap();
            let mut bytes = std::fs::read(&temp).unwrap();
            bytes[0x10] ^= 0xFF;
            let mut f = std::fs::OpenOptions::new().write(true).open(&temp).unwrap();
            std::io::Write::write_all(&mut f, &bytes).unwrap();
            f.set_modified(before.modified().unwrap()).unwrap();
            drop(f);
            let after = std::fs::metadata(&temp).unwrap();
            assert_eq!(
                (after.len(), after.modified().unwrap(), after.permissions()),
                (
                    before.len(),
                    before.modified().unwrap(),
                    before.permissions()
                ),
                "{side:?}"
            );
            ed.press_row("could not be removed", RowAction::Remove);
            assert_eq!(std::fs::read(&temp).unwrap(), bytes, "{side:?}: removed");
            let status = ed.win.status.text.clone();
            assert!(
                status.contains("is not the file the question was about")
                    && status.ends_with("— not removed"),
                "{side:?}: {status}"
            );
            assert!(
                ed.rows().iter().any(|l| l.ends_with("— remove it?")),
                "{side:?}"
            );
            let _ = save_record(&ed);
        }
    }

    /// `temp`'s path with its last character changed: the temp name of another Save (names are
    /// unique per Save).
    fn another_temp(temp: &str) -> String {
        let (head, last) = temp.split_at(temp.len() - 1);
        format!("{head}{}", if last == "0" { "1" } else { "0" })
    }

    /// P9, the record: Remove it acts only for the record that asked. That record ended some
    /// other way, or another Save's record (its own temp name) there instead — neither is a path
    /// of the window while the question is open, so they are made by hand: Remove it removes
    /// nothing, says why, and leaves whatever record is there — in the live window and in an
    /// open's row.
    #[test]
    fn remove_it_needs_the_record_that_asked() {
        for live in [true, false] {
            for another in [false, true] {
                let tag = format!("live={live} another={another}");
                let mut ed = ed(&body());
                ed.typed("AB");
                let (temp, other) = if live {
                    let (temp, _) = cut_off_leaving_a_temp(&mut ed);
                    ed.save();
                    assert!(live_question(&ed).is_some(), "{tag}");
                    let other = another_temp(temp.to_str().unwrap());
                    let snap = snapshot_of(&ed.win.doc, ed.win.baseline);
                    let mut slot = ed.win.journal.lock().unwrap();
                    let journal = slot.as_mut().unwrap();
                    journal.clear_save().unwrap();
                    if another {
                        // Another Save's record, cut off: its own temp.
                        journal
                            .begin_save(
                                &snap,
                                journal::Plan::Temp {
                                    route: journal::Route::ServerCopy,
                                    target: ed.remote.to_str().unwrap().to_string(),
                                    temp: other.clone(),
                                },
                            )
                            .unwrap();
                        journal.save_unknown();
                    }
                    drop(slot);
                    (temp, other)
                } else {
                    kill_at(&mut ed, patch::Step::Copy(1));
                    ed.reopen();
                    let temps = ed.temps();
                    let temp = ed.remote.parent().unwrap().join(&temps[0]);
                    let other = another_temp(temp.to_str().unwrap());
                    for row in &mut ed.win.rows {
                        if let Row::Edits { orphan, .. } = row {
                            orphan.save = match (&orphan.save, another) {
                                (journal::SaveFile::Intent(intent), true) => {
                                    let mut theirs = (**intent).clone();
                                    match &mut theirs.phase {
                                        journal::Phase::Naming { temp }
                                        | journal::Phase::Created { temp, .. }
                                        | journal::Phase::Staged { temp, .. }
                                        | journal::Phase::Renaming { temp, .. } => {
                                            *temp = other.clone()
                                        }
                                        journal::Phase::Writing { .. } => unreachable!(),
                                    }
                                    theirs.started += Duration::from_secs(1);
                                    journal::SaveFile::Intent(Box::new(theirs))
                                }
                                _ => journal::SaveFile::None,
                            };
                        }
                    }
                    (temp, other)
                };
                let held = std::fs::read(&temp).unwrap();
                if live {
                    ed.frames(2);
                    ed.drv.click_text(&mut ed.win, "Remove it");
                    ed.drv.until(&mut ed.win, 30, |w| w.answering.is_none());
                } else {
                    ed.press_row("could not verify", RowAction::Remove);
                }
                assert_eq!(std::fs::read(&temp).unwrap(), held, "{tag}: removed");
                let status = ed.win.status.text.clone();
                assert!(
                    status.starts_with("the interrupted save's record no longer names ")
                        && status.ends_with(" — not removed"),
                    "{tag}: {status}"
                );
                // The record there is left as it is.
                let there: Option<String> = if live {
                    ed.win
                        .journal
                        .lock()
                        .unwrap()
                        .as_ref()
                        .and_then(|j| j.pending_save())
                        .and_then(file::record_temp)
                        .map(str::to_string)
                } else {
                    ed.win.rows.iter().find_map(|row| match row {
                        Row::Edits { orphan, .. } => match &orphan.save {
                            journal::SaveFile::Intent(intent) => {
                                file::record_temp(intent).map(str::to_string)
                            }
                            _ => None,
                        },
                        _ => None,
                    })
                };
                assert_eq!(there, another.then_some(other), "{tag}: the record");
            }
        }
    }

    /// A temp provably the cut-off Save's whose removal fails at the next Save's start: that
    /// Save asks the question (Remove it tries again, Keep it ends the record) instead of
    /// failing every time; once the removal works, Remove it removes it and the Save lands.
    #[test]
    fn a_temp_the_next_save_cannot_remove_is_asked_about() {
        for side in sides() {
            let body = body();
            let mut ed = ed_side(Some(&body), side);
            ed.typed("AB");
            // Stopped (Esc) right before its first copy request; its own temp then cannot be
            // removed: the record stays, the temp as created.
            ed.moved.fail_remove.store(true, Ordering::SeqCst);
            let saving = ed.win.saving.clone();
            ed.hook(move |s| {
                if s == patch::Step::Copy(0) {
                    saving.cancel.store(true, Ordering::SeqCst);
                }
                patch::At::Go
            });
            ed.save();
            ed.win.file.lock().unwrap().step_hook = None;
            let temps = ed.temps();
            assert_eq!(temps.len(), 1, "{side:?}: {temps:?}");
            let temp = ed.remote.parent().unwrap().join(&temps[0]);
            assert_eq!(std::fs::read(&temp).unwrap(), b"", "{side:?}: as created");
            let _ = save_record(&ed);
            // The next Save, the removal still failing: asked, not failed.
            ed.save();
            let line = live_question(&ed);
            assert!(
                line.as_deref().is_some_and(
                    |l| l.contains("could not be removed") && l.ends_with("— remove it?")
                ),
                "{side:?}: {line:?} / {}",
                ed.win.status.text
            );
            ed.frames(2);
            ed.drv.click_text(&mut ed.win, "Remove it");
            ed.drv.until(&mut ed.win, 30, |w| w.answering.is_none());
            assert!(
                live_question(&ed).is_some(),
                "{side:?}: a failed removal ended it"
            );
            assert!(temp.exists());
            ed.moved.fail_remove.store(false, Ordering::SeqCst);
            ed.frames(2);
            ed.drv.click_text(&mut ed.win, "Remove it");
            ed.drv.until(&mut ed.win, 30, |w| {
                w.answering.is_none() && w.left.is_none()
            });
            assert!(!temp.exists(), "{side:?}");
            ed.save();
            assert!(
                ed.win.status.text.starts_with("saved "),
                "{side:?}: {}",
                ed.win.status.text
            );
            assert_eq!(bytes_at(&ed.remote, 0, 1), [0xAB], "{side:?}");
        }
    }

    /// A temp provably the cut-off Save's whose removal fails on reopen: the record stays and
    /// the question is asked; the next open, the removal failing again, still asks; once it
    /// can be removed the next open removes it and asks nothing more.
    #[test]
    fn a_temp_that_could_not_be_removed_is_offered_again() {
        for side in sides() {
            let body = body();
            let mut ed = ed_side(Some(&body), side);
            ed.typed("AB");
            kill_at(&mut ed, patch::Step::Copy(0));
            let temps = ed.temps();
            assert_eq!(temps.len(), 1, "{side:?}: {temps:?}");
            let temp = ed.remote.parent().unwrap().join(&temps[0]);
            ed.moved.fail_remove.store(true, Ordering::SeqCst);
            for open in 0..2 {
                ed.reopen();
                let rows = ed.rows();
                assert!(
                    rows.iter()
                        .any(|l| l.contains("could not be removed") && l.ends_with("— remove it?")),
                    "{side:?} open {open}: {rows:?}"
                );
                assert!(temp.exists(), "{side:?}");
                let _ = save_record(&ed);
            }
            ed.moved.fail_remove.store(false, Ordering::SeqCst);
            ed.reopen();
            assert!(!temp.exists(), "{side:?}: provably its own, removed now");
            let rows = ed.rows();
            assert!(
                rows.iter().all(|l| !l.ends_with("— remove it?")),
                "{side:?}: {rows:?}"
            );
            ed.press_row("unsaved edits", RowAction::Restore);
            ed.ready();
            assert_eq!(ed.win.doc.byte(0), Some(0xAB), "{side:?}");
        }
    }

    /// Restore pressed while the window has edits of its own is refused — and the row stays,
    /// its journal untouched; undone, Restore goes ahead.
    #[test]
    fn a_refused_restore_keeps_its_row() {
        let mut ed = ed(&body());
        ed.typed("AB");
        ed.kept();
        let dir = ed.journals().pop().unwrap();
        ed.reopen();
        let rows = ed.rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        ed.win.place(0x10, Pane::Hex);
        ed.typed("CD");
        ed.press_row("unsaved edits", RowAction::Restore);
        assert_eq!(ed.rows(), rows, "the row went");
        assert_eq!(
            ed.win.status.text,
            "not restored: this window has edits of its own — undo or save them first"
        );
        assert_eq!(kept_in(&dir).0, ["0x0: AB was 00"], "the journal changed");
        ed.key(Key::Z, command(false));
        ed.drv.until(&mut ed.win, 60, |w| w.writing.is_none());
        ed.press_row("unsaved edits", RowAction::Restore);
        ed.ready();
        assert_eq!(ed.win.doc.byte(0), Some(0xAB));
        assert_eq!(
            ed.win.doc.byte(0x10),
            Some(bytes_at(&ed.remote, 0x10, 1)[0]),
            "its own edit undone"
        );
    }

    /// 07 E18, killed mid-copy: the temp grew, so nothing proves it is sopen's. It is kept and
    /// named, and its removal is offered; the owner's answer removes it. Another file put under
    /// the temp's name after the kill is kept too, its bytes untouched.
    #[test]
    fn e18_a_temp_cut_off_mid_copy_is_kept_and_offered_for_removal() {
        for side in sides() {
            for other in [false, true] {
                let body = body();
                let mut ed = ed_side(Some(&body), side);
                ed.typed("AB");
                kill_at(&mut ed, patch::Step::Copy(1));
                let temps = ed.temps();
                assert_eq!(temps.len(), 1, "{temps:?}");
                let temp = ed.remote.parent().unwrap().join(&temps[0]);
                if other {
                    // After the kill, not sopen's: smaller than the target, other bytes, another
                    // mtime.
                    std::fs::remove_file(&temp).unwrap();
                    std::fs::write(&temp, b"another program's file").unwrap();
                }
                ed.reopen();
                let rows = ed.rows();
                let offer = rows
                    .iter()
                    .find(|l| l.ends_with("— remove it?"))
                    .unwrap_or_else(|| panic!("{other}: {rows:?}"));
                assert!(
                    offer.contains("sopen could not verify it is its own partial copy"),
                    "{offer}"
                );
                assert!(temp.exists(), "{other}: removed unasked");
                // Restore waits for the answer.
                ed.press_row("could not verify", RowAction::Restore);
                assert!(ed.win.doc.patches().is_empty(), "{other}: restored first");
                if other {
                    assert_eq!(std::fs::read(&temp).unwrap(), b"another program's file");
                } else {
                    ed.press_row("could not verify", RowAction::Remove);
                    assert!(!temp.exists(), "the owner's answer removes it");
                    // Answered: the edits can be restored now, and the next open asks nothing.
                    ed.press_row("unsaved edits", RowAction::Restore);
                    ed.ready();
                    assert_eq!(ed.win.doc.byte(0), Some(0xAB));
                    ed.kept();
                    ed.reopen();
                    assert!(
                        ed.rows().iter().all(|l| !l.ends_with("— remove it?")),
                        "{:?}",
                        ed.rows()
                    );
                }
                assert!(std::fs::read(&ed.remote).unwrap() == body);
            }
        }
    }

    /// 07 E18, the rename landed and its reply was lost: on reopen the Save is found landed, the
    /// journal retired, nothing offered twice.
    #[test]
    fn e18_a_landed_save_whose_reply_was_lost_is_found_on_reopen() {
        for side in sides() {
            let mut ed = ed_side(Some(&body()), side);
            ed.typed("AB");
            ed.hook(|s| {
                if s == patch::Step::Rename {
                    patch::At::LoseReply
                } else {
                    patch::At::Go
                }
            });
            ed.save();
            assert_eq!(ed.win.status.text, "save cut off: outcome unknown");
            assert_eq!(std::fs::read(&ed.remote).unwrap()[0], 0xAB, "it landed");
            ed.reopen();
            let rows = ed.rows();
            assert_eq!(rows.len(), 2, "{rows:?}");
            assert!(
                rows[0].starts_with("the save at ")
                    && rows[0].ends_with(" landed (found on reopen)"),
                "{rows:?}"
            );
            assert!(
                rows[1].starts_with("bytes replaced by the save at "),
                "{rows:?}"
            );
            assert!(ed.journals().is_empty(), "retired: {:?}", ed.journals());
            assert!(!ed.win.doc.dirty());
            // Nothing offered twice.
            ed.reopen();
            assert!(
                ed.rows().iter().all(|l| !l.contains("landed")),
                "{:?}",
                ed.rows()
            );
        }
    }

    /// 07 §3.1 in a live session: a Save whose rename got no reply is reconciled by the next
    /// Save before anything is sent — found landed, it is not written again (no second copy),
    /// and the window takes the saved bytes as the file's.
    #[test]
    fn e18_the_next_save_reconciles_a_cut_off_one_and_does_not_repeat_it() {
        for side in sides() {
            let mut ed = ed_side(Some(&body()), side);
            ed.typed("AB");
            ed.hook(|s| {
                if s == patch::Step::Rename {
                    patch::At::LoseReply
                } else {
                    patch::At::Go
                }
            });
            ed.save();
            assert_eq!(ed.win.status.text, "save cut off: outcome unknown");
            let copied = ed.moved.now().2;
            assert!(ed.win.doc.unsaved(), "not known to be saved yet");
            ed.win.file.lock().unwrap().step_hook = None;
            ed.save();
            assert!(
                ed.win
                    .status
                    .text
                    .ends_with("landed (found now): nothing was written again"),
                "{}",
                ed.win.status.text
            );
            assert_eq!(ed.moved.now().2, copied, "copied again");
            ed.ready();
            assert!(!ed.win.doc.dirty(), "the saved byte is the file's");
            assert!(ed.temps().is_empty());
        }
    }

    /// 07 E18 / §10, on both servers: while a Save runs, each way of editing is tried on its
    /// own — typing, undo, redo (with a step to redo), typing after Go to — and each changes
    /// nothing and makes the status row say `saving — editing resumes when the save is done`.
    /// A paste is no edit of the hex window at any time: it changes nothing and says nothing.
    /// A kill during that Save leaves one generation, the one it records, and the reopen offers
    /// it.
    #[test]
    fn e18_no_edit_while_a_save_runs_and_a_kill_then_reconciles_its_generation_alone() {
        for side in sides() {
            let body = body();
            let mut ed = ed_side(Some(&body), side);
            ed.typed("AB");
            ed.typed("CD");
            ed.key(Key::Z, command(false));
            assert!(ed.win.doc.can_redo(), "{side:?}: a step to redo");
            let (at_copy, go) = (
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            );
            let (reached, release) = (at_copy.clone(), go.clone());
            ed.hook(move |step| {
                if step == patch::Step::Copy(1) {
                    reached.store(true, Ordering::SeqCst);
                    while !release.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    return patch::At::Stop;
                }
                patch::At::Go
            });
            ed.key(Key::S, command(false));
            ed.drv
                .until(&mut ed.win, 30, |_| at_copy.load(Ordering::SeqCst));
            let patches = ed.win.doc.patches().clone();
            type Input<'a> = (&'a str, &'a dyn Fn(&mut Ed));
            let tries: [Input; 4] = [
                ("typing", &|ed| ed.typed("EF")),
                ("undo", &|ed| ed.key(Key::Z, command(false))),
                ("redo", &|ed| ed.key(Key::Z, command(true))),
                ("typing after Go to", &|ed| {
                    ed.key(Key::G, command(false));
                    ed.typed("0x30");
                    ed.key(Key::Enter, Modifiers::NONE);
                    ed.frames(1);
                    assert_eq!(ed.win.cursor.at, 0x30, "Go to moves while a save runs");
                    ed.typed("77");
                }),
            ];
            for (what, input) in tries {
                ed.win.edit_blocked = false;
                assert_ne!(ed.status(), EDITING_RESUMES, "{side:?} {what}: before");
                input(&mut ed);
                assert_eq!(*ed.win.doc.patches(), patches, "{side:?}: {what} edited");
                assert_eq!(ed.status(), EDITING_RESUMES, "{side:?}: {what}");
                assert!(ed.win.job.is_some(), "{side:?}: the save is held");
            }
            assert!(ed.win.doc.can_redo(), "{side:?}: the redo step stays");
            ed.win.edit_blocked = false;
            let before = ed.status();
            ed.drv.frame(
                &mut ed.win,
                vec![Event::Paste("EEEE".into())],
                Modifiers::NONE,
            );
            assert_eq!(*ed.win.doc.patches(), patches, "{side:?}: paste edited");
            assert_eq!(ed.status(), before, "{side:?}: a paste says nothing");
            go.store(true, Ordering::SeqCst);
            ed.drv.until(&mut ed.win, 30, |w| w.job.is_none());
            let dir = ed.journals().pop().unwrap();
            let gens: Vec<String> = entries(&dir)
                .into_iter()
                .filter(|n| n.starts_with("gen-"))
                .collect();
            assert_eq!(gens.len(), 1, "{side:?}: {gens:?}");
            ed.reopen();
            let rows = ed.rows();
            assert!(
                rows.iter().any(|l| l.contains("(1 byte changed)")),
                "{side:?}: {rows:?}"
            );
            assert!(std::fs::read(&ed.remote).unwrap() == body, "{side:?}");
        }
    }

    /// 07 E18: a file already at the temp's name makes the exclusive create fail: the Save fails
    /// and that file is never removed or changed.
    #[test]
    fn e18_an_exclusive_create_collision_fails_the_save_and_leaves_that_file() {
        for side in sides() {
            let body = body();
            let mut ed = ed_side(Some(&body), side);
            ed.typed("AB");
            let journal = ed.tmp.path().join("cache/.edits");
            let folder = ed.remote.parent().unwrap().to_path_buf();
            ed.hook(move |s| {
                if s == patch::Step::Create {
                    // The temp's name, from the record, taken by someone else's file.
                    let save = walk(&journal)
                        .into_iter()
                        .find(|p| p.file_name().is_some_and(|n| n == "save"))
                        .unwrap();
                    let intent = crate::journal::parse_save(&std::fs::read(save).unwrap()).unwrap();
                    let crate::journal::Phase::Naming { temp } = intent.phase else {
                        panic!("not naming")
                    };
                    std::fs::write(&temp, b"someone else's").unwrap();
                    assert!(Path::new(&temp).starts_with(&folder));
                }
                patch::At::Go
            });
            ed.save();
            assert_eq!(ed.win.status.text, "save failed");
            let temps = ed.temps();
            assert_eq!(temps.len(), 1);
            let left = ed.remote.parent().unwrap().join(&temps[0]);
            assert_eq!(std::fs::read(left).unwrap(), b"someone else's");
            assert!(std::fs::read(&ed.remote).unwrap() == body);
            assert!(ed.win.doc.unsaved());
        }
    }

    /// 07 E18: the recorded temp replaced by another file of the same size before the reopen is
    /// kept and named; the target changed by another writer after a landed rename matches
    /// neither evidence: unknown, both paths named, nothing removed.
    #[test]
    fn e18_a_replaced_temp_and_a_target_matching_neither_are_left_and_named() {
        for side in sides() {
            // The temp replaced before the reopen (stopped right before the rename: staged, whole).
            let body = body();
            let mut ed = ed_side(Some(&body), side);
            ed.typed("AB");
            ed.hook(|s| {
                if s == patch::Step::Rename {
                    patch::At::Stop
                } else {
                    patch::At::Go
                }
            });
            ed.save();
            let temps = ed.temps();
            assert_eq!(temps.len(), 1);
            let temp = ed.remote.parent().unwrap().join(&temps[0]);
            let same: Vec<u8> = body.iter().map(|b| b ^ 0x5A).collect();
            std::fs::remove_file(&temp).unwrap();
            std::fs::write(&temp, &same).unwrap();
            ed.reopen();
            let rows = ed.rows();
            assert!(
                rows.iter()
                    .any(|l| l.contains("it is not the file sopen staged")),
                "{rows:?}"
            );
            assert_eq!(std::fs::read(&temp).unwrap(), same, "kept");
            // A landed rename, then another writer: neither evidence holds.
            let mut ed = ed_side(Some(&body), side);
            ed.typed("AB");
            ed.hook(|s| {
                if s == patch::Step::Rename {
                    patch::At::LoseReply
                } else {
                    patch::At::Go
                }
            });
            ed.save();
            write_back(&ed.remote, 0, b"Q");
            ed.reopen();
            let rows = ed.rows();
            let cut = rows
                .iter()
                .find(|l| l.contains("a save was cut off"))
                .unwrap_or_else(|| panic!("{rows:?}"));
            assert!(cut.contains(&format!("check {}", ed.shown())), "{cut}");
            assert_eq!(
                std::fs::read(&ed.remote).unwrap()[0],
                b'Q',
                "nothing written"
            );
        }
    }

    /// 07 E8/E18 at the window: without `copy-data` a file above 256 MiB is refused with the
    /// cost, and its row offers Write in place… and Save As…. Write in place needs the dialog's
    /// button; stopped after the first of two runs (both in one page), the next open says the
    /// file holds some of the changes, names the undo file, and offers Load as edits and
    /// Restore.
    #[test]
    fn e18_write_in_place_cut_off_after_one_run_holds_some_of_the_changes() {
        for side in sides() {
            let mut ed = ed_side(None, side);
            ed.moved.no_copy.store(true, Ordering::SeqCst);
            ed.goto(E1_AT);
            ed.typed("DE");
            ed.goto(E1_AT + 8);
            ed.typed("AD");
            ed.save();
            let offer = ed
                .win
                .offer
                .clone()
                .expect("a refusal that offers other ways");
            assert!(offer.contains("this server has no copy-data"), "{offer}");
            assert!(ed.drv.find_text("Write in place…").is_some());
            assert_eq!(ed.moved.now(), (0, 0, 0), "refused before any transfer");
            ed.drv.click_text(&mut ed.win, "Write in place…");
            assert!(ed
                .win
                .in_place_ask
                .as_deref()
                .is_some_and(|q| q.contains("This is not atomic")));
            ed.hook(|s| {
                if s == patch::Step::Write(1) {
                    patch::At::Stop
                } else {
                    patch::At::Go
                }
            });
            ed.drv.click_text(&mut ed.win, "Write in place");
            ed.drv.until(&mut ed.win, 60, |w| w.job.is_none());
            assert_eq!(bytes_at(&ed.remote, E1_AT, 1), [0xDE], "the first run");
            ed.reopen();
            let rows = ed.rows();
            let part = rows
                .iter()
                .find(|l| l.contains("holds some of your changes; the original bytes are at "))
                .unwrap_or_else(|| panic!("{rows:?}"));
            assert!(part.contains(".undo.txt"), "{part}");
            assert!(
                rows.iter()
                    .any(|l| l.starts_with("bytes replaced by the save at ")),
                "{rows:?}"
            );
            assert!(ed.drv.find_text("Load as edits").is_some());
        }
    }

    // ---- Save copy, a reload, and the windowless hooks over a stream ----

    /// 07 §6: Save copy of a streamed file is made as a Save makes its temp — copied on the
    /// server, patched — and published under a new sibling's name, never over anything; the
    /// original is untouched, nothing is fetched whole, and the edits count as put somewhere.
    #[test]
    fn save_copy_of_a_streamed_file_writes_a_sibling_by_the_server_copy() {
        let body = body();
        let mut ed = ed(&body);
        ed.typed("AB");
        ed.key(Key::S, command(true));
        ed.drv.until(&mut ed.win, 60, |w| w.job.is_none());
        ed.frames(2);
        let copy = ed.remote.with_file_name("model.edited.bin");
        let mut want = body.clone();
        want[0] = 0xAB;
        assert!(std::fs::read(&copy).unwrap() == want, "the copy");
        assert!(std::fs::read(&ed.remote).unwrap() == body, "the original");
        assert_eq!(ed.moved.now().2, body.len() as u64, "copied on the server");
        assert_eq!(ed.moved.now().1, 1, "the one byte written");
        assert_eq!(
            ed.win.status.text,
            format!("saved a copy at host:{}", copy.display())
        );
        assert!(ed.temps().is_empty());
        assert!(ed.win.doc.dirty() && !ed.win.doc.unsaved());
    }

    /// After Cmd/Ctrl+R the file read is another version: its edits are against that version's
    /// baseline, and its Save keeps the bytes it replaced in an undo file of its own.
    #[test]
    fn a_save_after_a_reload_keeps_its_bytes_in_an_undo_file_of_its_own() {
        let body = body();
        let mut ed = ed(&body);
        ed.typed("AB");
        ed.save();
        assert_eq!(ed.undo_files().len(), 1);
        // Someone else rewrites the file; the owner reloads.
        let other: Vec<u8> = body.iter().map(|b| b ^ 0x55).collect();
        std::fs::write(&ed.remote, &other).unwrap();
        ed.key(Key::R, command(false));
        ed.drv
            .until(&mut ed.win, 60, |w| w.reloading.is_none() && w.ready());
        assert_eq!(ed.win.status.text, "reloaded");
        assert_eq!(ed.win.doc.byte(1), Some(other[1]));
        ed.typed("CD");
        ed.save();
        assert!(
            ed.win.status.text.starts_with("saved "),
            "{}",
            ed.win.status.text
        );
        let mut want = other.clone();
        want[1] = 0xCD;
        assert!(std::fs::read(&ed.remote).unwrap() == want);
        let undos = ed.undo_files();
        assert_eq!(undos.len(), 2, "{undos:?}");
        let texts: Vec<String> = undos
            .iter()
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect();
        assert!(
            texts
                .iter()
                .any(|t| t.ends_with(&format!("0x1: {:02X}\n", other[1]))),
            "{texts:?}"
        );
    }

    /// `SOPEN_AUTOSAVE` over a stream, no window: the state's patch reads its page through the
    /// stream, the baseline pass (at most 256 MiB) reads the file once through it — a Bulk
    /// read, not the `Origin` — and the Save is 07 §4.2's: copied on the server, the copy read
    /// back for the content check, the patch written; no journal is left, the undo file is.
    #[test]
    fn autosave_over_a_stream_patches_and_saves() {
        let tmp = tempfile::tempdir().unwrap();
        let body = body();
        let remote = remote_file(tmp.path(), Some(&body));
        let abs = remote.to_str().unwrap().to_string();
        let transport = Box::new(LocalFs::new()).into_ranged(&abs).unwrap();
        let repaint = Repaint::default();
        let source = Source::open(
            transport,
            Box::new(|| Err(anyhow!("no second connection"))),
            repaint.waker(),
        )
        .unwrap();
        let moved = Moved::default();
        let mut fs = Moving {
            inner: Box::new(LocalFs::new()),
            moved: moved.clone(),
        };
        let at = fs.stat(&abs).unwrap();
        let origin = Origin::new(Box::new(fs), None, Some("host".into()), abs, at);
        let file = HexFile::streamed(origin, source.clone(), repaint, &tmp.path().join("cache"));
        let hooks = Hooks {
            state: State::parse("patch=0x20000:BEEF").unwrap(),
            autosave: true,
            ..Hooks::default()
        };
        let outcome = open(file, &hooks, Vec::new()).unwrap();
        assert!(outcome.saved && outcome.refused.is_none());
        let mut want = body.clone();
        (want[0x20000], want[0x20001]) = (0xBE, 0xEF);
        assert!(std::fs::read(&remote).unwrap() == want);
        assert_eq!(
            source.stats().delivered,
            body.len() as u64,
            "the patch's page, within the baseline pass's one read"
        );
        // The copy read back once, for the content check (at most 256 MiB).
        let (read, written, copied) = moved.now();
        assert_eq!(
            (read, written, copied),
            (body.len() as u64, 2, body.len() as u64)
        );
        let journals: Vec<PathBuf> = walk(&tmp.path().join("cache/.edits"));
        assert!(journals.is_empty(), "{journals:?}");
        let undos = walk(&tmp.path().join("cache/.backup"));
        assert_eq!(undos.len(), 1, "{undos:?}");
    }

    /// `SOPEN_AUTOSAVE` with `SOPEN_SAVE_AS=<folder>`: the edits saved as a new local file
    /// without a window (07 §7), the remote untouched.
    #[test]
    fn autosave_as_writes_a_new_local_file() {
        let tmp = tempfile::tempdir().unwrap();
        let body = body();
        let remote = remote_file(tmp.path(), Some(&body));
        let out = tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let moved = Moved::default();
        let (source, win) = window_on(tmp.path(), &remote, Side::Local, None, &moved);
        drop(win);
        let abs = remote.to_str().unwrap().to_string();
        let mut fs = Moving {
            inner: Box::new(LocalFs::new()),
            moved: moved.clone(),
        };
        let at = fs.stat(&abs).unwrap();
        let origin = Origin::new(Box::new(fs), None, Some("host".into()), abs, at);
        let file = HexFile::streamed(
            origin,
            source,
            Repaint::default(),
            &tmp.path().join("cache"),
        );
        let mut doc = HexDoc::of(&file);
        let deadline = Instant::now() + Duration::from_secs(20);
        while doc.byte(0).is_none() {
            assert!(Instant::now() < deadline);
            doc.fetch(0).unwrap();
        }
        doc.set(0, 0xAB, Cursor::default());
        let outcome = autosave_as(file, &mut doc, &out).unwrap();
        assert!(outcome.saved, "{:?}", outcome.refused);
        let mut want = body.clone();
        want[0] = 0xAB;
        assert!(std::fs::read(out.join("model.edited.bin")).unwrap() == want);
        assert!(std::fs::read(&remote).unwrap() == body, "the remote");
        assert_eq!(moved.now().1, 0, "nothing written to the remote");
    }

    // ---- Restore while its bytes land, the baseline pass, Esc, scripted runs ----

    /// A baseline pass that fails (the file changed since it was opened) is not started again
    /// by the frames; a Save that needs it asks once more, and is refused.
    #[test]
    fn a_failed_baseline_pass_is_not_started_again_by_the_frames() {
        let mut ed = ed(&body());
        set_mtime(&ed.remote, OLD + 60);
        ed.typed("AB");
        let source = ed.source.clone();
        ed.drv
            .until(&mut ed.win, 20, |w| w.checking.ended(&source).is_some());
        assert!(matches!(ed.win.checking.ended(&source), Some(Err(_))));
        ed.frames(40);
        assert_eq!(
            ed.win.checking.runs.load(Ordering::SeqCst),
            1,
            "started again"
        );
        ed.save();
        assert_eq!(ed.win.status.text, "save failed");
        assert_eq!(
            ed.win.checking.runs.load(Ordering::SeqCst),
            2,
            "the Save's own try"
        );
        assert!(ed.temps().is_empty() && ed.win.doc.unsaved());
        let said = ed.win.notices.join("\n");
        assert!(said.contains("your edits are kept at "), "{said}");
    }

    /// 07 E10b at the window: an edited page changed under the kept edits, with the mtime put
    /// back, gets no Restore — the row says why, nothing is applied whatever asks, and the
    /// journal stays. A journal with no evidence (`seen: pending`) asks first: Restore anyway.
    #[test]
    fn e10b_restore_is_held_to_its_checks() {
        let body = body();
        let mut ed = ed(&body);
        ed.goto(0x2005);
        ed.typed("AB");
        ed.kept();
        ed.reopen();
        write_back(&ed.remote, 0x2010, b"!");
        ed.reopen();
        let rows = ed.rows();
        assert!(
            rows[0].contains(
                "— not restored: the bytes under your edits at 0x2000 changed since they were made"
            ),
            "{rows:?}"
        );
        assert!(ed.drv.find_text("Restore").is_none(), "no Restore button");
        assert!(ed.drv.find_text("Discard").is_some());
        ed.win.row_action(0, RowAction::Restore);
        ed.frames(3);
        assert_eq!(ed.win.doc.changed_count(), 0, "applied");
        assert!(ed.win.journal_dir.is_none());
        assert_eq!(ed.journals().len(), 1, "the journal stays");
        // `seen: pending`: Restore anyway.
        let tmp = tempfile::tempdir().unwrap();
        let remote = remote_file(tmp.path(), Some(&body));
        let abs = remote.to_str().unwrap().to_string();
        let cache = tmp.path().join("cache");
        let at = LocalFs::new().stat(&abs).unwrap();
        let mut journal = crate::journal::Journal::create(
            crate::journal::disk(),
            &cache,
            "host",
            &abs,
            crate::journal::Kind::Hex,
        )
        .unwrap();
        journal
            .publish(&crate::journal::Snapshot {
                baseline: at.fingerprint(),
                seen: crate::journal::Seen::Pending,
                edits: "0x3: CC was 03\n".to_string(),
                pastes: Vec::new(),
            })
            .unwrap();
        drop(journal);
        let moved = Moved::default();
        let (source, win) = window_on(tmp.path(), &remote, Side::Local, None, &moved);
        let mut ed = Ed {
            win,
            drv: Driver::new(760.0, 560.0),
            source,
            moved,
            remote,
            side: Side::Local,
            tmp,
        };
        ed.ready();
        ed.frames(3);
        let rows = ed.rows();
        assert!(rows[0].ends_with(crate::journal::ASK_PENDING), "{rows:?}");
        assert!(ed.drv.find_text("Restore anyway").is_some());
        ed.drv.click_text(&mut ed.win, "Restore anyway");
        ed.ready();
        assert_eq!(ed.win.doc.byte(3), Some(0xCC));
    }

    /// 07 §4.2 step 5: Esc between copy requests stops a Save — its temp removed, the target
    /// untouched, the edits kept in the window and in the journal.
    #[test]
    fn esc_stops_a_streamed_save_between_its_copy_requests() {
        let body = body();
        let mut ed = ed(&body);
        ed.typed("AB");
        let saving = ed.win.saving.clone();
        let (at_copy, waited) = (Arc::new(AtomicBool::new(false)), saving.clone());
        let reached = at_copy.clone();
        ed.hook(move |step| {
            if step == patch::Step::Copy(1) {
                reached.store(true, Ordering::SeqCst);
                while !waited.cancel.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            patch::At::Go
        });
        ed.key(Key::S, command(false));
        ed.drv
            .until(&mut ed.win, 30, |_| at_copy.load(Ordering::SeqCst));
        ed.key(Key::Escape, Modifiers::NONE);
        ed.drv.until(&mut ed.win, 30, |w| w.job.is_none());
        ed.frames(2);
        assert_eq!(ed.win.status.text, "stopped: nothing was written");
        assert!(ed.temps().is_empty(), "{:?}", ed.temps());
        assert!(std::fs::read(&ed.remote).unwrap() == body);
        assert!(ed.win.doc.unsaved());
        assert_eq!(ed.journals().len(), 1);
    }

    /// A scripted run (`SOPEN_SCREENSHOT`) keeps nothing of its state's edits when it ends.
    #[test]
    fn a_scripted_run_keeps_no_journal() {
        let mut ed = ed(&body());
        ed.win.scripted = true;
        ed.typed("AB");
        ed.kept();
        assert_eq!(ed.journals().len(), 1);
        let win = std::mem::replace(
            &mut ed.win,
            window_on(ed.tmp.path(), &ed.remote, Side::Local, None, &ed.moved).1,
        );
        drop(win);
        assert!(ed.journals().is_empty(), "{:?}", ed.journals());
    }

    /// Restore over a stream: its bytes land as their pages arrive. Until all have, the journal
    /// it took over is left as it is — not rewritten with a part of them, not removed by a close
    /// that finds nothing unsaved yet — so the next open offers the same edits again.
    #[test]
    fn a_restore_still_landing_keeps_the_journal_whole() {
        let mut ed = ed(&body());
        ed.goto(0x1000);
        ed.typed("AB");
        ed.goto(0x30000);
        ed.typed("CD");
        ed.kept();
        ed.reopen();
        let row = ed.rows()[0].clone();
        assert!(row.contains("(2 bytes changed)"), "{row}");
        // Restore, and the window goes before a frame has landed anything.
        ed.win.row_action(0, RowAction::Restore);
        assert!(
            !ed.win.close_requested(),
            "closing asks while edits are landing"
        );
        let win = std::mem::replace(
            &mut ed.win,
            window_on(ed.tmp.path(), &ed.remote, Side::Local, None, &ed.moved).1,
        );
        drop(win);
        ed.reopen();
        let rows = ed.rows();
        assert!(
            rows.iter().any(|l| l.contains("(2 bytes changed)")),
            "the restored edits are gone: {rows:?}"
        );
    }

    /// 07 §3: losing the keyboard focus writes the edits at once — kept before the 250 ms a
    /// change otherwise waits.
    #[test]
    fn losing_the_focus_keeps_the_edits_at_once() {
        let mut ed = ed(&body());
        ed.typed("AB");
        assert_eq!(ed.win.schedule.status(), Keep::Keeping);
        ed.drv.focused = false;
        ed.frames(1);
        assert_eq!(
            ed.win.schedule.status(),
            Keep::Kept,
            "written at the focus loss"
        );
        assert_eq!(ed.journals().len(), 1);
    }

    /// A refused `SOPEN_AUTOSAVE` is a scripted run's like a screenshot's: nothing of its
    /// state's edits is kept for the next open.
    #[test]
    fn a_refused_autosave_keeps_no_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let body = body();
        let remote = remote_file(tmp.path(), Some(&body));
        let abs = remote.to_str().unwrap().to_string();
        let transport = Box::new(LocalFs::new()).into_ranged(&abs).unwrap();
        let repaint = Repaint::default();
        let source = Source::open(
            transport,
            Box::new(|| Err(anyhow!("no second connection"))),
            repaint.waker(),
        )
        .unwrap();
        let at = LocalFs::new().stat(&abs).unwrap();
        let origin = Origin::new(Box::new(LocalFs::new()), None, Some("host".into()), abs, at);
        let file = HexFile::streamed(origin, source, repaint, &tmp.path().join("cache"));
        // Another writer before the Save: its size moved.
        std::fs::write(&remote, [&body[..], b"more"].concat()).unwrap();
        let hooks = Hooks {
            state: State::parse("patch=0x10:AB").unwrap(),
            autosave: true,
            ..Hooks::default()
        };
        let outcome = open(file, &hooks, Vec::new());
        let failed = match outcome {
            Ok(outcome) => outcome.refused.is_some() && !outcome.saved,
            Err(_) => true,
        };
        assert!(failed, "not saved");
        let left = walk(&tmp.path().join("cache/.edits"));
        assert!(left.is_empty(), "{left:?}");
    }

    // ---- the rate, Esc while Save As waits, a temp offered again ----

    /// 07 §7: the Save As modal shows the time expected at the rate the viewer is getting —
    /// the stream's own measure, once a read has come back.
    #[test]
    fn save_as_shows_the_time_at_the_streams_rate() {
        let mut sx = sx(MODEL);
        sx.settle();
        let tmp = tempfile::tempdir().unwrap();
        sx.win.save_as_folder = Some(tmp.path().to_path_buf());
        let lines = |sx: &Sx| -> Vec<String> {
            sx.drv
                .texts_starting_with("24.0 GB")
                .into_iter()
                .map(|(_, t)| t)
                .filter(|t| t.contains(" — "))
                .collect()
        };
        // The stream's own measure, whatever it is now (the engine is not stepped: it holds).
        let rate = sx.source.rate();
        assert!(rate.is_some(), "a rate once a read came back");
        sx.key(Key::F7, Modifiers::NONE);
        sx.frames(2);
        assert!(sx.win.save_as.is_some());
        assert_eq!(
            lines(&sx),
            [save_as::expected_line(save_as::Size::Exact(MODEL), rate)],
            "the stream's rate"
        );
        sx.key(Key::Escape, Modifiers::NONE);
        sx.frames(2);
        // A rate fixed here: 24 GB at 80 MB/s is 300 s.
        sx.win.test_rate = Some(80_000_000.0);
        sx.key(Key::F7, Modifiers::NONE);
        sx.frames(2);
        assert_eq!(lines(&sx), ["24.0 GB — about 5 min at 80.0 MB/s"]);
        sx.key(Key::Escape, Modifiers::NONE);
    }

    /// 07 §7 and §4.2 step 5: Esc stops a Save As that is waiting for the baseline pass (at
    /// most 256 MiB its check needs the pass's whole hash), as it stops a Save: the modal says
    /// it was cancelled, nothing is saved, the edits stay.
    #[test]
    fn esc_stops_a_save_as_waiting_for_the_baseline_pass() {
        let mut sx = sx(MIB);
        sx.settle();
        sx.typed("A");
        sx.typed("B");
        assert_eq!(sx.win.doc.byte(0), Some(0xAB));
        // The pass reads through the stream, whose engine this test does not step: it waits.
        sx.drv.until(&mut sx.win, 10, |w| {
            w.checking.active.load(Ordering::SeqCst)
        });
        let out = tempfile::tempdir().unwrap();
        sx.win.save_as_folder = Some(out.path().to_path_buf());
        sx.key(Key::F7, Modifiers::NONE);
        sx.frames(2);
        // Nothing waits for the pass yet (the count is of waits, one per 100 ms a caller spends
        // waiting): it goes up once the Save As's worker is at the wait.
        let waits = sx.win.checking.waiters.load(Ordering::SeqCst);
        sx.key(Key::Enter, Modifiers::NONE);
        // The Save As's worker reached the wait for the pass, and the pass is not done.
        sx.drv.until(&mut sx.win, 30, |w| {
            w.checking.waiters.load(Ordering::SeqCst) > waits
        });
        assert!(
            sx.win.checking.active.load(Ordering::SeqCst),
            "the pass ended"
        );
        assert!(
            sx.win.checking.ended(&sx.source).is_none(),
            "the pass ended"
        );
        assert!(sx.win.save_as.as_ref().is_some_and(|(m, ..)| m.busy()));
        sx.key(Key::Escape, Modifiers::NONE);
        let deadline = Instant::now() + Duration::from_secs(10);
        while sx.win.save_as.as_ref().is_some_and(|(m, ..)| m.busy()) {
            assert!(
                Instant::now() < deadline,
                "Esc did not stop the Save As waiting for the pass"
            );
            sx.frames(1);
            std::thread::sleep(Duration::from_millis(10));
        }
        sx.frames(2);
        assert!(sx
            .drv
            .find_text(crate::gui::binary::SAVE_CANCELLED)
            .is_some());
        assert!(walk(out.path()).is_empty(), "{:?}", walk(out.path()));
        assert!(sx.win.doc.unsaved());
    }

    /// 07 §3.1: a file at an interrupted Save's temp path that sopen cannot prove its own is
    /// offered for removal on every open until it is answered — Later leaves the question for
    /// the next open; Keep ends it (the file stays, and is not asked about again).
    #[test]
    fn a_temp_of_uncertain_ownership_is_offered_until_answered() {
        for side in sides() {
            let body = body();
            let mut ed = ed_side(Some(&body), side);
            ed.typed("AB");
            kill_at(&mut ed, patch::Step::Copy(1));
            let temp = ed.remote.parent().unwrap().join(&ed.temps()[0]);
            ed.reopen();
            let asks = |ed: &Ed| {
                ed.rows()
                    .iter()
                    .filter(|l| l.ends_with("— remove it?"))
                    .count()
            };
            assert_eq!(asks(&ed), 1, "{:?}", ed.rows());
            ed.press_row("could not verify", RowAction::Later);
            ed.reopen();
            assert_eq!(asks(&ed), 1, "Later: asked again: {:?}", ed.rows());
            assert!(temp.exists());
            // Keep it: the question ends; the file stays.
            ed.press_row("could not verify", RowAction::Keep);
            assert_eq!(asks(&ed), 0, "{:?}", ed.rows());
            ed.reopen();
            assert_eq!(asks(&ed), 0, "Keep ends it: {:?}", ed.rows());
            assert!(temp.exists(), "kept");
            assert!(
                ed.rows().iter().any(|l| l.contains("(1 byte changed)")),
                "the edits are still offered: {:?}",
                ed.rows()
            );
        }
    }

    /// 07 §4.1: without `copy-data`, the refusal of a file above 256 MiB states the cost at the
    /// rate the viewer is getting (over `sftp-server` the stream's reads take real time, so the
    /// stream has a rate; the local test transport answers at once and has none).
    #[test]
    fn the_cost_without_copy_data_is_said_at_the_streams_rate() {
        if !sides().contains(&Side::Sftp) {
            return;
        }
        let mut ed = ed_side(None, Side::Sftp);
        ed.moved.no_copy.store(true, Ordering::SeqCst);
        ed.goto(E1_AT);
        ed.typed("DE");
        // Over a real SFTP session the stream measures a rate, and the refusal says the cost at
        // it.
        assert!(ed.source.rate().is_some_and(|r| r > 0.0), "a measured rate");
        ed.save();
        let offer = ed
            .win
            .offer
            .clone()
            .expect("a refusal that offers other ways");
        assert!(
            offer.contains("moves 539.0 MB (about ") && offer.contains(" at the current "),
            "{offer}"
        );
        // At a rate fixed here the words are exact: 539.0 MB at 100 MB/s is 5.4 s.
        ed.win.file.lock().unwrap().rate = Some(100_000_000.0);
        ed.save();
        let offer = ed.win.offer.clone().expect("refused again");
        assert!(
            offer.ends_with(
                "rewriting 269.5 MB through this machine moves 539.0 MB (about 5 s at the \
                 current 100.0 MB/s); this server has no copy-data (OpenSSH before 9.0)"
            ),
            "{offer}"
        );
    }
}
