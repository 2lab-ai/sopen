//! The git panel as a side's content, driven by real egui key and pointer events through the test
//! kit, on views built from fixed rows (no git): the graph's columns, lanes, nodes and labels
//! (03 §4.2–§4.4), paging (§4.5), the filters (§4.6, G-F1), Find (§4.7, G-F2), the details pane
//! (§6.1, G-T1's view part), the keys (§9) and the look (`chrome::git`).

use super::*;
use crate::git::graph::{End, RowStream};
use crate::git::model::{CommitRow, ObjectType, Oid, RefKind, RefLabel, StashRow};
use crate::gui::chrome::git::{self as look, LOOK};
use crate::gui::chrome::PALETTE;
use crate::gui::testkit::Driver;
use crate::gui::Window;
use eframe::egui::{Color32, Event, Key, Modifiers, Pos2};

/// The git panel filling the window, as a side of the browser draws it, with what its keys and
/// clicks asked for.
struct Host {
    view: GitView,
    active: bool,
    actions: Vec<GitAction>,
    /// Where the last pass drew the panel.
    rect: Rect,
}

impl Window for Host {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.actions.extend(keys(&ctx, &mut self.view));
        chrome::key_panel(ui, "git-test-keys", &fkeys(Mode::Preview));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.max_rect().shrink(2.0);
                self.rect = rect;
                self.actions
                    .extend(draw(ui, rect, &mut self.view, self.active));
            });
    }

    fn ready(&self) -> bool {
        true
    }
}

struct Fx {
    drv: Driver,
    host: Host,
}

impl Fx {
    /// `view` drawn once in a `w`×`h` window.
    fn new(w: f32, h: f32, view: GitView) -> Self {
        let mut fx = Fx {
            drv: Driver::new(w, h),
            host: Host {
                view,
                active: true,
                actions: Vec::new(),
                rect: Rect::NOTHING,
            },
        };
        fx.idle();
        fx
    }

    fn idle(&mut self) {
        self.drv.idle(&mut self.host, 1);
    }

    /// What has been asked for since the last call.
    fn take(&mut self) -> Vec<GitAction> {
        std::mem::take(&mut self.host.actions)
    }

    fn key(&mut self, key: Key, mods: Modifiers) -> Vec<GitAction> {
        self.drv.key(&mut self.host, key, mods);
        self.take()
    }

    fn press(&mut self, key: Key) -> Vec<GitAction> {
        self.key(key, Modifiers::NONE)
    }

    /// `text` typed as one event (an input method's commit, a paste of keys).
    fn typed(&mut self, text: &str) -> Vec<GitAction> {
        self.drv.frame(
            &mut self.host,
            vec![Event::Text(text.into())],
            Modifiers::NONE,
        );
        self.idle();
        self.take()
    }

    /// `text` typed a character a pass, as the keyboard does.
    fn type_slowly(&mut self, text: &str) -> Vec<GitAction> {
        let mut asked = Vec::new();
        for c in text.chars() {
            asked.extend(self.typed(&c.to_string()));
        }
        asked
    }

    fn view(&self) -> &GitView {
        &self.host.view
    }

    fn view_mut(&mut self) -> &mut GitView {
        &mut self.host.view
    }

    /// The key of the row under the bar.
    fn bar(&self) -> RowKey {
        RowKey::of(self.view().bar_row().expect("a row under the bar"))
    }

    /// Every text the last pass drew whole and where, top to bottom, left to right.
    fn texts(&self) -> Vec<(Rect, String)> {
        let mut all = self.drv.texts_starting_with("");
        all.sort_by(|a, b| {
            a.0.top()
                .total_cmp(&b.0.top())
                .then(a.0.left().total_cmp(&b.0.left()))
        });
        all
    }

    /// Whether the last pass drew exactly `text` whole somewhere.
    fn shows(&self, text: &str) -> bool {
        self.drv.find_text(text).is_some()
    }

    /// Every text drawn on the row (same top) where `text` is drawn, left to right.
    fn row_of(&self, text: &str) -> Vec<String> {
        let y = self
            .drv
            .find_text(text)
            .unwrap_or_else(|| panic!("{text:?} is not drawn"))
            .top();
        // One line each: a scroll bar's column is one text from its top to its bottom.
        self.texts()
            .into_iter()
            .filter(|(r, t)| (r.top() - y).abs() < 0.5 && !t.contains('\n'))
            .map(|(_, t)| t)
            .collect()
    }

    /// The subjects drawn, top to bottom (each row's subject is a text of its own).
    fn drawn_subjects(&self, subjects: &[&str]) -> Vec<String> {
        self.texts()
            .into_iter()
            .map(|(_, t)| t)
            .filter(|t| subjects.contains(&t.as_str()))
            .collect()
    }
}

/// A commit id made of `n` (below 256): `0909…09`, its short form `09090909`.
fn oid(n: u32) -> Oid {
    assert!(n < 256);
    Oid::parse(&format!("{n:02x}").repeat(20)).unwrap()
}

/// A log record: commit `n` with `parents`, by `author` at `time` (author and committer).
fn commit(n: u32, parents: &[u32], author: &str, time: i64, subject: &str) -> CommitRow {
    CommitRow {
        oid: oid(n),
        parents: parents.iter().map(|&p| oid(p)).collect(),
        author_name: author.to_string(),
        author_email: format!("{}@example.com", author.to_lowercase()),
        author_time: time,
        commit_time: time,
        subject: subject.to_string(),
    }
}

/// A ref of the refs read; every label of these tests is made here.
fn label(refname: &str, object: u32, peeled: Option<u32>) -> RefLabel {
    let kind = if refname.starts_with("refs/heads/") {
        RefKind::LocalBranch
    } else if refname.starts_with("refs/remotes/") {
        RefKind::RemoteBranch
    } else {
        RefKind::Tag
    };
    RefLabel {
        object: oid(object),
        object_type: if peeled.is_some() {
            ObjectType::Tag
        } else {
            ObjectType::Commit
        },
        peeled: peeled.map(oid),
        refname: refname.to_string(),
        kind,
        upstream: None,
        track: String::new(),
        lossy: false,
    }
}

fn stash(n: u32, base: u32, selector: &str, subject: &str) -> StashRow {
    StashRow {
        oid: oid(n),
        base: oid(base),
        index: Some(oid(n + 100)),
        untracked: None,
        selector: selector.to_string(),
        commit_time: 1_794_000_000,
        subject: subject.to_string(),
    }
}

/// Seconds of the fixture's commits: a day apart, the newest last.
fn day(k: i64) -> i64 {
    1_790_000_000 + k * 86_400
}

/// G-G1's fixture (03 §11), in date order: two branches and a merge, an octopus merge, an
/// annotated and a lightweight tag, `origin` with `main` at the local `main` and `feature` behind
/// it, `up` with `main` too, a stash and uncommitted changes.
///
/// ```text
/// ○      Uncommitted changes (2)
/// ◙      ►main↔origin,up  Merge branch 'feature'        9
/// │ ■    feature  feature: second                       8
/// ■ │    light  Octopus merge of x and y                7   (5, 3, 2)
/// │ ■    origin/feature  feature: first                 6
/// │ ≡    stash@{0}  WIP on main                          (base 5)
/// ■      main: prepare to merge                         5
/// │ ■    x: one                                         3
/// │ │ ■  y: one                                         2
/// ■      v1.0  Initial commit                           1
/// ```
fn history() -> Vec<CommitRow> {
    vec![
        commit(9, &[7, 8], "Ann", day(9), "Merge branch 'feature'"),
        commit(8, &[6], "Bob", day(8), "feature: second"),
        commit(7, &[5, 3, 2], "Ann", day(7), "Octopus merge of x and y"),
        commit(6, &[5], "Bob", day(6), "feature: first"),
        commit(5, &[1], "Ann", day(5), "main: prepare to merge"),
        commit(3, &[1], "Cy", day(3), "x: one"),
        commit(2, &[1], "Cy", day(2), "y: one"),
        commit(1, &[], "Ann", day(1), "Initial commit"),
    ]
}

fn refs() -> Vec<RefLabel> {
    vec![
        label("refs/heads/feature", 8, None),
        label("refs/heads/main", 9, None),
        label("refs/remotes/origin/HEAD", 9, None),
        label("refs/remotes/origin/feature", 6, None),
        label("refs/remotes/origin/main", 9, None),
        label("refs/remotes/up/main", 9, None),
        label("refs/tags/light", 7, None),
        label("refs/tags/v1.0", 200, Some(1)),
    ]
}

fn stashes() -> Vec<StashRow> {
    vec![stash(50, 5, "stash@{0}", "WIP on main: prepare")]
}

/// The fixture's rows as the integration makes them: the row stream (03 §4.3) over `commits`.
fn rows_of(commits: Vec<CommitRow>, show_stashes: bool) -> Vec<Row> {
    let mut stream = RowStream::new(Some(oid(9)), true, stashes(), show_stashes);
    stream.page(commits)
}

/// A view of the whole fixture, the bar on the first row.
fn fixture_view() -> GitView {
    let mut view = GitView::new("box:/srv/repo");
    view.set_refs(refs(), HeadRef::Branch("refs/heads/main".into()));
    view.set_rows(rows_of(history(), true), false, 2);
    view.set_paging(Paging::AllLoaded);
    view
}

const SUBJECTS: &[&str] = &[
    "Uncommitted changes (2)",
    "Merge branch 'feature'",
    "feature: second",
    "Octopus merge of x and y",
    "feature: first",
    "WIP on main: prepare",
    "main: prepare to merge",
    "x: one",
    "y: one",
    "Initial commit",
];

/// The date column's text of `secs`, by this machine's local time.
fn date_of(secs: i64) -> String {
    let tm = chrome::local_tm(secs).unwrap();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

/// 03 §4.2, §4.4 (G-G1's view part): each row's labels in 03's order, its subject, date, author
/// and short hash; the uncommitted row's `*` columns; `origin/HEAD` not drawn; the frame title.
#[test]
fn rows_draw_their_labels_and_columns() {
    let fx = Fx::new(1000.0, 500.0, fixture_view());
    assert_eq!(fx.drawn_subjects(SUBJECTS), SUBJECTS, "every row, in order");
    assert_eq!(
        fx.row_of("Merge branch 'feature'"),
        [
            "◙",
            "►main↔origin,up",
            "Merge branch 'feature'",
            &date_of(day(9)),
            "Ann",
            "09090909"
        ],
        "the checked-out branch with both remotes, one label (03:405–407)"
    );
    assert!(fx
        .row_of("feature: first")
        .contains(&"origin/feature".into()));
    assert!(fx
        .row_of("Octopus merge of x and y")
        .contains(&"light".into()));
    assert!(
        fx.row_of("Initial commit").contains(&"v1.0".into()),
        "at *objectname"
    );
    assert!(fx
        .row_of("WIP on main: prepare")
        .contains(&"stash@{0}".into()));
    let uncommitted = fx.row_of("Uncommitted changes (2)");
    assert_eq!(uncommitted, ["○", "Uncommitted changes (2)", "*", "*"]);
    assert!(!fx.texts().iter().any(|(_, t)| t.contains("origin/HEAD")));
    assert!(fx.shows("box:/srv/repo"), "the frame's title");
    for head in ["Description", "Date", "Author", "Commit"] {
        assert!(fx.shows(head), "the head {head}");
    }
}

/// 03 §4.4: label order — HEAD (detached) first, then local, remote, tags; the checked-out
/// commit's `◙`.
#[test]
fn a_detached_head_is_labelled_first() {
    let mut view = fixture_view();
    view.set_refs(refs(), HeadRef::Detached(oid(9)));
    let fx = Fx::new(1000.0, 500.0, view);
    let row = fx.row_of("Merge branch 'feature'");
    assert_eq!(row[1..3], ["HEAD", "main↔origin,up"], "{row:?}");
}

/// 03 §4.4, AGENTS.md "One look": the label kinds' colours, the lanes' and the node glyphs, all
/// `chrome::git`'s; a merge commit's text dim (03 §4.2).
#[test]
fn the_colours_are_chrome_gits() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    // The bar off the rows checked here.
    fx.press(Key::End);
    let colour = |t: &str| fx.drv.colors_of(t).unwrap_or_else(|| panic!("{t}"))[0];
    assert_eq!(colour("►main↔origin,up"), LOOK.local.text);
    assert_eq!(colour("origin/feature"), LOOK.remote.text);
    assert_eq!(colour("v1.0"), LOOK.tag.text);
    assert_eq!(colour("stash@{0}"), LOOK.stash.text);
    assert_eq!(
        colour("Merge branch 'feature'"),
        LOOK.dim,
        "a merge is muted"
    );
    assert_eq!(colour("feature: second"), LOOK.text);
    assert_eq!(
        colour("WIP on main: prepare"),
        LOOK.dim,
        "a stash row is dim"
    );
    assert_eq!(colour("○"), LOOK.uncommitted);
    assert_eq!(colour("◙"), look::lane(fx.view().graph().row(1).colour));
    // Each label's back is a rectangle of its colour behind it.
    let behind = |t: &str| {
        let at = fx.drv.find_text(t).unwrap();
        fx.drv
            .rects()
            .into_iter()
            .filter(|(r, _, _)| r.contains(at.center()))
            .map(|(_, c, _)| c)
            .next_back()
            .unwrap()
    };
    assert_eq!(behind("►main↔origin,up"), LOOK.local.back);
    assert_eq!(behind("origin/feature"), LOOK.remote.back);
    assert_eq!(behind("v1.0"), LOOK.tag.back);
    assert!(
        !look::LANES.contains(&PALETTE.bar),
        "no lane is the bar's light red"
    );
}

/// The lane layout drawn (03 §4.3, S2-C's API): every node segment of every visible row as one
/// straight line from the node to the slot on the row's edge in its occupancy's colour, every
/// pass-through of the visible rows as a vertical line, the uncommitted row's line dashed and dim.
#[test]
fn the_graph_draws_s2cs_segments() {
    let fx = Fx::new(1000.0, 500.0, fixture_view());
    let view = fx.view();
    let geo = view.geometry().expect("a pass drew the graph");
    let lines = fx.drv.segments();
    let near = |a: Pos2, b: Pos2| (a - b).length() < 0.6;
    let has = |from: Pos2, to: Pos2, colour: Color32| {
        lines.iter().any(|(p, c)| {
            *c == colour
                && ((near(p[0], from) && near(p[1], to)) || (near(p[1], from) && near(p[0], to)))
        })
    };
    let dashed_on = |from: Pos2, to: Pos2| {
        let span = Rect::from_two_pos(from, to).expand(0.6);
        lines
            .iter()
            .any(|(p, c)| *c == LOOK.uncommitted && span.contains(p[0]) && span.contains(p[1]))
    };
    let range = view.on_screen();
    assert_eq!(range, 0..10, "every row fits");
    let mut solid = 0;
    for i in range.clone() {
        let node = view.graph().row(i);
        for s in view.graph().segments(i) {
            let from = geo.node(i, node.column);
            let to = match s.end {
                End::Top => geo.top(i, s.slot),
                End::Bottom => geo.bottom(i, s.slot),
            };
            if s.dashed {
                assert!(dashed_on(from, to), "row {i}: the dashed piece {s:?}");
            } else {
                assert!(has(from, to, look::lane(s.colour)), "row {i}: {s:?}");
                solid += 1;
            }
        }
    }
    assert!(
        solid >= 12,
        "the fixture's node segments are drawn ({solid})"
    );
    let passes = view.graph().pass_throughs(range);
    assert!(!passes.is_empty());
    for p in passes {
        let from = geo.top(p.rows.start, p.slot);
        let to = geo.bottom(p.rows.end - 1, p.slot);
        if p.dashed {
            assert!(dashed_on(from, to), "{p:?}");
        } else {
            assert!(has(from, to, look::lane(p.colour)), "{p:?}");
        }
    }
}

/// 03 §4.2: only the rows on screen are painted — 100 000 loaded rows draw no more than a screen
/// of them, and the lanes of the visible rows only.
#[test]
fn only_the_visible_rows_are_painted() {
    let n = 100_000u32;
    let commits: Vec<CommitRow> = (0..n)
        .map(|k| {
            let id = |k: u32| Oid::parse(&format!("{k:040x}")).unwrap();
            CommitRow {
                oid: id(n - k),
                parents: if k + 1 < n {
                    vec![id(n - k - 1)]
                } else {
                    vec![]
                },
                author_name: "Ann".into(),
                author_email: "ann@example.com".into(),
                author_time: day(1),
                commit_time: day(1),
                subject: format!("commit {}", n - k),
            }
        })
        .collect();
    let mut view = GitView::new("box:/srv/big");
    view.set_refs(Vec::new(), HeadRef::Unborn);
    let mut stream = RowStream::new(None, false, Vec::new(), true);
    view.set_rows(stream.page(commits), false, 0);
    view.set_paging(Paging::AllLoaded);
    let fx = Fx::new(1000.0, 500.0, view);
    let on_screen = fx.view().on_screen();
    assert!(
        on_screen.len() > 10 && on_screen.len() < 40,
        "{on_screen:?}"
    );
    let subjects = fx
        .texts()
        .into_iter()
        .filter(|(_, t)| t.starts_with("commit "))
        .count();
    assert_eq!(subjects, on_screen.len(), "one subject a visible row");
    assert!(fx.drv.segments().len() < 200, "{}", fx.drv.segments().len());
    assert!(fx.shows("100000 commits · all loaded"));
}

/// 03 §4.2: lanes beyond a third of the panel are cut with `»`; narrowing drops Author, then
/// Date, then Commit.
#[test]
fn narrow_panels_cut_lanes_and_drop_columns() {
    // Twelve branch tips on one root: twelve lanes.
    let mut commits: Vec<CommitRow> = (0..12)
        .map(|k| {
            commit(
                100 + k,
                &[1],
                "Ann",
                day(10 - k as i64),
                &format!("tip {k}"),
            )
        })
        .collect();
    commits.push(commit(1, &[], "Ann", day(0), "root"));
    let mut view = GitView::new("box:/srv/wide");
    view.set_refs(Vec::new(), HeadRef::Unborn);
    view.set_rows(
        RowStream::new(None, false, vec![], true).page(commits),
        false,
        0,
    );
    view.set_paging(Paging::AllLoaded);
    let mut fx = Fx::new(1000.0, 400.0, view);
    assert_eq!(fx.view().graph().width(), 12);
    assert!(!fx.shows("»"), "a third of 123 cells holds 12 lanes");
    assert!(fx.shows("Author") && fx.shows("Date") && fx.shows("Commit"));
    // 66 cells inside: a third holds 11 lanes; Author goes first.
    fx.drv.resize(66.0 * 8.0 + 20.0, 400.0);
    fx.idle();
    fx.idle();
    assert!(fx.texts().iter().any(|(_, t)| t == "»"), "lanes cut");
    assert!(!fx.shows("Author"), "Author goes first");
    assert!(fx.shows("Date") && fx.shows("Commit"));
    // 40 cells: then Date.
    fx.drv.resize(40.0 * 8.0 + 20.0, 400.0);
    fx.idle();
    fx.idle();
    assert!(!fx.shows("Date"), "then Date");
    assert!(fx.shows("Commit"), "Commit stays longest");
    // 30 cells: then Commit; the description keeps its room.
    fx.drv.resize(30.0 * 8.0 + 20.0, 400.0);
    fx.idle();
    fx.idle();
    assert!(!fx.shows("Commit"));
    assert!(fx.shows("Description"));
}

/// 03 §4.5: more is asked for once when the bar comes within one screen of the last loaded row;
/// a page appended keeps the bar on its row and in view, and rows above never move.
#[test]
fn paging_asks_once_and_keeps_the_bar_in_view() {
    let n = 600u32;
    let id = |k: u32| Oid::parse(&format!("{k:040x}")).unwrap();
    let make = |from: u32, to: u32| -> Vec<CommitRow> {
        (from..to)
            .map(|k| CommitRow {
                oid: id(k + 1),
                parents: vec![id(k + 2)],
                author_name: "Ann".into(),
                author_email: "ann@example.com".into(),
                author_time: day(1),
                commit_time: day(1),
                subject: format!("c{k}"),
            })
            .collect()
    };
    let mut stream = RowStream::new(None, false, Vec::new(), true);
    let mut view = GitView::new("box:/srv/long");
    view.set_refs(Vec::new(), HeadRef::Unborn);
    view.set_rows(stream.page(make(0, n)), false, 0);
    view.set_paging(Paging::More(500));
    let mut fx = Fx::new(1000.0, 500.0, view);
    assert!(fx.shows("600 commits · 500 more"));
    assert!(fx.take().is_empty(), "the bar is far from the end");
    let asked = fx.press(Key::PageDown);
    assert!(!asked.contains(&GitAction::MoreRows), "{asked:?}");
    let asked = fx.press(Key::End);
    assert_eq!(
        asked.iter().filter(|a| **a == GitAction::MoreRows).count(),
        1
    );
    fx.idle();
    fx.idle();
    assert!(fx.take().is_empty(), "asked once");
    let bar = fx.bar();
    let on_screen = fx.view().on_screen();
    let rows_before: Vec<Row> = fx.view().rows().to_vec();
    let lanes_before: Vec<_> = (0..600).map(|i| *fx.view().graph().row(i)).collect();
    fx.view_mut().append_rows(stream.page(make(n, n + 500)));
    fx.view_mut().set_paging(Paging::AllLoaded);
    fx.idle();
    assert_eq!(fx.bar(), bar, "the bar stays on its row");
    assert_eq!(
        fx.view().on_screen(),
        on_screen,
        "and on screen where it was"
    );
    assert_eq!(
        fx.view().rows()[..600],
        rows_before[..],
        "rows above never move"
    );
    let lanes_after: Vec<_> = (0..600).map(|i| *fx.view().graph().row(i)).collect();
    assert_eq!(lanes_after, lanes_before, "their lanes neither");
    assert!(fx.shows("1100 commits · all loaded"));
    assert!(fx.take().is_empty());
    // A reload keeps the bar on its commit.
    let rows = fx.view().rows().to_vec();
    fx.view_mut().set_rows(rows, false, 0);
    fx.idle();
    assert_eq!(fx.bar(), bar);
}

/// G-F2 (03:1155): a letter typed in the graph opens Find with it, the bar jumps to the first
/// match as the rest is typed, Enter goes to the next, Shift+Enter to the previous, the foot says
/// `k of n`, Esc closes it with the bar where it is.
#[test]
fn g_f2_typing_merge_finds_it() {
    for slowly in [true, false] {
        let mut fx = Fx::new(1000.0, 500.0, fixture_view());
        assert_eq!(fx.bar(), RowKey::Uncommitted);
        if slowly {
            fx.typed("m");
            assert_eq!(fx.view().find_text(), Some("m"), "Find opens with m");
            assert!(fx.shows("m"), "in its field");
            assert_eq!(fx.bar(), RowKey::Commit(oid(9)), "the first match of m");
            fx.type_slowly("erge");
        } else {
            fx.typed("merge");
        }
        assert_eq!(fx.view().find_text(), Some("merge"), "slowly: {slowly}");
        assert!(fx.shows("merge"));
        assert_eq!(fx.bar(), RowKey::Commit(oid(9)), "the first match");
        assert!(fx.shows("1 of 3"), "the foot: {:?}", fx.texts());
        fx.press(Key::Enter);
        assert_eq!(fx.bar(), RowKey::Commit(oid(7)), "Enter: the next");
        assert!(fx.shows("2 of 3"));
        fx.press(Key::Enter);
        assert_eq!(fx.bar(), RowKey::Commit(oid(5)));
        assert!(fx.shows("3 of 3"));
        fx.press(Key::Enter);
        assert_eq!(fx.bar(), RowKey::Commit(oid(9)), "around the end");
        fx.key(Key::Enter, Modifiers::SHIFT);
        assert_eq!(
            fx.bar(),
            RowKey::Commit(oid(5)),
            "Shift+Enter: the previous"
        );
        let asked = fx.press(Key::Escape);
        assert!(asked.is_empty(), "Esc closes Find only: {asked:?}");
        assert_eq!(fx.view().find_text(), None);
        assert_eq!(fx.bar(), RowKey::Commit(oid(5)), "the bar where it is");
        assert!(!fx.shows("3 of 3"));
    }
}

/// 03 §4.7: Find matches labels, author names and hash prefixes too, case-insensitive unless
/// asked; Cmd/Ctrl+F opens it empty; no F-key acts while its field has the typing.
#[test]
fn find_matches_labels_authors_and_hashes() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.key(Key::F, Modifiers::COMMAND);
    assert_eq!(fx.view().find_text(), Some(""));
    fx.type_slowly("ORIGIN/FEAT");
    assert_eq!(fx.bar(), RowKey::Commit(oid(6)), "a label, any case");
    let asked = fx.press(Key::F3);
    assert!(
        asked.is_empty(),
        "no plain F-key while Find types: {asked:?}"
    );
    fx.press(Key::Escape);
    fx.typed("cy");
    assert_eq!(fx.bar(), RowKey::Commit(oid(3)), "an author");
    fx.press(Key::Escape);
    fx.typed("0505");
    assert_eq!(fx.bar(), RowKey::Commit(oid(5)), "a hash prefix");
    fx.press(Key::Escape);
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.typed("x");
    fx.view_mut().set_find_case(true);
    fx.idle();
    fx.typed(": ONE");
    assert!(fx.shows("no match"), "case-sensitive");
}

/// G-T1's view part (03:1161): Enter on a commit opens the details pane and asks for its details;
/// they show the summary and the files with `A`/`M`/`D`/`R` and their counts; Enter on a file asks
/// for its diff and marks it `√`; Esc gives the keyboard back, Esc again closes the pane.
#[test]
fn g_t1_enter_shows_the_summary_and_files() {
    let mut fx = Fx::new(1000.0, 800.0, fixture_view());
    fx.press(Key::ArrowDown);
    let asked = fx.press(Key::Enter);
    let subject = Subject::Row(RowKey::Commit(oid(9)));
    assert_eq!(asked, [GitAction::Details(subject.clone())]);
    assert!(fx.view().details_open());
    let file = |letter, path: &str, old: Option<&str>, lines| FileRow {
        letter,
        path: path.into(),
        old_path: old.map(Into::into),
        lines,
        lossy: false,
    };
    let details = Details {
        subject: subject.clone(),
        summary: Some(Summary {
            oid: oid(9),
            parents: vec![oid(7), oid(8)],
            author_name: "Ann".into(),
            author_email: "ann@example.com".into(),
            author_time: day(9),
            committer_name: "Dee".into(),
            committer_email: "dee@example.com".into(),
            commit_time: day(9) + 60,
            message: "Merge branch 'feature'\n\nBrings the feature in.\n".into(),
        }),
        contains: None,
        groups: vec![FileGroup {
            group: Group::Files,
            files: vec![
                file('A', "new.txt", None, Some((3, 0))),
                file('M', "src/lib.rs", None, Some((10, 2))),
                file('D', "old.txt", None, Some((0, 7))),
                file('R', "b.txt", Some("a.txt"), Some((1, 1))),
                file('M', "logo.png", None, None),
            ],
        }],
        error: None,
    };
    // A result for another subject is dropped.
    let mut stale = details.clone();
    stale.subject = Subject::Row(RowKey::Commit(oid(8)));
    fx.view_mut().set_details(stale);
    fx.idle();
    assert!(!fx.shows("new.txt"), "a stale result is dropped");
    fx.view_mut().set_details(details);
    fx.idle();
    assert!(fx.shows(oid(9).as_str()), "Commit: the full hash");
    assert!(
        fx.shows(oid(7).as_str()) && fx.shows(oid(8).as_str()),
        "Parents"
    );
    assert!(fx.shows("Ann <ann@example.com>"));
    assert!(fx.shows("Dee <dee@example.com>"), "the committer differs");
    assert!(fx.shows(&date_of(day(9))) && fx.shows(&date_of(day(9) + 60)));
    assert!(fx.shows("…"), "In: not answered yet");
    assert!(fx.shows("Brings the feature in."), "the whole message");
    assert_eq!(fx.row_of("new.txt"), ["A", "new.txt", "+3 -0"]);
    assert_eq!(fx.row_of("src/lib.rs"), ["M", "src/lib.rs", "+10 -2"]);
    assert_eq!(fx.row_of("old.txt"), ["D", "old.txt", "+0 -7"]);
    assert_eq!(fx.row_of("a.txt → b.txt"), ["R", "a.txt → b.txt", "+1 -1"]);
    assert_eq!(fx.row_of("logo.png"), ["M", "logo.png"], "binary: blank");
    // The keyboard is in the file list, its bar on the first file.
    let asked = fx.press(Key::Enter);
    assert_eq!(
        asked,
        [GitAction::Diff {
            subject: subject.clone(),
            group: Group::Files,
            file: file('A', "new.txt", None, Some((3, 0))),
        }]
    );
    assert_eq!(
        fx.row_of("new.txt"),
        ["√", "A", "new.txt", "+3 -0"],
        "viewed"
    );
    fx.press(Key::ArrowDown);
    let asked = fx.key(Key::F3, Modifiers::SHIFT);
    assert!(
        matches!(&asked[..], [GitAction::RevisionFile { file, .. }] if file.path == "src/lib.rs"),
        "{asked:?}"
    );
    // Esc: the keyboard back to the graph; the pane follows the bar.
    assert!(fx.press(Key::Escape).is_empty());
    assert!(fx.view().details_open());
    let asked = fx.press(Key::ArrowDown);
    assert_eq!(
        asked,
        [GitAction::Details(Subject::Row(RowKey::Commit(oid(8))))]
    );
    // Esc again closes it; once more, with nothing open: unpin.
    assert!(fx.press(Key::Escape).is_empty());
    assert!(!fx.view().details_open());
    assert_eq!(fx.press(Key::Escape), [GitAction::Unpin]);
}

/// 03 §6.1: the uncommitted row's groups, a stash's untracked file and an untracked folder.
#[test]
fn the_uncommitted_rows_groups() {
    let mut fx = Fx::new(1000.0, 700.0, fixture_view());
    let asked = fx.press(Key::F3);
    let subject = Subject::Row(RowKey::Uncommitted);
    assert_eq!(asked, [GitAction::Details(subject.clone())]);
    let row = |letter, path: &str| FileRow {
        letter,
        path: path.into(),
        old_path: None,
        lines: None,
        lossy: false,
    };
    fx.view_mut().set_details(Details {
        subject: subject.clone(),
        summary: None,
        contains: None,
        groups: vec![
            FileGroup {
                group: Group::Staged,
                files: vec![row('M', "a.txt")],
            },
            FileGroup {
                group: Group::NotStaged,
                files: vec![row('M', "b.txt")],
            },
            FileGroup {
                group: Group::Untracked,
                files: vec![row('?', "dir/")],
            },
        ],
        error: None,
    });
    fx.idle();
    for head in ["Staged", "Not staged", "Untracked"] {
        assert!(fx.shows(head), "{head}");
    }
    assert_eq!(fx.row_of("dir/"), ["►", "?", "dir/"]);
    fx.press(Key::End);
    let asked = fx.press(Key::ArrowRight);
    assert_eq!(
        asked,
        [GitAction::ListFolder {
            subject,
            dir: "dir/".into()
        }]
    );
}

/// 03 §6.4: one marked row and F3 on another compares them, the lower row the older end; with
/// two marks F3 is the bar's own details and the status row says why.
#[test]
fn mark_and_compare() {
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    fx.press(Key::End);
    fx.press(Key::Space);
    assert!(fx.view().marked(&RowKey::Commit(oid(1))));
    let colour = fx.drv.colors_of("Initial commit").unwrap()[0];
    assert_eq!(
        colour, LOOK.marked,
        "a marked row is yellow, under the bar too"
    );
    fx.press(Key::Home);
    fx.press(Key::ArrowDown);
    let asked = fx.press(Key::F3);
    assert_eq!(
        asked,
        [GitAction::Details(Subject::Compare {
            from: RowKey::Commit(oid(1)),
            to: RowKey::Commit(oid(9)),
        })]
    );
    fx.press(Key::Escape);
    fx.press(Key::Escape);
    fx.press(Key::Insert);
    let asked = fx.press(Key::F3);
    assert_eq!(
        asked,
        [GitAction::Details(Subject::Row(RowKey::Commit(oid(8))))],
        "two marks: the bar's own"
    );
    assert!(fx
        .view()
        .status_line()
        .text
        .contains("exactly one marked row"));
}

/// 03 §9: the keys of slice 2 give their actions; slice 3's do nothing yet; the label cursor
/// moves over the row's labels and inverts the one it is on; Cmd/Ctrl+J goes to HEAD's row.
#[test]
fn each_key_gives_its_action() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    let cmd = Modifiers::COMMAND;
    let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
    assert_eq!(fx.press(Key::F1), [GitAction::Help]);
    assert_eq!(fx.press(Key::F4), [GitAction::Edit(None)]);
    assert_eq!(fx.press(Key::F9), [GitAction::SwitchMode]);
    assert_eq!(fx.press(Key::Tab), [GitAction::OtherSide]);
    assert_eq!(fx.key(Key::Tab, Modifiers::SHIFT), [GitAction::OtherSide]);
    assert_eq!(fx.key(Key::R, cmd), [GitAction::Refresh]);
    assert_eq!(fx.key(Key::G, cmd), [GitAction::Pin]);
    assert_eq!(fx.key(Key::G, cmd_shift), [GitAction::Wide]);
    for slice3 in [Key::F2, Key::F5, Key::F6, Key::F7, Key::F8, Key::F10] {
        assert!(fx.press(slice3).is_empty(), "{slice3:?} lands in S3");
    }
    assert!(fx.key(Key::Z, cmd).is_empty());
    fx.press(Key::End);
    fx.key(Key::J, cmd);
    assert_eq!(fx.bar(), RowKey::Commit(oid(9)), "Cmd/Ctrl+J: HEAD's row");
    fx.press(Key::ArrowRight);
    assert_eq!(
        fx.drv.colors_of("►main↔origin,up").unwrap()[0],
        LOOK.local.inverted().text,
        "the label cursor inverts its label"
    );
    fx.press(Key::ArrowRight);
    fx.press(Key::ArrowRight);
    fx.press(Key::ArrowLeft);
    fx.press(Key::ArrowLeft);
    assert_eq!(
        fx.drv.colors_of("►main↔origin,up").unwrap()[0],
        LOOK.local.text
    );
}

/// 03 §9.1: the F-key row while the git panel has the keyboard, slice 2's keys only.
#[test]
fn the_f_key_row_is_slice_twos() {
    let row = |mode| -> Vec<(u8, &str)> { fkeys(mode).iter().map(|k| (k.n, k.label)).collect() };
    assert_eq!(
        row(Mode::Preview),
        [(1, "Help"), (3, "View"), (4, "Edit"), (9, "Dual")]
    );
    assert_eq!(row(Mode::Dual)[3], (9, "Prevw"));
}

/// A click puts the bar on a row, a double click opens its details, the wheel scrolls.
#[test]
fn a_click_moves_the_bar_and_a_double_click_opens_details() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    let at = fx.drv.find_text("x: one").unwrap().center();
    fx.drv.click(&mut fx.host, at);
    assert_eq!(fx.bar(), RowKey::Commit(oid(3)));
    assert!(fx.take().is_empty());
    // Past egui's double-click time, so the next two clicks are a double click of their own.
    fx.drv.idle(&mut fx.host, 30);
    let at = fx.drv.find_text("y: one").unwrap().center();
    fx.drv.click(&mut fx.host, at);
    fx.drv.click(&mut fx.host, at);
    assert_eq!(
        fx.take(),
        [GitAction::Details(Subject::Row(RowKey::Commit(oid(2))))]
    );
}

/// G-F1 (03:1153), each filter of 03 §4.6 on G-G1's fixture: the view hides the labels and rows it
/// can at once, hands the integration what the log is read from — the revisions for
/// `history::roots`, HEAD, the stashes' bases, the order, first parent, reflog — and, given the
/// rows git returns for them (fixed here), shows exactly those; the title says `[filtered]`.
#[test]
fn g_f1_each_filter_changes_the_rows() {
    let names =
        |refs: Vec<RefLabel>| -> Vec<String> { refs.into_iter().map(|r| r.refname).collect() };
    let open = |fx: &mut Fx| {
        fx.view_mut().open_filters();
        // egui lays a new dialog out unseen first.
        fx.idle();
        fx.idle();
        assert!(fx.shows("Filters"), "the dialog");
    };
    let apply = |fx: &mut Fx| -> Filters {
        let asked = fx.press(Key::Enter);
        match &asked[..] {
            [GitAction::FiltersChanged(f)] => {
                assert_eq!(f, fx.view().filters());
                f.clone()
            }
            other => panic!("{other:?}"),
        }
    };
    let all: Vec<String> = SUBJECTS.iter().map(|s| s.to_string()).collect();

    // Tags off: no tag labels, no tags among the revisions.
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    open(&mut fx);
    fx.drv.click_text_on_top(&mut fx.host, "Tags");
    let f = apply(&mut fx);
    assert!(!f.tags);
    assert!(!names(f.revisions(&refs(), &[]))
        .iter()
        .any(|r| r.starts_with("refs/tags/")));
    assert!(f.head_is_revision());
    fx.idle();
    assert!(!fx.shows("v1.0") && !fx.shows("light"), "no tag labels");
    assert!(fx.shows("box:/srv/repo [filtered]"));

    // A remote off: its labels go, its branches are no revisions.
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    open(&mut fx);
    fx.drv.click_text_on_top(&mut fx.host, "origin");
    let f = apply(&mut fx);
    assert_eq!(
        names(f.revisions(&refs(), &[])),
        [
            "refs/heads/feature",
            "refs/heads/main",
            "refs/remotes/up/main",
            "refs/tags/light",
            "refs/tags/v1.0"
        ]
    );
    fx.idle();
    assert!(fx.shows("►main↔up") && !fx.shows("origin/feature"));

    // Branches: only `feature` — the chosen refs alone are the revisions (no HEAD, tags or
    // stashes: 03:298–299), and the rows are its history.
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    open(&mut fx);
    fx.drv.click_text_on_top(&mut fx.host, "All branches");
    fx.drv.click_text_on_top(&mut fx.host, "feature");
    let f = apply(&mut fx);
    assert_eq!(names(f.revisions(&refs(), &[])), ["refs/heads/feature"]);
    assert!(!f.head_is_revision());
    assert!(f.stash_revisions(&stashes()).is_empty());
    let feature: Vec<CommitRow> = history()
        .into_iter()
        .filter(|c| [8, 6, 5, 1].contains(&c.oid.as_str()[..2].parse::<u32>().unwrap()))
        .collect();
    fx.view_mut()
        .set_rows(rows_of(feature, f.stashes), f.first_parent, 2);
    fx.idle();
    assert_eq!(
        fx.drawn_subjects(SUBJECTS),
        [
            "Uncommitted changes (2)",
            "feature: second",
            "feature: first",
            "WIP on main: prepare",
            "main: prepare to merge",
            "Initial commit"
        ]
    );
    assert!(fx.shows("feature") && !fx.shows("►main↔origin,up") && !fx.shows("v1.0"));

    // Stashes off: no stash row, no stash base among the revisions.
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    open(&mut fx);
    fx.drv.click_text_on_top(&mut fx.host, "Stashes");
    let f = apply(&mut fx);
    assert!(f.stash_revisions(&stashes()).is_empty());
    fx.view_mut()
        .set_rows(rows_of(history(), f.stashes), f.first_parent, 2);
    fx.idle();
    let mut expected = all.clone();
    expected.retain(|s| s != "WIP on main: prepare");
    assert_eq!(fx.drawn_subjects(SUBJECTS), expected);

    // First parent only: git gives the first-parent chain and the layout draws first parents.
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    open(&mut fx);
    fx.drv.click_text_on_top(&mut fx.host, "First parent only");
    let f = apply(&mut fx);
    assert!(f.first_parent);
    let chain: Vec<CommitRow> = history()
        .into_iter()
        .filter(|c| [9, 7, 5, 1].contains(&c.oid.as_str()[..2].parse::<u32>().unwrap()))
        .collect();
    fx.view_mut()
        .set_rows(rows_of(chain, f.stashes), f.first_parent, 2);
    fx.idle();
    assert_eq!(
        fx.view().graph().width(),
        2,
        "the stash's lane beside one line"
    );
    let merge = fx
        .view()
        .rows()
        .iter()
        .position(|r| r.oid() == Some(&oid(9)))
        .unwrap();
    assert_eq!(
        fx.view().graph().segments(merge).len(),
        2,
        "into it and to one parent"
    );
    assert_eq!(
        fx.drawn_subjects(SUBJECTS),
        [
            "Uncommitted changes (2)",
            "Merge branch 'feature'",
            "Octopus merge of x and y",
            "WIP on main: prepare",
            "main: prepare to merge",
            "Initial commit"
        ]
    );

    // Order: the dialog's choice reaches the log's options; the rows are drawn as given.
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    open(&mut fx);
    fx.drv.click_text_on_top(&mut fx.host, "author date");
    assert_eq!(apply(&mut fx).order, Order::AuthorDate);
    open(&mut fx);
    fx.drv.click_text_on_top(&mut fx.host, "topological");
    assert_eq!(apply(&mut fx).order, Order::Topo);

    // Reflog commits: `--reflog` with every branch shown, a branch filter or not.
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    fx.view_mut().only_branch("refs/heads/feature");
    open(&mut fx);
    fx.drv.click_text_on_top(&mut fx.host, "Reflog commits");
    let f = apply(&mut fx);
    assert!(f.reflog);
    assert!(f.head_is_revision());
    assert_eq!(
        names(f.revisions(&refs(), &[])).len(),
        7,
        "every branch and tag, not origin/HEAD"
    );

    // Back to the defaults: no `[filtered]`.
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    open(&mut fx);
    fx.drv.click_text_on_top(&mut fx.host, "Tags");
    apply(&mut fx);
    open(&mut fx);
    fx.drv.click_text_on_top(&mut fx.host, "Tags");
    assert!(apply(&mut fx).is_default());
    fx.idle();
    assert!(fx.shows("box:/srv/repo"));

    // Esc cancels the dialog: nothing changes.
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    open(&mut fx);
    fx.drv.click_text_on_top(&mut fx.host, "Tags");
    assert!(fx.press(Key::Escape).is_empty());
    assert!(fx.view().filters().is_default());
    // A filter that leaves no ref: `no refs chosen` (03:302–304).
    fx.view_mut().set_paging(Paging::NoRefsChosen);
    fx.idle();
    assert!(fx.shows("no refs chosen"));
}

/// 03 §4.7: History search's dialog asks for the search; its results show as a flat list paged
/// like the graph; Enter on one asks for its details; Esc returns to the graph with the bar on that
/// commit when it is loaded.
#[test]
fn history_search_shows_a_flat_list() {
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    fx.key(Key::F, Modifiers::COMMAND | Modifiers::SHIFT);
    assert!(fx.shows("History search"));
    fx.typed("prepare");
    let asked = fx.press(Key::Enter);
    let query = SearchQuery {
        message: "prepare".into(),
        ..Default::default()
    };
    assert_eq!(asked, [GitAction::Search(query.clone())]);
    // Another search's rows are dropped.
    let other = SearchQuery {
        message: "other".into(),
        ..Default::default()
    };
    fx.view_mut().set_search_results(
        &other,
        vec![commit(3, &[1], "Cy", day(3), "x: one")],
        Paging::AllLoaded,
    );
    fx.idle();
    assert!(!fx.shows("1 found · all loaded"));
    fx.view_mut().set_search_results(
        &query,
        vec![commit(5, &[1], "Ann", day(5), "main: prepare to merge")],
        Paging::AllLoaded,
    );
    fx.idle();
    assert!(fx.shows("main: prepare to merge"));
    assert!(
        !fx.shows("Merge branch 'feature'"),
        "the graph's rows are not shown"
    );
    assert!(!fx.shows("◙") && !fx.shows("■"), "no lanes");
    assert!(fx.shows("1 found · all loaded"));
    let asked = fx.press(Key::Enter);
    assert_eq!(
        asked,
        [GitAction::Details(Subject::Row(RowKey::Commit(oid(5))))]
    );
    fx.press(Key::Escape);
    fx.press(Key::Escape);
    fx.press(Key::Escape);
    assert!(fx.shows("Merge branch 'feature'"), "the graph again");
    assert_eq!(fx.bar(), RowKey::Commit(oid(5)), "on the result's commit");
}

/// 03 §4.4: the glyphs are one cell of the VGA font each.
#[test]
fn the_glyphs_are_one_cell_of_the_vga_font() {
    let fx = Fx::new(1000.0, 500.0, fixture_view());
    for c in [
        look::COMMIT,
        look::CHECKED_OUT,
        look::STASH,
        look::UNCOMMITTED,
        look::CUT,
        look::CURRENT,
        look::BOTH,
        look::VIEWED,
        look::AHEAD,
        look::BEHIND,
    ] {
        fx.drv.ctx.fonts_mut(|f| {
            assert!(f.has_glyph(&chrome::font(), c), "{c:?}");
            assert_eq!(f.glyph_width(&chrome::font(), c), chrome::CELL.x, "{c:?}");
        });
    }
}

/// 03 §4.2: the foot's right end says the branch's ahead / behind when it fits.
#[test]
fn the_foot_says_ahead_and_behind() {
    let mut view = fixture_view();
    view.tracking = Some(Tracking {
        branch: "main".into(),
        upstream: "origin/main".into(),
        ahead: 2,
        behind: 1,
    });
    let fx = Fx::new(1000.0, 500.0, view);
    assert!(fx.shows("main ↑2 ↓1 origin/main"));
    assert!(fx.shows("8 commits · all loaded"));
}

/// 03 §4.4: labels that do not fit end in `+N`.
#[test]
fn labels_that_do_not_fit_end_in_a_count() {
    let mut view = fixture_view();
    let mut more = refs();
    more.push(label("refs/tags/release-candidate-long-name", 9, None));
    more.push(label("refs/tags/v2.0", 9, None));
    view.set_refs(more, HeadRef::Branch("refs/heads/main".into()));
    // 30 cells inside: the description has 20.
    let fx = Fx::new(30.0 * 8.0 + 20.0, 500.0, view);
    let row = fx.row_of("►main↔origin,up");
    assert!(row.contains(&"+2".to_string()), "{row:?}");
    assert!(!fx.shows("v2.0"));
}

/// The file list's rule for a name that is not UTF-8 (the read model's `lossy`): the row shows as it is
/// and opens nothing; the status row says why.
#[test]
fn a_lossy_file_row_opens_nothing() {
    let mut fx = Fx::new(1000.0, 800.0, fixture_view());
    fx.press(Key::ArrowDown);
    fx.press(Key::Enter);
    let subject = Subject::Row(RowKey::Commit(oid(9)));
    fx.view_mut().set_details(Details {
        subject,
        summary: None,
        contains: None,
        groups: vec![FileGroup {
            group: Group::Files,
            files: vec![FileRow {
                letter: 'M',
                path: "caf\u{FFFD}.txt".into(),
                old_path: None,
                lines: Some((1, 1)),
                lossy: true,
            }],
        }],
        error: None,
    });
    fx.idle();
    assert!(fx.shows("caf\u{FFFD}.txt"));
    assert!(fx.press(Key::Enter).is_empty());
    assert!(fx.key(Key::F3, Modifiers::SHIFT).is_empty());
    assert!(fx.press(Key::F4).is_empty());
    assert!(fx.view().status_line().text.contains("not valid UTF-8"));
}

/// 03 §5 (03:500): a refresh never moves the bar off its commit — with Find open too, whose
/// matches are worked out again without jumping.
#[test]
fn a_refresh_keeps_find_and_the_bar() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.typed("merge");
    fx.press(Key::Enter);
    assert_eq!(fx.bar(), RowKey::Commit(oid(7)));
    assert!(fx.shows("2 of 3"));
    fx.view_mut()
        .set_refs(refs(), HeadRef::Branch("refs/heads/main".into()));
    fx.view_mut().set_rows(rows_of(history(), true), false, 2);
    fx.idle();
    assert_eq!(fx.bar(), RowKey::Commit(oid(7)), "the bar stays");
    assert!(fx.shows("2 of 3"), "and Find with it");
}

/// 03:428–429, 440: a load that reads as many pages as were loaded keeps the bar on its commit
/// even when that commit comes in a later page.
#[test]
fn a_reload_finds_the_bar_on_a_later_page() {
    let id = |k: u32| Oid::parse(&format!("{k:040x}")).unwrap();
    let make = |from: u32, to: u32| -> Vec<CommitRow> {
        (from..to)
            .map(|k| CommitRow {
                oid: id(k + 1),
                parents: vec![id(k + 2)],
                author_name: "Ann".into(),
                author_email: "ann@example.com".into(),
                author_time: day(1),
                commit_time: day(1),
                subject: format!("c{k}"),
            })
            .collect()
    };
    let mut stream = RowStream::new(None, false, Vec::new(), true);
    let mut view = GitView::new("box:/srv/long");
    view.set_refs(Vec::new(), HeadRef::Unborn);
    view.set_rows(stream.page(make(0, 600)), false, 0);
    view.append_rows(stream.page(make(600, 1100)));
    view.set_paging(Paging::AllLoaded);
    let mut fx = Fx::new(1000.0, 500.0, view);
    fx.press(Key::End);
    for _ in 0..10 {
        fx.press(Key::PageUp);
    }
    let bar = fx.bar();
    let place = fx.view().bar_place();
    assert!(
        matches!(&bar, RowKey::Commit(_)) && place.0 > 600,
        "{place:?}"
    );
    // The load again, page by page.
    let mut stream = RowStream::new(None, false, Vec::new(), true);
    fx.view_mut().set_rows(stream.page(make(0, 600)), false, 0);
    fx.idle();
    fx.view_mut().append_rows(stream.page(make(600, 1100)));
    fx.idle();
    assert_eq!(fx.bar(), bar, "back on its commit");
    assert_eq!(fx.view().bar_place(), place, "where it was on screen");
}

/// `SOPEN_STATE`'s `select=REV;details=1;label=N` (02 §9), as S2-F reaches them: the bar to a row
/// at once or once a page brings it, its details, the label cursor.
#[test]
fn select_details_and_label_from_outside() {
    let mut view = GitView::new("box:/srv/repo");
    view.set_refs(refs(), HeadRef::Branch("refs/heads/main".into()));
    let mut rows = rows_of(history(), true);
    let later = rows.split_off(6);
    view.set_rows(rows, false, 2);
    view.set_paging(Paging::More(500));
    let mut fx = Fx::new(1000.0, 500.0, view);
    assert!(
        !fx.view_mut().select(RowKey::Commit(oid(1))),
        "not loaded yet"
    );
    assert_eq!(fx.view().selecting(), Some(&RowKey::Commit(oid(1))));
    fx.view_mut().append_rows(later);
    fx.idle();
    assert_eq!(fx.bar(), RowKey::Commit(oid(1)), "once its page came");
    assert_eq!(fx.view().selecting(), None);
    assert!(fx.view_mut().select(RowKey::Commit(oid(9))));
    fx.view_mut().set_label_cursor(5);
    assert_eq!(
        fx.view().bar_label().map(|l| l.text),
        Some("►main↔origin,up".to_string()),
        "clamped to its one label"
    );
    let asked = fx.view_mut().open_details();
    assert_eq!(
        asked,
        [GitAction::Details(Subject::Row(RowKey::Commit(oid(9))))]
    );
    fx.idle();
    assert!(fx.view().details_open());
}

// Review round 1 (PR #72).

/// Item 1: Cmd/Ctrl+F from a history result's details pane opens no Find nobody sees — Find is
/// over the graph's rows, which the results hide — so the keys go on acting where they are seen.
#[test]
fn find_is_not_opened_under_history_results() {
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    fx.key(Key::F, Modifiers::COMMAND | Modifiers::SHIFT);
    fx.typed("prepare");
    fx.press(Key::Enter);
    let query = SearchQuery {
        message: "prepare".into(),
        ..Default::default()
    };
    fx.view_mut().set_search_results(
        &query,
        vec![commit(5, &[1], "Ann", day(5), "main: prepare to merge")],
        Paging::AllLoaded,
    );
    fx.idle();
    fx.press(Key::Enter);
    assert!(fx.view().details_open());
    fx.key(Key::F, Modifiers::COMMAND);
    assert_eq!(fx.view().find_text(), None, "no Find under the results");
    fx.typed("m");
    assert_eq!(fx.view().find_text(), None);
    fx.press(Key::Escape);
    fx.press(Key::Escape);
    assert!(!fx.view().details_open(), "Esc reached the pane");
    assert!(
        fx.shows("main: prepare to merge"),
        "the results still shown"
    );
}

/// Item 2: a repository's text with control characters (a label, a subject, an author, a path)
/// shows them escaped, on one row — nothing paints into the rows below.
#[test]
fn control_characters_show_escaped_on_their_row() {
    let mut view = fixture_view();
    let mut r = refs();
    r.push(label("refs/tags/x\ny", 8, None));
    view.set_refs(r, HeadRef::Branch("refs/heads/main".into()));
    let mut commits = history();
    commits[1].subject = "tab\there\nand a line".into();
    commits[1].author_name = "B\rob".into();
    view.set_rows(rows_of(commits, true), false, 2);
    let mut fx = Fx::new(1000.0, 800.0, view);
    assert!(fx.shows("x\\ny"), "{:?}", fx.texts());
    assert!(fx.shows("tab\\there\\nand a line"));
    assert!(fx.shows("B\\rob"));
    fx.press(Key::ArrowDown);
    fx.press(Key::ArrowDown);
    fx.press(Key::Enter);
    fx.view_mut().set_details(Details {
        subject: Subject::Row(RowKey::Commit(oid(8))),
        summary: None,
        contains: None,
        groups: vec![FileGroup {
            group: Group::Files,
            files: vec![FileRow {
                letter: 'A',
                path: "new\nline.txt".into(),
                old_path: None,
                lines: Some((1, 0)),
                lossy: false,
            }],
        }],
        error: None,
    });
    fx.idle();
    assert!(fx.shows("new\\nline.txt"));
    for (_, t) in fx.texts() {
        // A scroll bar's column is the one text drawn over several rows.
        let column = t.starts_with('↑');
        assert!(column || !t.chars().any(char::is_control), "{t:?}");
    }
}

/// Items 3 and 4: a remote is the longest remote name a remote branch starts with (two toggles
/// for `team` and `team/up`), and only a remote's own `HEAD` is left out — a branch whose name
/// ends in `/HEAD` is a branch.
#[test]
fn remotes_by_their_whole_names_and_only_their_own_head() {
    let names =
        |refs: Vec<RefLabel>| -> Vec<String> { refs.into_iter().map(|r| r.refname).collect() };
    let r = vec![
        label("refs/heads/feature/HEAD", 8, None),
        label("refs/heads/main", 9, None),
        label("refs/remotes/origin/feature/HEAD", 6, None),
        label("refs/remotes/team/main", 9, None),
        label("refs/remotes/team/up/HEAD", 9, None),
        label("refs/remotes/team/up/main", 9, None),
    ];
    let remotes: Vec<String> = ["origin", "team", "team/up"].map(String::from).to_vec();
    assert_eq!(
        names(Filters::default().revisions(&r, &remotes)),
        [
            "refs/heads/feature/HEAD",
            "refs/heads/main",
            "refs/remotes/origin/feature/HEAD",
            "refs/remotes/team/main",
            "refs/remotes/team/up/main"
        ]
    );
    let mut view = GitView::new("box:/srv/repo");
    view.set_remotes(remotes.clone());
    view.set_refs(r.clone(), HeadRef::Branch("refs/heads/main".into()));
    view.set_rows(rows_of(history(), true), false, 2);
    view.set_paging(Paging::AllLoaded);
    let mut fx = Fx::new(1000.0, 600.0, view);
    assert!(fx.shows("►main↔team,team/up"), "{:?}", fx.texts());
    assert!(fx.shows("feature/HEAD") && fx.shows("origin/feature/HEAD"));
    fx.view_mut().open_filters();
    fx.idle();
    fx.idle();
    assert!(fx.shows("team") && fx.shows("team/up"), "a toggle each");
    fx.drv.click_text_on_top(&mut fx.host, "team/up");
    let asked = fx.press(Key::Enter);
    let [GitAction::FiltersChanged(f)] = &asked[..] else {
        panic!("{asked:?}");
    };
    assert_eq!(
        names(f.revisions(&r, &remotes)),
        [
            "refs/heads/feature/HEAD",
            "refs/heads/main",
            "refs/remotes/origin/feature/HEAD",
            "refs/remotes/team/main"
        ]
    );
    fx.idle();
    assert!(fx.shows("►main↔team"));
    // Without the remotes' list: the first component, as before.
    assert_eq!(
        names(Filters::default().revisions(&r, &[])),
        [
            "refs/heads/feature/HEAD",
            "refs/heads/main",
            "refs/remotes/origin/feature/HEAD",
            "refs/remotes/team/main",
            "refs/remotes/team/up/HEAD",
            "refs/remotes/team/up/main"
        ]
    );
}

/// Item 5: the branch chooser lays out only the branches it shows on screen, however many there
/// are, and narrows as the filter is typed.
#[test]
fn the_branch_chooser_draws_only_its_visible_rows() {
    let mut many = refs();
    many.extend((0..20_000).map(|k| {
        let mut b = label(&format!("refs/heads/b{k:05}"), 1, None);
        b.object = Oid::parse(&format!("{k:040x}")).unwrap();
        b
    }));
    let mut view = fixture_view();
    view.set_refs(many, HeadRef::Branch("refs/heads/main".into()));
    let mut fx = Fx::new(1000.0, 600.0, view);
    fx.view_mut().open_filters();
    fx.idle();
    fx.idle();
    // egui paints only what is on screen; what a pass costs is the widgets it lays out.
    fx.idle();
    let widgets: usize = fx
        .drv
        .ctx
        .viewport(|v| v.prev_pass.widgets.layers().map(|(_, w)| w.len()).sum());
    assert!(widgets < 400, "{widgets} widgets laid out");
    fx.typed("b1999");
    fx.idle();
    assert!(fx.shows("b19995") && !fx.shows("b00001"));
}

/// Item 6: a message line longer than the pane wraps into lines of the pane, all of it readable.
#[test]
fn a_long_message_line_wraps_in_the_pane() {
    let long: Vec<String> = (0..60).map(|k| format!("word{k:02}")).collect();
    let long = long.join(" ");
    let mut fx = Fx::new(1000.0, 800.0, fixture_view());
    fx.press(Key::ArrowDown);
    fx.press(Key::Enter);
    fx.view_mut().set_details(Details {
        subject: Subject::Row(RowKey::Commit(oid(9))),
        summary: Some(Summary {
            oid: oid(9),
            parents: vec![oid(7), oid(8)],
            author_name: "Ann".into(),
            author_email: "ann@example.com".into(),
            author_time: day(9),
            committer_name: "Ann".into(),
            committer_email: "ann@example.com".into(),
            commit_time: day(9),
            message: format!("Merge\n\n{long}\n"),
        }),
        contains: Some(Vec::new()),
        groups: Vec::new(),
        error: None,
    });
    fx.idle();
    let lines: Vec<String> = fx
        .texts()
        .into_iter()
        .map(|(_, t)| t)
        .filter(|t| t.starts_with("word"))
        .collect();
    assert!(lines.len() >= 3, "{lines:?}");
    let squeezed = |s: &str| s.split_whitespace().collect::<String>();
    assert_eq!(
        squeezed(&lines.concat()),
        squeezed(&long),
        "every word, in order"
    );
    assert!(!lines.iter().any(|l| l.contains('…')));
}

/// Item 7: a label lays out no more of its text than its room and one character.
#[test]
fn a_huge_label_is_capped_before_it_is_laid_out() {
    let huge = "x".repeat(100_000);
    let shown = graph_view::capped(&huge, 40);
    assert_eq!(shown.chars().count(), 41);
    assert!(matches!(
        graph_view::capped("short", 40),
        std::borrow::Cow::Borrowed("short")
    ));
}

/// Item 8: an untracked folder whose name is not UTF-8 is not listed by its shown name.
#[test]
fn a_lossy_folder_is_not_listed() {
    let mut fx = Fx::new(1000.0, 800.0, fixture_view());
    fx.press(Key::F3);
    fx.view_mut().set_details(Details {
        subject: Subject::Row(RowKey::Uncommitted),
        summary: None,
        contains: None,
        groups: vec![FileGroup {
            group: Group::Untracked,
            files: vec![FileRow {
                letter: '?',
                path: "d\u{FFFD}r/".into(),
                old_path: None,
                lines: None,
                lossy: true,
            }],
        }],
        error: None,
    });
    fx.idle();
    assert!(fx.press(Key::Enter).is_empty());
    assert!(fx.press(Key::ArrowRight).is_empty());
    assert!(fx.view().status_line().text.contains("not valid UTF-8"));
}

/// The read model's `RefLabel::lossy` reaches the label (shown as it is, naming no ref).
#[test]
fn a_lossy_ref_makes_a_lossy_label() {
    let mut r = refs();
    let feature = r
        .iter_mut()
        .find(|l| l.refname == "refs/heads/feature")
        .unwrap();
    feature.refname = "refs/heads/f\u{FFFD}ature".into();
    feature.lossy = true;
    let mut view = fixture_view();
    view.set_refs(r, HeadRef::Branch("refs/heads/main".into()));
    let mut fx = Fx::new(1000.0, 500.0, view);
    assert!(fx.shows("f\u{FFFD}ature"));
    assert!(fx.view_mut().select(RowKey::Commit(oid(8))));
    fx.view_mut().set_label_cursor(1);
    let shown = fx.view().bar_label().unwrap();
    assert!(shown.lossy, "{shown:?}");
    assert!(fx.view_mut().select(RowKey::Commit(oid(9))));
    fx.view_mut().set_label_cursor(1);
    assert!(!fx.view().bar_label().unwrap().lossy);
}

/// Review round 2: details delivered again (`In` answered) keep the bar on its file, however the
/// message wrapped.
#[test]
fn details_again_keep_the_bar_on_its_file() {
    let long: Vec<String> = (0..100).map(|k| format!("word{k:02}")).collect();
    let long = long.join(" ");
    let file = |path: &str| FileRow {
        letter: 'M',
        path: path.into(),
        old_path: None,
        lines: Some((1, 1)),
        lossy: false,
    };
    let details = |contains: Option<Vec<String>>| Details {
        subject: Subject::Row(RowKey::Commit(oid(9))),
        summary: Some(Summary {
            oid: oid(9),
            parents: vec![oid(7), oid(8)],
            author_name: "Ann".into(),
            author_email: "ann@example.com".into(),
            author_time: day(9),
            committer_name: "Ann".into(),
            committer_email: "ann@example.com".into(),
            commit_time: day(9),
            message: format!("Merge\n\n{long}\n"),
        }),
        contains,
        groups: vec![FileGroup {
            group: Group::Files,
            files: vec![file("a.txt"), file("b.txt")],
        }],
        error: None,
    };
    let mut fx = Fx::new(1000.0, 800.0, fixture_view());
    fx.press(Key::ArrowDown);
    fx.press(Key::Enter);
    fx.view_mut().set_details(details(None));
    fx.idle();
    fx.view_mut()
        .set_details(details(Some(vec!["main".into()])));
    let asked = fx.press(Key::Enter);
    assert!(
        matches!(&asked[..], [GitAction::Diff { file, .. }] if file.path == "a.txt"),
        "{asked:?}"
    );
}

/// A message's tabs go to the next eighth cell, counted along the line.
#[test]
fn message_tabs_stop_at_every_eighth_cell() {
    let mut fx = Fx::new(1000.0, 800.0, fixture_view());
    fx.press(Key::ArrowDown);
    fx.press(Key::Enter);
    fx.view_mut().set_details(Details {
        subject: Subject::Row(RowKey::Commit(oid(9))),
        summary: Some(Summary {
            oid: oid(9),
            parents: vec![oid(7)],
            author_name: "Ann".into(),
            author_email: "ann@example.com".into(),
            author_time: day(9),
            committer_name: "Ann".into(),
            committer_email: "ann@example.com".into(),
            commit_time: day(9),
            message: "Merge\n\nx\tyz\t\tw|abcdefgh\tz\n".into(),
        }),
        contains: Some(Vec::new()),
        groups: Vec::new(),
        error: None,
    });
    fx.idle();
    assert!(
        fx.shows("x       yz              w|abcdefgh      z"),
        "{:?}",
        fx.texts()
    );
}
