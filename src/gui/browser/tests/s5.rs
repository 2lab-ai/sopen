//! Slice S5's scenarios (02 §8.1's table) through the browser's own window: F10 and Alt+F10 show
//! the tree in place of the active list and Enter lists its folder in the panel (05 `T-open`, 04
//! K-R1 slot 10, K-R3), the other side is a snapshot while it is open (02 P11), Esc during a deep
//! scan closes it at once (05 D-8), a link whose target is gone is said, F1 lists the tree's keys,
//! the first frame and the screen's info lines arrive in time (05 D-2), and a double click right
//! after a click on another row opens (the list's and the tree's).
//!
//! The tree reads the real disk of a temp folder through a [`Disk`] of the test's own — the home
//! is the folder the test names, every listing is counted, a deep scan's listings can be slowed
//! or held — on a watchdog pool of the test's own, under a host name of its own (the per-process
//! cache keeps a home per host).

use super::*;
use crate::remote::treefs::{Budget, Disk, FsStat, Listing, Live, Mounts, Resolved};
use crate::remote::treefs::{Reply, TreeFs};
use crate::remote::Stat;
use crate::tree::watchdog::{Pool, Timing};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::Condvar;
use std::time::{Duration, Instant};

/// The real disk, the home where the test says, every listing counted; a scan's listings
/// slowed by `slow` and held while `held` is set (they beat meanwhile, and end when stopped).
struct TestDisk {
    real: Arc<dyn Disk>,
    home: String,
    /// Every listing: its path and whether it was a row's (`true`) or a deep scan's.
    lists: Mutex<Vec<(String, bool)>>,
    slow: Mutex<Duration>,
    held: Mutex<bool>,
    moved: Condvar,
}

impl TestDisk {
    fn new(home: &str) -> Arc<TestDisk> {
        Arc::new(TestDisk {
            real: LocalFs::disk(),
            home: home.to_string(),
            lists: Mutex::default(),
            slow: Mutex::default(),
            held: Mutex::new(false),
            moved: Condvar::new(),
        })
    }

    fn scans(&self) -> usize {
        self.lists
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, row)| !row)
            .count()
    }

    fn hold(&self, held: bool) {
        *self.held.lock().unwrap() = held;
        self.moved.notify_all();
    }
}

impl Disk for TestDisk {
    fn list(
        &self,
        path: &str,
        budget: Option<Budget>,
        full: bool,
        guard: &[String],
        live: &Live,
    ) -> std::io::Result<Listing> {
        if (live.stop)() {
            return Err(std::io::Error::other("stopped"));
        }
        self.lists.lock().unwrap().push((path.to_string(), full));
        if !full {
            let mut held = self.held.lock().unwrap();
            while *held {
                if (live.stop)() {
                    return Err(std::io::Error::other("stopped"));
                }
                (live.beat)();
                held = self.moved.wait_timeout(held, MS(50)).unwrap().0;
            }
            drop(held);
            std::thread::sleep(*self.slow.lock().unwrap());
        }
        self.real.list(path, budget, full, guard, live)
    }

    fn resolve(&self, path: &str) -> std::io::Result<Resolved> {
        self.real.resolve(path)
    }

    fn home(&self) -> std::io::Result<Resolved> {
        self.real.resolve(&self.home)
    }

    fn mounts(&self) -> std::io::Result<Mounts> {
        self.real.mounts()
    }

    fn statvfs(&self, path: &str) -> std::io::Result<FsStat> {
        self.real.statvfs(path)
    }
}

const MS: fn(u64) -> Duration = Duration::from_millis;

/// The tree of this test: its disk and pool.
struct Tree5 {
    disk: Arc<TestDisk>,
    pool: Pool,
    /// The host name its cache is keyed by (and the title's prefix).
    host: String,
}

impl Drop for Tree5 {
    fn drop(&mut self) {
        self.disk.hold(false);
        self.pool.shutdown();
    }
}

/// The browser's tree reads `home` as the home, through a [`TestDisk`] on a pool of its own.
fn with_tree(fx: &mut Fx, home: &str) -> Tree5 {
    static N: AtomicU64 = AtomicU64::new(0);
    let host = format!("s5-{}", N.fetch_add(1, Ordering::Relaxed));
    let disk = TestDisk::new(home);
    let pool = Pool::new(Timing::default());
    let (d, p) = (disk.clone(), pool.clone());
    let connect: TreeConnect = Arc::new(move |_: &AtomicBool| {
        let fs = LocalFs::tree_with(d.clone() as Arc<dyn Disk>, p.clone());
        Ok((fs, None))
    });
    fx.b.set_tree_connect(connect, &host);
    Tree5 { disk, pool, host }
}

/// F10 (or `mods`+F10), then passes until the tree is up and settled.
fn open_tree(fx: &mut Fx, mods: Modifiers) {
    fx.drv.key(&mut fx.b, Key::F10, mods);
    fx.settle();
    assert!(fx.b.tree.is_some(), "F10 opened the tree");
}

/// The names of the tree's rows, top to bottom.
fn rows(fx: &Fx) -> Vec<String> {
    let t = fx.b.tree.as_ref().expect("the tree is open");
    t.view.rows.iter().map(|r| r.name.clone()).collect()
}

/// The name on the tree's bar.
fn tree_bar(fx: &Fx) -> String {
    let t = fx.b.tree.as_ref().expect("the tree is open");
    t.view.bar_row().map(|r| r.name.clone()).unwrap_or_default()
}

/// The tree's bar on `name`, by typing its first letter.
fn tree_bar_on(fx: &mut Fx, name: &str) {
    let first = name.chars().next().unwrap().to_string();
    for _ in 0..10 {
        if tree_bar(fx) == name {
            return;
        }
        fx.typed(&first);
    }
    panic!("{name} not reached in the tree; at {}", tree_bar(fx));
}

/// What the panel shows: its folder, its bar, its first row on screen.
fn panel_state(fx: &Fx, i: usize) -> (String, usize, usize) {
    let p = &fx.b.panels[i];
    (p.dir.clone(), p.highlight, p.top)
}

/// The F-key row has a slot labelled `label`.
fn has_label(fx: &Fx, label: &str) -> bool {
    fx.drv.find_text(&format!("{label:<7}")).is_some()
}

/// Where `text` is drawn whole on the left half (the tree's side in these tests).
fn on_left(fx: &Fx, text: &str) -> Option<Rect> {
    fx.drv
        .texts_starting_with(text)
        .into_iter()
        .find(|(r, t)| t == text && r.left() < 500.0)
        .map(|(r, _)| r)
}

/// T-open (05:37): F10 shows the tree on the active side — titled with the root, the bar on
/// the panel's folder — Enter on a row sets the panel to that folder, Esc restores the panel
/// unchanged, and Alt+F10 does the same as F10.
#[test]
fn t_open_f10_shows_the_tree_enter_lists_a_folder_esc_restores() {
    let mut fx = fixture();
    let root = fx.root.clone();
    let t5 = with_tree(&mut fx, &root);
    fx.select("alpha");
    let before = panel_state(&fx, 0);
    let right = panel_state(&fx, 1);
    for mods in [Modifiers::NONE, Modifiers::ALT] {
        open_tree(&mut fx, mods);
        let shown = fx.b.shown.clone().unwrap();
        assert_eq!(shown[0], view::Content::Tree, "{mods:?}: the active side");
        assert_eq!(
            shown[1],
            view::Content::List,
            "{mods:?}: the other side kept"
        );
        assert_eq!(rows(&fx), ["~", "alpha", "beta"], "{mods:?}");
        // The bar starts on the panel's folder, the home here (05 §1).
        assert_eq!(tree_bar(&fx), "~");
        let title = format!("{}:{root}", t5.host);
        assert!(
            fx.drv
                .find_text(&chrome::frame_title(&fx.drv.ctx, &title, 496.0))
                .is_some(),
            "{mods:?}: the frame's title is the root"
        );
        assert!(on_left(&fx, "alpha").is_some() && on_left(&fx, "~").is_some());
        assert!(
            has_label(&fx, "Tree") && has_label(&fx, "Help"),
            "1Help 10Tree"
        );
        assert!(!has_label(&fx, "Copy"), "the list's slots are gone");
        // A click on the other side's list moves nothing there: it is a snapshot (O4).
        let beta = fx
            .drv
            .texts_starting_with("beta")
            .into_iter()
            .find(|(r, t)| t == "beta" && r.left() >= 500.0)
            .map(|(r, _)| r.center())
            .expect("beta in the right panel");
        fx.drv.click(&mut fx.b, beta);
        assert_eq!((fx.b.active, panel_state(&fx, 1)), (0, right.clone()));
        // Esc: the panel as it was.
        fx.key(Key::Escape);
        assert!(fx.b.tree.is_none(), "{mods:?}: Esc closed the tree");
        assert_eq!(panel_state(&fx, 0), before, "{mods:?}: unchanged");
        assert_eq!(fx.b.shown.clone().unwrap()[0], view::Content::List);
        // The same key closes it too.
        open_tree(&mut fx, mods);
        fx.drv.key(&mut fx.b, Key::F10, mods);
        assert!(fx.b.tree.is_none(), "{mods:?}+F10 closed the tree");
        assert_eq!(panel_state(&fx, 0), before);
    }
    // Enter on a row: the panel lists that folder, the tree is gone.
    open_tree(&mut fx, Modifiers::NONE);
    fx.key(Key::ArrowDown);
    assert_eq!(tree_bar(&fx), "alpha");
    fx.key(Key::Enter);
    fx.settle();
    assert!(fx.b.tree.is_none(), "Enter closed the tree");
    assert_eq!(fx.b.panels[0].dir, format!("{root}/alpha"));
    assert_eq!(fx.names(0), ["..", "inner.txt"]);
    assert_eq!(panel_state(&fx, 1), right, "the other panel is untouched");
    // From the right panel the tree opens on the right.
    fx.key(Key::Tab);
    open_tree(&mut fx, Modifiers::NONE);
    assert_eq!(fx.b.tree.as_ref().unwrap().side, 1);
    assert_eq!(fx.b.shown.clone().unwrap()[1], view::Content::Tree);
    fx.key(Key::Escape);
    assert!(fx.b.tree.is_none());
}

/// T-open over SFTP (05 §6.4): the tree on a worker of its own over this machine's `sftp-server`
/// started in the folder (`-d`; the home is `realpath(".")`), the remote rules: titled with the
/// root, the bar on the panel's folder, Enter lists the folder in the panel. The window's own
/// connection (`tree_connect`'s ssh and `ssh -G`) is the one part not run here.
#[test]
fn t_open_over_sftp() {
    if SftpRemote::local_server().is_none() {
        return;
    }
    let mut fx = fixture();
    let root = fx.root.clone();
    let host = "s5-sftp-tree";
    let home = root.clone();
    let connect: TreeConnect = Arc::new(move |_: &AtomicBool| {
        let conn = SftpRemote::local_server_args(&["-d", &home], None)
            .ok_or_else(|| anyhow::anyhow!("no sftp-server"))?;
        Ok((Box::new(conn).into_tree_renewing(Box::new(|| None)), None))
    });
    fx.b.set_tree_connect(connect, host);
    open_tree(&mut fx, Modifiers::NONE);
    assert_eq!(rows(&fx), ["~", "alpha", "beta"]);
    assert_eq!(tree_bar(&fx), "~");
    let title = format!("{host}:{root}");
    assert!(fx
        .drv
        .find_text(&chrome::frame_title(&fx.drv.ctx, &title, 496.0))
        .is_some());
    fx.key(Key::ArrowDown);
    fx.key(Key::Enter);
    fx.settle();
    assert!(fx.b.tree.is_none());
    assert_eq!(fx.b.panels[0].dir, format!("{root}/alpha"));
}

/// 05 §1: the bar starts on the panel's folder with its row in the middle — also when the
/// folders 05 §5 opens by themselves add rows above it after the first frame.
#[test]
fn the_bar_starts_in_the_middle_once_the_tree_is_built() {
    let mut fx = fixture();
    let root = fx.root.clone();
    let home = format!("{root}/home");
    for i in 0..10 {
        for j in 0..2 {
            std::fs::create_dir_all(format!("{home}/a{i}/s{j}")).unwrap();
            std::fs::create_dir_all(format!("{home}/z{i}/s{j}")).unwrap();
        }
    }
    std::fs::create_dir_all(format!("{home}/m/x")).unwrap();
    std::fs::create_dir_all(format!("{home}/m/y")).unwrap();
    let _t5 = with_tree(&mut fx, &home);
    fx.b.list(Some(0), format!("{home}/m"), None, None);
    fx.settle();
    open_tree(&mut fx, Modifiers::NONE);
    fx.drv.idle(&mut fx.b, 2);
    let t = fx.b.tree.as_ref().unwrap();
    let height = t.view.height();
    assert!(
        t.view.rows.len() > height,
        "taller than the screen: {:?}",
        rows(&fx)
    );
    assert_eq!(tree_bar(&fx), "m");
    let place = t.view.bar - t.view.top;
    assert!(
        place.abs_diff(height / 2) <= 1,
        "the bar's row {place} of {height} on screen"
    );
}

/// 04 K-R1 (slot 10) and K-R2: the list's row names `10Tree`; a click on the slot, or
/// Cmd/Ctrl+0, does what F10 does — opens the tree, and in the tree closes it.
#[test]
fn k_r1_slot_10_is_the_tree() {
    let mut fx = fixture();
    let root = fx.root.clone();
    let _t5 = with_tree(&mut fx, &root);
    assert!(has_label(&fx, "Tree"), "10Tree in the list's row");
    fx.drv.click_text(&mut fx.b, "Tree   ");
    fx.settle();
    assert!(fx.b.tree.is_some(), "slot 10 opened the tree");
    fx.drv.click_text(&mut fx.b, "Tree   ");
    fx.settle();
    assert!(fx.b.tree.is_none(), "slot 10 closed it");
    fx.drv
        .key(&mut fx.b, Key::Num0, crate::gui::testkit::command(false));
    fx.settle();
    assert!(fx.b.tree.is_some(), "Cmd/Ctrl+0 opened it");
    fx.drv
        .key(&mut fx.b, Key::Num0, crate::gui::testkit::command(false));
    assert!(fx.b.tree.is_none(), "Cmd/Ctrl+0 closed it");
}

/// 02 P11, §3.1 O4: while the tree is open the other side is the snapshot taken when it opened — a
/// preview stays on screen, its preparation is not restarted or cancelled, and moving the
/// tree's bar prepares nothing; closing the tree applies the rules again at once.
#[test]
fn o4_the_other_side_is_a_snapshot_while_the_tree_is_open() {
    let mut fx = fixture();
    let root = fx.root.clone();
    let _t5 = with_tree(&mut fx, &root);
    fx.select("notes.txt");
    fx.drv.idle(&mut fx.b, 12);
    fx.settle();
    let notes = format!("{root}/notes.txt");
    let previewed = [view::Content::List, view::Content::Preview(notes.clone())];
    assert_eq!(fx.b.shown.clone().unwrap(), previewed);
    let spawned = fx.b.preview.spawned();
    open_tree(&mut fx, Modifiers::NONE);
    let snapshot = [view::Content::Tree, view::Content::Preview(notes.clone())];
    assert_eq!(fx.b.shown.clone().unwrap(), snapshot);
    for key in [
        Key::ArrowDown,
        Key::ArrowDown,
        Key::ArrowUp,
        Key::End,
        Key::Home,
    ] {
        fx.key(key);
        fx.drv.idle(&mut fx.b, 12);
        assert_eq!(fx.b.shown.clone().unwrap(), snapshot, "{key:?}");
        assert!(
            fx.drv.find_text("plain notes").is_some(),
            "{key:?}: the preview shows"
        );
    }
    assert_eq!(fx.b.preview.spawned(), spawned, "nothing prepared again");
    assert!(!fx.b.preview.cancelling());
    fx.key(Key::Escape);
    assert_eq!(
        fx.b.shown.clone().unwrap(),
        previewed,
        "the rules again, at once"
    );
    assert_eq!(fx.b.preview.spawned(), spawned);
}

/// D-8: Esc during a deep scan closes the tree at once, and no listing is issued afterwards
/// (the pool's calls, counted by the disk).
#[test]
fn d8_esc_during_a_deep_scan_closes_at_once_and_lists_nothing_more() {
    let mut fx = fixture();
    let root = fx.root.clone();
    for i in 0..60 {
        for j in 0..10 {
            std::fs::create_dir_all(format!("{root}/big/b{i:02}/c{j}")).unwrap();
        }
    }
    let t5 = with_tree(&mut fx, &root);
    // 660 listings at 25 ms each on 4 workers: over 4 s, however loaded the machine — the scan
    // is still running when Esc comes.
    *t5.disk.slow.lock().unwrap() = MS(25);
    fx.relist_root();
    open_tree(&mut fx, Modifiers::NONE);
    assert!(rows(&fx).contains(&"big".to_string()), "{:?}", rows(&fx));
    // The scan of `big` (closed: 40 subfolders) is running.
    fx.drv.until(&mut fx.b, 10, |_| t5.disk.scans() >= 10);
    let began = Instant::now();
    fx.drv.key(&mut fx.b, Key::Escape, Modifiers::NONE);
    assert!(fx.b.tree.is_none(), "Esc closed the tree");
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "{:?}",
        began.elapsed()
    );
    // A listing that had passed its last look at the stop when the tree closed ends now.
    std::thread::sleep(MS(50));
    let at_close = t5.disk.scans();
    std::thread::sleep(MS(300));
    assert_eq!(t5.disk.scans(), at_close, "no listing after the close");
    assert!(
        at_close < 660,
        "the scan was still running: {at_close} of 660"
    );
}

/// 05 §6.3: Esc during an owner's own work (Space's scan) stops that work and leaves the tree
/// open; the next Esc closes it — F10 closes it whatever runs.
#[test]
fn esc_stops_spaces_scan_first_then_closes() {
    let mut fx = fixture();
    let root = fx.root.clone();
    for i in 0..3 {
        std::fs::create_dir_all(format!("{root}/alpha/d{i}/e")).unwrap();
    }
    let t5 = with_tree(&mut fx, &root);
    fx.relist_root();
    open_tree(&mut fx, Modifiers::NONE);
    t5.disk.hold(true);
    let busy = |b: &Browser| match b.tree.as_ref().map(|t| &t.model) {
        Some(TreeModel::Open(model)) => model.owner_busy(),
        _ => false,
    };
    tree_bar_on(&mut fx, "alpha");
    fx.key(Key::Space);
    fx.drv.until(&mut fx.b, 10, busy);
    fx.key(Key::Escape);
    assert!(
        fx.b.tree.is_some(),
        "the first Esc stops the scan, the tree stays"
    );
    assert!(!busy(&fx.b));
    fx.key(Key::Escape);
    assert!(fx.b.tree.is_none(), "the second Esc closes it");
    // F10 closes even while Space's scan runs.
    open_tree(&mut fx, Modifiers::NONE);
    tree_bar_on(&mut fx, "alpha");
    fx.key(Key::Space);
    fx.drv.until(&mut fx.b, 10, busy);
    fx.key(Key::F10);
    assert!(fx.b.tree.is_none(), "F10 closed the tree");
    t5.disk.hold(false);
}

/// 05 §1: Enter on a link lists its target, resolved; a link whose target is gone says so on
/// the status row, and the tree stays.
#[test]
fn enter_on_a_link_lists_its_target_or_says_it_is_gone() {
    let mut fx = fixture();
    let root = fx.root.clone();
    std::fs::create_dir(format!("{root}/target")).unwrap();
    std::os::unix::fs::symlink(format!("{root}/target"), format!("{root}/link")).unwrap();
    std::fs::create_dir(format!("{root}/vanishing")).unwrap();
    std::os::unix::fs::symlink(format!("{root}/vanishing"), format!("{root}/gone")).unwrap();
    let _t5 = with_tree(&mut fx, &root);
    fx.relist_root();
    open_tree(&mut fx, Modifiers::NONE);
    assert!(fx.drv.find_text("<LINK>").is_some(), "{:?}", rows(&fx));
    // The target goes after the tree has listed the link.
    std::fs::remove_dir(format!("{root}/vanishing")).unwrap();
    tree_bar_on(&mut fx, "gone");
    fx.key(Key::Enter);
    fx.wait_for(|b| b.tree.as_ref().is_some_and(|t| t.said.is_some()));
    let said = fx.b.status_line();
    assert!(said.error, "{said:?}");
    assert!(
        said.text
            .contains("gone: the link's target cannot be reached"),
        "{said:?}"
    );
    assert!(fx.b.tree.is_some(), "the tree stays");
    assert_eq!(fx.b.panels[0].dir, root, "the panel is where it was");
    // A key clears it; Enter on a link that resolves lists its target.
    tree_bar_on(&mut fx, "link");
    assert!(fx.b.tree.as_ref().unwrap().said.is_none());
    fx.key(Key::Enter);
    fx.wait_for(|b| b.tree.is_none());
    fx.settle();
    assert_eq!(fx.b.panels[0].dir, format!("{root}/target"));
}

/// F1 in the tree opens the tree's own page of the key help (it was empty before S5 landed).
#[test]
fn f1_in_the_tree_lists_the_trees_keys() {
    let mut fx = fixture();
    let root = fx.root.clone();
    let _t5 = with_tree(&mut fx, &root);
    open_tree(&mut fx, Modifiers::NONE);
    fx.key(Key::F1);
    fx.drv.idle(&mut fx.b, 2);
    assert!(crate::gui::help::is_open(&fx.drv.ctx));
    assert!(fx.drv.find_text("Keys in the directory tree").is_some());
    assert!(fx
        .drv
        .find_text("Close the tree, the list as it was")
        .is_some());
    fx.key(Key::Escape);
    assert!(!crate::gui::help::is_open(&fx.drv.ctx));
    assert!(fx.b.tree.is_some(), "Esc closed the help, not the tree");
}

/// A click on one row, then within egui's 0.3 s a double click on another: the second row
/// opens — in the list (egui alone counts the pair as a triple click and nothing opened).
#[test]
fn a_double_click_right_after_a_click_on_another_row_opens_it_in_the_list() {
    let mut fx = fixture();
    let alpha = fx.drv.find_text("alpha").unwrap().center();
    let beta = fx.drv.find_text("beta").unwrap().center();
    fx.drv.click(&mut fx.b, alpha);
    assert_eq!(fx.current(), "alpha");
    fx.drv.click(&mut fx.b, beta);
    fx.drv.click(&mut fx.b, beta);
    fx.settle();
    assert_eq!(
        fx.b.panels[0].dir,
        format!("{}/beta", fx.root),
        "beta opened"
    );
}

/// Two rows that say what each click on them was (`second_click`).
struct TwoRows {
    got: Vec<(usize, bool)>,
}

impl Window for TwoRows {
    fn ui(&mut self, ui: &mut egui::Ui) {
        for k in 0..2 {
            let rect = Rect::from_min_size(Pos2::new(0.0, 40.0 * k as f32), Vec2::new(200.0, 20.0));
            let resp = ui.interact(rect, egui::Id::unique(("s5-row", k)), Sense::click());
            if resp.clicked() {
                let opens = second_click(&resp, egui::Id::unique(("s5-pair", k)));
                self.got.push((k, opens));
            }
        }
    }

    fn ready(&self) -> bool {
        true
    }
}

/// `second_click`: a click opens a row only as the second of a quick pair on that row — after a
/// click on another row too — and a click that opened starts no pair (the third of a triple
/// click opens nothing).
#[test]
fn a_row_opens_on_the_second_click_of_a_pair_on_it() {
    let mut drv = crate::gui::testkit::Driver::new(400.0, 200.0);
    let mut w = TwoRows { got: Vec::new() };
    let (a, b) = (Pos2::new(100.0, 10.0), Pos2::new(100.0, 50.0));
    for at in [a, b, b, b] {
        drv.click(&mut w, at);
    }
    assert_eq!(w.got, [(0, false), (1, false), (1, true), (1, false)]);
    // Past the double-click time: a first click again.
    drv.idle(&mut w, 30);
    drv.click(&mut w, b);
    assert_eq!(w.got.last(), Some(&(1, false)));
}

/// The same in the tree: a click on `alpha`, then at once a double click on `beta`, lists
/// `beta` in the panel.
#[test]
fn a_double_click_right_after_a_click_on_another_row_opens_it_in_the_tree() {
    let mut fx = fixture();
    let root = fx.root.clone();
    let _t5 = with_tree(&mut fx, &root);
    open_tree(&mut fx, Modifiers::NONE);
    let alpha = on_left(&fx, "alpha").unwrap().center();
    let beta = on_left(&fx, "beta").unwrap().center();
    fx.drv.click(&mut fx.b, alpha);
    assert_eq!(tree_bar(&fx), "alpha");
    fx.drv.click(&mut fx.b, beta);
    fx.drv.click(&mut fx.b, beta);
    fx.settle();
    assert!(fx.b.tree.is_none(), "the double click went to beta");
    assert_eq!(fx.b.panels[0].dir, format!("{root}/beta"));
}

/// F10 once to warm, Esc, then F10 timed from the frame it is pressed in: until the tree's
/// first frame with rows (phase 1), and until every row on screen has its info line (05 §6.1; a
/// tag or the hidden folders' row is one).
fn d2_times(fx: &mut Fx) -> (Duration, Duration) {
    open_tree(fx, Modifiers::NONE);
    fx.key(Key::Escape);
    let press = |pressed| Event::Key {
        key: Key::F10,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    let began = Instant::now();
    fx.drv.frame(&mut fx.b, vec![press(true)], Modifiers::NONE);
    fx.drv.frame(&mut fx.b, vec![press(false)], Modifiers::NONE);
    let pass = |fx: &mut Fx| {
        fx.drv.frame(&mut fx.b, Vec::new(), Modifiers::NONE);
        std::thread::sleep(MS(1));
    };
    let drawn = |b: &Browser| b.tree.as_ref().is_some_and(|t| t.told.is_some());
    while !drawn(&fx.b) && began.elapsed() < Duration::from_secs(5) {
        pass(fx);
    }
    let first = began.elapsed();
    let lined = |b: &Browser| {
        b.tree.as_ref().is_some_and(|t| {
            t.view.on_screen().all(|at| {
                let row = &t.view.rows[at];
                matches!(row.kind, tree::RowKind::Hidden(_))
                    || row.info.dirs.is_some()
                    || matches!(row.info.size, tree::Size::Tag(_))
            })
        })
    };
    while !(drawn(&fx.b) && lined(&fx.b)) && began.elapsed() < Duration::from_secs(10) {
        pass(fx);
    }
    let info = began.elapsed();
    assert!(drawn(&fx.b) && lined(&fx.b), "{:?}", rows(fx));
    (first, info)
}

/// D-2 on this machine's real home, by hand (it reads the home; nothing is written):
/// `SOPEN_TEST_TREE_FOLDER=<a folder two levels below the home> cargo test --lib --
/// --ignored --nocapture d2_on_the_real_home`. Prints the times; 05 §8's bounds are asserted.
#[test]
#[ignore = "reads the real home; run by hand with SOPEN_TEST_TREE_FOLDER"]
fn d2_on_the_real_home() {
    let Ok(folder) = std::env::var("SOPEN_TEST_TREE_FOLDER") else {
        panic!("SOPEN_TEST_TREE_FOLDER names a folder two levels below the home");
    };
    let mut fx = fixture();
    let home = std::env::var("HOME").expect("HOME");
    let t5 = with_tree(&mut fx, &home);
    fx.b.list(Some(0), folder, None, None);
    fx.settle();
    let (first, info) = d2_times(&mut fx);
    let listed = t5
        .disk
        .lists
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, row)| *row)
        .count();
    eprintln!(
        "D-2 real home: first frame {first:?}, the screen's info lines {info:?}, {listed} row \
         listings in both opens, {} rows",
        rows(&fx).len()
    );
    assert_eq!(rows(&fx)[0], "~");
    assert!(first < Duration::from_millis(50), "first frame {first:?}");
    assert!(info < Duration::from_millis(250), "info lines {info:?}");
}

/// D-2 (05 §8), locally, on a folder two levels below the home: the tree's first frame (phase
/// 1: its rows drawn) and every info line of the screen, from F10, warm (a first F10 before).
/// The bounds here are ten times 05's (50 ms and 250 ms) for a loaded test machine; the times
/// measured are printed.
#[test]
fn d2_the_first_frame_and_the_screens_info_lines_arrive_in_time() {
    let mut fx = fixture();
    let root = fx.root.clone();
    let home = format!("{root}/home");
    for dir in ["a/b/c1", "a/b/c2", "a/x", "s1/t", "s2", "s3/u/v"] {
        std::fs::create_dir_all(format!("{home}/{dir}")).unwrap();
    }
    for (i, dir) in ["a", "a/b", "s1", "s2", "s3"].iter().enumerate() {
        std::fs::write(format!("{home}/{dir}/f{i}"), vec![b'x'; 10 * (i + 1)]).unwrap();
    }
    let _t5 = with_tree(&mut fx, &home);
    fx.b.list(Some(0), format!("{home}/a/b"), None, None);
    fx.settle();
    let (first, info) = d2_times(&mut fx);
    eprintln!("D-2: first frame {first:?}, the screen's info lines {info:?}");
    assert_eq!(rows(&fx)[0], "~", "the root reads ~");
    assert_eq!(tree_bar(&fx), "b", "the bar on the current folder");
    let t = fx.b.tree.as_ref().unwrap();
    let on = t.view.on_screen();
    assert!(on.contains(&t.view.bar), "the bar's row on screen: {on:?}");
    assert!(first < Duration::from_millis(500), "first frame {first:?}");
    assert!(info < Duration::from_millis(2500), "info lines {info:?}");
}

impl Fx {
    /// Both panels list the root again (after the test changed it on disk).
    fn relist_root(&mut self) {
        let root = self.root.clone();
        self.b.list(None, root, None, None);
        self.settle();
    }

    /// Passes until `done` (at most 20 s).
    fn wait_for(&mut self, done: impl FnMut(&Browser) -> bool) {
        self.drv.until(&mut self.b, 20, done);
    }
}

/// The model's rows as the view draws them (05 §3, §6.1): the ids round-trip, a folder opened
/// by itself is `AutoOpened`, a link `<LINK>`, an R3 row's `=… fs` becomes the view's boundary
/// figure (kept for the status row once a scan replaces the size), a scan in progress marks its
/// size, and the hidden folders' row keeps its count.
#[test]
fn the_models_rows_map_to_the_views() {
    use crate::tree::{Count, Glyph, Info, Row, RowId, RowKind, Size, Tag};
    let row = |id, kind, glyph, auto, info| Row {
        id,
        depth: 1,
        name: "n".into(),
        kind,
        glyph,
        open: glyph == Glyph::Open,
        auto,
        dimmed: matches!(kind, RowKind::Hidden(_)),
        current: false,
        path: "/n".into(),
        info,
    };
    for id in [RowId::Folder(0), RowId::Folder(7), RowId::Hidden(7)] {
        assert_eq!(tree::model_id(tree::view_id(id)), id);
    }
    assert_ne!(
        tree::view_id(RowId::Folder(7)),
        tree::view_id(RowId::Hidden(7))
    );
    let counted = Info {
        dirs: Some(Count { n: 2, more: false }),
        files: Some(Count { n: 9, more: true }),
        size: Size::Bytes {
            bytes: 60,
            more: true,
            cached: false,
            scanning: true,
        },
        fs_files: None,
    };
    let auto = tree::view_row(
        &row(
            RowId::Folder(1),
            RowKind::Folder,
            Glyph::Open,
            true,
            counted,
        ),
        None,
    );
    assert_eq!(auto.fold, tree::Fold::AutoOpened);
    assert_eq!(auto.info.dirs, Some(tree::Count { n: 2, more: false }));
    assert_eq!(auto.info.files, Some(tree::Count { n: 9, more: true }));
    assert_eq!(
        auto.info.size,
        tree::Size::Bytes {
            n: 60,
            more: true,
            cached: false
        }
    );
    assert!(auto.info.scanning && !auto.boundary);
    let link = tree::view_row(
        &row(
            RowId::Folder(2),
            RowKind::Link,
            Glyph::Link,
            false,
            Info {
                size: Size::Tag(Tag::Link),
                ..Info::default()
            },
        ),
        None,
    );
    assert_eq!(
        (link.kind, link.info.size),
        (tree::RowKind::Link, tree::Size::Tag(tree::Tag::Link))
    );
    let disk = |size| Info {
        size,
        fs_files: Some(1234),
        ..Info::default()
    };
    let fs = tree::view_row(
        &row(
            RowId::Folder(3),
            RowKind::Folder,
            Glyph::Closed,
            false,
            disk(Size::Fs { used_bytes: 87_000 }),
        ),
        None,
    );
    let figure = Some(tree::FsUse {
        bytes: 87_000,
        files: 1234,
    });
    assert!(fs.boundary);
    assert_eq!((fs.info.size, fs.info.fs), (tree::Size::Unknown, figure));
    let scanned = Size::Bytes {
        bytes: 5,
        more: true,
        cached: false,
        scanning: true,
    };
    let fs = tree::view_row(
        &row(
            RowId::Folder(3),
            RowKind::Folder,
            Glyph::Closed,
            false,
            disk(scanned),
        ),
        Some(87_000),
    );
    assert_eq!(fs.info.fs, figure, "the figure seen before the scan");
    let hidden = tree::view_row(
        &row(
            RowId::Hidden(1),
            RowKind::Hidden(3),
            Glyph::None,
            false,
            Info::default(),
        ),
        None,
    );
    assert_eq!(
        (hidden.kind, hidden.fold, hidden.hidden),
        (tree::RowKind::Hidden(3), tree::Fold::Leaf, true)
    );
}

// ---------------------------------------------------------------------------------------------
// Review round 1: what the tree started on other threads stops with it.

/// A stand-in connection, counted while it lives.
struct Conn(Arc<AtomicUsize>);

impl Conn {
    fn new(live: &Arc<AtomicUsize>) -> Self {
        live.fetch_add(1, Ordering::SeqCst);
        Conn(live.clone())
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A tree worker on a stand-in connection that answers nothing.
struct OnConn(#[allow(dead_code)] Conn);

impl TreeFs for OnConn {
    fn send(&mut self, _: u64, _: u64, _: crate::remote::treefs::Ask) {}
    fn cancel(&mut self, _: u64) {}
    fn next(&mut self, _: Duration) -> Option<Reply> {
        None
    }
    fn set_wake(&mut self, _: crate::remote::Wake) {}
    fn close(&mut self) {}
    fn held(&self) -> Option<String> {
        None
    }
    fn is_local(&self) -> bool {
        false
    }
}

/// Closing the tree while its connect blocks — by Esc, or by the window closing — builds no
/// worker on the connection that comes, and keeps no connection afterwards.
#[test]
fn closing_while_the_tree_connects_builds_nothing_and_keeps_no_connection() {
    let mut fx = fixture();
    let (live, built, returned) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    for (n, way) in ["Esc", "the window closing"].into_iter().enumerate() {
        let gate = Gate::default();
        let (g, l, b, r) = (gate.clone(), live.clone(), built.clone(), returned.clone());
        let connect: TreeConnect = Arc::new(move |closed: &AtomicBool| {
            // A connect that blocks until the test lets it return.
            g.hold();
            let made = built_unless_closed(closed, Ok(Conn::new(&l)), |conn| {
                b.fetch_add(1, Ordering::SeqCst);
                (Box::new(OnConn(conn)) as Box<dyn TreeFs>, None)
            });
            r.fetch_add(1, Ordering::SeqCst);
            made
        });
        fx.b.set_tree_connect(connect, "s5-connect");
        fx.key(Key::F10);
        gate.reached();
        let connecting = |b: &Browser| {
            matches!(
                b.tree.as_ref().map(|t| &t.model),
                Some(TreeModel::Connecting(_))
            )
        };
        assert!(connecting(&fx.b), "{way}: the tree is connecting");
        match n {
            0 => fx.key(Key::Escape),
            _ => assert!(fx.b.close_requested()),
        }
        assert!(fx.b.tree.is_none(), "{way}: the tree closed");
        gate.release();
        let deadline = Instant::now() + Duration::from_secs(10);
        while returned.load(Ordering::SeqCst) <= n {
            assert!(
                Instant::now() < deadline,
                "{way}: the connect never returned"
            );
            std::thread::sleep(MS(5));
        }
        // The connector's thread drops what it made right after returning.
        std::thread::sleep(MS(50));
        assert_eq!(built.load(Ordering::SeqCst), 0, "{way}: nothing built");
        assert_eq!(live.load(Ordering::SeqCst), 0, "{way}: no connection kept");
        fx.drv.idle(&mut fx.b, 2);
        assert!(fx.b.tree.is_none(), "{way}: nothing reached the window");
    }
}

/// The local disk as the browser's own connection, its `realpath` calls counted and its next
/// listing held at a gate when one is set.
struct Watched {
    fs: LocalFs,
    hold: Arc<Mutex<Option<Gate>>>,
    realpaths: Arc<AtomicUsize>,
}

impl RemoteFs for Watched {
    fn stat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
        self.fs.stat_opt(p)
    }
    fn lstat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
        self.fs.lstat_opt(p)
    }
    fn realpath(&mut self, p: &str) -> Result<String> {
        self.realpaths.fetch_add(1, Ordering::SeqCst);
        self.fs.realpath(p)
    }
    fn read_head(&mut self, p: &str, max: usize) -> Result<Vec<u8>> {
        self.fs.read_head(p, max)
    }
    fn list_dir(&mut self, p: &str) -> Result<Vec<String>> {
        self.fs.list_dir(p)
    }
    fn list_entries(&mut self, p: &str) -> Result<Vec<crate::remote::Entry>> {
        let gate = self.hold.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.hold();
        }
        self.fs.list_entries(p)
    }
    fn read_to(
        &mut self,
        p: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        self.fs.read_to(p, out, progress)
    }
    fn create_from(
        &mut self,
        local: &std::path::Path,
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
}

/// The fixture with a link `link` to the folder `target`, the tree open on it with the bar on
/// the link, the browser's own connection [`Watched`]: its listing gate and its `realpath`
/// count.
fn on_a_link() -> (Fx, Tree5, Arc<Mutex<Option<Gate>>>, Arc<AtomicUsize>) {
    let mut fx = fixture();
    let root = fx.root.clone();
    std::fs::create_dir(format!("{root}/target")).unwrap();
    std::os::unix::fs::symlink(format!("{root}/target"), format!("{root}/link")).unwrap();
    let t5 = with_tree(&mut fx, &root);
    fx.relist_root();
    let (hold, realpaths) = (Arc::default(), Arc::new(AtomicUsize::new(0)));
    *fx.b.conn.lock().unwrap() = Box::new(Watched {
        fs: LocalFs::new(),
        hold: Arc::clone(&hold),
        realpaths: realpaths.clone(),
    });
    open_tree(&mut fx, Modifiers::NONE);
    tree_bar_on(&mut fx, "link");
    (fx, t5, hold, realpaths)
}

/// The right panel lists the root on the browser's connection, held at a gate there.
fn panel_listing_held(fx: &mut Fx, hold: &Arc<Mutex<Option<Gate>>>) -> Gate {
    let gate = Gate::default();
    *hold.lock().unwrap() = Some(gate.clone());
    let root = fx.root.clone();
    fx.b.relist(Some(1), root, None, None, None);
    gate.reached();
    gate
}

/// Esc while a link's target waits for the browser's connection (a panel's listing has it):
/// `realpath` is never called after the close, and the panel's next listing runs as soon as
/// the first ends — not behind the link's resolution.
#[test]
fn closing_during_a_links_resolution_calls_nothing_and_delays_no_listing() {
    let (mut fx, _t5, hold, realpaths) = on_a_link();
    let gate = panel_listing_held(&mut fx, &hold);
    fx.key(Key::Enter);
    assert!(
        fx.b.tree.as_ref().is_some_and(|t| t.target.is_some()),
        "the link's target is being resolved"
    );
    fx.key(Key::Escape);
    assert!(fx.b.tree.is_none(), "Esc closed the tree");
    // The left panel's listing queues behind the held one; then the held one ends.
    let root = fx.root.clone();
    fx.b.relist(Some(0), root, None, None, None);
    gate.release();
    fx.drv.until(&mut fx.b, 10, |b| b.listing.is_empty());
    // Past the time the resolution would have waited for the connection.
    std::thread::sleep(LINK_WAIT + MS(200));
    assert_eq!(
        realpaths.load(Ordering::SeqCst),
        0,
        "no realpath after the close"
    );
}

/// A link's target waits a moment at most for the browser's connection, never queued behind
/// the panel's listing: it gives up without calling `realpath` and says so; once the
/// connection is free, Enter resolves it and the panel lists the target.
#[test]
fn a_links_resolution_gives_up_while_the_panel_has_the_connection() {
    let (mut fx, _t5, hold, realpaths) = on_a_link();
    let gate = panel_listing_held(&mut fx, &hold);
    fx.key(Key::Enter);
    fx.drv.until(&mut fx.b, 5, |b| {
        b.tree.as_ref().is_some_and(|t| t.said.is_some())
    });
    let said = fx.b.status_line();
    assert!(said.error && said.text.contains("busy"), "{said:?}");
    assert_eq!(realpaths.load(Ordering::SeqCst), 0);
    assert!(fx.b.tree.is_some(), "the tree stays");
    gate.release();
    fx.drv.until(&mut fx.b, 10, |b| b.listing.is_empty());
    fx.key(Key::Enter);
    fx.drv.until(&mut fx.b, 10, |b| b.tree.is_none());
    fx.settle();
    assert_eq!(realpaths.load(Ordering::SeqCst), 1);
    assert_eq!(fx.b.panels[0].dir, format!("{}/target", fx.root));
}
