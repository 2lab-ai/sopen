//! The hex window: the model, the write side, and the window driven by real input events (keys
//! with modifiers, typed text, clicks) through its `ui`.

use super::*;
use crate::gui::testkit::{command, Driver};
use crate::gui::Window;
use crate::remote::{LocalFs, RemoteFs};
use eframe::egui::{Event, Modifiers};

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

/// Files under `dir`, recursively.
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

/// Save order: the backup is on disk before anything is staged or written — here staging
/// fails (the edits folder cannot be made), the backup is there and the remote untouched.
#[test]
fn save_backs_up_before_staging_or_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("a.bin");
    std::fs::write(&remote, FIXTURE).unwrap();
    let abs = remote.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let base = tmp.path().join("copy/a.bin");
    let at = fs.fetch(&abs, &base, &mut |_, _| {}).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let cache = tmp.path().join("cache");
    let mut file = HexFile::open(origin, &base, None, &cache).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(crate::cache::edits_root(&cache), b"not a folder").unwrap();
    let patches = BTreeMap::from([(0u64, 0u8)]);
    let err = file.save(&patches).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("was not touched"), "{msg}");
    assert!(msg.contains("backed up at"), "{msg}");
    let backups = walk(&crate::cache::backup_root(&cache));
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(std::fs::read(&backups[0]).unwrap(), FIXTURE);
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
    let refused = fx.win.outcome.lock().unwrap().refused.clone().unwrap();
    assert!(refused.contains("changed since it was opened"), "{refused}");
    let cache = fx.tmp.path().join("cache");
    assert!(walk(&cache).is_empty(), "{:?}", walk(&cache));
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
    // Scrolled through half a file's worth of pages: still at most the cache's bound held.
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
        rev: fx.win.doc.rev(),
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
