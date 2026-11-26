//! The switch's state (10 §7.4, X2-state): taken from one browser, written as one JSON line,
//! read and given to a new browser on the same folders, which then lays out the same cells
//! (`SOPEN_CELLS`' receipt of each, the foot's free bytes aside); a v1 line without its optional
//! fields read, a newer one refused with §7.4's line, v0 as one no sopen wrote; paths rewritten
//! out and back; a folder that is gone.

use super::*;
use crate::encoding::Encoding;
use crate::git::history::fixture::{self, Repo};
use crate::git::runner::GitRunner;
use crate::gui::browser::{panel, BrowserEvent, Connect, GitConnect, Start};
use crate::gui::chrome::CELL;
use crate::gui::testkit::Driver;
use crate::gui::{cells, term, Window};
use crate::remote::{LocalFs, RemoteFs};
use eframe::egui::{Event, Key, Modifiers, Pos2};
use std::sync::Arc;

/// The receipts' grid: wide enough that no title row cuts a path (10 §15).
const COLS: u16 = 150;
const ROWS: u16 = 30;

/// A browser on `root`, with this machine's files (and the fixture's git, when given), its cache
/// and state folders of its own.
struct Side {
    _tmp: tempfile::TempDir,
    drv: Driver,
    b: Browser,
}

impl Side {
    fn new(root: &str, git: Option<GitConnect>) -> Side {
        let tmp = tempfile::tempdir().unwrap();
        let connect: Connect =
            Arc::new(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>));
        let b = Browser::new(Start {
            fs: Box::new(LocalFs::new()),
            connect,
            label: None,
            destination: None,
            abs: root.to_string(),
            cache: tmp.path().join("cache"),
            state_dir: tmp.path().join("state"),
            launch: Arc::new(|abs: &str, _| Ok(format!("opened {abs}"))),
            git,
        });
        let size = (f32::from(COLS) * CELL.x, f32::from(ROWS) * CELL.y);
        Side {
            _tmp: tmp,
            drv: Driver::new(size.0, size.1),
            b,
        }
    }

    /// Passes until the browser is ready, then two more.
    fn settle(&mut self) {
        self.drv.until(&mut self.b, 30, |b| b.ready());
        self.drv.idle(&mut self.b, 2);
        self.drv.until(&mut self.b, 30, |b| b.ready());
    }

    fn key(&mut self, key: Key) {
        self.drv.key(&mut self.b, key, Modifiers::NONE);
    }

    fn typed(&mut self, text: &str) {
        self.drv
            .frame(&mut self.b, vec![Event::Text(text.into())], Modifiers::NONE);
        self.drv.idle(&mut self.b, 1);
    }

    /// The bar on `name` in panel `i`.
    fn bar(&mut self, i: usize, name: &str) {
        let p = &mut self.b.panels[i];
        p.highlight = p.entries.iter().position(|e| e.name == name).unwrap();
        p.step(0);
    }

    /// `names` selected in panel `i`, as Space selects them; the bar back where it was.
    fn select(&mut self, i: usize, names: &[&str]) {
        let at = self.b.panels[i].highlight;
        for name in names {
            self.bar(i, name);
            self.b.panels[i].toggle();
        }
        self.b.panels[i].highlight = at;
    }

    /// The `SOPEN_CELLS` receipt of one more pass, as the window writes it — without the disk's
    /// free bytes in the foot, which other writers move between two listings (it is no part of
    /// the state).
    fn cells(&mut self) -> String {
        for p in &mut self.b.panels {
            p.free = None;
        }
        let size = egui::vec2(f32::from(COLS) * CELL.x, f32::from(ROWS) * CELL.y);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(10_000.0),
            max_texture_side: Some(8192),
            focused: true,
            ..Default::default()
        };
        let mut pass = None;
        let b = &mut self.b;
        let mut out = self.drv.ctx.run_ui(raw, |ui| {
            cells::clear(ui.ctx());
            b.ui(ui);
            pass = Some(term::keep_window_pass(ui.ctx()));
        });
        out.textures_delta.clear();
        term::window_receipt(&self.drv.ctx, &pass.expect("the pass ran"))
    }

    fn take(&self) -> SwitchState {
        self.b.take_state(&self.drv.ctx)
    }
}

/// The folder every test lists: folders, a long one to scroll, text, a long text file.
fn tree(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("alpha")).unwrap();
    std::fs::write(root.join("alpha/inner.txt"), "inside\n").unwrap();
    std::fs::create_dir(root.join("beta")).unwrap();
    std::fs::create_dir(root.join("many")).unwrap();
    for n in 0..60 {
        std::fs::write(root.join(format!("many/f{n:02}.txt")), "x\n".repeat(n)).unwrap();
    }
    std::fs::write(root.join("notes.txt"), "plain notes\n").unwrap();
    let mut long = String::new();
    for n in 0..400 {
        // Every fifth line wider than the viewer, so wrapping shows.
        let tail = if n % 5 == 0 {
            "wide ".repeat(40)
        } else {
            String::new()
        };
        long.push_str(&format!("line {n:03} caf\u{e9} {tail}\n"));
    }
    std::fs::write(root.join("long.txt"), long).unwrap();
}

/// A root with [`tree`] in it, its path canonical (the title prints it).
fn root() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap().join("root");
    tree(&root);
    (tmp, root.to_str().unwrap().to_string())
}

/// The state through its line: one line, `"v":1` first, read back as it was.
fn through_the_line(state: &SwitchState) -> SwitchState {
    let line = state.line();
    assert!(!line.contains('\n'), "one line: {line}");
    assert!(line.starts_with(r#"{"v":1,"#), "{line}");
    let read = read(&line, "HOST").expect("its own line reads");
    assert_eq!(&read, state, "{line}");
    read
}

/// `state` given to a new browser on `root`: once it is ready, its receipt.
fn given(root: &str, git: Option<GitConnect>, state: SwitchState) -> (Side, String) {
    let mut b = Side::new(root, git);
    b.b.give_state(state).expect("the state is this machine's");
    b.settle();
    let cells = b.cells();
    (b, cells)
}

/// The rows of a receipt (its grid, without the attribute table), for a failure's message.
fn rows(receipt: &str) -> String {
    receipt
        .lines()
        .take(usize::from(ROWS) + 1)
        .collect::<Vec<_>>()
        .join("\n")
}

fn same_screen(taken: &str, given: &str) {
    assert!(
        taken == given,
        "the same cells\n--- taken\n{}\n--- given\n{}",
        rows(taken),
        rows(given)
    );
}

/// Both panels on their own folders, bars, selections and scroll, dual mode, the right side
/// active, the make-folder dialog open there with Hangul typed, refused, the caret moved off what
/// the refusal selected: taken (the browser unchanged), one JSON line, given to a new browser — the
/// same cells, caret included.
#[test]
fn panels_and_a_refused_dialog_cross_as_the_same_screen() {
    let (_tmp, root) = root();
    let mut a = Side::new(&root, None);
    a.settle();
    a.key(Key::F9);
    a.bar(0, "notes.txt");
    a.select(0, &["beta", "notes.txt"]);
    a.key(Key::Tab);
    a.settle();
    a.b.list(
        Some(1),
        format!("{root}/many"),
        Some("f40.txt".into()),
        None,
    );
    a.settle();
    a.select(1, &["f10.txt", "f41.txt"]);
    a.bar(1, "f44.txt");
    a.settle();
    // Back up within the rows on screen: the scroll stays where the bar took it, which no
    // listing would put it at.
    a.bar(1, "f30.txt");
    a.settle();
    let top = a.b.panels[1].top;
    assert!(top > 0 && top < 31 - 1, "the right panel scrolled ({top})");
    a.key(Key::F7);
    a.settle();
    a.typed("새/폴더");
    a.key(Key::Enter);
    a.settle();
    a.key(Key::End);
    a.key(Key::ArrowLeft);
    a.key(Key::ArrowLeft);
    a.settle();
    let before = a.cells();
    assert!(
        before.contains("a name cannot hold a /"),
        "{}",
        rows(&before)
    );
    let state = a.take();
    assert_eq!(a.cells(), before, "taking the state changes nothing");
    assert_eq!(state.mode, Mode::Dual);
    assert_eq!(state.active, 1);
    assert_eq!(state.panels[0].selected, ["beta", "notes.txt"]);
    assert_eq!(state.panels[1].selected, ["f10.txt", "f41.txt"]);
    assert_eq!(state.panels[1].bar.as_deref(), Some("f30.txt"));
    assert_eq!(state.panels[1].top, top);
    let dialog = state.dialog.clone().expect("the dialog");
    assert_eq!(dialog.kind, DialogKind::MakeFolder);
    assert_eq!(dialog.text.as_deref(), Some("새/폴더"));
    assert_eq!((dialog.anchor, dialog.caret), (Some(2), Some(2)));
    assert_eq!(
        dialog.error.as_deref(),
        Some("새/폴더: a name cannot hold a /")
    );
    let (b, after) = given(&root, None, through_the_line(&state));
    same_screen(&before, &after);
    assert_eq!(b.take(), state, "the state given is the state it has");
}

/// The text viewer at a place in the file, its lines read in Latin-1 (not the UTF-8 its bytes
/// pick), unwrapped: the viewer opens on the same file, view, place, encoding and wrap.
#[test]
fn the_text_viewer_crosses_at_its_place_encoding_and_wrap() {
    let (_tmp, root) = root();
    let mut a = Side::new(&root, None);
    a.settle();
    a.bar(0, "long.txt");
    a.b.open_enc = Some(Encoding::Latin1);
    a.key(Key::Enter);
    a.settle();
    for _ in 0..3 {
        a.key(Key::PageDown);
        a.settle();
    }
    a.key(Key::W);
    a.settle();
    let before = a.cells();
    let state = a.take();
    let viewer = state.viewer.clone().expect("the viewer");
    assert_eq!(viewer.file, format!("{root}/long.txt"));
    assert_eq!(viewer.view, 1);
    assert!(viewer.place.is_some_and(|p| p > 0), "{viewer:?}");
    assert_eq!(viewer.encoding.as_deref(), Some("Latin-1"));
    assert_eq!(viewer.wrap, Some(false));
    assert!(before.contains("Latin-1"), "{}", rows(&before));
    let (b, after) = given(&root, None, through_the_line(&state));
    same_screen(&before, &after);
    assert_eq!(b.take().viewer, state.viewer);
}

/// The viewer on 08's view 2 (Hex) of a text file, its cursor where the text view's top row was:
/// the given viewer is on view 2 at that byte, the same cells.
#[test]
fn the_viewer_crosses_on_its_view() {
    let (_tmp, root) = root();
    let mut a = Side::new(&root, None);
    a.settle();
    a.bar(0, "long.txt");
    a.key(Key::Enter);
    a.settle();
    a.key(Key::PageDown);
    a.settle();
    a.drv.press_typed(&mut a.b, Key::Num2, "2");
    a.settle();
    let before = a.cells();
    let state = a.take();
    let viewer = state.viewer.clone().expect("the viewer");
    assert_eq!(viewer.view, 2, "{viewer:?}");
    assert!(viewer.place.is_some_and(|p| p > 0), "{viewer:?}");
    assert_eq!(viewer.wrap, Some(true), "the text view behind it wraps");
    assert!(
        before.contains("1 Text · 2 Hex · 3 Raw"),
        "{}",
        rows(&before)
    );
    let (b, after) = given(&root, None, through_the_line(&state));
    same_screen(&before, &after);
    assert_eq!(b.take().viewer, state.viewer);
}

/// A repository of git's own fixture (`None` without git: the test skips, and fails under
/// `SOPEN_REQUIRE_GIT=1`), and how a browser's git reaches it.
fn repository() -> Option<(Repo, impl Fn() -> GitConnect)> {
    let git = fixture::git()?;
    let repo = Repo::new(&git);
    std::fs::create_dir(repo.top.join("src")).unwrap();
    repo.write("src/lib.rs", "pub fn one() {}\n");
    repo.write("README", "a repository\n");
    repo.commit("first");
    let local = repo.local();
    let connect = move || -> GitConnect {
        let local = local.clone();
        Arc::new(move || Ok(GitRunner::local(local.clone())))
    };
    Some((repo, connect))
}

/// A browser on `repo`'s top level with its repository pinned to the other side (Cmd/Ctrl+G).
fn pinned(repo: &Repo, git: GitConnect) -> Side {
    let mut a = Side::new(repo.top(), Some(git));
    a.settle();
    a.drv.key(&mut a.b, Key::G, Modifiers::COMMAND);
    a.settle();
    let pinned = a.b.git.pin.is_some();
    assert!(pinned, "pinned: {:?}", a.b.status_line().text);
    a
}

/// The directory tree open on the active side and the folder's repository pinned to the other
/// (git's own fixture): both come back, the same cells.
#[test]
fn the_tree_and_a_git_pin_cross() {
    let Some((repo, connect)) = repository() else {
        eprintln!("skipped: no git");
        return;
    };
    let mut a = pinned(&repo, connect());
    a.key(Key::F10);
    a.settle();
    let before = a.cells();
    let state = a.take();
    assert!(state.panels[0].tree);
    assert_eq!(state.panels[1].pin.as_deref(), Some(repo.top()));
    assert!(!state.git_focus);
    let (b, after) = given(repo.top(), Some(connect()), through_the_line(&state));
    same_screen(&before, &after);
    assert_eq!(b.take(), state);
}

/// Tab into the pinned git panel: the keyboard is there (its F-key row, its frame active, the
/// list's not) — and there in the browser given the state.
#[test]
fn the_keyboard_in_the_git_panel_crosses() {
    let Some((repo, connect)) = repository() else {
        eprintln!("skipped: no git");
        return;
    };
    let mut a = pinned(&repo, connect());
    a.key(Key::Tab);
    a.settle();
    assert!(a.b.git.focus, "the keyboard in the git panel");
    let before = a.cells();
    let state = a.take();
    assert!(state.git_focus);
    let (b, after) = given(repo.top(), Some(connect()), through_the_line(&state));
    same_screen(&before, &after);
    assert_eq!(b.take(), state);
}

/// A v1 line without its optional fields, and with a field this sopen does not know (ignored,
/// §7.4), reads with their defaults; `"v": 2` is refused with §7.4's line, naming who sent it,
/// and `"v": 0` — no sopen wrote one: v1 is the first — with its own.
#[test]
fn a_sparse_line_reads_and_other_versions_are_refused() {
    let line = r#"{"v":1,"panels":[{"folder":"/a"},{"folder":"/b","bar":"x"}],"later":[1,2]}"#;
    let state = read(line, "laptop").expect("a v1 line");
    let panel = |folder: &str, bar: Option<&str>| PanelState {
        folder: folder.to_string(),
        bar: bar.map(str::to_string),
        selected: vec![],
        top: 0,
        tree: false,
        pin: None,
    };
    assert_eq!(
        state,
        SwitchState {
            v: 1,
            host: None,
            mode: Mode::Preview,
            active: 0,
            panels: [panel("/a", None), panel("/b", Some("x"))],
            git_focus: false,
            dialog: None,
            viewer: None,
        }
    );
    assert_eq!(
        read(r#"{"v":2,"panels":"a later shape"}"#, "laptop"),
        Err("laptop sent switch state v2; this sopen reads up to v1 — update one of them".into())
    );
    let zero = r#"{"v":0,"panels":[{"folder":"/a"},{"folder":"/b"}]}"#;
    assert_eq!(
        read(zero, "laptop"),
        Err("state version 0 is not one sopen wrote".into())
    );
    assert!(read(r#"{"panels":[]}"#, "laptop").is_err(), "no version");
    assert!(read("not json", "laptop").is_err());
}

/// Paths go out as `forward::source_for` rewrites SOURCE (this machine's `/abs` →
/// `<this machine>:/abs`) and come back through the caller's map (`mac:/abs` → `/abs`): the host
/// moves, the paths stay, out and back is the state again — a Copy target typed as an absolute
/// place too, its caret and selection on the same characters; a name, or another machine's place
/// the map leaves alone, stays as typed. A map that gives two hosts, or a path that is not
/// absolute, is refused.
#[test]
fn paths_are_rewritten_out_and_back_by_the_callers_map() {
    let panel = |folder: &str, pin: Option<&str>| PanelState {
        folder: folder.to_string(),
        bar: Some("a.txt".into()),
        selected: vec!["a.txt".into()],
        top: 2,
        tree: false,
        pin: pin.map(str::to_string),
    };
    let here = SwitchState {
        v: 1,
        host: None,
        mode: Mode::Dual,
        active: 1,
        panels: [panel("/w/one", None), panel("/w/two", Some("/w"))],
        git_focus: true,
        dialog: Some(DialogState {
            kind: DialogKind::Copy,
            text: Some("/w/one".into()),
            caret: Some(3),
            anchor: Some(0),
            error: Some("a refusal".into()),
            entries: vec!["a.txt".into()],
        }),
        viewer: Some(ViewerState {
            file: "/w/two/a.txt".into(),
            view: 2,
            place: Some(7),
            encoding: None,
            wrap: Some(true),
            revision: false,
        }),
    };
    let out_map = |source: &str| {
        crate::forward::source_for("laptop", source, Some("mac.local"), |p| Ok(p.to_string()))
    };
    let back_map = |source: &str| match source.strip_prefix("mac:") {
        Some(abs) => Ok(abs.to_string()),
        None => anyhow::bail!("not this machine's: {source}"),
    };
    let out = here.rewritten(out_map).expect("out");
    assert_eq!(out.host.as_deref(), Some("mac"));
    assert_eq!(out.panels[1].folder, "/w/two");
    assert_eq!(out.panels[1].pin.as_deref(), Some("/w"));
    assert_eq!(
        out.viewer.as_ref().map(|v| v.file.as_str()),
        Some("/w/two/a.txt")
    );
    let typed = out.dialog.clone().unwrap();
    assert_eq!(typed.text.as_deref(), Some("mac:/w/one"));
    assert_eq!((typed.anchor, typed.caret), (Some(4), Some(7)));
    let back = out.rewritten(back_map).expect("back");
    assert_eq!(back, here);
    for kept in ["sub/dir", "third:/x"] {
        let mut state = here.clone();
        state.dialog.as_mut().unwrap().text = Some(kept.into());
        let out = state.rewritten(out_map).expect("out");
        assert_eq!(out.dialog, state.dialog, "{kept} out");
    }
    let mut named = here.clone();
    named.dialog.as_mut().unwrap().text = Some("sub/dir".into());
    let out = named.rewritten(out_map).expect("out");
    assert_eq!(out.rewritten(back_map).expect("back"), named, "a name back");
    let two_hosts = here.rewritten(|source| {
        Ok(match source {
            "/w" => "other:/w".to_string(),
            _ => format!("mac:{source}"),
        })
    });
    assert!(two_hosts.is_err(), "{two_hosts:?}");
    assert!(here.rewritten(|_| Ok("rel/path".into())).is_err());
    assert!(here.rewritten(|_| anyhow::bail!("unmapped")).is_err());
}

/// A folder of the state that is gone here shows its nearest parent that lists, and the status
/// row says so; the tree, the dialog and the viewer it had are not opened there. The other panel
/// is as given.
#[test]
fn a_gone_folder_shows_its_nearest_parent() {
    let (_tmp, root) = root();
    let gone = format!("{root}/gone/deeper");
    let state = read(
        &format!(
            r#"{{"v":1,"panels":[{{"folder":"{gone}","bar":"x.txt","tree":true}},{{"folder":"{root}/many","bar":"f03.txt"}}],
               "dialog":{{"kind":"make_folder","text":"made"}},
               "viewer":{{"file":"{gone}/x.txt"}}}}"#
        ),
        "HOST",
    )
    .unwrap();
    let (b, after) = given(&root, None, state);
    assert!(b.b.ops.open_dialog(&b.drv.ctx).is_none(), "no dialog");
    assert!(b.b.tree.is_none(), "no tree");
    assert_eq!(b.b.panels[0].dir, root);
    assert_eq!(b.b.panels[1].dir, format!("{root}/many"));
    assert_eq!(
        b.b.panels[1].current().map(|e| e.name.as_str()),
        Some("f03.txt")
    );
    assert!(b.b.viewer.is_none());
    let said = b.b.status_line().text;
    let dialog = format!("make folder was not opened again: the list is on {root} here");
    assert!(
        said.starts_with(&format!("{gone}: "))
            && said.contains(&format!("— showing {root} · "))
            && said.ends_with(&dialog),
        "{said}"
    );
    assert!(after.contains(&said[..40]), "{}", rows(&after));
    assert_eq!(
        b.b.panels[0].current().map(|e| e.name.as_str()),
        Some(panel::UP)
    );
}

/// A state of another host is not given to this window: nothing changes.
#[test]
fn a_state_of_another_host_is_refused() {
    let (_tmp, root) = root();
    let mut b = Side::new(&root, None);
    let mut state = b.take();
    state.host = Some("elsewhere".into());
    assert!(b.b.give_state(state).is_err());
    assert!(b.b.given.is_none());
}

// The review's cases (X2-state, 2026-11-26): the line comes over ssh, so it is untrusted.

/// `state` given to a new browser on `root`, not yet drawn.
fn fresh_given(root: &str, state: SwitchState) -> Side {
    let mut b = Side::new(root, None);
    b.b.give_state(state).expect("the state is this machine's");
    b
}

/// A line read as HOST sent it.
fn line(text: &str) -> SwitchState {
    read(text, "HOST").expect("the line reads")
}

/// The rename field and a Copy dialog cross on the entries they act on: the same cells.
#[test]
fn a_rename_and_a_copy_cross_on_their_entries() {
    let (_tmp, root) = root();
    let mut a = Side::new(&root, None);
    a.settle();
    a.bar(0, "notes.txt");
    a.key(Key::F2);
    a.settle();
    a.typed("renamed");
    a.settle();
    let before = a.cells();
    let state = a.take();
    let dialog = state.dialog.clone().expect("the rename field");
    assert_eq!(dialog.kind, DialogKind::Rename);
    assert_eq!(dialog.text.as_deref(), Some("renamed.txt"));
    assert_eq!(dialog.entries, ["notes.txt"]);
    let (_b, after) = given(&root, None, through_the_line(&state));
    same_screen(&before, &after);

    let mut a = Side::new(&root, None);
    a.settle();
    a.select(0, &["beta", "notes.txt"]);
    a.key(Key::F5);
    a.settle();
    a.typed("sub");
    a.settle();
    let before = a.cells();
    let state = a.take();
    let dialog = state.dialog.clone().expect("the Copy dialog");
    assert_eq!(dialog.kind, DialogKind::Copy);
    assert_eq!(dialog.entries, ["beta", "notes.txt"]);
    let (_b, after) = given(&root, None, through_the_line(&state));
    same_screen(&before, &after);
}

/// A dialog is opened again only on what it acted on: a line that names no entry opens no rename
/// on whatever the bar is on (here `/`'s first entry); an entry gone here opens none; the bar
/// moved (↑) during the putting back opens none on the new row. Each says why on the status row.
#[test]
fn a_dialog_opens_only_on_its_own_entries() {
    let b = given(
        "/",
        None,
        line(r#"{"v":1,"panels":[{"folder":"/"},{"folder":"/"}],"dialog":{"kind":"rename","text":"x"}}"#),
    )
    .0;
    assert!(
        b.b.ops.renaming().is_none(),
        "no rename on {:?}",
        b.b.panels[0].current().map(|e| &e.name)
    );
    let said = b.b.status_line().text;
    assert!(said.contains("rename was not opened again"), "{said}");

    let (_tmp, root) = root();
    let gone = line(&format!(
        r#"{{"v":1,"panels":[{{"folder":"{root}","bar":"vanished.txt"}},{{"folder":"{root}"}}],
           "dialog":{{"kind":"rename","text":"x","entries":["vanished.txt"]}}}}"#
    ));
    let (b, _) = given(&root, None, gone);
    assert!(b.b.ops.renaming().is_none());
    assert!(
        b.b.status_line().text.contains("vanished.txt"),
        "{}",
        b.b.status_line().text
    );

    let moved = line(&format!(
        r#"{{"v":1,"panels":[{{"folder":"{root}","bar":"notes.txt"}},{{"folder":"{root}"}}],
           "dialog":{{"kind":"rename","text":"x","entries":["notes.txt"]}}}}"#
    ));
    let mut b = fresh_given(&root, moved);
    b.drv.until(&mut b.b, 30, |b| {
        b.given.as_ref().is_some_and(|g| g.step > Step::Lists)
    });
    // (`notes.txt` is the last row: the bar moves up.)
    b.key(Key::ArrowUp);
    b.settle();
    assert_ne!(
        b.b.panels[0].current().map(|e| e.name.as_str()),
        Some("notes.txt")
    );
    assert!(b.b.ops.renaming().is_none(), "no rename on the new row");
}

/// A Copy target the map cannot place goes as nothing (the receiver's own target) with the
/// reason as its refusal; the other paths are mapped as ever. A caret past the text is held to
/// it before it moves (no overflow on `usize::MAX`).
#[test]
fn a_target_the_map_cannot_place_is_not_carried_bare() {
    let state = line(
        r#"{"v":1,"panels":[{"folder":"/w"},{"folder":"/x"}],
            "dialog":{"kind":"copy","text":"/elsewhere/t","caret":3,"anchor":0,"entries":["a"]}}"#,
    );
    let out = state
        .rewritten(|source| match source {
            "/elsewhere/t" => anyhow::bail!("no way there"),
            _ => Ok(format!("mac:{source}")),
        })
        .expect("the folders map");
    assert_eq!(out.panels[0].folder, "/w");
    let d = out.dialog.expect("the dialog");
    assert_eq!((d.text, d.caret, d.anchor), (None, None, None));
    assert_eq!(
        d.error.as_deref(),
        Some("/elsewhere/t: no way there — give the folder again")
    );

    let mut far = state.clone();
    far.dialog.as_mut().unwrap().caret = Some(usize::MAX);
    far.dialog.as_mut().unwrap().anchor = Some(usize::MAX);
    let out = far.rewritten(|s| Ok(format!("mac:{s}"))).expect("out");
    let d = out.dialog.expect("the dialog");
    assert_eq!(d.text.as_deref(), Some("mac:/elsewhere/t"));
    assert_eq!((d.caret, d.anchor), (Some(16), Some(16)));
}

/// What arrives is checked before anything is listed: folders, pins and the viewer's file must
/// be absolute, the host a label ssh cannot read as an option (`-o…`) or as two words — by
/// `read`, by `rewritten`'s map, and by `give_state` for a state made any other way.
#[test]
fn the_line_is_checked_at_the_boundary() {
    for bad in [
        r#"{"v":1,"panels":[{"folder":"rel"},{"folder":"/b"}]}"#,
        r#"{"v":1,"panels":[{"folder":"/a","pin":"repo"},{"folder":"/b"}]}"#,
        r#"{"v":1,"panels":[{"folder":"/a"},{"folder":"/b"}],"viewer":{"file":"x.txt"}}"#,
        r#"{"v":1,"host":"-oProxyCommand=x","panels":[{"folder":"/a"},{"folder":"/b"}]}"#,
        r#"{"v":1,"host":"a b","panels":[{"folder":"/a"},{"folder":"/b"}]}"#,
        r#"{"v":1,"active":2,"panels":[{"folder":"/a"},{"folder":"/b"}]}"#,
    ] {
        assert!(read(bad, "HOST").is_err(), "{bad}");
    }
    let state = line(r#"{"v":1,"panels":[{"folder":"/a"},{"folder":"/b"}]}"#);
    let option = state.rewritten(|s| Ok(format!("-oProxyCommand=x:{s}")));
    assert!(option.is_err(), "{option:?}");
    let (_tmp, root) = root();
    let mut relative = line(&format!(
        r#"{{"v":1,"panels":[{{"folder":"{root}"}},{{"folder":"{root}"}}]}}"#
    ));
    relative.panels[1].folder = "many".into();
    let mut b = Side::new(&root, None);
    assert!(b.b.give_state(relative).is_err());
    assert!(b.b.given.is_none());
}

/// A state is given once, to a browser that has not been drawn: again, or after a pass, is an
/// error — never a panic, nothing changed.
#[test]
fn a_state_is_given_once_to_a_fresh_browser() {
    let (_tmp, root) = root();
    let state = line(&format!(
        r#"{{"v":1,"panels":[{{"folder":"{root}"}},{{"folder":"{root}/many"}}]}}"#
    ));
    let mut b = fresh_given(&root, state.clone());
    assert!(b.b.give_state(state.clone()).is_err(), "a second time");
    b.settle();
    assert!(b.b.give_state(state).is_err(), "after a pass");
    assert!(b.b.given.is_none());
}

/// A listing the putting back did not ask for (a file operation's, a git write's re-read) ends
/// it: nothing of it comes later, and the browser settles.
#[test]
fn a_listing_of_its_own_ends_a_restore() {
    let (_tmp, root) = root();
    let state = line(&format!(
        r#"{{"v":1,"panels":[{{"folder":"{root}/many"}},{{"folder":"{root}/alpha"}}],
           "dialog":{{"kind":"make_folder","text":"later"}}}}"#
    ));
    let mut b = fresh_given(&root, state);
    let event = BrowserEvent::FilesChanged {
        host: None,
        dirs: vec![root.clone()],
    };
    b.b.event(&b.drv.ctx.clone(), event);
    b.drv.until(&mut b.b, 5, |b| b.ready());
    assert!(b.b.given.is_none());
    assert!(b.b.ops.open_dialog(&b.drv.ctx).is_none(), "no dialog later");
    assert_eq!(b.b.panels[0].dir, root);
}

/// Taken while it is still being put back, a state is what was given where it is not back yet.
#[test]
fn a_state_taken_while_it_is_put_back_is_the_one_given() {
    let (_tmp, root) = root();
    let state = line(&format!(
        r#"{{"v":1,"mode":"dual","active":1,
           "panels":[{{"folder":"{root}/many","bar":"f05.txt","selected":["f01.txt"],"top":3}},
                     {{"folder":"{root}","bar":"notes.txt","tree":true}}],
           "dialog":{{"kind":"make_folder","text":"half","caret":2,"anchor":2}},
           "viewer":{{"file":"{root}/notes.txt","view":1,"place":0,"wrap":false}}}}"#
    ));
    let b = fresh_given(&root, state.clone());
    assert_eq!(b.take(), state);
}

/// The text view's wrap goes with the state while the hex view is on screen: back in the text
/// view, the receiver's is unwrapped as the sender's. An encoding the file picked (not chosen)
/// is not carried: the receiver picks it again.
#[test]
fn the_hidden_text_views_wrap_crosses_and_a_picked_encoding_does_not() {
    let (_tmp, root) = root();
    let mut a = Side::new(&root, None);
    a.settle();
    a.bar(0, "long.txt");
    a.key(Key::Enter);
    a.settle();
    a.key(Key::PageDown);
    a.settle();
    a.key(Key::W);
    a.settle();
    let on_text = a.take().viewer.expect("the viewer");
    assert_eq!(on_text.view, 1);
    assert_eq!(on_text.encoding, None, "UTF-8 was picked, not chosen");
    a.drv.press_typed(&mut a.b, Key::Num2, "2");
    a.settle();
    let state = a.take();
    let viewer = state.viewer.clone().expect("the viewer");
    assert_eq!((viewer.view, viewer.wrap), (2, Some(false)), "{viewer:?}");
    assert_eq!(viewer.encoding, None);
    let (mut b, _) = given(&root, None, through_the_line(&state));
    for side in [&mut a, &mut b] {
        side.drv.press_typed(&mut side.b, Key::Num1, "1");
        side.settle();
    }
    same_screen(&a.cells(), &b.cells());
}

/// A diff or a file at a revision is not carried: the receiver says so, and opens nothing.
#[test]
fn a_revision_viewer_is_said_not_carried() {
    let (_tmp, root) = root();
    let state = line(&format!(
        r#"{{"v":1,"panels":[{{"folder":"{root}","bar":"notes.txt"}},{{"folder":"{root}"}}],
           "viewer":{{"file":"{root}/notes.txt","revision":true}}}}"#
    ));
    let (b, _) = given(&root, None, state);
    assert!(b.b.viewer.is_none());
    let said = b.b.status_line().text;
    assert!(
        said.starts_with(&format!("{root}/notes.txt: a diff or a file at a revision")),
        "{said}"
    );
}
