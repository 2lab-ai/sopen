//! The tree model's acceptance tests (05 §8 D-1): `T-root`, `T-rows`, `T-norules`, `T-auto`,
//! `T-info`, `T-watchdog`, `T-lazy` — on temp folders through the real disk's calls, behind a
//! [`Disk`] that counts, holds or blocks them and stands in for the mount table and the home.
//! Every tree runs on a pool of its test's own, so a call a test leaves blocked holds back no
//! other test. The SFTP worker's tests are in `remote::sftp::treefs`.

use super::*;
use crate::remote::treefs::{Budget, Disk, FsStat, Live, Mount, Mounts};
use crate::remote::LocalFs;
use crate::tree::watchdog::{Pool, Timing};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

mod round1;

/// A filesystem call the fake let through: its path, and whether it was a row's listing (`full`)
/// or a scan's.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    path: String,
    full: bool,
    budget: Option<Budget>,
}

#[derive(Default)]
struct Gate {
    /// Listings of these paths wait for a release.
    paths: HashSet<String>,
    /// Every listing waits for a release.
    all: bool,
    /// Scans' listings wait for a release.
    scans: bool,
    releases: usize,
    /// Listings waiting now.
    waiting: usize,
}

/// The real disk behind a fake: a home and a mount table of the test's, listings counted and
/// held at a gate.
struct Fake {
    real: Arc<dyn Disk>,
    home: PathBuf,
    mounts: Option<Mounts>,
    lists: Mutex<Vec<Listed>>,
    gate: Mutex<Gate>,
    moved: Condvar,
    /// Each scan listing sleeps this long first (a slow disk).
    slow: Mutex<Duration>,
    /// An automatic listing stops after this many entries, never reaching the named one (a
    /// budget, scaled down).
    cut: Mutex<Option<(usize, String)>>,
}

impl Fake {
    fn new(home: &Path) -> Arc<Fake> {
        Arc::new(Fake {
            real: LocalFs::disk(),
            home: home.to_path_buf(),
            mounts: None,
            lists: Mutex::default(),
            gate: Mutex::default(),
            moved: Condvar::new(),
            slow: Mutex::default(),
            cut: Mutex::default(),
        })
    }

    fn with_mounts(home: &Path, mounts: Mounts) -> Arc<Fake> {
        Arc::new(Fake {
            mounts: Some(mounts),
            ..Arc::try_unwrap(Fake::new(home)).ok().unwrap()
        })
    }

    fn lists(&self) -> Vec<Listed> {
        self.lists.lock().unwrap().clone()
    }

    fn listed(&self, path: &Path) -> usize {
        let p = path.to_str().unwrap();
        self.lists()
            .iter()
            .filter(|l| l.full && l.path == p)
            .count()
    }

    fn gate(&self) -> std::sync::MutexGuard<'_, Gate> {
        self.gate.lock().unwrap()
    }

    fn release(&self, n: usize) {
        self.gate().releases += n;
        self.moved.notify_all();
    }

    /// Waits until `n` listings wait at the gate.
    fn wait_held(&self, n: usize, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut g = self.gate();
        while g.waiting < n {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            g = self.moved.wait_timeout(g, left).unwrap().0;
        }
        true
    }
}

impl Disk for Fake {
    fn list(
        &self,
        path: &str,
        budget: Option<Budget>,
        full: bool,
        guard: &[String],
        live: &Live,
    ) -> std::io::Result<Listing> {
        // A call no longer wanted never reaches the filesystem (the real disk checks first too).
        if (live.stop)() {
            return Err(std::io::Error::other("stopped"));
        }
        self.lists.lock().unwrap().push(Listed {
            path: path.to_string(),
            full,
            budget,
        });
        {
            let mut g = self.gate();
            if g.all || g.paths.contains(path) || (g.scans && !full) {
                g.waiting += 1;
                self.moved.notify_all();
                while g.releases == 0 {
                    g = self.moved.wait(g).unwrap();
                }
                g.releases -= 1;
                g.waiting -= 1;
            }
        }
        if !full {
            let slow = *self.slow.lock().unwrap();
            std::thread::sleep(slow);
        }
        let mut listing = self.real.list(path, budget, full, guard, live)?;
        if let (Some(_), Some((n, never))) = (budget, self.cut.lock().unwrap().clone()) {
            listing.children.retain(|c| c.name != never);
            if listing.children.len() > n {
                listing.children.truncate(n);
                listing.complete = false;
            }
        }
        Ok(listing)
    }

    fn resolve(&self, path: &str) -> std::io::Result<Resolved> {
        self.real.resolve(path)
    }

    fn home(&self) -> std::io::Result<Resolved> {
        self.real.resolve(self.home.to_str().unwrap())
    }

    fn mounts(&self) -> std::io::Result<Mounts> {
        match &self.mounts {
            Some(m) => Ok(m.clone()),
            None => Ok(Mounts {
                table: Some(Vec::new()),
                macos: false,
                statvfs: true,
            }),
        }
    }

    fn statvfs(&self, path: &str) -> std::io::Result<FsStat> {
        self.real.statvfs(path)
    }
}

/// A host name of the test's own: the process cache keeps homes and sizes per host.
fn host() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!("test-{}", N.fetch_add(1, Ordering::Relaxed))
}

/// A temp folder, resolved (macOS: `/var` → `/private/var`).
fn temp() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(tmp.path()).unwrap();
    (tmp, real)
}

fn mkdirs(root: &Path, dirs: &[&str]) {
    for d in dirs {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
}

fn pool(watchdog: Duration) -> Pool {
    Pool::new(Timing {
        watchdog,
        tick: Duration::from_millis(10),
        progress: Duration::ZERO,
    })
}

fn open(fake: &Arc<Fake>, pool: &Pool, folder: &Path, params: Params) -> Tree {
    let fs = LocalFs::tree_with(fake.clone(), pool.clone());
    Tree::open(
        fs,
        Open {
            host: host(),
            folder: folder.to_str().unwrap().to_string(),
            user: None,
            params,
        },
    )
}

/// Polls until `done` holds, at most `limit`.
fn until(tree: &mut Tree, limit: Duration, done: impl Fn(&Tree) -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        tree.poll();
        if done(tree) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tree.poll_wait(Duration::from_millis(20));
    }
}

/// Polls until the build (05 §5) is over and nothing changes for a moment.
fn settle(tree: &mut Tree) {
    assert!(
        until(tree, Duration::from_secs(10), Tree::built),
        "the tree was built: {:?}",
        tree.error()
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !tree.poll_wait(Duration::from_millis(100)) {
            return;
        }
    }
}

fn row(tree: &Tree, name: &str) -> Row {
    tree.rows()
        .into_iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no row {name}: {}", names(tree)))
}

fn names(tree: &Tree) -> String {
    tree.rows()
        .iter()
        .map(|r| {
            format!(
                "{}{}{}",
                "  ".repeat(r.depth),
                r.name,
                if r.open { "/" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn p(path: &Path) -> String {
    path.to_str().unwrap().to_string()
}

/// T-root (05 §2), the rows of its table that a local disk can hold, by the real disk's
/// resolutions and identities.
#[test]
fn t_root_is_the_home_by_identity_else_slash() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["a/b"]);
    let (_out, outside) = temp();
    std::os::unix::fs::symlink(&outside, h.join("out")).unwrap();
    let disk = LocalFs::disk();
    let home = disk.resolve(&p(&h)).unwrap();
    let root_of = |folder: &str| root::local(&disk.resolve(folder).unwrap(), Some(&home));

    // H, and H/a/b.
    let at_home = root_of(&p(&h));
    assert_eq!(
        (at_home.root.as_str(), at_home.home),
        (p(&h).as_str(), true)
    );
    assert_eq!(at_home.folder, p(&h));
    let below = root_of(&p(&h.join("a/b")));
    assert_eq!(
        (below.root.as_str(), below.folder.as_str()),
        (p(&h).as_str(), p(&h.join("a/b")).as_str())
    );
    // H/../<home name>/a, unresolved.
    let name = h.file_name().unwrap().to_str().unwrap();
    let dotted = format!("{}/../{name}/a", p(&h));
    assert_eq!(root_of(&dotted).root, p(&h));
    // A link inside H pointing outside H, entered from the panel: resolved first.
    let linked = root_of(&p(&h.join("out")));
    assert_eq!((linked.root.as_str(), linked.home), ("/", false));
    // A home of `/` gives `/`.
    let slash = disk.resolve("/").unwrap();
    assert_eq!(
        root::local(&disk.resolve(&p(&h)).unwrap(), Some(&slash)).root,
        "/"
    );

    if cfg!(target_os = "macos") {
        // `/tmp` (→ `/private/tmp`).
        let tmp = root_of("/tmp");
        assert_eq!(
            (tmp.root.as_str(), tmp.folder.as_str()),
            ("/", "/private/tmp")
        );
        // `/System/Volumes/Data` + H + `/a`: the string keeps the alias, the identity is H's.
        let data = format!("/System/Volumes/Data{}/a", p(&h));
        let resolved = disk.resolve(&data).unwrap();
        assert!(
            resolved.path.starts_with("/System/Volumes/Data/"),
            "{resolved:?}"
        );
        let via_data = root::local(&resolved, Some(&home));
        assert_eq!(
            (via_data.root.as_str(), via_data.folder.as_str()),
            (p(&h).as_str(), p(&h.join("a")).as_str())
        );
        // `/Volumes/<boot volume>` + H: a link to `/`, resolved first.
        let boot = std::fs::read_dir("/Volumes").ok().and_then(|d| {
            d.flatten()
                .map(|e| e.path())
                .find(|e| std::fs::read_link(e).is_ok_and(|t| t == Path::new("/")))
        });
        match boot {
            Some(boot) => {
                let through = format!("{}{}", boot.display(), p(&h));
                assert_eq!(root_of(&through).root, p(&h));
            }
            None => eprintln!("skipping the /Volumes row: no link to / there"),
        }
    }

    // Remotely, a component-wise prefix of the resolved paths.
    assert_eq!(root::remote("/home/abc/x", Some("/home/ab")).root, "/");
    assert_eq!(
        root::remote("/home/ab/x", Some("/home/ab")).root,
        "/home/ab"
    );
    assert_eq!(root::remote("/home/ab", Some("/home/ab")).root, "/home/ab");
    // A remote whose home is `/` (chrooted SFTP).
    let chrooted = root::remote("/x/y", Some("/"));
    assert_eq!((chrooted.root.as_str(), chrooted.home), ("/", false));
}

/// T-root through the tree: the root row reads `~`, the title is the root, the bar starts on
/// the current folder.
#[test]
fn t_root_the_tree_starts_at_the_home_with_the_bar_on_the_folder() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["a/b/c", "a/b/d"]);
    let fake = Fake::new(&h);
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h.join("a/b"), Params::default());
    settle(&mut tree);
    let rows = tree.rows();
    assert_eq!(rows[0].name, "~", "{}", names(&tree));
    assert!(tree.title().unwrap().ends_with(&format!(":{}", p(&h))));
    let bar = tree.current().unwrap();
    let at = tree.row_of(bar).unwrap();
    assert_eq!(rows[at].path, p(&h.join("a/b")));
    assert!(rows[at].current);
    pool.shutdown();
}

/// T-root through the tree on macOS: `/System/Volumes/Data` + H + `/a/b` is under the home by
/// identity — the root reads `~` and the bar is on `H/a/b` (05:64).
#[test]
fn t_root_the_data_volume_alias_is_under_the_home() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let (_tmp, h) = temp();
    mkdirs(&h, &["a/b/c", "a/b/d"]);
    let fake = Fake::new(&h);
    let pool = pool(Duration::from_secs(30));
    let alias = PathBuf::from(format!("/System/Volumes/Data{}", p(&h.join("a/b"))));
    let mut tree = open(&fake, &pool, &alias, Params::default());
    settle(&mut tree);
    assert_eq!(tree.rows()[0].name, "~", "{}", names(&tree));
    let bar = tree.row_of(tree.current().unwrap()).unwrap();
    assert_eq!(tree.rows()[bar].path, p(&h.join("a/b")));
    pool.shutdown();
}

/// T-rows (05 §3): hidden folders grouped in one dimmed row after the others; a compact chain on
/// one row; a link marked and never opened in place.
#[test]
fn t_rows_hidden_grouped_chains_compact_links_never_open() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["proj/.git", "proj/.github", "proj/src", "a/b/c"]);
    std::os::unix::fs::symlink("..", h.join("l")).unwrap();
    let fake = Fake::new(&h);
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params::default());
    settle(&mut tree);
    let proj = row(&tree, "proj");
    if !proj.open {
        tree.expand(proj.id);
        settle(&mut tree);
    }
    let rows = tree.rows();
    let at = tree.row_of(proj.id).unwrap();
    let below: Vec<&str> = rows[at + 1..at + 3]
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(below, ["src", "· 2 hidden folders"], "{}", names(&tree));
    assert!(rows[at + 2].dimmed);
    assert_eq!(rows[at + 2].kind, RowKind::Hidden(2));
    // → on the hidden row: one dimmed row each.
    tree.expand(rows[at + 2].id);
    let opened: Vec<(String, bool)> = tree.rows()[at + 2..at + 4]
        .iter()
        .map(|r| (r.name.clone(), r.dimmed))
        .collect();
    assert_eq!(opened, [(".git".into(), true), (".github".into(), true)]);

    let chain = row(&tree, "a/b/c");
    assert_eq!(chain.depth, 1);
    assert_eq!(chain.glyph, Glyph::None, "c has no subfolders");

    let link = row(&tree, "l");
    assert_eq!((link.kind, link.glyph), (RowKind::Link, Glyph::Link));
    assert_eq!(link.info.text(), "<LINK>");
    let before = tree.rows();
    assert!(!tree.expand(link.id), "→ on a link does nothing");
    assert_eq!(tree.rows(), before);
    pool.shutdown();
}

/// T-norules (05 §4): each rule and heuristic, built in a temp folder (mounts and macOS volumes
/// through a fake mount table), stays closed after F10, though each would open by 05 §5
/// otherwise; R1, R4–R7 and H3 issue no listing.
#[test]
fn t_norules_rules_and_heuristics_stay_closed() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, h) = temp();
    let (_out, outside) = temp();
    mkdirs(&outside, &["x", "y"]);
    let two = |d: &str| [format!("{d}/x"), format!("{d}/y")];
    let mut dirs: Vec<String> = Vec::new();
    for d in [
        "plain",
        "locked",
        "disk",
        "sys",
        "net",
        "auto",
        "sysvol",
        "target",
        "venv",
        ".hidden",
        "node_modules",
        "Library",
        ".Trash",
        ".hg",
    ] {
        dirs.extend(two(d));
    }
    dirs.extend(["repo.git/objects".to_string(), "repo.git/refs".to_string()]);
    mkdirs(&h, &dirs.iter().map(String::as_str).collect::<Vec<_>>());
    std::fs::write(h.join("repo.git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(
        h.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    std::fs::write(h.join("venv/pyvenv.cfg"), "home = /usr/bin\n").unwrap();
    std::os::unix::fs::symlink(&outside, h.join("link")).unwrap();
    std::fs::set_permissions(h.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_dir(h.join("locked")).is_ok();

    let at = |d: &str, fstype: &str, nobrowse: bool| Mount {
        at: p(&h.join(d)),
        fstype: fstype.to_string(),
        nobrowse,
    };
    let fake = Fake::with_mounts(
        &h,
        Mounts {
            table: Some(vec![
                at("disk", "apfs", false),
                at("sys", "proc", false),
                at("net", "nfs", false),
                at("auto", "autofs", false),
                at("sysvol", "apfs", true),
            ]),
            macos: true,
            statvfs: true,
        },
    );
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 2, e: 200 });
    settle(&mut tree);
    // The hidden ones are drawn too.
    let group = tree
        .rows()
        .into_iter()
        .find(|r| matches!(r.kind, RowKind::Hidden(_)))
        .unwrap();
    tree.expand(group.id);
    settle(&mut tree);
    std::fs::set_permissions(h.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        row(&tree, "plain").open,
        "the control opens: {}",
        names(&tree)
    );
    for name in [
        "link",
        "locked",
        "disk",
        "sys",
        "net",
        "auto",
        "sysvol",
        "target",
        "repo.git",
        "venv",
        ".hidden",
        "node_modules",
        "Library",
        ".Trash",
        ".hg",
    ] {
        if name == "locked" && readable {
            // This account (root) reads a 000 folder: it is no R2 here.
            continue;
        }
        let r = row(&tree, name);
        assert!(!r.open && !r.auto, "{name} stays closed: {}", names(&tree));
    }
    for name in ["link", "sys", "net", "auto", "sysvol", "Library", ".Trash"] {
        assert_eq!(fake.listed(&h.join(name)), 0, "{name} is not listed");
    }
    for name in [
        "disk",
        "target",
        "repo.git",
        "venv",
        "node_modules",
        "plain",
    ] {
        assert_eq!(fake.listed(&h.join(name)), 1, "{name} is listed once");
    }
    let tag = |name: &str| row(&tree, name).info.columns()[2].clone();
    assert_eq!(tag("link"), "<LINK>");
    assert_eq!(tag("sys"), "<SYS>");
    assert_eq!(tag("net"), "<NET>");
    assert_eq!(tag("auto"), "<AUTO>");
    assert_eq!(tag("sysvol"), "<SYSVOL>");
    if !readable {
        assert_eq!(tag("locked"), "?");
    } else {
        eprintln!("skipping the unreadable row's tag: this account reads a 000 folder");
    }
    // R3 is sized: once its scan runs, the folder's own size replaces `=… fs`; the used file
    // count stays for the status row.
    let disk = row(&tree, "disk").info;
    assert!(disk.fs_files.is_some(), "{disk:?}");
    assert!(
        matches!(disk.size, Size::Fs { .. } | Size::Bytes { .. }),
        "{disk:?}"
    );
    pool.shutdown();
}

/// T-norules / 05 §6.1: an R3 mount point shows at once the filesystem mounted there
/// (`=… fs`, from one `statvfs`) and its used file count, before any scan of it.
#[test]
fn t_norules_a_disk_mount_shows_its_filesystem_at_once() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["adisk/x", "adisk/y", "zz"]);
    let fake = Fake::with_mounts(
        &h,
        Mounts {
            table: Some(vec![Mount {
                at: p(&h.join("adisk")),
                fstype: "apfs".into(),
                nobrowse: false,
            }]),
            macos: false,
            statvfs: true,
        },
    );
    let pool = pool(Duration::from_secs(30));
    // A screen of one row (the root's): `adisk` is listed (one screen below it) but not on
    // screen, so not scanned.
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 2, e: 1 });
    settle(&mut tree);
    let info = row(&tree, "adisk").info;
    assert!(
        info.columns()[2].starts_with('=') && info.columns()[2].ends_with(" fs"),
        "{info:?}"
    );
    assert!(info.fs_files.is_some());
    assert!(!row(&tree, "adisk").open);
    pool.shutdown();
}

/// Builds `root/<dir>/s0 … s<n-1>` (and `root/<dir>` itself).
fn subs(root: &Path, dir: &str, n: usize) {
    std::fs::create_dir_all(root.join(dir)).unwrap();
    for i in 0..n {
        std::fs::create_dir_all(root.join(dir).join(format!("s{i}"))).unwrap();
    }
}

/// T-auto (05 §5), its first cases: the root and the path open; the current folder with 3
/// shown subfolders opens, with 5 it stays closed; a sibling of the path with 2 subfolders
/// opens, its children do not (D = 2).
#[test]
fn t_auto_the_path_opens_and_n_and_d_decide_the_rest() {
    for (q, opens) in [(3, true), (5, false)] {
        let (_tmp, h) = temp();
        subs(&h, "p/q", q);
        subs(&h, "sib", 2);
        subs(&h, "sib/s0", 2);
        subs(&h, "sib/s1", 2);
        let fake = Fake::new(&h);
        let pool = pool(Duration::from_secs(30));
        let mut tree = open(&fake, &pool, &h.join("p/q"), Params::default());
        settle(&mut tree);
        assert!(row(&tree, "~").open, "{}", names(&tree));
        assert!(row(&tree, "p").open, "{}", names(&tree));
        let current = row(&tree, "q");
        assert!(current.current);
        assert_eq!(
            current.open,
            opens,
            "q with {q} subfolders: {}",
            names(&tree)
        );
        assert_eq!(current.auto, opens);
        let sib = row(&tree, "sib");
        assert!(sib.open && sib.auto, "{}", names(&tree));
        let children: Vec<Row> = tree
            .rows()
            .into_iter()
            .filter(|r| r.path.starts_with(&format!("{}/", sib.path)) && r.depth == sib.depth + 1)
            .collect();
        assert_eq!(children.len(), 2);
        assert!(children.iter().all(|r| !r.open), "D = 2: {}", names(&tree));
        pool.shutdown();
    }
}

/// T-auto: F10 at the home itself with 30 subfolders — the root shows all 30, and what opens
/// by itself adds no more than E rows.
#[test]
fn t_auto_at_the_home_the_root_is_open_and_e_bounds_the_rest() {
    let (_tmp, h) = temp();
    for i in 0..30 {
        subs(&h, &format!("d{i:02}"), 2);
    }
    let fake = Fake::new(&h);
    let pool = pool(Duration::from_secs(30));
    let e = 36;
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 2, e });
    settle(&mut tree);
    let rows = tree.rows();
    assert!(rows[0].open && rows[0].current);
    assert_eq!(rows.iter().filter(|r| r.depth == 1).count(), 30);
    let added: usize = rows.iter().filter(|r| r.depth == 2).count();
    assert!(added <= e, "{added} rows added by 05 §5, E = {e}");
    assert!(rows.iter().any(|r| r.auto), "some open by themselves");
    pool.shutdown();
}

/// T-auto: with E = 4 and six 2-subfolder siblings only the two nearest the bar open.
#[test]
fn t_auto_the_budget_goes_to_the_rows_nearest_the_bar() {
    let (_tmp, h) = temp();
    for i in 1..=6 {
        subs(&h, &format!("x{i}"), 2);
    }
    let fake = Fake::new(&h);
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 2, e: 4 });
    settle(&mut tree);
    let open: Vec<String> = tree
        .rows()
        .into_iter()
        .filter(|r| r.auto)
        .map(|r| r.name)
        .collect();
    assert_eq!(open, ["x1", "x2"], "{}", names(&tree));
    pool.shutdown();
}

/// T-auto: a folder with 1 shown and 2 hidden subfolders opens to 2 rows; one with only hidden
/// subfolders stays closed.
#[test]
fn t_auto_hidden_subfolders_do_not_count() {
    let (_tmp, h) = temp();
    mkdirs(
        &h,
        &[
            "mixed/shown",
            "mixed/.h1",
            "mixed/.h2",
            "secret/.h1",
            "secret/.h2",
        ],
    );
    let fake = Fake::new(&h);
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params::default());
    settle(&mut tree);
    let mixed = row(&tree, "mixed");
    assert!(mixed.open && mixed.auto, "{}", names(&tree));
    let at = tree.row_of(mixed.id).unwrap();
    let rows = tree.rows();
    assert_eq!(rows[at + 1].name, "shown");
    assert_eq!(rows[at + 2].kind, RowKind::Hidden(2));
    assert!(rows.get(at + 3).is_none_or(|r| r.depth <= mixed.depth));
    assert!(!row(&tree, "secret").open, "{}", names(&tree));
    pool.shutdown();
}

/// T-info (05 §6.1): 2 subfolders and 3 files of 10, 20 and 30 bytes plus a hard link to the
/// first → `2 d 4 f 60B+` from the listing, then `60B` once scanned with nothing below.
#[test]
fn t_info_counts_from_the_listing_then_the_scan_closes_the_bound() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["f/e1", "f/e2"]);
    std::fs::write(h.join("f/ten"), [0u8; 10]).unwrap();
    std::fs::write(h.join("f/twenty"), [0u8; 20]).unwrap();
    std::fs::write(h.join("f/thirty"), [0u8; 30]).unwrap();
    std::fs::hard_link(h.join("f/ten"), h.join("f/ten-again")).unwrap();
    let fake = Fake::new(&h);
    fake.gate().scans = true;
    let pool = pool(Duration::from_secs(30));
    // D = 1: only the current folder may open by itself, so `f` stays closed and is scanned.
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    tree.view(0, 36, None);
    let f = row(&tree, "f");
    assert!(!f.open);
    assert_eq!(
        f.info.dirs.zip(f.info.files),
        Some((Count { n: 2, more: false }, Count { n: 4, more: false }))
    );
    assert!(
        matches!(
            f.info.size,
            Size::Bytes {
                bytes: 60,
                more: true,
                ..
            }
        ),
        "{:?}",
        f.info
    );
    fake.release(100);
    assert!(until(&mut tree, Duration::from_secs(10), |t| {
        matches!(
            row(t, "f").info.size,
            Size::Bytes {
                bytes: 60,
                more: false,
                scanning: false,
                ..
            }
        )
    }));
    assert_eq!(row(&tree, "f").info.text(), "2 d 4 f 60B");
    pool.shutdown();
}

/// T-info: a deep folder shows a growing `+` size that ends without `+`; scrolled away and back
/// it shows the size reached so far.
#[test]
fn t_info_a_deep_size_grows_and_survives_scrolling_away() {
    let (_tmp, h) = temp();
    let mut at = h.join("deep");
    for _ in 0..6 {
        std::fs::create_dir_all(&at).unwrap();
        std::fs::write(at.join("data"), [0u8; 100]).unwrap();
        at = at.join("d");
    }
    // Rows below `deep` so that it can be scrolled off screen.
    for i in 0..10 {
        mkdirs(&h, &[&format!("z{i}")]);
    }
    let fake = Fake::new(&h);
    fake.gate().scans = true;
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    tree.view(0, 36, None);
    let bytes = |t: &Tree| match row(t, "deep").info.size {
        Size::Bytes { bytes, more, .. } => (bytes, more),
        other => panic!("{other:?}"),
    };
    assert_eq!(bytes(&tree), (100, true));
    let mut seen = vec![100];
    for _ in 0..2 {
        assert!(fake.wait_held(1, Duration::from_secs(5)));
        fake.release(1);
        let last = *seen.last().unwrap();
        assert!(until(&mut tree, Duration::from_secs(5), |t| bytes(t).0 > last));
        seen.push(bytes(&tree).0);
        assert!(bytes(&tree).1, "still a lower bound");
    }
    assert_eq!(seen, [100, 200, 300]);
    // Off screen for more than a second: its scan stops; the figure stays.
    let deep_row = tree.row_of(row(&tree, "deep").id).unwrap();
    tree.view(deep_row + 1, 5, None);
    std::thread::sleep(Duration::from_millis(1200));
    tree.view(deep_row + 1, 5, None);
    assert!(until(&mut tree, Duration::from_secs(5), |t| {
        !matches!(row(t, "deep").info.size, Size::Bytes { scanning: true, .. })
    }));
    assert!(bytes(&tree).0 >= 300, "kept: {:?}", bytes(&tree));
    // Back on screen: the size so far, then the rest.
    tree.view(0, 36, None);
    assert!(bytes(&tree).0 >= 300);
    fake.gate().scans = false;
    fake.release(1000);
    assert!(until(&mut tree, Duration::from_secs(10), |t| bytes(t)
        == (600, false)));
    assert_eq!(row(&tree, "deep").info.columns()[2], "600B");
    pool.shutdown();
}

/// T-watchdog (05 §6.2), on the real 2 s: a listing that never returns → its row shows
/// `not answering` after 2 s; the frame keeps painting; closing takes no time; the blocked
/// thread is left behind, never joined.
#[test]
fn t_watchdog_a_listing_that_never_returns_is_left_behind() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["stuck/x", "fine/x", "fine/y"]);
    let fake = Fake::new(&h);
    fake.gate().paths.insert(p(&h.join("stuck")));
    let pool = Pool::new(Timing::default());
    let mut tree = open(&fake, &pool, &h, Params::default());
    let started = Instant::now();
    assert!(
        until(&mut tree, Duration::from_secs(5), |_| fake.gate().waiting
            == 1),
        "the listing of stuck started"
    );
    let mut slowest = Duration::ZERO;
    let shown = loop {
        let t = Instant::now();
        tree.poll();
        let rows = tree.rows();
        slowest = slowest.max(t.elapsed());
        if rows
            .iter()
            .any(|r| r.name == "stuck" && r.info.size == Size::Tag(Tag::NotAnswering))
        {
            break started.elapsed();
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{}",
            names(&tree)
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        shown >= Duration::from_millis(1900),
        "not before 2 s: {shown:?}"
    );
    assert!(
        // Far below the 2 s the call blocks: a frame never waits for it.
        slowest < Duration::from_millis(500),
        "a frame took {slowest:?}"
    );
    assert!(
        row(&tree, "fine").info.dirs.is_some(),
        "other rows were listed"
    );
    let t = Instant::now();
    tree.close();
    drop(tree);
    assert!(t.elapsed() < Duration::from_millis(500), "closing waited");
    // Calls already running when it closed end by themselves; the blocked one stays behind.
    let deadline = Instant::now() + Duration::from_secs(5);
    while pool.outstanding() != (1, 1) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        pool.outstanding(),
        (1, 1),
        "the blocked call is left behind"
    );
}

/// T-watchdog: open and close the tree ten times on folders whose listings never return → at
/// most 4 calls are ever outstanding, the fifth is never started and the status row says so;
/// one call let go returns, its slot comes back and the next listing runs.
#[test]
fn t_watchdog_at_most_four_calls_are_ever_left_behind() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["a"]);
    let fake = Fake::new(&h);
    let pool = pool(Duration::from_millis(100));
    let root = |f: &Fake| f.lists().iter().filter(|l| l.path == p(&h)).count();
    let mut most = 0;
    for i in 0..10 {
        fake.gate().all = true;
        let mut tree = open(&fake, &pool, &h, Params::default());
        // Its listing starts while a slot is free, and the watchdog gives up on it.
        let want = (i + 1).min(4);
        let deadline = Instant::now() + Duration::from_secs(10);
        while (root(&fake) < want || pool.outstanding().1 < want) && Instant::now() < deadline {
            tree.poll();
            most = most.max(pool.outstanding().0);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(pool.outstanding().1, want, "cycle {i}");
        // At the cap: a while longer, and the fifth listing still never starts.
        let wait = Instant::now() + Duration::from_millis(if i >= 4 { 300 } else { 0 });
        while Instant::now() < wait {
            tree.poll();
            most = most.max(pool.outstanding().0);
            std::thread::sleep(Duration::from_millis(5));
        }
        if i >= 4 {
            assert_eq!(
                tree.status().as_deref(),
                Some("4 calls not answering; the tree waits for one to return")
            );
        }
        tree.close();
    }
    assert!(most <= 4, "{most} calls outstanding at once");
    assert_eq!(root(&fake), 4, "the fifth listing never started");
    assert_eq!(pool.outstanding(), (4, 4));
    // One more tree waits; one call let go → its slot comes back, the next listing runs.
    fake.gate().all = false;
    let mut tree = open(&fake, &pool, &h, Params::default());
    tree.poll();
    assert_eq!(root(&fake), 4);
    fake.release(1);
    assert!(until(&mut tree, Duration::from_secs(5), Tree::ready));
    assert_eq!(root(&fake), 5);
    assert_eq!(pool.outstanding().1, 3);
    tree.close();
}

/// T-lazy (05 §6.3): on a 1 000-folder root the tree lists at most the root, the path, three
/// screens of rows, E and one per compact-chain link; a folder never drawn is never listed.
#[test]
fn t_lazy_lists_only_what_is_near_the_screen() {
    let (_tmp, h) = temp();
    for i in 0..1000 {
        std::fs::create_dir(h.join(format!("f{i:04}"))).unwrap();
    }
    let fake = Fake::new(&h);
    let pool = pool(Duration::from_secs(30));
    let e = 36;
    let mut tree = open(&fake, &pool, &h.join("f0500"), Params { n: 5, d: 2, e });
    settle(&mut tree);
    tree.view(500 - e / 2, e, None);
    settle(&mut tree);
    let rows: Vec<Listed> = fake.lists().into_iter().filter(|l| l.full).collect();
    let bound = 1 + 1 + 3 * e + e;
    assert!(
        rows.len() <= bound,
        "{} listings, bound {bound}",
        rows.len()
    );
    let drawn: HashSet<String> = tree.rows().into_iter().map(|r| r.path).collect();
    for l in &rows {
        assert!(
            drawn.contains(&l.path),
            "{} was listed but never drawn",
            l.path
        );
    }
    let unique: HashSet<&String> = rows.iter().map(|l| &l.path).collect();
    assert_eq!(unique.len(), rows.len(), "no row is listed twice");
    assert!(fake.listed(&h.join("f0000")) == 0 && fake.listed(&h.join("f0999")) == 0);
    pool.shutdown();
}

/// Releases for every call a test's gate may hold.
const MAX_SCAN_RELEASE: usize = 1000;

/// T-lazy: closing the tree mid-scan stops the workers — no listing after the close.
#[test]
fn t_lazy_closing_mid_scan_stops_the_workers() {
    let (_tmp, h) = temp();
    for i in 0..40 {
        for j in 0..10 {
            std::fs::create_dir_all(h.join(format!("big/b{i:02}/c{j}"))).unwrap();
        }
    }
    std::fs::write(h.join("big/file"), b"x").unwrap();
    let fake = Fake::new(&h);
    *fake.slow.lock().unwrap() = Duration::from_millis(5);
    // The scan waits at the gate until the tree is settled, then runs.
    fake.gate().scans = true;
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    tree.view(0, 36, None);
    let scans = |f: &Fake| f.lists().iter().filter(|l| !l.full).count();
    fake.gate().scans = false;
    fake.release(MAX_SCAN_RELEASE);
    assert!(until(&mut tree, Duration::from_secs(5), |_| scans(&fake) >= 10));
    tree.close();
    // A listing that had passed its last look at the stop when the tree closed ends now.
    std::thread::sleep(Duration::from_millis(50));
    let at_close = scans(&fake);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(scans(&fake), at_close, "no listing after the close");
    assert!(at_close < 440, "the scan had not finished: {at_close}");
    drop(tree);
    pool.shutdown();
}

/// The tree over SFTP (05 §6.4) against this machine's `sftp-server` started in H (`-d`): the
/// home is `realpath(".")` without a user, the rows and info lines are read the same way, and a
/// hard-linked file counts once per name (SFTP has no inode).
#[test]
fn the_tree_over_sftp_reads_the_same_rows() {
    use crate::remote::SftpRemote;
    if SftpRemote::local_server().is_none() {
        return;
    }
    let (_tmp, h) = temp();
    mkdirs(&h, &["a/b/c", "a/b/d", "f/e1", "f/e2"]);
    std::fs::write(h.join("a/notes"), b"x").unwrap();
    std::fs::write(h.join("f/ten"), [0u8; 10]).unwrap();
    std::fs::write(h.join("f/twenty"), [0u8; 20]).unwrap();
    std::fs::write(h.join("f/thirty"), [0u8; 30]).unwrap();
    std::fs::hard_link(h.join("f/ten"), h.join("f/ten-again")).unwrap();
    std::os::unix::fs::symlink("a", h.join("l")).unwrap();
    let conn = SftpRemote::local_server_args(&["-d", &p(&h)], None).unwrap();
    let mut tree = Tree::open(
        Box::new(conn).into_tree(),
        Open {
            host: host(),
            folder: p(&h.join("a/b")),
            user: None,
            params: Params { n: 5, d: 1, e: 36 },
        },
    );
    settle(&mut tree);
    tree.view(0, 36, None);
    assert_eq!(tree.rows()[0].name, "~", "{}", names(&tree));
    let bar = tree.row_of(tree.current().unwrap()).unwrap();
    assert_eq!(tree.rows()[bar].path, p(&h.join("a/b")));
    assert_eq!(row(&tree, "l").info.text(), "<LINK>");
    assert!(until(&mut tree, Duration::from_secs(10), |t| {
        matches!(row(t, "f").info.size, Size::Bytes { more: false, .. })
    }));
    assert_eq!(row(&tree, "f").info.text(), "2 d 4 f 70B");
    tree.close();
}

/// The owner's keys: → lists a row the rules keep unread, ← on a closed row gives its parent's
/// row, Space sizes an H3 row, Enter on a link resolves its target, Cmd/Ctrl+R lists again, Esc
/// stops a whole listing.
#[test]
fn the_owners_keys_open_close_size_enter_and_reread() {
    let (_tmp, h) = temp();
    mkdirs(
        &h,
        &["Library/x/y", "sys/a", "slow/a", "plain/a", "plain/b"],
    );
    std::fs::write(h.join("Library/x/y/data"), [0u8; 50]).unwrap();
    std::os::unix::fs::symlink("plain", h.join("link")).unwrap();
    std::fs::write(h.join("slow/file"), b"x").unwrap();
    let fake = Fake::with_mounts(
        &h,
        Mounts {
            table: Some(vec![Mount {
                at: p(&h.join("sys")),
                fstype: "proc".into(),
                nobrowse: false,
            }]),
            macos: true,
            statvfs: true,
        },
    );
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    tree.view(0, 36, None);

    // → on an R4 row: the owner lists it.
    assert_eq!(fake.listed(&h.join("sys")), 0);
    assert!(tree.expand(row(&tree, "sys").id));
    assert!(until(&mut tree, Duration::from_secs(5), |t| row(t, "sys")
        .info
        .dirs
        .is_some()));
    assert!(row(&tree, "sys").open);
    // ← on an open row closes it; on a closed row gives the parent's row.
    assert_eq!(tree.collapse(row(&tree, "sys").id), None);
    assert!(!row(&tree, "sys").open);
    assert_eq!(
        tree.collapse(row(&tree, "sys").id),
        Some(row(&tree, "~").id)
    );

    // Space on H3: listed and sized then, not before.
    assert_eq!(fake.listed(&h.join("Library")), 0);
    assert_eq!(row(&tree, "Library").info.size, Size::Unknown);
    tree.size_now(row(&tree, "Library").id);
    assert!(until(&mut tree, Duration::from_secs(5), |t| {
        matches!(
            row(t, "Library").info.size,
            Size::Bytes {
                bytes: 50,
                more: false,
                ..
            }
        )
    }));

    // Enter: a folder's path at once; a link's target resolved.
    assert_eq!(
        tree.enter(row(&tree, "plain").id),
        Some(p(&h.join("plain")))
    );
    assert_eq!(tree.enter(row(&tree, "link").id), None);
    assert!(until(&mut tree, Duration::from_secs(5), |_| true));
    let mut target = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while target.is_none() && Instant::now() < deadline {
        tree.poll_wait(Duration::from_millis(20));
        target = tree.entered();
    }
    assert_eq!(target, Some(p(&h.join("plain"))));

    // Cmd/Ctrl+R lists the row again, whole.
    let before = fake.listed(&h.join("plain"));
    std::fs::create_dir(h.join("plain/c")).unwrap();
    tree.reread(row(&tree, "plain").id);
    assert!(until(&mut tree, Duration::from_secs(5), |t| {
        row(t, "plain").info.dirs == Some(Count { n: 3, more: false })
    }));
    assert_eq!(fake.listed(&h.join("plain")), before + 1);

    // Esc during an owner's whole listing stops it; the row keeps what it showed.
    fake.gate().paths.insert(p(&h.join("slow")));
    tree.reread(row(&tree, "slow").id);
    assert!(until(&mut tree, Duration::from_secs(5), |_| fake
        .gate()
        .waiting
        == 1));
    assert!(tree.owner_busy());
    let shown = row(&tree, "slow").info;
    tree.stop_owner();
    assert!(!tree.owner_busy());
    fake.release(1);
    std::thread::sleep(Duration::from_millis(100));
    tree.poll();
    assert_eq!(row(&tree, "slow").info.dirs, shown.dirs);
    pool.shutdown();
}

/// 05 §6.3: a listing stopped early shows lower bounds (`d+`, `f+`), keeps `►`, is never opened
/// by itself nor sized; on the path, the next folder is drawn though the listing never reached
/// it; → lists it whole and the counts are exact.
#[test]
fn t_lazy_a_listing_stopped_early_is_a_lower_bound() {
    let (_tmp, h) = temp();
    for i in 0..20 {
        mkdirs(&h, &[&format!("p/s{i:02}")]);
    }
    mkdirs(&h, &["p/q/x", "few/a", "few/b"]);
    for i in 0..20 {
        std::fs::write(h.join(format!("few/f{i:02}")), b"x").unwrap();
    }
    let fake = Fake::new(&h);
    *fake.cut.lock().unwrap() = Some((5, "q".into()));
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h.join("p/q"), Params::default());
    settle(&mut tree);
    // On the path: `p`'s listing stopped before `q`, which is drawn all the same.
    let p_row = row(&tree, "p");
    assert!(p_row.open);
    assert_eq!(p_row.info.dirs, Some(Count { n: 5, more: true }));
    assert!(row(&tree, "q").current);
    // Elsewhere: lower bounds, `►`, not opened, not sized.
    let few = row(&tree, "few");
    // The order a folder is read in is the filesystem's: 5 entries, of whichever kind.
    let (d, f) = few.info.dirs.zip(few.info.files).unwrap();
    assert!(d.more && f.more && d.n + f.n == 5, "{:?}", few.info);
    assert!(few.info.text().contains("d+") && few.info.text().contains("f+"));
    assert_eq!((few.glyph, few.open), (Glyph::Closed, false));
    assert!(
        fake.lists().iter().all(|l| l.full),
        "no scan of a partial listing"
    );
    // → lists it whole.
    tree.expand(few.id);
    assert!(until(&mut tree, Duration::from_secs(5), |t| {
        row(t, "few").info.files == Some(Count { n: 20, more: false })
    }));
    assert!(row(&tree, "few").open);
    pool.shutdown();
}

/// D-6 over SFTP: a remote row with more than 300 subfolders below stops at its budget with `+`;
/// Space resumes it without the budget, and it ends without `+`.
#[test]
fn a_remote_row_stops_at_its_budget_and_space_resumes_it() {
    use crate::remote::SftpRemote;
    if SftpRemote::local_server().is_none() {
        return;
    }
    let (_tmp, h) = temp();
    for i in 0..320 {
        let sub = h.join(format!("big/s{i:03}"));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("f"), [0u8; 10]).unwrap();
    }
    std::fs::write(h.join("big/own"), [0u8; 5]).unwrap();
    let conn = SftpRemote::local_server_args(&["-d", &p(&h)], None).unwrap();
    let mut tree = Tree::open(
        Box::new(conn).into_tree(),
        Open {
            host: host(),
            folder: p(&h),
            user: None,
            params: Params { n: 5, d: 1, e: 36 },
        },
    );
    settle(&mut tree);
    tree.view(0, 36, None);
    let size = |t: &Tree| row(t, "big").info.size;
    assert!(until(&mut tree, Duration::from_secs(20), |t| {
        matches!(
            size(t),
            Size::Bytes {
                more: true,
                scanning: false,
                ..
            }
        )
    }));
    let Size::Bytes { bytes, .. } = size(&tree) else {
        unreachable!()
    };
    assert!((5 + 3000..5 + 3200).contains(&bytes), "{bytes}");
    tree.size_now(row(&tree, "big").id);
    assert!(until(&mut tree, Duration::from_secs(20), |t| {
        matches!(
            size(t),
            Size::Bytes {
                bytes: 3205,
                more: false,
                scanning: false,
                ..
            }
        )
    }));
    tree.close();
}
