//! S6 through the window (02 §8.1's row: 04 K-R1 slots 7 and 8, K-R3 Shift+F8, K-K1, K-D1, K-N1
//! for remote folders, K-C4 without `hardlink@`): F7 makes a folder and F8 deletes — on this
//! machine to the Trash where there is one (in a test build, the tests' stand-in Trash), on a
//! remote permanently — and a remote folder's F2 and F6, and a copy's publish where the server has
//! no `hardlink@`, are SFTP's plain rename. Real keys, S6-B's dialogs, S6-A's engine on each
//! operation's own connections; a remote is a local `sftp-server` whose sessions carry a
//! plain-rename channel in their own slot, as `SftpRemote::connect` gives them.

use super::*;
use crate::gui::testkit::command;
use crate::remote::ops::FileOps;
use crate::remote::SftpRemote;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Passes until `done` (at most 20 s), then two more: after a file operation, until the panels
/// have re-read too (the list takes no key while one is listed).
fn wait(fx: &mut Fx, done: impl FnMut(&Browser) -> bool) {
    fx.drv.until(&mut fx.b, 20, done);
    fx.drv.idle(&mut fx.b, 2);
}

/// Until the running operation and the re-reads after it are over.
fn ended(fx: &mut Fx) {
    wait(fx, |b| !b.ops.busy() && b.listing.is_empty());
}

fn status(fx: &Fx) -> String {
    fx.b.status_line().text
}

/// The F-key row has the slot `label`.
fn has_label(fx: &Fx, label: &str) -> bool {
    fx.drv.find_text(&format!("{label:<7}")).is_some()
}

/// Both panels list the root again (after the test changed it on disk).
fn relist(fx: &mut Fx) {
    let root = fx.root.clone();
    fx.b.list(None, root, None, None);
    fx.settle();
}

/// The right panel lists the folder `sub` of the root.
fn other_into(fx: &mut Fx, sub: &str) -> String {
    let dir = format!("{}/{sub}", fx.root);
    fx.b.list(Some(1), dir.clone(), None, None);
    fx.settle();
    dir
}

/// `key` pressed with exactly `modifiers`.
fn press(fx: &mut Fx, key: Key, modifiers: Modifiers) {
    fx.drv.key(&mut fx.b, key, modifiers);
}

/// The dialog's text `text` is in the error colour.
fn in_error_colour(fx: &Fx, text: &str) -> bool {
    let colors = fx.drv.colors_of(text).unwrap_or_default();
    colors.contains(&chrome::PALETTE.dialog_error)
}

/// File-operation connections for the host `box`: per operation a local `sftp-server` (with
/// `args`), its request log `main-<n>.log`, the session carrying a plain-rename channel in its
/// own slot — opened when first needed, as `SftpRemote::connect`'s is, to a second `sftp-server`
/// logging to `plain-<n>.log`; `opened` counts the channels opened. This machine's file layer
/// for no host.
struct Remote {
    connect: OpsConnect,
    logs: tempfile::TempDir,
    opened: Arc<AtomicUsize>,
}

/// `None` where `sftp-server` is not installed (a failure under `SOPEN_REQUIRE_SFTP_SERVER`).
fn remote(args: &'static [&'static str]) -> Option<Remote> {
    drop(SftpRemote::local_server()?);
    let logs = tempfile::tempdir().unwrap();
    let opened = Arc::new(AtomicUsize::new(0));
    let (dir, count, n) = (
        logs.path().to_path_buf(),
        opened.clone(),
        AtomicUsize::new(0),
    );
    let connect: OpsConnect = Arc::new(move |host: Option<&str>| {
        let i = n.fetch_add(1, Ordering::SeqCst);
        Ok(match host {
            Some(_) => {
                let main = dir.join(format!("main-{i}.log"));
                let session =
                    SftpRemote::local_server_args(args, Some(&main)).expect("sftp-server");
                let plain = dir.join(format!("plain-{i}.log"));
                Box::new(session.with_local_plain(Some(&plain), count.clone()))
                    as Box<dyn FileOps + Send>
            }
            None => Box::new(LocalFs::new()),
        })
    });
    Some(Remote {
        connect,
        logs,
        opened,
    })
}

impl Remote {
    /// Every request log of `kind` (`main`: the sessions', `plain`: their channels') so far,
    /// one after another.
    fn log(&self, kind: &str) -> String {
        let mut names: Vec<_> = std::fs::read_dir(self.logs.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                name.starts_with(&format!("{kind}-"))
            })
            .collect();
        names.sort();
        names
            .iter()
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect()
    }

    fn opened(&self) -> usize {
        self.opened.load(Ordering::SeqCst)
    }
}

/// The browser posing as host `box`, its file operations over `remote`'s connections.
fn remote_fixture(remote: &Remote) -> Fx {
    let mut fx = fixture_with(Some("box"), local_connect(), None);
    fx.b.set_ops_connect(remote.connect.clone());
    fx
}

// ---------------------------------------------------------------------------------------------
// K-R1, K-R2, K-R3: the keys.

/// K-R1 slots 7 and 8: the list's row reads `7MkDir 8Delete` now S6 has landed; a click on a
/// slot and Cmd/Ctrl+7, +8 do what F7 and F8 do (K-R2): the make-folder and delete dialogs, Esc
/// closing each with nothing done.
#[test]
fn k_r1_slots_7_and_8_make_a_folder_and_delete() {
    let mut fx = fixture();
    for label in ["MkDir", "Delete"] {
        assert!(has_label(&fx, label), "{label} in the list's row");
    }
    fx.select("main.rs");
    let asked = format!("Make a folder in {}", fx.root);
    for way in 0..3 {
        match way {
            0 => fx.key(Key::F7),
            1 => fx.drv.click_text(&mut fx.b, "MkDir  "),
            _ => fx.drv.key(&mut fx.b, Key::Num7, command(false)),
        }
        fx.drv.idle(&mut fx.b, 2);
        assert!(fx.drv.find_text(&asked).is_some(), "F7, way {way}");
        fx.key(Key::Escape);
        assert!(fx.drv.find_text(&asked).is_none(), "Esc closed it");
    }
    for way in 0..3 {
        match way {
            0 => fx.key(Key::F8),
            1 => fx.drv.click_text(&mut fx.b, "Delete "),
            _ => fx.drv.key(&mut fx.b, Key::Num8, command(false)),
        }
        fx.drv.idle(&mut fx.b, 2);
        assert!(
            fx.drv.find_text("Delete main.rs?").is_some(),
            "F8, way {way}"
        );
        fx.key(Key::Escape);
        assert!(fx.drv.find_text("Delete main.rs?").is_none());
    }
    assert!(Path::new(&format!("{}/main.rs", fx.root)).exists());
    assert!(!Path::new(&format!("{}/new", fx.root)).exists());
}

/// K-R3: F8 asks for the Trash and Shift+F8 deletes permanently, `permanent` in the error colour;
/// Del is F8 and Shift+Del Shift+F8; ⌘⌫ is F8 on macOS (elsewhere Ctrl+Backspace has no row);
/// Alt+F8 and Shift+F7 do nothing. Esc cancels each dialog with nothing removed.
#[test]
fn k_r3_f8_asks_for_the_trash_and_shift_f8_is_permanent() {
    let mut fx = fixture();
    fx.select("zeta.md");
    let mac_cmd = Modifiers {
        mac_cmd: true,
        command: true,
        ..Modifiers::NONE
    };
    let ctrl = Modifiers {
        ctrl: true,
        command: true,
        ..Modifiers::NONE
    };
    let mac = cfg!(target_os = "macos");
    for (key, mods, opens) in [
        (Key::F8, Modifiers::NONE, Some("to the Trash")),
        (Key::Delete, Modifiers::NONE, Some("to the Trash")),
        (Key::F8, Modifiers::SHIFT, Some("permanent")),
        (Key::Delete, Modifiers::SHIFT, Some("permanent")),
        (Key::Backspace, mac_cmd, mac.then_some("to the Trash")),
        (Key::Backspace, ctrl, None),
        (Key::F8, Modifiers::ALT, None),
        (Key::F7, Modifiers::SHIFT, None),
    ] {
        press(&mut fx, key, mods);
        fx.drv.idle(&mut fx.b, 2);
        let asked = fx.drv.find_text("Delete zeta.md?").is_some();
        assert_eq!(asked, opens.is_some(), "{key:?} {mods:?}");
        assert!(
            fx.drv.find_text("Make a folder in").is_none(),
            "{key:?} {mods:?}"
        );
        if let Some(line) = opens {
            assert!(fx.drv.find_text(line).is_some(), "{key:?} {mods:?}: {line}");
            assert_eq!(in_error_colour(&fx, line), line == "permanent", "{line}");
            fx.key(Key::Escape);
            assert!(fx.drv.find_text("Delete zeta.md?").is_none());
        }
    }
    assert_eq!(
        std::fs::read_to_string(format!("{}/zeta.md", fx.root)).unwrap(),
        "# zeta\n",
        "nothing removed"
    );
}

// ---------------------------------------------------------------------------------------------
// K-K1: F7.

/// K-K1 on this machine: F7 `new` → the folder exists, the bar on it, the status row says so —
/// never the selection (04 §4); F7 `new` again → an error, read whole in the report, nothing
/// changed.
#[test]
fn k_k1_f7_makes_a_folder_and_the_bar_goes_on_it() {
    let mut fx = fixture();
    fx.select("notes.txt");
    fx.key(Key::Space);
    fx.select("main.rs");
    fx.key(Key::F7);
    fx.typed("new");
    fx.key(Key::Enter);
    ended(&mut fx);
    let new = format!("{}/new", fx.root);
    assert!(Path::new(&new).is_dir(), "made");
    assert_eq!(fx.current(), "new", "the bar is on it");
    assert_eq!(status(&fx), "made folder new");
    assert!(
        fx.names(1).contains(&"new".to_string()),
        "the other panel too"
    );
    assert!(Path::new(&format!("{}/notes.txt", fx.root)).exists());
    std::fs::write(format!("{new}/kept"), "kept").unwrap();

    fx.key(Key::F7);
    fx.typed("new");
    fx.key(Key::Enter);
    ended(&mut fx);
    assert!(fx.b.ops.holds_keyboard(), "the report is up");
    assert!(fx
        .drv
        .find_text(&format!("Make folder new in {}: 1 failed", fx.root))
        .is_some());
    assert!(fx.drv.find_text("failed      new — new exists").is_some());
    assert_eq!(
        std::fs::read_to_string(format!("{new}/kept")).unwrap(),
        "kept"
    );
    fx.key(Key::Enter);
    assert!(!fx.b.ops.holds_keyboard(), "Enter closed the report");
}

/// K-K1 on a remote: F7 `made` → `MKDIR` over the operation's own SFTP session, the bar on the
/// new folder; the plain-rename channel is not opened for it. A name holding `/` never reaches
/// the server.
#[test]
fn k_k1_f7_on_a_remote_is_one_mkdir() {
    let Some(remote) = remote(&[]) else {
        return;
    };
    let mut fx = remote_fixture(&remote);
    fx.key(Key::F7);
    assert!(fx
        .drv
        .find_text(&format!("Make a folder in box:{}", fx.root))
        .is_some());
    fx.typed("a/b");
    fx.key(Key::Enter);
    assert!(fx.drv.find_text("a/b: a name cannot hold a /").is_some());
    fx.typed("made");
    fx.key(Key::Enter);
    ended(&mut fx);
    let made = format!("{}/made", fx.root);
    assert!(Path::new(&made).is_dir());
    assert_eq!(fx.current(), "made");
    assert_eq!(status(&fx), "made folder made");
    let main = remote.log("main");
    assert!(main.contains(&format!("mkdir name \"{made}\"")), "{main}");
    assert!(!main.contains("a/b"), "{main}");
    assert_eq!(remote.opened(), 0, "no channel for a MKDIR");
}

// ---------------------------------------------------------------------------------------------
// K-D1: F8.

/// K-D1 on this machine: F8 on a file → the dialog says `to the Trash`; Enter → it is in the
/// Trash with its bytes (the tests' stand-in; the system Trash is the by-hand receipt). A folder
/// goes whole. Shift+F8 → gone, not in the Trash. Where this machine has no Trash for the folder,
/// F8 says `permanent — no Trash here` before anything is removed, and removes permanently.
#[test]
fn k_d1_f8_here_goes_to_the_trash_and_shift_f8_does_not() {
    let mut fx = fixture();
    let path = |fx: &Fx, n: &str| format!("{}/{n}", fx.root);
    fx.select("notes.txt");
    fx.key(Key::F8);
    assert!(fx.drv.find_text("Delete notes.txt?").is_some());
    assert!(fx.drv.find_text("to the Trash").is_some());
    fx.key(Key::Enter);
    ended(&mut fx);
    assert!(!Path::new(&path(&fx, "notes.txt")).exists());
    let trashed = LocalFs::trashed_by_tests(&path(&fx, "notes.txt")).expect("in the Trash");
    assert_eq!(std::fs::read_to_string(&trashed).unwrap(), "plain notes\n");
    assert_eq!(status(&fx), "moved notes.txt to the Trash");
    assert!(!fx.names(0).contains(&"notes.txt".to_string()), "re-read");

    fx.select("alpha");
    fx.key(Key::F8);
    assert!(fx
        .drv
        .find_text("Delete alpha and everything inside it?")
        .is_some());
    fx.key(Key::Enter);
    ended(&mut fx);
    let trashed = LocalFs::trashed_by_tests(&path(&fx, "alpha")).expect("in the Trash");
    assert_eq!(
        std::fs::read_to_string(format!("{trashed}/inner.txt")).unwrap(),
        "inside\n",
        "whole"
    );

    fx.select("main.rs");
    press(&mut fx, Key::F8, Modifiers::SHIFT);
    assert!(in_error_colour(&fx, "permanent"));
    fx.key(Key::Enter);
    ended(&mut fx);
    assert!(!Path::new(&path(&fx, "main.rs")).exists());
    assert_eq!(LocalFs::trashed_by_tests(&path(&fx, "main.rs")), None);
    assert_eq!(status(&fx), "deleted main.rs");

    fx.b.trash_answer = Some(false);
    fx.select("zeta.md");
    fx.key(Key::F8);
    assert!(in_error_colour(&fx, "permanent — no Trash here"));
    assert!(Path::new(&path(&fx, "zeta.md")).exists(), "asked first");
    fx.key(Key::Enter);
    ended(&mut fx);
    assert!(!Path::new(&path(&fx, "zeta.md")).exists());
    assert_eq!(LocalFs::trashed_by_tests(&path(&fx, "zeta.md")), None);
    assert_eq!(status(&fx), "deleted zeta.md");
}

/// K-D1 on a remote: F8 on a folder → `permanent — no Trash on box` in the error colour; Enter
/// removes it and everything inside it, bottom-up, a link inside removed as a link — the folder
/// it points at, outside, untouched.
#[test]
fn k_d1_f8_on_a_remote_folder_removes_it_all_permanently() {
    let Some(remote) = remote(&[]) else {
        return;
    };
    let mut fx = remote_fixture(&remote);
    let tree = format!("{}/tree", fx.root);
    std::fs::create_dir_all(format!("{tree}/sub/deeper")).unwrap();
    std::fs::write(format!("{tree}/top.txt"), "top").unwrap();
    std::fs::write(format!("{tree}/sub/deeper/c"), "c").unwrap();
    std::os::unix::fs::symlink(format!("{}/alpha", fx.root), format!("{tree}/sub/out")).unwrap();
    relist(&mut fx);
    fx.select("tree");
    fx.key(Key::F8);
    assert!(fx
        .drv
        .find_text("Delete tree and everything inside it?")
        .is_some());
    assert!(in_error_colour(&fx, "permanent — no Trash on box"));
    assert!(fx.drv.find_text("to the Trash").is_none());
    fx.key(Key::Enter);
    ended(&mut fx);
    assert!(!Path::new(&tree).exists(), "removed with everything inside");
    assert_eq!(
        std::fs::read_to_string(format!("{}/alpha/inner.txt", fx.root)).unwrap(),
        "inside\n",
        "the link's target is untouched"
    );
    assert_eq!(status(&fx), "deleted tree");
    assert_eq!(LocalFs::trashed_by_tests(&tree), None);
    let main = remote.log("main");
    let at = |what: &str| {
        main.find(what)
            .unwrap_or_else(|| panic!("{what} not in {main}"))
    };
    assert!(
        at(&format!("remove name \"{tree}/sub/deeper/c\""))
            < at(&format!("rmdir name \"{tree}/sub/deeper\""))
    );
    assert!(at(&format!("remove name \"{tree}/sub/out\"")) < at(&format!("rmdir name \"{tree}\"")));
}

// ---------------------------------------------------------------------------------------------
// K-N1 and K-M1 for remote folders, K-C4 without `hardlink@`: the plain rename.

/// K-N1 for a remote folder: F2 → the field, a free name → one plain `SSH_FXP_RENAME` (the
/// inode kept, never `posix-rename`), the bar on the new name; onto an existing name → an error
/// on the status row with both left, the field still open.
#[test]
fn k_n1_f2_on_a_remote_folder_is_one_plain_rename_and_never_replaces() {
    let Some(remote) = remote(&[]) else {
        return;
    };
    let mut fx = remote_fixture(&remote);
    let at = |fx: &Fx, n: &str| format!("{}/{n}", fx.root);
    let ino = std::fs::metadata(at(&fx, "alpha")).unwrap().ino();
    fx.select("alpha");
    fx.key(Key::F2);
    assert_eq!(fx.b.ops.renaming(), Some("alpha"), "the field opens");
    fx.typed("gamma");
    fx.key(Key::Enter);
    ended(&mut fx);
    assert!(!Path::new(&at(&fx, "alpha")).exists());
    assert_eq!(std::fs::metadata(at(&fx, "gamma")).unwrap().ino(), ino);
    assert_eq!(
        std::fs::read_to_string(at(&fx, "gamma/inner.txt")).unwrap(),
        "inside\n"
    );
    assert_eq!(fx.current(), "gamma", "the bar is on the new name");
    assert_eq!(status(&fx), "renamed alpha → gamma");
    assert_eq!(remote.log("plain").matches("rename old").count(), 1);

    fx.key(Key::F2);
    fx.typed("beta");
    fx.key(Key::Enter);
    wait(&mut fx, |b| !b.ops.busy());
    let said = fx.b.status_line();
    assert!(
        said.error && said.text.contains("beta exists"),
        "{}",
        said.text
    );
    assert_eq!(fx.b.ops.renaming(), Some("gamma"), "the field stays");
    assert!(Path::new(&at(&fx, "gamma/inner.txt")).exists(), "both left");
    assert!(Path::new(&at(&fx, "beta")).is_dir());
    fx.key(Key::Escape);
    let plain = remote.log("plain");
    assert_eq!(plain.matches("rename old").count(), 2, "{plain}");
    let main = remote.log("main");
    assert!(!main.contains("rename"), "never posix-rename: {main}");
}

/// K-M1 for a remote folder from S6: F6 into another folder of the same host → one plain
/// `SSH_FXP_RENAME`, the inode kept — no copy, check and remove (no `MKDIR`, `REMOVE` or `RMDIR`).
#[test]
fn f6_of_a_remote_folder_is_one_plain_rename() {
    let Some(remote) = remote(&[]) else {
        return;
    };
    let mut fx = remote_fixture(&remote);
    let beta = other_into(&mut fx, "beta");
    let ino = std::fs::metadata(format!("{}/alpha", fx.root))
        .unwrap()
        .ino();
    fx.select("alpha");
    fx.key(Key::F6);
    assert!(fx.drv.find_text("Move alpha").is_some());
    fx.key(Key::Enter);
    ended(&mut fx);
    assert!(!Path::new(&format!("{}/alpha", fx.root)).exists());
    assert_eq!(
        std::fs::metadata(format!("{beta}/alpha")).unwrap().ino(),
        ino
    );
    assert_eq!(
        std::fs::read_to_string(format!("{beta}/alpha/inner.txt")).unwrap(),
        "inside\n"
    );
    assert_eq!(status(&fx), format!("moved alpha to box:{beta}"));
    assert_eq!(remote.log("plain").matches("rename old").count(), 1);
    let main = remote.log("main");
    for not in ["mkdir", "remove", "rmdir", "rename"] {
        assert!(!main.contains(not), "{not}: {main}");
    }
}

/// K-C4 without `hardlink@` (a local `sftp-server -P hardlink`): F5 copies — the rename probe
/// once (its folder, two exclusive files, one plain rename refused, both removed), then the temp
/// published by one plain rename; the bytes arrive, no `.sopen-` entry is left.
#[test]
fn k_c4_f5_without_hardlink_publishes_through_the_probed_plain_rename() {
    let Some(remote) = remote(&["-P", "hardlink"]) else {
        return;
    };
    let mut fx = remote_fixture(&remote);
    let alpha = other_into(&mut fx, "alpha");
    fx.select("main.rs");
    fx.key(Key::F5);
    fx.key(Key::Enter);
    ended(&mut fx);
    assert_eq!(
        std::fs::read_to_string(format!("{alpha}/main.rs")).unwrap(),
        RUST
    );
    assert_eq!(status(&fx), format!("copied main.rs to box:{alpha}"));
    let names: Vec<String> = std::fs::read_dir(&alpha)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(names.iter().all(|n| !n.starts_with(".sopen-")), "{names:?}");
    let main = remote.log("main");
    assert!(main.contains(".sopen-probe-"), "{main}");
    assert_eq!(
        main.matches("mkdir name").count(),
        1,
        "one probe folder: {main}"
    );
    assert!(!main.contains("rename"), "{main}");
    let plain = remote.log("plain");
    assert_eq!(
        plain.matches("rename old").count(),
        2,
        "the probe's and the publish: {plain}"
    );
    assert_eq!(remote.opened(), 1);
}

// ---------------------------------------------------------------------------------------------
// The receipts' hook.

/// `SOPEN_KEYS` (AGENTS.md "Receipts"): a played F2 settles the window only once its rename has
/// ended — the field shown pending while the engine works is no question waiting for the owner —
/// so a screenshot never shoots a rename half-way (the F2 receipt over a real host shot
/// `renaming dirA → renamed…`). A question still counts as settled (S1's rule).
#[test]
fn a_played_rename_settles_the_window_once_it_has_ended() {
    let mut fx = fixture();
    let gate = Gate::default();
    let held = gate.clone();
    fx.b.set_ops_connect(Arc::new(move |_: Option<&str>| {
        held.hold();
        Ok(Box::new(LocalFs::new()) as Box<dyn FileOps + Send>)
    }));
    fx.select("main.rs");
    fx.b.script = parse_keys("F2,text:prog,Enter").unwrap().into();
    gate.reached_running(&mut fx);
    fx.drv.idle(&mut fx.b, 3);
    let ready_while_held = fx.b.ready();
    let pending = fx.b.ops.busy() && fx.b.ops.renaming() == Some("main.rs");
    gate.release();
    assert!(pending, "the rename runs, its field shown");
    assert!(!ready_while_held, "not settled while the rename runs");
    fx.settle();
    assert!(Path::new(&format!("{}/prog.rs", fx.root)).exists());
    assert_eq!(fx.current(), "prog.rs");
    assert_eq!(status(&fx), "renamed main.rs → prog.rs");
}
