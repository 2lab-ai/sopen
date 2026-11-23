//! Slice S2's scenarios (02 §8.1's table) through the browser's own window: the git panel on
//! the other side of a folder in a work tree (02 P8), the pin and the keyboard in it (P9, P10),
//! the graph's first page and its paging (03 G-P1, G-P2), refresh (03 §5), the details pane, a
//! diff and a file at a revision opened read-only (G-T1–G-T4), compare, find and the filters
//! (G-F1, G-F2), and `SOPEN_LIST=git`'s text (G-D1, G-D2, G-G1).
//!
//! Every repository is a fixture of the test's own (`git::history::fixture`): git with no config
//! of the machine's, a fixed identity, fixed dates. A test skips with a note where there is no
//! git, and fails under `SOPEN_REQUIRE_GIT=1` (CI).

use super::*;
use crate::git::history::fixture::{self, Repo};
use std::time::{Duration, Instant};

/// How long the bar rests before the other side follows it, in passes of the test kit (its
/// clock runs a frame at a time).
const REST: usize = 12;

/// The browser on `repo`'s top level (or `folder` in it), its git the fixture's own.
struct Gx {
    repo: Repo,
    fx: Fx,
}

impl Gx {
    fn new(repo: Repo) -> Gx {
        Self::at(repo, None)
    }

    /// The browser opened on `sub` of the repository (its top level with `None`).
    fn at(repo: Repo, sub: Option<&str>) -> Gx {
        let tmp = tempfile::tempdir().unwrap();
        let root = match sub {
            Some(sub) => format!("{}/{sub}", repo.top()),
            None => repo.top().to_string(),
        };
        let cache = tmp.path().join("cache");
        let launched: Launched = Arc::default();
        let record = launched.clone();
        let local = repo.local();
        let git: GitConnect = Arc::new(move || Ok(GitRunner::local(local.clone())));
        let b = Browser::new(Start {
            fs: Box::new(LocalFs::new()),
            connect: local_connect(),
            label: None,
            destination: None,
            abs: root.clone(),
            cache: cache.clone(),
            state_dir: tmp.path().join("state"),
            launch: Arc::new(move |abs: &str, is_dir: bool| {
                record.lock().unwrap().push((abs.to_string(), is_dir));
                Ok(format!("opened {abs}"))
            }),
            git: Some(git),
        });
        let mut fx = Fx {
            _tmp: tmp,
            root,
            cache,
            drv: Driver::new(1000.0, 700.0),
            b,
            launched,
        };
        fx.settle();
        let mut gx = Gx { repo, fx };
        gx.idle(2);
        gx
    }

    fn b(&self) -> &Browser {
        &self.fx.b
    }

    fn idle(&mut self, n: usize) {
        self.fx.drv.idle(&mut self.fx.b, n);
    }

    /// Passes until `done` (at most 20 s), then the window settles.
    fn until(&mut self, what: &str, done: impl Fn(&Browser) -> bool) {
        let t = Instant::now();
        while !done(&self.fx.b) {
            assert!(
                t.elapsed() < Duration::from_secs(20),
                "{what}: not within 20 s; shown {:?}, status {:?}, the last git command {:?}",
                self.fx.b.shown,
                self.fx.b.status_line().text,
                self.fx
                    .b
                    .git_runner()
                    .and_then(|r| r.log().last())
                    .map(|e| e.debug_line())
            );
            self.fx
                .drv
                .frame(&mut self.fx.b, Vec::new(), Modifiers::NONE);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn settle(&mut self) {
        self.fx.settle();
        self.idle(2);
    }

    /// The bar rests where it is.
    fn rest(&mut self) {
        self.idle(REST);
        self.settle();
    }

    fn key(&mut self, key: Key) {
        self.fx.key(key);
    }

    fn with(&mut self, key: Key, modifiers: Modifiers) {
        self.fx.drv.key(&mut self.fx.b, key, modifiers);
    }

    fn cmd(&mut self, key: Key) {
        self.with(key, Modifiers::COMMAND);
    }

    fn shown(&self) -> [view::Content; 2] {
        self.fx.b.shown.clone().expect("a display")
    }

    fn top(&self) -> String {
        self.repo.top().to_string()
    }

    fn git(&self) -> view::Content {
        view::Content::Git(self.top())
    }

    /// The git panel of the repository.
    fn panel(&self) -> &RepoPanel {
        let i = self.fx.b.git.panel(self.repo.top()).expect("a git panel");
        &self.fx.b.git.panels[i]
    }

    fn panel_mut(&mut self) -> &mut RepoPanel {
        let i = self.fx.b.git.panel(self.repo.top()).expect("a git panel");
        &mut self.fx.b.git.panels[i]
    }

    /// The rows the panel shows, as `SOPEN_LIST=git` would print their subjects.
    fn subjects(&self) -> Vec<String> {
        let view = &self.panel().view;
        (0..view.rows().len())
            .filter_map(|i| view.line(i).map(|r| r.subject))
            .collect()
    }

    /// The git panel shows on the right with its first page.
    fn wait_for_the_panel(&mut self) {
        let top = self.top();
        self.until("the git panel with its rows", |b| {
            b.shown
                .as_ref()
                .is_some_and(|s| s[1] == view::Content::Git(top.clone()))
                && b.git
                    .panel(&top)
                    .is_some_and(|i| !b.git.panels[i].view.rows().is_empty())
        });
        self.settle();
    }

    /// Tab into the git panel on the right.
    fn tab_into_git(&mut self) {
        self.key(Key::Tab);
        self.settle();
        assert!(self.fx.b.git_focused().is_some(), "the keyboard in Git");
    }

    /// The panel's bar on the row whose subject is `subject`.
    fn bar_to(&mut self, subject: &str) {
        let at = self
            .subjects()
            .iter()
            .position(|s| s == subject)
            .unwrap_or_else(|| panic!("{subject} not in {:?}", self.subjects()));
        let key = git_panel::RowKey::of(&self.panel().view.rows()[at]);
        assert!(self.panel_mut().view.select(key));
        self.idle(2);
    }

    fn painted(&self, text: &str) -> bool {
        self.fx.drv.find_text(text).is_some()
    }

    fn on_right(&self, text: &str) -> bool {
        self.fx
            .drv
            .texts_starting_with(text)
            .iter()
            .any(|(r, _)| r.left() >= 500.0)
    }
}

/// A small history (G-G1's shape, the remote aside): `main` with three commits, a merged
/// `feature`, an annotated tag `v1.0` and a lightweight `light`, an image, a folder; then a
/// stash holding an untracked `u.txt`, and two uncommitted changes.
fn small(git: &std::path::Path) -> Repo {
    let repo = Repo::new(git);
    repo.write("a.txt", "one\n");
    repo.write("sub/s.txt", "in a folder\n");
    repo.commit("first");
    repo.run(&["branch", "feature"]);
    repo.write("b.txt", "two\n");
    let png = |shade: u8| {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_fn(16, 12, |x, y| {
            image::Rgb([x as u8 * 16, y as u8 * 20, shade])
        })
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
        out.into_inner()
    };
    repo.write("picture.png", png(10));
    repo.commit("second");
    repo.run(&["tag", "light"]);
    repo.run(&["checkout", "-q", "feature"]);
    repo.write("f.txt", "feature\n");
    repo.commit("feature work");
    repo.run(&["checkout", "-q", "main"]);
    repo.run(&[
        "merge",
        "-q",
        "--no-ff",
        "-m",
        "Merge branch 'feature'",
        "feature",
    ]);
    repo.run(&["tag", "-a", "-m", "version one", "v1.0"]);
    repo.write("a.txt", "one\nmore\n");
    repo.write("picture.png", png(200));
    repo.commit("third");
    // A stash with an untracked file, while a working u.txt holds other bytes.
    repo.write("u.txt", "stashed bytes\n");
    repo.write("a.txt", "one\nmore\nstashed\n");
    repo.run(&["stash", "push", "-q", "-u", "-m", "saved"]);
    repo.write("u.txt", "working bytes\n");
    repo.write("a.txt", "one\nmore\nchanged\n");
    repo
}

/// The fixture's git, or the test skips (fails under `SOPEN_REQUIRE_GIT=1`).
macro_rules! git_or_skip {
    () => {
        match fixture::git() {
            Some(git) => git,
            None => return,
        }
    };
}

/// 02 P8: in a work tree the bar on `..` shows the git panel on the other side; on a file, its
/// preview; Cmd/Ctrl+G pins the git panel, which stays while the bar moves over files; Esc
/// unpins it and the preview is back.
#[test]
fn p8_the_bar_on_a_folder_shows_git_and_the_pin_keeps_it() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    assert_eq!(g.fx.current(), "..");
    g.wait_for_the_panel();
    assert_eq!(g.shown(), [view::Content::List, g.git()]);
    assert!(g.on_right("third"), "the graph's rows on the right");
    // The uncommitted row first, the stash row above its base (03 §4.3).
    assert_eq!(
        g.subjects()[..4],
        [
            "Uncommitted changes (2)",
            "On main: saved",
            "third",
            "Merge branch 'feature'"
        ]
    );
    // The bar on a file: its preview.
    g.fx.select("b.txt");
    g.rest();
    assert!(matches!(&g.shown()[1], view::Content::Preview(p) if p.ends_with("/b.txt")));
    // Pinned: the git panel stays while the bar moves over files.
    g.cmd(Key::G);
    g.settle();
    assert_eq!(g.shown()[1], g.git());
    for name in ["a.txt", "picture.png", "b.txt"] {
        g.fx.select(name);
        g.rest();
        assert_eq!(g.shown()[1], g.git(), "pinned, the bar on {name}");
    }
    // Esc unpins: the preview again.
    g.key(Key::Escape);
    g.rest();
    assert!(matches!(&g.shown()[1], view::Content::Preview(p) if p.ends_with("/b.txt")));
}

/// 02 P9: List → Cmd/Ctrl+G → Tab: the keyboard is in Git and the other side still shows the
/// origin list; Tab back: the list, its bar where it was; Tab, Esc: unpinned, the keyboard in the
/// origin list. In a folder outside the repository the pin is refused; pinned inside, then off
/// to a folder outside: the pinned panel stays, titled with its repository; Tab into it, Esc: the
/// side shows that folder's own list (O7).
#[test]
fn p9_the_pin_and_the_keyboard_in_it() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    g.fx.select("b.txt");
    g.rest();
    g.cmd(Key::G);
    g.settle();
    g.tab_into_git();
    assert_eq!(
        g.shown(),
        [view::Content::List, g.git()],
        "O3: the origin list"
    );
    assert_eq!(
        g.b().status_line().text,
        git_panel_hint(),
        "the panel's own words"
    );
    g.key(Key::Tab);
    g.settle();
    assert!(g.b().git_focused().is_none());
    assert_eq!(g.fx.current(), "b.txt", "the bar where it was");
    g.tab_into_git();
    g.key(Key::Escape);
    g.rest();
    assert!(
        g.b().git_focused().is_none() && g.b().git.pin.is_none(),
        "unpinned"
    );
    assert!(matches!(&g.shown()[1], view::Content::Preview(_)));
    // Outside the repository: nothing to pin.
    let outside = tempfile::tempdir().unwrap();
    let out = std::fs::canonicalize(outside.path()).unwrap();
    let out = out.to_str().unwrap().to_string();
    g.fx.b.list(Some(0), out.clone(), None, None);
    g.settle();
    g.until("the folder detected", |b| {
        b.git.detections.contains_key(&out)
    });
    g.cmd(Key::G);
    g.settle();
    assert!(g.b().git.pin.is_none());
    assert!(
        g.b().status_line().text.contains("nothing to pin"),
        "{}",
        g.b().status_line().text
    );
    // Pinned inside, then outside: the panel stays, titled with its repository.
    let top = g.top();
    g.fx.b.list(Some(0), top.clone(), None, None);
    g.settle();
    g.cmd(Key::G);
    g.settle();
    g.fx.b.list(Some(0), out.clone(), None, None);
    g.settle();
    g.rest();
    assert_eq!(g.shown(), [view::Content::List, g.git()]);
    assert!(
        g.painted(&top) || g.on_right(&top[..top.len().min(20)]),
        "titled"
    );
    g.tab_into_git();
    g.key(Key::Escape);
    g.rest();
    assert_eq!(g.shown(), [view::Content::List, view::Content::List], "O7");
}

/// The git panel's hint, said while nothing else is.
fn git_panel_hint() -> String {
    git_panel::GitView::new("t").status_line().text
}

/// 02 P10: Tab into an automatic git panel (O6), F9 → both sides show lists and the keyboard is
/// in the origin list; with a pin, F9 keeps the pinned side's panel and the keyboard in it.
#[test]
fn p10_f9_from_an_automatic_panel_gives_the_keyboard_back() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    g.tab_into_git();
    g.key(Key::F9);
    g.settle();
    assert_eq!(g.b().mode, view::Mode::Dual);
    assert!(g.b().git_focused().is_none());
    assert_eq!(g.shown(), [view::Content::List, view::Content::List]);
    // Pinned: the pin outranks the mode.
    g.key(Key::F9);
    g.rest();
    g.cmd(Key::G);
    g.settle();
    g.tab_into_git();
    g.key(Key::F9);
    g.settle();
    assert_eq!(g.b().mode, view::Mode::Dual);
    assert!(
        g.b().git_focused().is_some(),
        "the keyboard stays in the pinned panel"
    );
    assert_eq!(g.shown(), [view::Content::List, g.git()]);
}

/// Waits until the details pane shows `subject`'s files (`reading…` no more).
fn details_of(g: &mut Gx, file: &str) {
    let file = file.to_string();
    let top = g.top();
    g.until("the details", |b| {
        let i = b.git.panel(&top).unwrap();
        let p = &b.git.panels[i];
        p.details.is_none()
            || p.last_details.as_ref().is_some_and(|d| {
                d.groups
                    .iter()
                    .any(|gr| gr.files.iter().any(|f| f.path == file))
            })
    });
    g.settle();
    assert!(g.painted(&file) || g.on_right(&file), "{file} in the pane");
}

/// Steps the pane's bar to the file row `path` (the pane starts on its first file).
fn pane_to(g: &mut Gx, path: &str) {
    for _ in 0..30 {
        let i = g.b().git.panel(g.repo.top()).unwrap();
        let on = g.b().git.panels[i].view.pane_file().map(|f| f.path.clone());
        if on.as_deref() == Some(path) {
            return;
        }
        g.key(Key::ArrowDown);
    }
    panic!("{path} not reached in the pane");
}

/// 03 G-T1: Enter on a commit → its summary and files (`M`, `A`, counts); Enter on a file → the
/// diff in the text viewer, read-only, titled `… @ <hash>`; Esc → back, the pane's bar on that
/// file, `√` on it.
#[test]
fn g_t1_enter_on_a_commit_then_on_a_file_shows_its_diff_read_only() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    let third = g.repo.rev("main");
    g.wait_for_the_panel();
    g.tab_into_git();
    g.bar_to("third");
    g.key(Key::Enter);
    details_of(&mut g, "a.txt");
    let d = g.panel().last_details.clone().unwrap();
    assert_eq!(
        d.summary.as_ref().map(|s| s.oid.as_str().to_string()),
        Some(third.clone())
    );
    type Shown<'a> = (char, &'a str, Option<(u64, u64)>);
    let files: Vec<Shown> = d.groups[0]
        .files
        .iter()
        .map(|f| (f.letter, f.path.as_str(), f.lines))
        .collect();
    assert_eq!(
        files,
        [('M', "a.txt", Some((1, 0))), ('M', "picture.png", None)]
    );
    pane_to(&mut g, "a.txt");
    g.key(Key::Enter);
    g.until("the diff's viewer", |b| b.viewer.is_some());
    g.settle();
    let short = &third[..8];
    let title = format!("{}/a.txt @ {short}", g.top());
    assert!(
        g.fx.drv.texts().iter().any(|(_, _, t)| t.contains(&title)),
        "titled {title}"
    );
    assert!(g.painted("+more"), "the diff's added line");
    let viewer = g.b().viewer.as_ref().unwrap();
    assert!(viewer.read_only);
    assert_eq!(
        viewer.abs,
        format!("{}/a.txt", g.top()),
        "F4 opens the path now"
    );
    // Under the cache's `.revisions`, never next to the repository's files.
    let revisions = crate::cache::revisions_root(&g.fx.cache);
    assert!(revisions.exists());
    g.key(Key::Escape);
    g.settle();
    assert!(g.b().viewer.is_none());
    assert!(g.b().git_focused().is_some(), "back in the git panel");
    let i = g.b().git.panel(g.repo.top()).unwrap();
    assert_eq!(
        g.b().git.panels[i]
            .view
            .pane_file()
            .map(|f| f.path.as_str()),
        Some("a.txt"),
        "the pane's bar on that file"
    );
    assert!(g.painted("√"), "the viewed mark");
}

/// 03 G-T2 (and S2-E's note: the flag set before the first pass): Shift+F3 on an image changed
/// in a commit → the image window shows that version with Save and Save copy off, the reason
/// said; F4 opens the path now.
#[test]
fn g_t2_shift_f3_on_an_image_shows_that_version_read_only() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    g.tab_into_git();
    g.bar_to("second");
    g.key(Key::Enter);
    details_of(&mut g, "picture.png");
    pane_to(&mut g, "picture.png");
    g.with(Key::F3, Modifiers::SHIFT);
    g.until("the image's viewer", |b| b.viewer.is_some());
    g.settle();
    assert!(g.b().viewer.as_ref().unwrap().read_only);
    assert!(
        g.painted("read-only: a file at a revision has no place to be saved"),
        "the reason on the image window's row"
    );
    // Its bytes are the commit's (10 in the blue channel), not the working file's (200).
    let revisions = crate::cache::revisions_root(&g.fx.cache);
    let found = walk(&revisions)
        .into_iter()
        .find(|p| p.ends_with("picture.png"))
        .expect("the file under .revisions");
    let shown = image::open(&found).unwrap().to_rgb8();
    assert_eq!(shown.get_pixel(0, 0)[2], 10);

    // Cmd/Ctrl+S saves nothing: the reason on the status row.
    g.cmd(Key::S);
    g.idle(2);
    assert!(
        g.fx.drv
            .texts()
            .iter()
            .any(|(_, _, t)| t.contains("no place to be saved")),
        "Save says why"
    );
    assert!(image::open(&found).is_ok(), "nothing written over it");
    g.key(Key::Escape);
    g.settle();
}

/// Every file under `dir`.
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// 03 G-T3: mark a commit, F3 on another → the comparison's files; F3 with the uncommitted row
/// as one end → the work tree's changes and its untracked files.
#[test]
fn g_t3_compare_two_commits_and_the_work_tree() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    g.tab_into_git();
    g.bar_to("first");
    g.key(Key::Space);
    g.bar_to("third");
    g.key(Key::F3);
    details_of(&mut g, "b.txt");
    let d = g.panel().last_details.clone().unwrap();
    assert!(matches!(d.subject, git_panel::Subject::Compare { .. }));
    let mut paths: Vec<&str> = d.groups[0].files.iter().map(|f| f.path.as_str()).collect();
    paths.sort();
    assert_eq!(paths, ["a.txt", "b.txt", "f.txt", "picture.png"]);
    // The uncommitted row as the other end: the work tree against `first`, with u.txt.
    g.key(Key::Escape);
    g.key(Key::Escape);
    g.bar_to("Uncommitted changes (2)");
    g.key(Key::F3);
    details_of(&mut g, "u.txt");
    let d = g.panel().last_details.clone().unwrap();
    let all: Vec<(char, &str)> = d
        .groups
        .iter()
        .flat_map(|gr| gr.files.iter().map(|f| (f.letter, f.path.as_str())))
        .collect();
    assert!(all.contains(&('M', "a.txt")), "{all:?}");
    assert!(all.contains(&('?', "u.txt")), "{all:?}");
}

/// 03 G-T4: a stash holding an untracked `u.txt` while a working `u.txt` with other bytes
/// exists → Enter on the stash's `u.txt` shows the stash's bytes (`<s>^3`), not the working
/// file's.
#[test]
fn g_t4_a_stashs_untracked_file_is_the_stashs_bytes() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    g.tab_into_git();
    g.bar_to("On main: saved");
    g.key(Key::Enter);
    details_of(&mut g, "u.txt");
    let d = g.panel().last_details.clone().unwrap();
    let untracked = d
        .groups
        .iter()
        .find(|gr| gr.group == git_panel::Group::Untracked)
        .expect("the untracked group");
    assert_eq!(untracked.files[0].letter, '?');
    pane_to(&mut g, "u.txt");
    g.key(Key::Enter);
    g.until("the diff's viewer", |b| b.viewer.is_some());
    g.settle();
    assert!(g.painted("+stashed bytes"), "the stash's bytes");
    assert!(!g.painted("+working bytes"));
    g.key(Key::Escape);
    g.settle();
    // Shift+F3: the stash's own file, under .revisions.
    pane_to(&mut g, "u.txt");
    g.with(Key::F3, Modifiers::SHIFT);
    g.until("the file's viewer", |b| b.viewer.is_some());
    g.settle();
    assert!(g.painted("stashed bytes"));
}

/// 03 §9.2 (S2-D's hand-over): Cmd/Ctrl+K in the git panel opens the filters' dialog, through
/// the key table; Tags off reloads the graph without tag labels, and the title says
/// `[filtered]` (03 G-F1, end to end).
#[test]
fn command_k_opens_the_filters_and_a_filter_reloads() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    g.tab_into_git();
    assert!(g
        .panel()
        .view
        .line(3)
        .unwrap()
        .labels
        .contains(&"v1.0".to_string()));
    g.cmd(Key::K);
    g.idle(3);
    assert!(g.painted("Tags"), "the filters' dialog");
    g.fx.drv.click_text(&mut g.fx.b, "Tags");
    g.idle(2);
    g.fx.drv.click_text(&mut g.fx.b, "Apply");
    g.idle(2);
    g.until("the reload", |b| {
        let i = b.git.panel(&b.panels[0].dir).unwrap();
        b.git.panels[i]
            .stream
            .as_ref()
            .is_some_and(|s| s.asked == 0)
    });
    g.settle();
    assert!(!g.panel().view.filters().tags);
    let labels: Vec<String> = (0..g.panel().view.rows().len())
        .flat_map(|i| g.panel().view.line(i).unwrap().labels)
        .collect();
    assert!(
        !labels.iter().any(|l| l == "v1.0" || l == "light"),
        "{labels:?}"
    );
    assert!(
        g.fx.drv
            .texts()
            .iter()
            .any(|(_, _, t)| t.contains("[filtered]")),
        "the title says so"
    );
}

/// The keyboard's git panel: its `GitView`.
fn git_view(g: &Gx) -> &GitView {
    &g.panel().view
}

/// The panel's runner (the host's slots and log).
fn runner(g: &Gx) -> GitRunner {
    g.b().git_runner().expect("the runner was made")
}

/// The commits' ids of the loaded rows, in order.
fn hashes(view: &GitView) -> Vec<String> {
    view.rows()
        .iter()
        .filter_map(|r| match r {
            Row::Commit { commit, .. } => Some(commit.oid.as_str().to_string()),
            _ => None,
        })
        .collect()
}

/// Waits until the panel's stream has given every page asked for.
fn pages_in(g: &mut Gx) {
    let top = g.top();
    g.until("the pages asked for", |b| {
        let i = b.git.panel(&top).unwrap();
        let p = &b.git.panels[i];
        p.read.is_none() && p.stream.as_ref().is_none_or(|s| s.asked == 0)
    });
    g.settle();
}

/// 03 G-P1 and G-P2 in the window: on a 100 000-commit fixture the first page (500 rows) is on
/// screen within 5 s (CI's bound) and git is still running, parked; End three times reads three
/// more pages of the same stream — 2 000 commits, no hash twice, the first 500 rows and their
/// lanes unchanged.
#[test]
fn g_p1_g_p2_the_first_page_in_time_and_more_of_the_same_stream() {
    let git = git_or_skip!();
    let repo = fixture::big(&git, 100_000);
    let t = Instant::now();
    let mut g = Gx::new(repo);
    g.wait_for_the_panel();
    let first = t.elapsed();
    eprintln!("G-P1: the first page on screen {first:?} after the window opened");
    assert!(first < Duration::from_secs(5), "{first:?}");
    assert_eq!(hashes(git_view(&g)).len(), 500);
    assert_eq!(*git_view(&g).paging(), Paging::More(500));
    assert!(g.on_right("500 commits · 500 more"), "the foot");
    assert_eq!(
        runner(&g).in_flight(),
        1,
        "the log parked, holding its slot"
    );
    let first_rows = hashes(git_view(&g));
    let lanes: Vec<_> = (0..500)
        .map(|i| git_view(&g).graph().row(i).column)
        .collect();
    g.tab_into_git();
    for n in 1..=3 {
        g.key(Key::End);
        g.idle(2);
        pages_in(&mut g);
        assert_eq!(hashes(git_view(&g)).len(), 500 * (n + 1), "after End {n}");
    }
    let all = hashes(git_view(&g));
    let distinct: std::collections::HashSet<&String> = all.iter().collect();
    assert_eq!(distinct.len(), 2000, "no hash twice");
    assert_eq!(all[..500], first_rows[..], "rows 1–500 unchanged");
    let again: Vec<_> = (0..500)
        .map(|i| git_view(&g).graph().row(i).column)
        .collect();
    assert_eq!(again, lanes, "and their lanes");
    // The rows are git's own order.
    let want = g
        .repo
        .run(&["rev-list", "--date-order", "--max-count=2000", "main"]);
    let want: Vec<&str> = want.lines().collect();
    assert_eq!(all.iter().map(String::as_str).collect::<Vec<_>>(), want);
}

/// 03 §4.5: a panel hidden for longer than its bound (30 s; 300 ms here) ends its stream — the
/// host's slot is free — and shown again it reads as many pages as were loaded, the bar on its
/// commit.
#[test]
fn a_hidden_panel_ends_its_stream_and_reads_its_pages_again() {
    let git = git_or_skip!();
    let mut repo = fixture::big(&git, 3000);
    repo.write("note.txt", "a file to put the bar on\n");
    let mut g = Gx::new(std::mem::replace(&mut repo, Repo::new(&git)));
    g.fx.b.git.hidden_stream = Duration::from_millis(300);
    g.wait_for_the_panel();
    g.tab_into_git();
    g.key(Key::End);
    pages_in(&mut g);
    assert_eq!(hashes(git_view(&g)).len(), 1000);
    g.key(Key::ArrowUp);
    g.idle(2);
    let bar = git_view(&g).bar_row().map(git_panel::RowKey::of);
    let before = hashes(git_view(&g));
    g.key(Key::Tab);
    g.fx.select("note.txt");
    g.rest();
    assert!(matches!(&g.shown()[1], view::Content::Preview(_)));
    let r = runner(&g);
    g.until("the hidden panel's stream ended", |b| {
        b.git.panels[0].stream_dropped && r.in_flight() == 0
    });
    // Shown again: the same rows read again, the bar where it was.
    g.key(Key::Home);
    g.rest();
    pages_in(&mut g);
    // The pages loaded before, read again (the bar near their end then asks for one more).
    let after = hashes(git_view(&g));
    assert!(after.len() >= 1000, "{}", after.len());
    assert_eq!(after[..1000], before[..]);
    assert_eq!(git_view(&g).bar_row().map(git_panel::RowKey::of), bar);
}

/// 02 §6: a parked stream holds one of the host's 3 command slots, so a host keeps at most two
/// — a third panel's stream ends the least recently shown hidden panel's, and a read always
/// finds a slot.
#[test]
fn at_most_two_parked_streams_per_host() {
    let git = git_or_skip!();
    let repos: Vec<Repo> = (0..3).map(|_| fixture::big(&git, 3000)).collect();
    let tops: Vec<String> = repos.iter().map(|r| r.top().to_string()).collect();
    let first = Repo::new(&git);
    first.commit("first");
    let mut g = Gx::new(first);
    let r = {
        g.wait_for_the_panel();
        runner(&g)
    };
    for top in &tops {
        g.fx.b.list(Some(0), top.clone(), None, None);
        g.settle();
        let top2 = top.clone();
        g.until("its panel's first page", |b| {
            b.git.panel(&top2).is_some_and(|i| {
                let p = &b.git.panels[i];
                p.stream.as_ref().is_some_and(|s| s.asked == 0)
            })
        });
        g.settle();
        assert!(r.in_flight() <= 2, "{} slots held", r.in_flight());
    }
    let dropped: Vec<bool> = tops
        .iter()
        .map(|t| {
            let i = g.b().git.panel(t).unwrap();
            g.b().git.panels[i].stream_dropped
        })
        .collect();
    assert_eq!(
        dropped,
        [true, false, false],
        "the least recently shown gave way"
    );
}

/// 03 G-F2: a history search for a message only in a commit beyond the first page finds it.
#[test]
fn g_f2_history_search_finds_a_commit_beyond_the_first_page() {
    let git = git_or_skip!();
    let mut g = Gx::new(fixture::big(&git, 1200));
    g.wait_for_the_panel();
    assert!(
        !g.subjects().contains(&"commit 17".to_string()),
        "beyond the first page"
    );
    g.tab_into_git();
    g.with(Key::F, Modifiers::COMMAND | Modifiers::SHIFT);
    g.idle(3);
    assert!(g.painted("History search"));
    g.fx.typed("^commit 17$");
    g.key(Key::Enter);
    let top = g.top();
    g.until("the results", |b| {
        let i = b.git.panel(&top).unwrap();
        b.git.panels[i]
            .search
            .as_ref()
            .is_some_and(|s| s.link.asked == 0)
    });
    g.settle();
    assert!(g.on_right("commit 17"), "found");
    assert!(g.on_right("1 found · all loaded"), "one, all of them");
}

/// 03 §5, soft: files changed inside the top level (02 §7 `FilesChanged`) → refs, HEAD and
/// stashes unchanged, so the rows stay; the uncommitted row appears (the layout runs again over
/// the loaded rows, no history read again) and the bar stays on its commit. Hard (Cmd/Ctrl+R
/// in the panel): a commit made meanwhile by another process is on top.
#[test]
fn refresh_soft_keeps_the_rows_and_hard_reads_everything_again() {
    let git = git_or_skip!();
    let repo = Repo::new(&git);
    repo.write("a.txt", "a\n");
    repo.commit("first");
    repo.write("a.txt", "b\n");
    repo.commit("second");
    let mut g = Gx::new(repo);
    g.wait_for_the_panel();
    assert_eq!(g.subjects(), ["second", "first"]);
    g.tab_into_git();
    g.bar_to("first");
    let stream = g.panel().stream.as_ref().unwrap().generation;
    let logs = runner(&g).log().entries().len();
    g.repo.write("new.txt", "new\n");
    let ctx = g.fx.drv.ctx.clone();
    let top = g.top();
    g.fx.b.event(
        &ctx,
        BrowserEvent::FilesChanged {
            host: None,
            dirs: vec![top.clone()],
        },
    );
    g.until("the soft refresh", |b| {
        let i = b.git.panel(&top).unwrap();
        b.git.panels[i].read.is_none()
    });
    g.settle();
    assert_eq!(
        g.subjects(),
        ["Uncommitted changes (1)", "second", "first"],
        "the row appears"
    );
    assert_eq!(
        g.panel().stream.as_ref().unwrap().generation,
        stream,
        "no new log"
    );
    let logged: Vec<String> = runner(&g).log().entries()[..runner(&g).log().entries().len() - logs]
        .iter()
        .map(|e| e.line.clone())
        .collect();
    assert!(!logged.iter().any(|l| l.contains(" log ")), "{logged:?}");
    assert_eq!(
        git_view(&g).bar_row().map(git_panel::RowKey::of),
        Some(git_panel::RowKey::Commit(
            Oid::parse(&g.repo.rev("HEAD~1")).unwrap()
        )),
        "the bar on its commit"
    );
    // Hard: a commit made by another process.
    g.repo.commit("third");
    g.cmd(Key::R);
    pages_in(&mut g);
    assert_eq!(g.subjects(), ["third", "second", "first"]);
}

/// 02 §9's receipt keys: `git=pin;select=REV;details=1;label=N` — pinned once detection
/// answers, the keyboard in the panel, the bar on REV, its label cursor and its details.
#[test]
fn sopen_state_git_pin_select_details_label() {
    let git = git_or_skip!();
    let repo = small(&git);
    let tmp = tempfile::tempdir().unwrap();
    let local = repo.local();
    let git_connect: GitConnect = Arc::new(move || Ok(GitRunner::local(local.clone())));
    let mut b = Browser::new(Start {
        fs: Box::new(LocalFs::new()),
        connect: local_connect(),
        label: None,
        destination: None,
        abs: repo.top().to_string(),
        cache: tmp.path().join("cache"),
        state_dir: tmp.path().join("state"),
        launch: Arc::new(|_: &str, _: bool| Ok(String::new())),
        git: Some(git_connect),
    });
    let state = crate::gui::State::parse("git=pin;select=v1.0;details=1;label=1").unwrap();
    b.scripted(&state);
    let mut drv = Driver::new(1000.0, 700.0);
    drv.until(&mut b, 20, |b| b.ready());
    drv.idle(&mut b, 2);
    assert!(b.git.pin.is_some() && b.git_focused().is_some());
    let i = b.git.panel(repo.top()).unwrap();
    let view = &b.git.panels[i].view;
    let merge = Oid::parse(&repo.rev("v1.0^{commit}")).unwrap();
    assert_eq!(
        view.bar_row().map(git_panel::RowKey::of),
        Some(git_panel::RowKey::Commit(merge.clone()))
    );
    assert!(view.details_open());
    assert_eq!(view.bar_label().map(|l| l.text), Some("v1.0".to_string()));
    let d = b.git.panels[i].last_details.clone().unwrap();
    assert_eq!(d.summary.map(|s| s.oid), Some(merge));
    // A key the browser does not know is refused before the window opens.
    assert!(crate::gui::State::parse("git=open").is_err());
    assert!(crate::gui::State::parse("dialog=drop").is_err());
}

/// `SOPEN_LIST=git`'s text for `abs` with the fixture's git.
fn listed(repo: &Repo, abs: &str) -> Vec<String> {
    let local = repo.local();
    let connect: GitConnect = Arc::new(move || Ok(GitRunner::local(local.clone())));
    git_listing(&mut LocalFs::new(), None, abs, &connect).unwrap()
}

/// 03 G-D2: in a submodule, a linked worktree and a shallow clone `SOPEN_LIST=git` prints the
/// submodule's own graph with `(submodule of …)`, the worktree's with `(worktree of …)`, the
/// shallow clone's ending in `history ends here (shallow clone)`.
#[test]
fn g_d2_submodule_worktree_and_shallow_clone() {
    let git = git_or_skip!();
    let inner = Repo::new(&git);
    inner.write("i.txt", "inner\n");
    inner.commit("inner first");
    inner.write("i.txt", "inner two\n");
    inner.commit("inner second");
    let outer = Repo::new(&git);
    outer.write("o.txt", "outer\n");
    outer.commit("outer first");
    outer.run(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        inner.top(),
        "lib",
    ]);
    outer.commit("add lib");
    let lib = format!("{}/lib", outer.top());
    let text = listed(&outer, &lib);
    assert_eq!(text[0], format!("{lib} (submodule of {})", outer.top()));
    assert!(text.iter().any(|l| l.contains("inner second")), "{text:#?}");
    assert!(
        !text.iter().any(|l| l.contains("outer first")),
        "its own graph"
    );
    // A linked worktree.
    let wt = format!("{}-wt", outer.top());
    outer.run(&["worktree", "add", "-q", "-b", "side", &wt]);
    let text = listed(&outer, &wt);
    assert!(
        text[0].starts_with(&format!("{wt} (worktree of ")),
        "{text:#?}"
    );
    assert!(text.iter().any(|l| l.contains("add lib")), "{text:#?}");
    // A shallow clone.
    let shallow = format!("{}-shallow", inner.top());
    let url = format!("file://{}", inner.top());
    inner.run(&["clone", "-q", "--depth", "1", &url, &shallow]);
    let text = listed(&inner, &shallow);
    assert_eq!(text[0], format!("{shallow} (shallow)"));
    assert_eq!(
        text.last().map(String::as_str),
        Some("1 commits · history ends here (shallow clone)")
    );
    let _ = std::fs::remove_dir_all(&wt);
    let _ = std::fs::remove_dir_all(&shallow);
}

/// 03 G-G1 in-process (the CLI's golden test is `tests/cli.rs`): the rows and labels of the
/// small fixture, the uncommitted row first, the stash above its base, both tags.
#[test]
fn g_g1_the_rows_and_labels_as_text() {
    let git = git_or_skip!();
    let repo = small(&git);
    let text = listed(&repo, repo.top());
    assert_eq!(text[0], repo.top());
    let rows: Vec<&str> = text[1..text.len() - 1].iter().map(String::as_str).collect();
    assert!(rows[0].starts_with("o ") && rows[0].contains("Uncommitted changes (2)"));
    assert!(rows[1].contains("[stash@{0}] On main: saved"), "{rows:#?}");
    assert!(
        rows[2].starts_with('@') && rows[2].contains("[►main] third"),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.contains("[v1.0] Merge branch 'feature'")),
        "{rows:#?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("[light] second")),
        "{rows:#?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("[feature] feature work")),
        "{rows:#?}"
    );
    assert_eq!(
        text.last().map(String::as_str),
        Some("5 commits · all loaded")
    );
}

/// S2-E's notes (the hex window a file at a revision gives way to; the flag set before the
/// first pass): an image at a revision that does not decode opens in the hex window, and that
/// window is read-only from its first pass — its Save says why, nothing is written.
#[test]
fn an_undecodable_image_at_a_revision_is_a_read_only_hex_window() {
    let git = git_or_skip!();
    let repo = Repo::new(&git);
    repo.write("broken.png", b"\x89PNG not really a picture");
    repo.commit("a broken picture");
    let mut g = Gx::new(repo);
    g.wait_for_the_panel();
    g.tab_into_git();
    g.bar_to("a broken picture");
    g.key(Key::Enter);
    details_of(&mut g, "broken.png");
    g.with(Key::F3, Modifiers::SHIFT);
    g.until("the viewer", |b| b.viewer.is_some());
    g.settle();
    assert!(g.b().viewer.as_ref().unwrap().read_only);
    let slot = |l: &str| g.fx.drv.find_text(&format!("{l:<7}")).is_some();
    assert!(slot("Goto") && slot("Asc/hx"), "the hex window's row");
    let file = walk(&crate::cache::revisions_root(&g.fx.cache))
        .into_iter()
        .find(|p| p.ends_with("broken.png"))
        .unwrap();
    // Typing over a byte edits nothing, and Save says why.
    g.fx.typed("ff");
    g.with(Key::S, Modifiers::COMMAND);
    g.idle(3);
    assert!(
        g.fx.drv
            .texts()
            .iter()
            .any(|(_, _, t)| t.contains("no place to be saved")),
        "Save says why"
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"\x89PNG not really a picture"
    );
}

/// 03 §6.1: an untracked folder is one row `dir/` with `►`; → lists its files, read then
/// (`ls-files --others --exclude-standard -- dir/`), after its row.
#[test]
fn an_untracked_folders_files_are_listed_when_it_is_opened() {
    let git = git_or_skip!();
    let repo = Repo::new(&git);
    repo.write("a.txt", "a\n");
    repo.commit("first");
    repo.write("new/one.txt", "1\n");
    repo.write("new/deep/two.txt", "2\n");
    let mut g = Gx::new(repo);
    g.wait_for_the_panel();
    g.tab_into_git();
    g.bar_to("Uncommitted changes (1)");
    g.key(Key::Enter);
    details_of(&mut g, "new/");
    pane_to(&mut g, "new/");
    g.key(Key::ArrowRight);
    details_of(&mut g, "new/one.txt");
    let d = g.panel().last_details.clone().unwrap();
    let untracked: Vec<&str> = d
        .groups
        .iter()
        .find(|gr| gr.group == git_panel::Group::Untracked)
        .unwrap()
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(untracked, ["new/", "new/deep/two.txt", "new/one.txt"]);
}

/// 03 G-T5's read: "Diff against the working file" follows the file's renames since the
/// revision (`diff -z --name-status --find-renames --diff-filter=R <rev>`) and diffs both paths
/// as one rename — a pure rename shows `rename from/to` and no hunk; a rename with a changed
/// line shows the rename and that hunk; neither shows the file as added. (Its menu item is
/// S3's; S2 has the read.)
#[test]
fn g_t5_a_file_renamed_since_the_revision_diffs_as_a_rename() {
    let git = git_or_skip!();
    let repo = Repo::new(&git);
    let body: String = (0..20).map(|i| format!("line {i}\n")).collect();
    let other: String = (0..20).map(|i| format!("other {i}\n")).collect();
    repo.write("old.txt", &other);
    repo.write("kept.txt", &body);
    let rev = Oid::parse(&repo.commit("first")).unwrap();
    repo.run(&["mv", "old.txt", "new.txt"]);
    repo.run(&["mv", "kept.txt", "moved.txt"]);
    repo.write("moved.txt", body.replace("line 3\n", "line three\n"));
    repo.run(&["add", "-A"]);
    let reads = RunnerGit::new(repo.runner(), Handle::new());
    let top = repo.top();
    for (then, now, hunk) in [
        ("old.txt", "new.txt", false),
        ("kept.txt", "moved.txt", true),
    ] {
        let found = history::path_now(&reads, top, &rev, then).unwrap();
        assert_eq!(found, now);
        let words = history::argv::diff_text(
            &DiffSide::Tree(rev.clone()),
            &DiffSide::WorkTree,
            &[then, &found],
        );
        let ran = history::Git::run(&reads, top, &words, b"", history::Bound::Read).unwrap();
        let diff = String::from_utf8_lossy(&ran.stdout);
        assert!(diff.contains(&format!("rename from {then}")), "{diff}");
        assert!(diff.contains(&format!("rename to {now}")), "{diff}");
        assert!(!diff.contains("new file mode"), "not added: {diff}");
        assert_eq!(diff.contains("+line three"), hunk, "{diff}");
    }
}

/// 03 §3.4 (S2-B's hand-over b): Esc in the git panel first stops what the panel waits for — a
/// file row's diff on its way here — and nothing of the panel's own chain acts with it; the next
/// Esc walks the chain (the pane's keyboard back to the graph).
#[test]
fn esc_stops_a_read_the_panel_waits_for_first() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    g.tab_into_git();
    g.bar_to("third");
    g.key(Key::Enter);
    details_of(&mut g, "a.txt");
    // A diff on its way: its handle, as `ask_reveal` keeps it.
    let handle = Handle::new();
    let (_tx, rx) = mpsc::channel();
    g.panel_mut().reveal = Some(Pending {
        rx,
        handle: handle.clone(),
    });
    g.key(Key::Escape);
    assert!(handle.is_stopped(), "Esc stopped it");
    assert_eq!(g.b().status_line().text, "stopped");
    assert!(git_view(&g).details_open(), "the pane is as it was");
    assert!(git_view(&g).pane_file().is_some(), "its keyboard too");
    // Nothing waits now: Esc is the panel's.
    g.panel_mut().reveal = None;
    g.key(Key::Escape);
    assert!(git_view(&g).details_open());
    assert!(
        g.b().status_line().text != "stopped",
        "said until the next key"
    );
}

/// S2-B's decisions 5–6: a detection that failed (an answer it could not read) is asked again on
/// the next visit of the folder; a host whose git could not be reached is asked again after the
/// list's Cmd/Ctrl+R.
#[test]
fn a_failed_detection_and_a_missing_git_are_asked_again() {
    let git = git_or_skip!();
    let repo = small(&git);
    let tmp = tempfile::tempdir().unwrap();
    let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let local = repo.local();
    let once = first.clone();
    let connect: GitConnect = Arc::new(move || {
        if once.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(Unavailable::NoGit);
        }
        Ok(GitRunner::local(local.clone()))
    });
    let b = Browser::new(Start {
        fs: Box::new(LocalFs::new()),
        connect: local_connect(),
        label: None,
        destination: None,
        abs: repo.top().to_string(),
        cache: tmp.path().join("cache"),
        state_dir: tmp.path().join("state"),
        launch: Arc::new(|_: &str, _: bool| Ok(String::new())),
        git: Some(connect),
    });
    let mut g = Gx {
        repo,
        fx: Fx {
            _tmp: tmp,
            root: String::new(),
            cache: PathBuf::new(),
            drv: Driver::new(1000.0, 700.0),
            b,
            launched: Arc::default(),
        },
    };
    let top = g.top();
    g.until("no git, said", |b| b.git.detections.contains_key(&top));
    g.settle();
    assert_eq!(
        g.b().git.detections.get(&top),
        Some(&Detection::Unavailable(Unavailable::NoGit))
    );
    assert!(
        g.b().status_line().text.contains("no git"),
        "{}",
        g.b().status_line().text
    );
    assert_eq!(g.shown(), [view::Content::List, view::Content::List]);
    // The list's Cmd/Ctrl+R: the folder and the host are asked again.
    g.cmd(Key::R);
    g.wait_for_the_panel();
    // A failed answer is not kept: the next listing of the folder asks again.
    g.fx.b.git.detections.insert(
        top.clone(),
        Detection::Unavailable(Unavailable::Failed("cut".into())),
    );
    g.fx.b.list(Some(0), top.clone(), None, None);
    g.settle();
    g.until("detected again", |b| {
        matches!(b.git.detections.get(&top), Some(Detection::WorkTree { .. }))
    });
}

/// 03 §4.5 and §5: a soft refresh that finds the refs moved starts a new stream reading as many
/// pages as were loaded, and the bar stays on its commit.
#[test]
fn a_soft_refresh_with_moved_refs_reads_the_loaded_pages_again() {
    let git = git_or_skip!();
    let mut g = Gx::new(fixture::big(&git, 1200));
    g.wait_for_the_panel();
    g.tab_into_git();
    g.key(Key::End);
    pages_in(&mut g);
    assert!(hashes(git_view(&g)).len() >= 1000);
    // The bar on the first page, 1 000 rows loaded: nothing of the bar's asks for more.
    let target = hashes(git_view(&g))[10].clone();
    g.bar_to(&format!("commit {}", 1200 - 10));
    let bar = git_view(&g).bar_row().map(git_panel::RowKey::of);
    assert_eq!(
        bar,
        Some(git_panel::RowKey::Commit(Oid::parse(&target).unwrap()))
    );
    let before = g.panel().stream.as_ref().unwrap().generation;
    // Another process commits on top: the refs moved.
    g.repo.write("new.txt", "new\n");
    g.repo.commit("made elsewhere");
    let ctx = g.fx.drv.ctx.clone();
    let top = g.top();
    g.fx.b.event(
        &ctx,
        BrowserEvent::FilesChanged {
            host: None,
            dirs: vec![top],
        },
    );
    g.until("the new stream", |b| {
        b.git.panels[0]
            .stream
            .as_ref()
            .is_some_and(|s| s.generation != before && s.asked == 0)
    });
    g.settle();
    assert_eq!(g.subjects()[0], "made elsewhere");
    assert!(
        hashes(git_view(&g)).len() >= 1000,
        "{} commits after the refresh",
        hashes(git_view(&g)).len()
    );
    assert_eq!(git_view(&g).bar_row().map(git_panel::RowKey::of), bar);
}
