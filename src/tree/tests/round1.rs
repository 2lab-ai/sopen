//! Review round 1 of PR #71: the model's requests — never to what 05 §4 forbids, bounded while
//! working, resources held over a long session, sizes that never exceed the truth. Most assert the
//! requests issued, through [`Scripted`]: a host in memory behind `TreeFs` that answers at once
//! and records every request.

use super::*;
use crate::remote::treefs::{
    scan_step, Child, Got, Kind, Reply, ScanAsk, ScanEnd, Scanned, Target, TreeFs,
};
use std::collections::{HashMap, VecDeque};

#[derive(Default)]
struct HostState {
    local: bool,
    dirs: HashMap<String, Vec<Child>>,
    home: String,
    mounts: Option<Mounts>,
    /// `statvfs` of these is held: `not answering` at once, its answer on [`Scripted::release`].
    hold: HashSet<String>,
    held: Vec<(u64, String)>,
    asks: Vec<Ask>,
    /// Folders a scan listed.
    scanned: Vec<String>,
    /// Scans starting at these answer `not answering` (and nothing more).
    hang_scans: HashSet<String>,
    out: VecDeque<Reply>,
}

/// A host in memory: folders by path, a home, a mount table; every request recorded.
#[derive(Clone, Default)]
struct Scripted(Arc<Mutex<HostState>>);

fn d(name: &str) -> Child {
    Child::plain(name, Kind::Dir, 0)
}

fn f(name: &str, size: u64) -> Child {
    Child::plain(name, Kind::File, size)
}

fn l(name: &str, to: Target) -> Child {
    Child::plain(name, Kind::Link(to), 0)
}

impl Scripted {
    fn new(home: &str, mounts: Mounts) -> Self {
        let s = Scripted::default();
        s.state().home = home.to_string();
        s.state().mounts = Some(mounts);
        s
    }

    fn state(&self) -> std::sync::MutexGuard<'_, HostState> {
        self.0.lock().unwrap()
    }

    fn dir(&self, path: &str, children: Vec<Child>) {
        self.state().dirs.insert(path.to_string(), children);
    }

    fn tree(&self, folder: &str, params: Params) -> Tree {
        Tree::open(
            Box::new(self.clone()),
            Open {
                host: host(),
                folder: folder.to_string(),
                user: None,
                params,
            },
        )
    }

    fn asks(&self) -> Vec<Ask> {
        self.state().asks.clone()
    }

    fn lists(&self) -> Vec<String> {
        self.asks()
            .into_iter()
            .filter_map(|a| match a {
                Ask::List { path, .. } => Some(path),
                _ => None,
            })
            .collect()
    }

    fn statvfs(&self) -> Vec<String> {
        self.asks()
            .into_iter()
            .filter_map(|a| match a {
                Ask::Statvfs(path) => Some(path),
                _ => None,
            })
            .collect()
    }

    fn scans(&self) -> Vec<ScanAsk> {
        self.asks()
            .into_iter()
            .filter_map(|a| match a {
                Ask::Scan(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    /// Lets the held `statvfs` of `path` answer.
    fn release(&self, path: &str) {
        let mut s = self.state();
        let ids: Vec<u64> = s
            .held
            .iter()
            .filter(|(_, p)| p == path)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            s.out.push_back(Reply {
                id,
                got: Got::Statvfs(Ok(FsStat {
                    used_bytes: 87_000,
                    used_files: 9,
                    fsid: 1,
                })),
            });
        }
        s.hold.remove(path);
    }
}

impl TreeFs for Scripted {
    fn send(&mut self, id: u64, _prio: u64, ask: Ask) {
        let mut s = self.state();
        s.asks.push(ask.clone());
        let got = match ask {
            Ask::Resolve(path) => Got::Resolved(Ok(Resolved {
                path,
                ids: Vec::new(),
            })),
            Ask::Home(_) => Got::Home(Ok(Resolved {
                path: s.home.clone(),
                ids: Vec::new(),
            })),
            Ask::Mounts => Got::Mounts(s.mounts.clone().unwrap()),
            Ask::List { path, .. } => Got::Listed(
                s.dirs
                    .get(&path)
                    .map(|c| Listing {
                        children: c.clone(),
                        complete: true,
                        cachedir: false,
                    })
                    .ok_or_else(|| format!("{path}: no such folder")),
            ),
            Ask::Statvfs(path) if s.hold.contains(&path) => {
                s.held.push((id, path));
                Got::NotAnswering
            }
            Ask::Statvfs(_) => Got::Statvfs(Ok(FsStat {
                used_bytes: 87_000,
                used_files: 9,
                fsid: 1,
            })),
            Ask::Scan(a) if a.start.iter().any(|p| s.hang_scans.contains(p)) => Got::NotAnswering,
            Ask::Scan(a) => {
                let skip: HashSet<String> = a.skip.iter().cloned().collect();
                let hold: HashSet<String> = a.hold.iter().cloned().collect();
                let mut seen = a.seen.iter().copied().collect();
                let mut totals = a.base;
                let mut queue: VecDeque<String> = a.start.iter().cloned().collect();
                while let Some(dir) = queue.pop_front() {
                    if skip.contains(&dir) {
                        continue;
                    }
                    s.scanned.push(dir.clone());
                    match s.dirs.get(&dir) {
                        Some(children) => {
                            let listing = Listing {
                                children: children.clone(),
                                complete: true,
                                cachedir: false,
                            };
                            queue.extend(scan_step(
                                &dir,
                                &listing,
                                &skip,
                                &hold,
                                &mut seen,
                                &mut totals,
                            ));
                        }
                        None => totals.unread += 1,
                    }
                }
                Got::Scan(Scanned {
                    totals,
                    end: Some(ScanEnd::Complete),
                })
            }
        };
        s.out.push_back(Reply { id, got });
    }

    fn cancel(&mut self, _id: u64) {}

    fn next(&mut self, _wait: Duration) -> Option<Reply> {
        self.state().out.pop_front()
    }

    fn set_wake(&mut self, _wake: crate::remote::Wake) {}

    fn close(&mut self) {}

    fn held(&self) -> Option<String> {
        None
    }

    fn is_local(&self) -> bool {
        self.state().local
    }
}

/// A macOS host without a mount table that offers `statvfs`: 05 §4.1's fences and names.
fn mac_remote() -> Mounts {
    Mounts {
        table: None,
        macos: true,
        statvfs: true,
    }
}

fn linux_table(mounts: &[(&str, &str)]) -> Mounts {
    Mounts {
        table: Some(
            mounts
                .iter()
                .map(|(at, fstype)| Mount {
                    at: at.to_string(),
                    fstype: fstype.to_string(),
                    nobrowse: false,
                })
                .collect(),
        ),
        macos: false,
        statvfs: true,
    }
}

/// The root `/` of a macOS host, with `/net` (R6 by name) and `/System/Volumes/Data`.
fn mac_root(host: &Scripted) {
    host.dir(
        "/",
        vec![d("Users"), d("System"), d("dev"), d("net"), d("usr")],
    );
    host.dir("/Users", vec![d("u")]);
    host.dir("/Users/u", vec![d("docs")]);
    host.dir("/Users/u/docs", vec![f("a", 10)]);
    host.dir("/System", vec![d("Volumes"), d("Library")]);
    host.dir("/System/Library", vec![f("k", 5)]);
    host.dir("/System/Volumes", vec![d("Data")]);
    host.dir("/System/Volumes/Data", vec![d("home"), d("Users")]);
    host.dir("/System/Volumes/Data/home", vec![d("share")]);
    host.dir("/dev", vec![f("null", 0)]);
    host.dir("/net", vec![d("host")]);
    host.dir("/net/host", vec![d("x")]);
    host.dir("/usr", vec![d("bin"), f("README", 1)]);
    host.dir("/usr/bin", vec![f("sh", 100)]);
}

/// Item 1: phase 1 never lists an R4 folder on the path, nor what is below it; the path's rows
/// stay, unlisted.
#[test]
fn r1_1_the_path_through_a_kernel_mount_is_drawn_unlisted() {
    for current in ["sys", "sys/a"] {
        let (_tmp, h) = temp();
        mkdirs(&h, &["sys/a/b", "sys/c", "other"]);
        let fake = Fake::with_mounts(&h, linux_table(&[(&p(&h.join("sys")), "proc")]));
        let pool = pool(Duration::from_secs(30));
        let mut tree = open(&fake, &pool, &h.join(current), Params::default());
        settle(&mut tree);
        assert_eq!(
            fake.listed(&h.join("sys")),
            0,
            "{current}: sys is not listed"
        );
        assert_eq!(
            fake.listed(&h.join("sys/a")),
            0,
            "{current}: below it neither"
        );
        assert_eq!(row(&tree, "sys").info.columns()[2], "<SYS>");
        let bar = tree.row_of(tree.current().unwrap()).unwrap();
        assert_eq!(
            tree.rows()[bar].path,
            p(&h.join(current)),
            "the path's row stays"
        );
        assert_eq!(fake.listed(&h), 1, "the root is listed");
        pool.shutdown();
    }
}

/// Item 1 on a fenced remote: the path through `/net` (R6 by name) gets no listing and no
/// `statvfs` — a lookup there can mount a share.
#[test]
fn r1_1_a_fenced_remote_classifies_the_path_before_any_request() {
    let host = Scripted::new("/Users/u", mac_remote());
    mac_root(&host);
    let mut tree = host.tree("/net/host", Params::default());
    settle(&mut tree);
    for path in ["/net", "/net/host"] {
        assert!(!host.lists().contains(&path.to_string()), "{path} listed");
        assert!(
            !host.statvfs().contains(&path.to_string()),
            "{path} statvfs"
        );
    }
    assert!(host.lists().contains(&"/".to_string()));
    assert!(tree.rows().iter().any(|r| r.path == "/net/host"));
}

/// Item 2: the current folder follows the rules for opening by itself (H2 here); only H1's path
/// override stays.
#[test]
fn r1_2_the_current_folder_keeps_its_rules_but_h1() {
    for (current, opens) in [("node_modules", false), (".cfg", true)] {
        let (_tmp, h) = temp();
        subs(&h, current, 2);
        let fake = Fake::new(&h);
        let pool = pool(Duration::from_secs(30));
        let mut tree = open(&fake, &pool, &h.join(current), Params::default());
        settle(&mut tree);
        let r = row(&tree, current);
        assert!(r.current);
        assert_eq!(
            (r.open, r.auto),
            (opens, opens),
            "{current}: {}",
            names(&tree)
        );
        pool.shutdown();
    }
}

/// Item 3: a row's listing carries the mounts a link must not be followed into (R4–R6, not R3
/// or R7); a link whose target was not read is a link row all the same.
#[test]
fn r1_3_a_listing_carries_the_guard_and_an_unread_link_is_a_row() {
    let host = Scripted::new(
        "/home/u",
        linux_table(&[
            ("/", "ext4"),
            ("/mnt/auto", "autofs"),
            ("/proc", "proc"),
            ("/srv/nfs", "nfs4"),
            ("/data", "xfs"),
        ]),
    );
    host.dir(
        "/",
        vec![d("mnt"), d("data"), l("shortcut", Target::Unread)],
    );
    host.dir("/mnt", vec![d("auto")]);
    host.dir("/data", vec![f("x", 1)]);
    let mut tree = host.tree("/", Params::default());
    settle(&mut tree);
    let guards: Vec<Vec<String>> = host
        .asks()
        .into_iter()
        .filter_map(|a| match a {
            Ask::List { guard, .. } => Some(guard),
            _ => None,
        })
        .collect();
    assert!(!guards.is_empty());
    for mut g in guards {
        g.sort();
        assert_eq!(g, ["/mnt/auto", "/proc", "/srv/nfs"]);
    }
    let link = row(&tree, "shortcut");
    assert_eq!(
        (link.kind, link.info.text().as_str()),
        (RowKind::Link, "<LINK>")
    );
}

/// Item 4: Space on the macOS home never scans `~/Library` (H3).
#[test]
fn r1_4_space_on_the_home_never_scans_library() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["Library/x/y", "docs/z"]);
    std::fs::write(h.join("Library/x/y/big"), [0u8; 50]).unwrap();
    std::fs::write(h.join("docs/z/a"), [0u8; 5]).unwrap();
    let fake = Fake::with_mounts(
        &h,
        Mounts {
            table: Some(Vec::new()),
            macos: true,
            statvfs: true,
        },
    );
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    tree.size_now(row(&tree, "~").id);
    assert!(until(&mut tree, Duration::from_secs(5), |t| {
        matches!(
            row(t, "~").info.size,
            Size::Bytes {
                scanning: false,
                ..
            }
        ) && fake.lists().iter().any(|l| !l.full)
    }));
    settle(&mut tree);
    let library = p(&h.join("Library"));
    let into: Vec<String> = fake
        .lists()
        .into_iter()
        .filter(|l| l.path == library || l.path.starts_with(&format!("{library}/")))
        .map(|l| l.path)
        .collect();
    assert!(into.is_empty(), "scanned under ~/Library: {into:?}");
    pool.shutdown();
}

/// Item 4 on a remote without a table: a scan of `/` skips the name rules' folders below it —
/// `/dev`, `/net`, `/System/Volumes` (its every child is R7, and `Data/home` is R6).
#[test]
fn r1_4_a_remote_scan_skips_the_name_rules_below_it() {
    let host = Scripted::new("/Users/u", mac_remote());
    mac_root(&host);
    let mut tree = host.tree("/", Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    tree.size_now(row(&tree, "/").id);
    settle(&mut tree);
    assert!(!host.scans().is_empty());
    let scanned = host.state().scanned.clone();
    for no in [
        "/dev",
        "/net",
        "/System/Volumes",
        "/System/Volumes/Data/home",
    ] {
        assert!(
            !scanned.iter().any(|s| s == no),
            "{no} scanned: {scanned:?}"
        );
    }
    assert!(scanned.iter().any(|s| s == "/usr/bin"), "{scanned:?}");
}

/// Item 6: Space on a fenced remote row waits for its `statvfs`, then scans with the fence.
#[test]
fn r1_6_space_waits_for_the_fence() {
    let host = Scripted::new("/Users/u", mac_remote());
    mac_root(&host);
    host.state().hold.insert("/usr".into());
    let mut tree = host.tree("/", Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    let usr = row(&tree, "usr");
    tree.size_now(usr.id);
    settle(&mut tree);
    let of_usr = |h: &Scripted| {
        h.scans()
            .into_iter()
            .filter(|s| s.start.iter().any(|p| p.starts_with("/usr/")))
            .collect::<Vec<_>>()
    };
    assert!(
        of_usr(&host).is_empty(),
        "scanned before its fence: {:?}",
        of_usr(&host)
    );
    host.release("/usr");
    settle(&mut tree);
    let scans = of_usr(&host);
    assert_eq!(scans.len(), 1);
    assert!(scans[0].fence.is_some(), "{:?}", scans[0]);
}

/// Item 7: a scan bounds each folder's listing; one read in part leaves the row's size `+`.
#[test]
fn r1_7_a_scan_bounds_each_listing() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["row/many"]);
    std::fs::write(h.join("row/readme"), b"x").unwrap();
    for i in 0..20 {
        std::fs::write(h.join(format!("row/many/f{i:02}")), b"x").unwrap();
    }
    let fake = Fake::new(&h);
    *fake.cut.lock().unwrap() = Some((5, "never".into()));
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    tree.view(0, 36, None);
    settle(&mut tree);
    // The scan's end, not a quiet moment.
    assert!(until(&mut tree, Duration::from_secs(20), |t| {
        matches!(
            row(t, "row").info.size,
            Size::Bytes {
                scanning: false,
                ..
            }
        ) && fake.lists().iter().any(|l| !l.full)
    }));
    let scans: Vec<Listed> = fake.lists().into_iter().filter(|l| !l.full).collect();
    assert!(!scans.is_empty());
    assert!(
        scans.iter().all(|l| l.budget.is_some()),
        "every scan listing is bounded: {scans:?}"
    );
    let size = row(&tree, "row").info.size;
    assert!(
        matches!(
            size,
            Size::Bytes {
                more: true,
                scanning: false,
                ..
            }
        ),
        "{size:?}"
    );
    pool.shutdown();
}

/// Item 10: a `statvfs` that does not answer shows on its row, and the answer clears it.
#[test]
fn r1_10_a_statvfs_that_does_not_answer_shows_on_its_row() {
    let host = Scripted::new("/home/u", linux_table(&[("/", "ext4"), ("/disk", "xfs")]));
    host.dir("/", vec![d("disk"), d("home")]);
    host.dir("/disk", vec![d("a"), d("b"), f("x", 3)]);
    host.state().hold.insert("/disk".into());
    let mut tree = host.tree("/", Params { n: 5, d: 2, e: 1 });
    settle(&mut tree);
    assert_eq!(row(&tree, "disk").info.size, Size::Tag(Tag::NotAnswering));
    host.release("/disk");
    settle(&mut tree);
    assert_eq!(row(&tree, "disk").info.columns()[2], "=87K fs");
}

/// Item 13: the process cache forgets its oldest sizes past its bound, across tree sessions;
/// homes are bounded too.
#[test]
fn r1_13_the_cache_forgets_the_oldest_across_sessions() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["d/e"]);
    std::fs::write(h.join("d/e/x"), [0u8; 7]).unwrap();
    std::fs::write(h.join("d/own"), [0u8; 1]).unwrap();
    let fake = Fake::new(&h);
    let pool = pool(Duration::from_secs(30));
    let at = host();
    let mut tree = Tree::open(
        LocalFs::tree_with(fake.clone(), pool.clone()),
        Open {
            host: at.clone(),
            folder: p(&h),
            user: None,
            params: Params { n: 5, d: 1, e: 36 },
        },
    );
    settle(&mut tree);
    // `d`'s scan to its end (own 1 B + 7 B below), not a quiet moment.
    assert!(until(&mut tree, Duration::from_secs(20), |t| {
        matches!(
            row(t, "d").info.size,
            Size::Bytes {
                bytes: 8,
                more: false,
                scanning: false,
                ..
            }
        )
    }));
    tree.close();
    let d = p(&h.join("d"));
    assert!(cache::size(&at, &d).is_some(), "the scan was kept");
    // Others' scans, as many as the cache holds.
    let flood = host();
    for i in 0..cache::SIZES {
        cache::store(&flood, &format!("/f{i}"), Default::default(), true);
    }
    assert!(cache::size(&at, &d).is_none(), "the oldest went");
    assert!(cache::with(|c| c.len().0) <= cache::SIZES);
    for i in 0..cache::HOMES + 10 {
        cache::set_home(
            &format!("{flood}-{i}"),
            Some(Resolved {
                path: "/".into(),
                ids: Vec::new(),
            }),
        );
    }
    assert!(cache::with(|c| c.len().1) <= cache::HOMES);
    pool.shutdown();
}

/// Item 14: re-reading a folder whose subfolders change frees the rows it no longer holds; a
/// row that stays keeps its id.
#[test]
fn r1_14_a_refresh_frees_the_folders_it_no_longer_holds() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["w/keep", "w/g0a", "w/g0b"]);
    let fake = Fake::new(&h);
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    tree.expand(row(&tree, "w").id);
    settle(&mut tree);
    let keep = row(&tree, "keep").id;
    let before = tree.slots();
    for gen in 1..=20 {
        std::fs::rename(
            h.join(format!("w/g{}a", gen - 1)),
            h.join(format!("w/g{gen}a")),
        )
        .unwrap();
        std::fs::rename(
            h.join(format!("w/g{}b", gen - 1)),
            h.join(format!("w/g{gen}b")),
        )
        .unwrap();
        tree.reread(row(&tree, "w").id);
        assert!(until(&mut tree, Duration::from_secs(5), |t| {
            t.rows().iter().any(|r| r.name == format!("g{gen}a"))
        }));
        settle(&mut tree);
    }
    assert!(
        tree.slots() <= before + 2,
        "{} slots, {before} before",
        tree.slots()
    );
    assert_eq!(row(&tree, "keep").id, keep, "a row that stays keeps its id");
    pool.shutdown();
}

/// Item 15: a file hard-linked into two sibling folders counts once in their open parent: its
/// total never exceeds the true size, and says `+` where it may be short.
#[test]
fn r1_15_hard_links_across_siblings_count_once() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["a", "b", "c"]);
    std::fs::write(h.join("a/f"), [0u8; 100]).unwrap();
    std::fs::hard_link(h.join("a/f"), h.join("b/g")).unwrap();
    std::fs::write(h.join("c/single"), [0u8; 7]).unwrap();
    let fake = Fake::new(&h);
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params::default());
    settle(&mut tree);
    let Size::Bytes { bytes, more, .. } = row(&tree, "~").info.size else {
        panic!("{:?}", row(&tree, "~").info);
    };
    assert!(bytes <= 107, "{bytes} > the true 107");
    assert_eq!((bytes, more), (107, true));
    pool.shutdown();
}

/// Item 16: a mount on the current path is another filesystem: not in its parent's size.
#[test]
fn r1_16_a_mount_on_the_path_is_not_in_its_parents_size() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["mnt/x", "other"]);
    std::fs::write(h.join("mnt/x/big"), [0u8; 1000]).unwrap();
    std::fs::write(h.join("other/small"), [0u8; 10]).unwrap();
    let fake = Fake::with_mounts(&h, linux_table(&[(&p(&h.join("mnt")), "ext4")]));
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h.join("mnt/x"), Params::default());
    settle(&mut tree);
    let size = row(&tree, "~").info.size;
    assert!(matches!(size, Size::Bytes { bytes: 10, .. }), "{size:?}");
    pool.shutdown();
}

/// Item 9: a remote row whose scan once did not answer is not stuck: off screen its scan stops,
/// and back on screen it is scanned again.
#[test]
fn r1_9_a_remote_row_that_did_not_answer_is_scanned_again() {
    let host = Scripted::new("/home/u", linux_table(&[("/", "ext4")]));
    host.dir("/", vec![d("big"), d("z1"), d("z2"), d("z3")]);
    host.dir("/big", vec![d("a"), f("own", 5)]);
    host.dir("/big/a", vec![f("x", 10)]);
    host.state().hang_scans.insert("/big/a".into());
    let mut tree = host.tree("/", Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    tree.view(0, 36, None);
    settle(&mut tree);
    assert_eq!(row(&tree, "big").info.size, Size::Tag(Tag::NotAnswering));
    // Off screen for more than a second: its scan stops.
    let big = tree.row_of(row(&tree, "big").id).unwrap();
    tree.view(big + 1, 2, None);
    std::thread::sleep(super::super::scan::LEAVE + Duration::from_millis(200));
    tree.view(big + 1, 2, None);
    host.state().hang_scans.clear();
    tree.view(0, 36, None);
    settle(&mut tree);
    let size = row(&tree, "big").info.size;
    assert!(
        matches!(
            size,
            Size::Bytes {
                bytes: 15,
                more: false,
                ..
            }
        ),
        "{size:?}"
    );
}

/// Item 6 with H3: Space on `~/Library` of a fenced macOS remote asks its `statvfs` (phase 2
/// never did: H3 is not read unopened), then scans it fenced (05:117 "sized only on Space").
#[test]
fn r1_6_space_on_h3_asks_its_fence_first() {
    let host = Scripted::new("/Users/u", mac_remote());
    host.dir("/Users/u", vec![d("Library"), d("docs")]);
    host.dir("/Users/u/Library", vec![d("Caches"), f("k", 3)]);
    host.dir("/Users/u/Library/Caches", vec![f("c", 4)]);
    host.dir("/Users/u/docs", vec![f("a", 1)]);
    let mut tree = host.tree("/Users/u", Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    assert!(!host.statvfs().contains(&"/Users/u/Library".to_string()));
    tree.size_now(row(&tree, "Library").id);
    settle(&mut tree);
    assert!(host.statvfs().contains(&"/Users/u/Library".to_string()));
    let scans: Vec<ScanAsk> = host
        .scans()
        .into_iter()
        .filter(|s| s.start.iter().any(|p| p.starts_with("/Users/u/Library/")))
        .collect();
    assert_eq!(scans.len(), 1, "{scans:?}");
    assert!(scans[0].fence.is_some());
    let size = row(&tree, "Library").info.size;
    assert!(
        matches!(
            size,
            Size::Bytes {
                bytes: 7,
                more: false,
                ..
            }
        ),
        "{size:?}"
    );
}

/// D-6 / follow-up: Space is the owner's own action — its scan lists every folder whole, so a
/// folder of 12 000 entries below a closed row ends exact; the automatic scan keeps `+`. Locally
/// and over the local `sftp-server`.
#[test]
fn r1_7_space_lists_every_folder_whole() {
    use crate::remote::SftpRemote;
    let (_tmp, h) = temp();
    mkdirs(&h, &["row/big"]);
    std::fs::write(h.join("row/readme"), b"x").unwrap();
    for i in 0..12_000 {
        std::fs::write(h.join(format!("row/big/f{i:05}")), b"x").unwrap();
    }
    let check = |mut tree: Tree| {
        settle(&mut tree);
        tree.view(0, 36, None);
        settle(&mut tree);
        let size = |t: &Tree| row(t, "row").info.size;
        // `settle` waits for a quiet moment, not for a scan's end: a 12 000-entry listing on a
        // slow runner is a long quiet moment. Wait for the automatic scan itself (Space while it
        // runs would test something else).
        let idle = |t: &Tree| {
            matches!(
                size(t),
                Size::Bytes {
                    scanning: false,
                    ..
                }
            )
        };
        assert!(
            until(&mut tree, Duration::from_secs(20), idle),
            "the automatic scan never ended: {:?}",
            size(&tree)
        );
        assert!(
            matches!(
                size(&tree),
                Size::Bytes {
                    more: true,
                    scanning: false,
                    ..
                }
            ),
            "automatic: {:?}",
            size(&tree)
        );
        tree.size_now(row(&tree, "row").id);
        let exact = Size::Bytes {
            bytes: 12_001,
            more: false,
            cached: false,
            scanning: false,
        };
        // The Space scan's own end, not a quiet moment.
        until(&mut tree, Duration::from_secs(30), |t| {
            size(t) == exact && !t.owner_busy()
        });
        assert_eq!(size(&tree), exact);
        tree.close();
    };
    let fake = Fake::new(&h);
    let pool = pool(Duration::from_secs(30));
    check(open(&fake, &pool, &h, Params { n: 5, d: 1, e: 36 }));
    pool.shutdown();
    if SftpRemote::local_server().is_none() {
        return;
    }
    let conn = SftpRemote::local_server_args(&["-d", &p(&h)], None).unwrap();
    check(Tree::open(
        Box::new(conn).into_tree(),
        Open {
            host: host(),
            folder: p(&h),
            user: None,
            params: Params { n: 5, d: 1, e: 36 },
        },
    ));
}

/// Follow-up: Esc stops a Space scan at once (it is the owner's whole listing of a subtree).
#[test]
fn r1_7_esc_stops_a_space_scan() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["row/a/b", "row/c"]);
    let fake = Fake::new(&h);
    fake.gate().scans = true;
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 1, e: 1 });
    settle(&mut tree);
    tree.size_now(row(&tree, "row").id);
    assert!(until(&mut tree, Duration::from_secs(5), |_| fake
        .gate()
        .waiting
        >= 1));
    assert!(tree.owner_busy());
    tree.stop_owner();
    assert!(!tree.owner_busy());
    assert!(!matches!(
        row(&tree, "row").info.size,
        Size::Bytes { scanning: true, .. }
    ));
    let at_stop = fake.lists().iter().filter(|l| !l.full).count();
    fake.gate().scans = false;
    fake.release(100);
    std::thread::sleep(Duration::from_millis(200));
    tree.poll();
    assert_eq!(
        fake.lists().iter().filter(|l| !l.full).count(),
        at_stop,
        "no scan listing after Esc"
    );
    pool.shutdown();
}

/// Review round 2, item 4: one listing of a Space scan abandoned by the watchdog stops its
/// siblings too (the scan has ended), instead of losing their stop.
#[test]
fn r2_4_an_abandoned_listing_stops_its_siblings() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["row/a/x", "row/b/y"]);
    std::fs::write(h.join("row/readme"), b"x").unwrap();
    let fake = Fake::new(&h);
    fake.gate().paths.insert(p(&h.join("row/a")));
    fake.long.lock().unwrap().insert(p(&h.join("row/b")));
    let pool = pool(Duration::from_millis(300));
    // A screen of one row: only Space scans `row`.
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 1, e: 1 });
    settle(&mut tree);
    tree.size_now(row(&tree, "row").id);
    let b = p(&h.join("row/b"));
    let stopped = |f: &Fake| f.stopped.lock().unwrap().iter().any(|(path, _)| *path == b);
    assert!(
        until(&mut tree, Duration::from_secs(2), |_| stopped(&fake)),
        "the sibling read on: {:?}",
        fake.stopped.lock().unwrap()
    );
    fake.release(1);
    pool.shutdown();
}

/// Review round 2, item 5: a fence probe (`statvfs`) that does not answer shows on any row
/// that needs it, not only on a mount's.
#[test]
fn r2_5_a_late_fence_shows_on_an_ordinary_row() {
    let host = Scripted::new("/Users/u", mac_remote());
    mac_root(&host);
    host.state().hold.insert("/usr".into());
    let mut tree = host.tree("/", Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    assert_eq!(row(&tree, "usr").info.size, Size::Tag(Tag::NotAnswering));
    host.release("/usr");
    settle(&mut tree);
    assert!(matches!(row(&tree, "usr").info.size, Size::Bytes { .. }));
}

/// Review round 2, item 6: a scan that leaves out an H3 folder keeps the size a lower bound —
/// the macOS home holding only a non-empty `Library` is `+` after Space, never an exact zero.
#[test]
fn r2_6_an_h3_folder_left_out_keeps_the_size_partial() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["Library/x"]);
    std::fs::write(h.join("Library/x/data"), [0u8; 50]).unwrap();
    let fake = Fake::with_mounts(
        &h,
        Mounts {
            table: Some(Vec::new()),
            macos: true,
            statvfs: true,
        },
    );
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    tree.size_now(row(&tree, "~").id);
    settle(&mut tree);
    until(&mut tree, Duration::from_secs(20), |t| {
        matches!(
            row(t, "~").info.size,
            Size::Bytes {
                scanning: false,
                ..
            }
        ) && !t.owner_busy()
    });
    let size = row(&tree, "~").info.size;
    assert!(
        matches!(
            size,
            Size::Bytes {
                more: true,
                scanning: false,
                ..
            }
        ),
        "{size:?}"
    );
    pool.shutdown();
}

/// Space pressed while the row's automatic scan is running is not lost: the automatic scan
/// gives way to Space's, which lists every folder whole, and the row ends exact (D-6).
#[test]
fn r2_space_during_an_automatic_scan_is_not_lost() {
    let (_tmp, h) = temp();
    mkdirs(&h, &["row/many"]);
    std::fs::write(h.join("row/readme"), b"x").unwrap();
    for i in 0..20 {
        std::fs::write(h.join(format!("row/many/f{i:02}")), b"x").unwrap();
    }
    let fake = Fake::new(&h);
    // An automatic listing stops after 5 entries; Space's are whole.
    *fake.cut.lock().unwrap() = Some((5, "never".into()));
    fake.gate().scans = true;
    let pool = pool(Duration::from_secs(30));
    let mut tree = open(&fake, &pool, &h, Params { n: 5, d: 1, e: 36 });
    settle(&mut tree);
    tree.view(0, 36, None);
    assert!(
        until(&mut tree, Duration::from_secs(10), |_| fake.gate().waiting
            == 1),
        "the automatic scan is running"
    );
    tree.size_now(row(&tree, "row").id);
    fake.gate().scans = false;
    fake.release(10);
    let exact = |t: &Tree| {
        matches!(
            row(t, "row").info.size,
            Size::Bytes {
                bytes: 21,
                more: false,
                scanning: false,
                ..
            }
        )
    };
    until(&mut tree, Duration::from_secs(20), exact);
    assert!(exact(&tree), "{:?}", row(&tree, "row").info.size);
    pool.shutdown();
}
