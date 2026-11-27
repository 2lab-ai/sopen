//! S4-B's tests (03 §7.8, §8.5's edit stop, G-W1's planner): the planner pane, the quick edits'
//! dialogs, the commit menu's history row and an edit stop's Amend and Split, each drawn through
//! the test kit with real egui keys, typed text and clicks. No git runs, and none of S4-A's
//! `todo!()` bodies is reached: the pane and the dialogs take `Model::apply`, `changed`,
//! `problems` and `rebase::plan` through their seams, which these tests fill with fakes whose
//! plans name the edit they were asked for.

use super::in_progress::{self, BannerView, EditStop, Pressed, StopEnd, StopPressed};
use super::menu::{self, Eligibility, HistoryMenu, HistoryMenuEnd, MenuEnd, Target};
use super::planner::{
    self, EditPlanner, PlannerEnd, PlannerView, QuickDialog, QuickEnd, QuickOpened, Seams,
};
use crate::git::actions::{
    Action, ActionId, Banner, Confirmation, Plan, Proceed, Refused, Repo, Seen,
};
use crate::git::model::{
    BranchStatus, Change, CommitRow, GitVersion, Head, InProgress, Oid, Operation, RebaseProgress,
    Status, StatusEntry, StatusState,
};
use crate::git::rebase::{
    self, Author, Base, Commit, CommitterDate, Date, Edit, EditId, Model, Op, Quick, Toward, Verb,
};
use crate::gui::browser::keys::{self, Command, Context};
use crate::gui::chrome::git::LOOK;
use crate::gui::chrome::PALETTE;
use crate::gui::testkit::{self, Driver};
use crate::gui::Window;
use eframe::egui::{self, Key, Modifiers};
use std::cell::RefCell;

// ---------------------------------------------------------------------------------------------
// Fixtures.

/// A full object id of one repeated hex digit.
fn oid(c: char) -> Oid {
    Oid::parse(&c.to_string().repeat(40)).expect("a hex digit")
}

fn row(c: char, parents: &[char], subject: &str) -> CommitRow {
    CommitRow {
        oid: oid(c),
        parents: parents.iter().map(|&p| oid(p)).collect(),
        author_name: "Ada".into(),
        author_email: "ada@example.com".into(),
        author_time: 1_794_389_040,
        commit_time: 1_794_389_040,
        subject: subject.into(),
    }
}

/// 2026-11-11 18:24:00 +0900.
const WHEN: Date = Date {
    seconds: 1_794_389_040,
    offset_minutes: 540,
};

fn ada() -> Author {
    Author {
        name: "Ada".into(),
        email: "ada@example.com".into(),
        date: WHEN,
        committer: CommitterDate::Keep,
    }
}

/// Commit `c` (its parent `parent`, none for a root) as the planner reads it: its message is
/// `<subject>`, a blank line and a body.
fn commit(c: char, parent: Option<char>, subject: &str) -> Commit {
    let parents: Vec<char> = parent.into_iter().collect();
    Commit {
        row: row(c, &parents, subject),
        message: format!("{subject}\n\nbody of {subject}\n"),
        message_lossy: false,
        author: ada(),
        committed: WHEN,
    }
}

/// `a` (the root) ← `b` ← `c` ← `d` ← `e` (HEAD, `main`).
fn chain() -> Vec<Commit> {
    vec![
        commit('e', Some('d'), "fifth"),
        commit('d', Some('c'), "fourth"),
        commit('c', Some('b'), "third"),
        commit('b', Some('a'), "second"),
        commit('a', None, "first"),
    ]
}

fn find(c: char) -> Commit {
    chain()
        .into_iter()
        .find(|k| k.row.oid == oid(c))
        .expect("a commit of the chain")
}

/// The planner's model above `a`: `e` `d` `c` `b`, newest first, each a `pick`.
pub(super) fn model() -> Model {
    let mut rows = chain();
    rows.pop();
    Model::new(Base::Commit(oid('a')), rows)
}

/// `main` checked out at `e`, a clean work tree. The one place a `Repo` is built.
pub(super) fn repo() -> Repo {
    Repo {
        top: "/srv/sopen".into(),
        git_dir: "/srv/sopen/.git".into(),
        version: GitVersion {
            major: 2,
            minor: 45,
            patch: 0,
            text: "2.45.0".into(),
        },
        head: Head::Branch {
            refname: "refs/heads/main".into(),
            lossy: false,
            oid: oid('e'),
        },
        refs: Vec::new(),
        stashes: Vec::new(),
        status: Status {
            branch: BranchStatus::default(),
            entries: Vec::new(),
        },
        in_progress: InProgress::default(),
        remotes: Vec::new(),
        seen: Seen::default(),
    }
}

/// The same repository with a staged file (what `fold` folds).
fn staged() -> Repo {
    let mut r = repo();
    r.status.entries = vec![StatusEntry {
        path: "s.txt".into(),
        old_path: None,
        state: StatusState::Changed {
            staged: Some(Change::Modified),
            unstaged: None,
        },
        lossy: false,
    }];
    r
}

/// The one place a `Plan` is built.
fn plan(confirm: Confirmation) -> Plan {
    Plan {
        confirm: Some(confirm),
        backups: Vec::new(),
        message: String::new(),
        steps: Vec::new(),
        changes_files: false,
    }
}

thread_local! {
    /// Every op the fake `apply` was given, in order (each test runs on its own thread).
    static OPS: RefCell<Vec<Op>> = const { RefCell::new(Vec::new()) };
    /// Every edit the fake planner was asked for, in order.
    static EDITS: RefCell<Vec<Edit>> = const { RefCell::new(Vec::new()) };
}

fn ops() -> Vec<Op> {
    OPS.with(|o| o.borrow().clone())
}

fn edits() -> Vec<Edit> {
    EDITS.with(|e| e.borrow().clone())
}

fn short(c: &Oid) -> &str {
    c.short()
}

/// The command line the fake planner's confirmation shows for `edit`: it names every part of
/// the edit, so what a dialog shows can be held to what it returns.
fn describe(edit: &Edit) -> String {
    match edit {
        Edit::Quick(Quick::Reword { commit, message }) => {
            format!("reword {} {message:?}", short(commit))
        }
        Edit::Quick(Quick::Squash { commit, message }) => {
            format!("squash {} {message:?}", short(commit))
        }
        Edit::Quick(Quick::Author { commit, author }) => format!(
            "author {} {} <{}> {}{:+} {:?}",
            short(commit),
            author.name,
            author.email,
            author.date.seconds,
            author.date.offset_minutes,
            author.committer
        ),
        Edit::Quick(q) => {
            let c = match q {
                Quick::Fixup { commit }
                | Quick::Move { commit, .. }
                | Quick::Drop { commit }
                | Quick::Edit { commit }
                | Quick::Split { commit } => commit,
                Quick::Fold { into } => into,
                Quick::Reword { commit, .. }
                | Quick::Squash { commit, .. }
                | Quick::Author { commit, .. } => commit,
            };
            format!("{} {}", q.id().as_str(), short(c))
        }
        Edit::Plan(m) => {
            let rows: Vec<String> = m
                .rows
                .iter()
                .map(|r| format!("{} {}", r.verb.word(), short(&r.commit.row.oid)))
                .collect();
            let stash = if m.autostash { " --autostash" } else { "" };
            format!("plan{stash} {}", rows.join(", "))
        }
        Edit::SplitStop { commit } => format!("split-stop {}", short(commit)),
        Edit::ContinueSplit(s) => format!("continue-split {}", short(&s.commit)),
    }
}

/// What the fake planner calls an edit in its title.
fn what(edit: &Edit) -> &'static str {
    match edit {
        Edit::Quick(q) => q.id().as_str(),
        Edit::Plan(_) => "plan",
        Edit::SplitStop { .. } => "split-stop",
        Edit::ContinueSplit(_) => "continue-split",
    }
}

/// A planner whose every plan has a confirmation naming the edit in each part.
fn fake_plan(edit: &Edit, _: &Repo) -> Result<Plan, Refused> {
    EDITS.with(|e| e.borrow_mut().push(edit.clone()));
    let name = what(edit);
    Ok(plan(Confirmation {
        title: format!("Fake {name}"),
        loses: Some(format!("{name} loses this")),
        changes: vec![format!("{name}: 11111111 → 22222222")],
        backups: vec!["heads/main".into()],
        commands: vec![describe(edit)],
        notes: vec![format!("{name} note")],
    }))
}

/// A planner that refuses everything (03 §8.6).
fn refuse_plan(edit: &Edit, _: &Repo) -> Result<Plan, Refused> {
    EDITS.with(|e| e.borrow_mut().push(edit.clone()));
    Err(Refused("an ignored file is in the way: build/out.o".into()))
}

/// `Model::apply` as far as the tests need it: the verb (reword and squash refused on a message
/// that is not UTF-8), the author, the message, a move (refused past either end).
fn fake_apply(m: &mut Model, op: Op) -> Result<(), Refused> {
    OPS.with(|o| o.borrow_mut().push(op.clone()));
    let at = |m: &Model, c: &Oid| m.rows.iter().position(|r| &r.commit.row.oid == c);
    match op {
        Op::Verb { commit, verb } => {
            let k = at(m, &commit).expect("a row");
            let row = &mut m.rows[k];
            if matches!(verb, Verb::Reword | Verb::Squash) && row.commit.message_lossy {
                return Err(Refused(format!(
                    "{}: its message is not valid UTF-8",
                    commit.short()
                )));
            }
            row.verb = verb;
        }
        Op::Author { commit, author } => {
            let k = at(m, &commit).expect("a row");
            m.rows[k].author = author;
        }
        Op::Message { commit, message } => {
            let k = at(m, &commit).expect("a row");
            m.rows[k].message = Some(message);
        }
        Op::Move { commit, toward } => {
            let k = at(m, &commit).expect("a row");
            let to = match toward {
                Toward::Up => k.checked_sub(1),
                Toward::Down => Some(k + 1).filter(|&j| j < m.rows.len()),
            };
            let Some(to) = to else {
                return Err(Refused(format!("{}: nothing to move past", commit.short())));
            };
            m.rows.swap(k, to);
        }
    }
    Ok(())
}

/// Changed: any row not a plain `pick`, or the rows out of the fixture's order (descending
/// hashes).
fn fake_changed(m: &Model) -> bool {
    let plain = m
        .rows
        .iter()
        .all(|r| r.verb == Verb::Pick && r.message.is_none() && r.author.is_none());
    let ordered = m
        .rows
        .windows(2)
        .all(|w| w[0].commit.row.oid.as_str() > w[1].commit.row.oid.as_str());
    !(plain && ordered)
}

/// The checks: a `squash` or `fixup` row with no kept row below it; uncommitted changes and no
/// `--autostash`.
fn fake_problems(m: &Model, repo: &Repo) -> Vec<String> {
    let mut out = Vec::new();
    for (k, r) in m.rows.iter().enumerate() {
        if matches!(r.verb, Verb::Squash | Verb::Fixup)
            && !m.rows[k + 1..].iter().any(|b| b.verb != Verb::Drop)
        {
            out.push(format!(
                "nothing below {} to fold it into",
                r.commit.row.oid.short()
            ));
        }
    }
    if !repo.status.entries.is_empty() && !m.autostash {
        out.push("uncommitted changes: stash them for the run".into());
    }
    out
}

const FAKE: Seams = Seams {
    apply: fake_apply,
    changed: fake_changed,
    problems: fake_problems,
    plan: fake_plan,
};

fn quick(opened: QuickOpened) -> QuickDialog {
    match opened {
        QuickOpened::Dialog(d) => *d,
        other => panic!("not a dialog: {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// The host: every screen of S4-B drawn as the git panel will draw it, with what each returned.

pub(super) struct Host {
    pub(super) menu: Option<HistoryMenu>,
    pub(super) menu_ends: Vec<HistoryMenuEnd>,
    pub(super) quick: Option<QuickDialog>,
    pub(super) plan: EditPlanner,
    pub(super) quick_ends: Vec<QuickEnd>,
    pub(super) planner: Option<PlannerView>,
    pub(super) seams: Seams,
    pub(super) planner_ends: Vec<PlannerEnd>,
    /// The size of the pane's rectangle (the panel's body when `None`).
    pub(super) body: Option<egui::Vec2>,
    /// The git context's rows found after the pane's pass: what would reach the panel.
    pub(super) leaked: Vec<Command>,
    pub(super) banner: Option<BannerView>,
    pub(super) stop: Option<EditStop>,
    pub(super) stop_ends: Vec<StopEnd>,
}

impl Default for Host {
    fn default() -> Self {
        Host {
            menu: None,
            menu_ends: Vec::new(),
            quick: None,
            plan: fake_plan,
            quick_ends: Vec::new(),
            planner: None,
            seams: FAKE,
            planner_ends: Vec::new(),
            body: None,
            leaked: Vec::new(),
            banner: None,
            stop: None,
            stop_ends: Vec::new(),
        }
    }
}

impl Window for Host {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let modal = self.menu.is_some() || self.quick.is_some();
        if let (false, Some(b)) = (modal, &self.banner) {
            let stop = in_progress::stop_keys(&ctx, b, self.stop.as_ref());
            self.stop_ends.extend(stop.map(StopEnd::Stop));
            self.stop_ends
                .extend(in_progress::keys(&ctx, b).map(StopEnd::Banner));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                use crate::gui::cells::{Buffer, Cells, Grid};
                let grid = Grid::over(ui.max_rect());
                let all = grid.all();
                let mut buf = Buffer::empty(all);
                let mut body = all;
                if let Some(b) = &self.banner {
                    let h = u16::try_from(b.rows()).unwrap_or(u16::MAX).min(all.height);
                    let top = Cells::new(all.x, all.y, all.width, h);
                    body = Cells::new(all.x, all.y + h, all.width, all.height - h);
                    match &self.stop {
                        Some(stop) => self
                            .stop_ends
                            .extend(in_progress::draw_stop(ui, &grid, top, &mut buf, b, stop)),
                        None => self.stop_ends.extend(
                            in_progress::draw(ui, &grid, top, &mut buf, b).map(StopEnd::Banner),
                        ),
                    }
                }
                if let Some(size) = self.body {
                    // In whole cells, as the panel hands it over (a size under a cell: none).
                    let cells = |v: f32, cell: f32| (v / cell).floor() as u16;
                    let cell = crate::gui::chrome::CELL;
                    let (w, h) = (cells(size.x, cell.x), cells(size.y, cell.y));
                    body = Cells::new(body.x, body.y, w.min(body.width), h.min(body.height));
                }
                if let Some(p) = &mut self.planner {
                    let end = planner::draw(ui, &grid, body, &mut buf, p, !modal, &self.seams);
                    if end.is_some() {
                        self.planner = None;
                    }
                    self.planner_ends.extend(end);
                }
                // The parts' cells, painted once, as the panel paints its buffer.
                crate::gui::cells::paint(&ui.painter_at(grid.rect(all)), grid.origin, &buf, None);
            });
        // What the panel's own key lookup would still find: no typed letter may start Find.
        if !modal {
            while let Some(c) = ctx
                .input_mut(|i| keys::find(&mut i.events, Context::Git, keys::LANDED_SLICES.with(3)))
            {
                self.leaked.push(c);
            }
        }
        let grid = crate::gui::cells::Grid::over(ctx.content_rect());
        if let Some(m) = &mut self.menu {
            if let Some(end) = m.ui(&ctx, grid) {
                self.menu = None;
                self.menu_ends.push(end);
            }
        }
        if let Some(d) = &mut self.quick {
            if let Some(end) = d.ui(&ctx, grid, self.plan) {
                self.quick = None;
                self.quick_ends.push(end);
            }
        }
    }

    fn ready(&self) -> bool {
        true
    }
}

pub(super) struct Fx {
    pub(super) drv: Driver,
    pub(super) host: Host,
}

impl Fx {
    /// A window wider than any dialog, `host` laid out twice (a new modal is laid out unseen
    /// first).
    pub(super) fn new(host: Host) -> Fx {
        let mut fx = Fx {
            drv: Driver::new(1200.0, 900.0),
            host,
        };
        fx.idle();
        fx.idle();
        fx
    }

    pub(super) fn idle(&mut self) {
        self.drv.idle(&mut self.host, 1);
    }

    pub(super) fn key(&mut self, key: Key, mods: Modifiers) {
        self.drv.key(&mut self.host, key, mods);
    }

    pub(super) fn press(&mut self, key: Key) {
        self.key(key, Modifiers::NONE);
    }

    /// `text` typed as one event (as egui-winit hands over a typed letter), then a pass.
    pub(super) fn type_text(&mut self, text: &str) {
        self.drv.frame(
            &mut self.host,
            vec![egui::Event::Text(text.into())],
            Modifiers::NONE,
        );
        self.idle();
    }

    pub(super) fn click(&mut self, text: &str) {
        self.drv.click_text_on_top(&mut self.host, text);
        self.idle();
    }

    /// A click into the field showing `shown`, Cmd/Ctrl+A, then `text` typed over all of it.
    pub(super) fn retype(&mut self, shown: &str, text: &str) {
        let at = self
            .drv
            .find_text(shown)
            .unwrap_or_else(|| panic!("no field shows {shown:?}"))
            .center();
        self.drv.click(&mut self.host, at);
        self.key(Key::A, testkit::command(false));
        self.type_text(text);
    }

    pub(super) fn shows(&self, text: &str) -> bool {
        self.drv.find_text(text).is_some()
    }

    pub(super) fn assert_shows(&self, text: &str) {
        assert!(
            self.shows(text),
            "{text:?} is not drawn; drawn: {:?}",
            self.drv
                .texts_starting_with("")
                .into_iter()
                .map(|(_, t)| t)
                .collect::<Vec<_>>()
        );
    }

    /// Whether the last pass painted a rectangle in the dialog's cyan (a dialog's box).
    pub(super) fn dialog_box(&self) -> bool {
        self.drv
            .rects()
            .iter()
            .any(|(r, fill, _)| *fill == PALETTE.dialog_back && r.width() > 100.0)
    }

    pub(super) fn pane(&self) -> &PlannerView {
        self.host.planner.as_ref().expect("the planner is open")
    }

    /// The top of the row that shows `text` (a hash, a word).
    pub(super) fn top_of(&self, text: &str) -> f32 {
        self.drv
            .find_text(text)
            .unwrap_or_else(|| panic!("{text:?} is not drawn"))
            .top()
    }
}

pub(super) fn with_menu(menu: HistoryMenu) -> Fx {
    Fx::new(Host {
        menu: Some(menu),
        ..Host::default()
    })
}

pub(super) fn with_quick(d: QuickDialog, plan: EditPlanner) -> Fx {
    Fx::new(Host {
        quick: Some(d),
        plan,
        ..Host::default()
    })
}

pub(super) fn with_planner(model: Model, repo: &Repo) -> Fx {
    Fx::new(Host {
        planner: Some(PlannerView::new(model, repo)),
        ..Host::default()
    })
}

pub(super) fn cmd(key: Key) -> (Key, Modifiers) {
    (key, testkit::command(false))
}

// ---------------------------------------------------------------------------------------------
// The commit menu's history row (03 §7.1's last row, §7.8.1).

/// An ancestor of HEAD on a linear range, no merge below it.
const LINEAR: Eligibility = Eligibility {
    ancestor: true,
    linear: true,
    below_merge: false,
};

fn history_ids(c: &CommitRow, eligible: Eligibility, r: &Repo) -> Vec<EditId> {
    menu::history_items(c, eligible, r)
        .iter()
        .map(|i| i.id)
        .collect()
}

/// The history row offers §7.8.1's ids in its table's order, on ancestors of HEAD only, each
/// where it can apply: nothing above HEAD to move past or plan, nothing below a root commit to
/// fold into, `fold` only with staged changes; `split` stays on a root commit, whose plan refuses
/// it (G-H3).
#[test]
fn the_history_row_offers_its_ids_where_they_apply() {
    use EditId as E;
    let r = repo();
    let mid = row('c', &['b'], "third");
    assert_eq!(
        history_ids(&mid, LINEAR, &r),
        [
            E::Reword,
            E::Author,
            E::Squash,
            E::Fixup,
            E::MoveUp,
            E::MoveDown,
            E::Drop,
            E::Edit,
            E::Split,
            E::Plan
        ]
    );
    assert_eq!(
        history_ids(&mid, LINEAR, &staged()),
        [
            E::Reword,
            E::Author,
            E::Squash,
            E::Fixup,
            E::MoveUp,
            E::MoveDown,
            E::Drop,
            E::Edit,
            E::Split,
            E::Fold,
            E::Plan
        ]
    );
    let head = row('e', &['d'], "fifth");
    assert_eq!(
        history_ids(&head, LINEAR, &r),
        [
            E::Reword,
            E::Author,
            E::Squash,
            E::Fixup,
            E::MoveDown,
            E::Drop,
            E::Edit,
            E::Split
        ],
        "HEAD: nothing above it"
    );
    let root = row('a', &[], "first");
    assert_eq!(
        history_ids(&root, LINEAR, &r),
        [
            E::Reword,
            E::Author,
            E::MoveUp,
            E::Drop,
            E::Edit,
            E::Split,
            E::Plan
        ],
        "a root commit: nothing below it"
    );
    assert!(
        history_ids(&mid, Eligibility::default(), &r).is_empty(),
        "not an ancestor of HEAD"
    );
    // 03 §7.8.1's words, and the keys of 03 §9.1 and §9.2.
    let items = menu::history_items(&mid, LINEAR, &staged());
    let words: Vec<(&str, Option<&str>)> = items
        .iter()
        .map(|i| (i.label.as_str(), i.key.as_deref()))
        .collect();
    let plan_key = "Ctrl/⌘+E";
    assert_eq!(
        words,
        [
            ("Reword…", Some("F2")),
            ("Author and date…", None),
            ("Squash into the commit below…", None),
            ("Fix up into the commit below", None),
            ("Move up", None),
            ("Move down", None),
            ("Drop…", Some("F8")),
            ("Stop here to edit…", None),
            ("Split…", None),
            ("Fold the staged changes into this commit…", None),
            ("Edit history from here…", Some(plan_key)),
        ]
    );
}

/// The quick edits rewrite the range from the commit up to HEAD, and a range with a merge is
/// refused (03 §7.8.2, G-H4): on a merge, and on a commit below one, the history row offers only
/// the planner, which opens on the range and names the merge; above a merge it offers them, but
/// squash, fixup and move-down, whose range holds the commit below. The planner is offered in
/// every case.
#[test]
fn quick_edits_are_offered_only_on_a_linear_range() {
    use EditId as E;
    let r = repo();
    let merges = Eligibility {
        ancestor: true,
        linear: false,
        below_merge: false,
    };
    let merge = row('c', &['b', 'f'], "merge feature");
    assert_eq!(history_ids(&merge, merges, &r), [E::Plan], "a merge");
    let below = row('b', &['a'], "second");
    assert_eq!(
        history_ids(&below, merges, &r),
        [E::Plan],
        "a linear commit below a merge: its range holds the merge"
    );
    let above = row('d', &['c'], "fourth");
    let over_merge = Eligibility {
        below_merge: true,
        ..LINEAR
    };
    assert_eq!(
        history_ids(&above, over_merge, &r),
        [
            E::Reword,
            E::Author,
            E::MoveUp,
            E::Drop,
            E::Edit,
            E::Split,
            E::Plan
        ],
        "right above a merge: nothing that rewrites the commit below"
    );
    assert_eq!(history_ids(&above, LINEAR, &r).len(), 10, "a linear range");
    assert!(history_ids(&above, LINEAR, &r).contains(&E::Plan));
    // Drawn: the merge's menu has the planner and no quick edit.
    let target = Target::Commit {
        commit: merge,
        marked: Vec::new(),
    };
    let fx = with_menu(HistoryMenu::new(target, merges, &r));
    fx.assert_shows("Edit history from here…");
    assert!(!fx.shows("Reword…") && !fx.shows("Drop…"));
}

/// The commit's menu with its history row: S3-B's items first, then §7.8's, then
/// `Repository…`; a history item gives its id and commit, S3-B's items give S3-B's `MenuEnd`
/// unchanged, and `Repository…` leaves the history row behind.
#[test]
fn the_commit_menu_draws_its_history_row_and_gives_the_choice() {
    let r = repo();
    let mid = row('c', &['b'], "third");
    let target = Target::Commit {
        commit: mid.clone(),
        marked: Vec::new(),
    };
    let mut fx = with_menu(HistoryMenu::new(target.clone(), LINEAR, &r));
    assert!(fx.dialog_box(), "the cyan box");
    for text in [
        "Tag…",
        "Reword…",
        "F2",
        "Drop…",
        "F8",
        "Edit history from here…",
        "Repository…",
    ] {
        fx.assert_shows(text);
    }
    fx.assert_shows("Ctrl/⌘+E");
    let (tag, reword, repository) = (
        fx.top_of("Tag…"),
        fx.top_of("Reword…"),
        fx.top_of("Repository…"),
    );
    assert!(
        tag < reword && reword < repository,
        "S3-B's items, §7.8's, Repository…"
    );
    fx.click("Split…");
    assert_eq!(
        fx.host.menu_ends,
        [HistoryMenuEnd::Edit {
            id: EditId::Split,
            commit: mid.clone()
        }]
    );

    // Keys: End is Repository…, ↑ the history row's last item.
    let mut fx = with_menu(HistoryMenu::new(target.clone(), LINEAR, &r));
    fx.press(Key::End);
    fx.press(Key::ArrowUp);
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.menu_ends,
        [HistoryMenuEnd::Edit {
            id: EditId::Plan,
            commit: mid.clone()
        }]
    );
    let mut fx = with_menu(HistoryMenu::new(target.clone(), LINEAR, &r));
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.menu_ends,
        [HistoryMenuEnd::Menu(MenuEnd::Chose {
            id: ActionId::Tag,
            target: target.clone()
        })]
    );
    let mut fx = with_menu(HistoryMenu::new(target.clone(), LINEAR, &r));
    fx.press(Key::Escape);
    assert_eq!(fx.host.menu_ends, [HistoryMenuEnd::Menu(MenuEnd::Cancel)]);

    let mut fx = with_menu(HistoryMenu::new(target, LINEAR, &r));
    fx.click("Repository…");
    fx.idle();
    fx.assert_shows("Add a remote…");
    assert!(
        !fx.shows("Reword…"),
        "the repository's menu has no history row"
    );
    fx.press(Key::End);
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.menu_ends,
        [HistoryMenuEnd::Menu(MenuEnd::Chose {
            id: ActionId::Undo,
            target: Target::Repository
        })]
    );
}

// ---------------------------------------------------------------------------------------------
// The quick edits' dialogs (03 §7.8.1, §8.1).

fn open(id: EditId, c: char) -> QuickOpened {
    let k = find(c);
    let below = k
        .row
        .parents
        .first()
        .map(|p| find(p.as_str().chars().next().unwrap()));
    QuickDialog::open(id, &k, below.as_ref(), &staged(), fake_plan)
}

/// Every quick edit opens its dialog, which shows its plan's title, command line and backup; the
/// history row's `plan` opens the planner on the commit's row instead.
#[test]
fn every_quick_edit_opens_its_dialog_with_its_plan() {
    for id in EditId::ALL {
        let opened = open(id, 'c');
        if id == EditId::Plan {
            assert!(
                matches!(&opened, QuickOpened::Planner(Base::Commit(c)) if *c == oid('c')),
                "{opened:?}"
            );
            continue;
        }
        let d = quick(opened);
        let edit = d.draft().clone();
        assert!(matches!(&edit, Edit::Quick(q) if q.id() == id), "{edit:?}");
        let fx = with_quick(d, fake_plan);
        assert!(fx.dialog_box(), "{id:?}");
        fx.assert_shows(&format!("Fake {}", id.as_str()));
        fx.assert_shows(&describe(&edit));
        fx.assert_shows("heads/main");
        fx.assert_shows(&format!("{} loses this", id.as_str()));
    }
}

/// The confirmation shown is always the plan of the edit Enter returns (03 §8.1 "It updates as
/// options change"): a typed message and a changed committer date each plan again, the old plan
/// is gone, and Enter returns exactly what was shown last.
#[test]
fn the_shown_confirmation_is_the_edit_enter_returns() {
    let d = quick(open(EditId::Reword, 'c'));
    let mut fx = with_quick(d, fake_plan);
    let before = Edit::Quick(Quick::Reword {
        commit: oid('c'),
        message: find('c').message,
    });
    fx.assert_shows(&describe(&before));
    fx.retype(&find('c').message, "new subject");
    let after = Edit::Quick(Quick::Reword {
        commit: oid('c'),
        message: "new subject".into(),
    });
    fx.assert_shows(&describe(&after));
    assert!(!fx.shows(&describe(&before)), "the old plan is gone");
    assert_eq!(edits().last(), Some(&after));
    fx.press(Key::Enter);
    assert_eq!(fx.host.quick_ends, [QuickEnd::Run(after)]);

    let d = quick(open(EditId::Author, 'c'));
    let mut fx = with_quick(d, fake_plan);
    let keep = Edit::Quick(Quick::Author {
        commit: oid('c'),
        author: ada(),
    });
    fx.assert_shows(&describe(&keep));
    fx.click("now");
    let now = Edit::Quick(Quick::Author {
        commit: oid('c'),
        author: Author {
            committer: CommitterDate::Now,
            ..ada()
        },
    });
    fx.assert_shows(&describe(&now));
    assert!(!fx.shows(&describe(&keep)));
    fx.press(Key::Enter);
    assert_eq!(fx.host.quick_ends, [QuickEnd::Run(now)]);
}

/// Enter never runs a refused edit, nor one the dialog itself refuses: a plan's refusal is the
/// dialog's text in the error colour; a message that is not UTF-8 is never put into an editor and
/// never planned (03 §3.3, G-H3); a date that does not read is refused before any plan.
#[test]
fn a_refused_quick_edit_never_runs() {
    let fixup = QuickDialog::open(
        EditId::Fixup,
        &find('c'),
        Some(&find('b')),
        &repo(),
        refuse_plan,
    );
    let mut fx = with_quick(quick(fixup), refuse_plan);
    let why = "an ignored file is in the way: build/out.o";
    fx.assert_shows(why);
    assert_eq!(
        fx.drv.painted_colors_of(why),
        Some(vec![PALETTE.dialog_error])
    );
    fx.press(Key::Enter);
    assert!(fx.host.quick_ends.is_empty(), "nothing to run");
    assert!(fx.host.quick.is_some(), "the dialog stays");

    // A message that is not UTF-8: reword and squash refuse it, without an editor or a plan.
    for (id, lossy) in [(EditId::Reword, 'c'), (EditId::Squash, 'b')] {
        EDITS.with(|e| e.borrow_mut().clear());
        let mut c = find('c');
        let mut b = find('b');
        let odd = "caf\u{fffd} \u{fffd}\u{fffd}".to_string();
        for k in [&mut c, &mut b] {
            if k.row.oid == oid(lossy) {
                k.message = odd.clone();
                k.message_lossy = true;
            }
        }
        let d = quick(QuickDialog::open(id, &c, Some(&b), &repo(), fake_plan));
        let mut fx = with_quick(d, fake_plan);
        let refusal = fx
            .drv
            .text_starting_with(&format!("{}: ", oid(lossy).short()))
            .unwrap_or_else(|| panic!("{id:?}: no refusal naming the commit"));
        assert!(refusal.contains("not valid UTF-8"), "{refusal}");
        assert_eq!(
            fx.drv.painted_colors_of(&refusal),
            Some(vec![PALETTE.dialog_error])
        );
        assert!(
            !fx.drv
                .texts_starting_with("")
                .iter()
                .any(|(_, t)| t.contains('\u{fffd}')),
            "{id:?}: the lossy message is in no field"
        );
        fx.press(Key::Enter);
        assert!(fx.host.quick_ends.is_empty(), "{id:?}");
        assert!(edits().is_empty(), "{id:?}: never planned");
    }

    // A date that does not read.
    let d = quick(open(EditId::Author, 'c'));
    let mut fx = with_quick(d, fake_plan);
    let shown = planner::date_text(&WHEN);
    assert_eq!(shown, "2026-11-11 18:24:00 +0900");
    fx.retype(&shown, "yesterday");
    fx.assert_shows(planner::BAD_DATE);
    assert_eq!(
        fx.drv.painted_colors_of(planner::BAD_DATE),
        Some(vec![PALETTE.dialog_error])
    );
    let planned = edits().len();
    fx.press(Key::Enter);
    assert!(fx.host.quick_ends.is_empty());
    assert_eq!(edits().len(), planned, "not planned while refused");
}

/// Messages go as they are, byte for byte (03 §3.3): reword's prefill comes back unchanged when
/// nothing is typed — quotes, `\n` as text, `%`, `$`, backquotes, `#` lines, Hangul, a leading
/// blank line, trailing spaces and blank lines, a tab, CR and ESC; squash joins both messages,
/// the older first, a blank line between.
#[test]
fn messages_are_kept_byte_for_byte() {
    let odd = "\nsubject \"q\" 'q' \\n % $ `x`\n# not a comment\n한글\ttab\rcr\x1b[0m  \n\n  \n";
    let mut c = find('c');
    c.message = odd.into();
    let d = quick(QuickDialog::open(
        EditId::Reword,
        &c,
        Some(&find('b')),
        &repo(),
        fake_plan,
    ));
    let mut fx = with_quick(d, fake_plan);
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.quick_ends,
        [QuickEnd::Run(Edit::Quick(Quick::Reword {
            commit: oid('c'),
            message: odd.into()
        }))]
    );

    assert_eq!(rebase::squash_message("a\n", "b\n"), "a\n\nb\n");
    assert_eq!(rebase::squash_message("a", "b"), "a\n\nb");
    assert_eq!(rebase::squash_message("a\n\n", "b"), "a\n\n\nb");
    let d = quick(open(EditId::Squash, 'c'));
    let joined = rebase::squash_message(&find('b').message, &find('c').message);
    assert_eq!(
        joined,
        "second\n\nbody of second\n\nthird\n\nbody of third\n"
    );
    let mut fx = with_quick(d, fake_plan);
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.quick_ends,
        [QuickEnd::Run(Edit::Quick(Quick::Squash {
            commit: oid('c'),
            message: joined
        }))]
    );
}

// ---------------------------------------------------------------------------------------------
// The planner (03 §7.8.2).

/// The pane lists the range newest first: each row's verb, hash, subject, author and date, the
/// `A` mark on a row whose author or date changed (its new name and date shown), the bar on the
/// newest; in the panel's look, not a dialog's.
#[test]
fn the_planner_shows_each_rows_verb_hash_subject_author_date_and_mark() {
    let mut m = model();
    m.rows[1].verb = Verb::Drop;
    let grace = Author {
        name: "Grace".into(),
        email: "grace@example.com".into(),
        date: Date {
            seconds: 1_700_000_000,
            offset_minutes: 0,
        },
        committer: CommitterDate::Keep,
    };
    m.rows[2].author = Some(grace);
    let fx = with_planner(m, &repo());
    for head in ["Verb", "Commit", "Subject", "Author", "Date"] {
        fx.assert_shows(head);
    }
    for (c, verb, subject) in [
        ('e', "pick", "fifth"),
        ('d', "drop", "fourth"),
        ('c', "pick", "third"),
        ('b', "pick", "second"),
    ] {
        let hash = oid(c).short().to_string();
        let y = fx.top_of(&hash);
        assert_eq!(fx.top_of(subject), y, "{subject}");
        let verbs: Vec<f32> = fx
            .drv
            .texts_starting_with(verb)
            .iter()
            .filter(|(_, t)| t == verb)
            .map(|(r, _)| r.top())
            .collect();
        assert!(verbs.contains(&y), "{verb} on {hash}'s row");
    }
    let c_row = fx.top_of("cccccccc");
    assert_eq!(fx.top_of("A"), c_row, "the A mark on c's row only");
    assert_eq!(fx.top_of("Grace"), c_row);
    assert_eq!(
        fx.top_of(&super::graph_view::date_text(1_700_000_000)),
        c_row
    );
    assert_eq!(
        fx.drv
            .texts_starting_with("A")
            .iter()
            .filter(|(_, t)| t == "A")
            .count(),
        1
    );
    assert_eq!(fx.top_of("Ada"), fx.top_of("eeeeeeee"));
    assert_eq!(fx.pane().bar(), Some(&oid('e')), "the bar on the newest");
    assert!(!fx.dialog_box(), "a pane, not a dialog");
    assert!(
        fx.drv
            .rects()
            .iter()
            .any(|(r, fill, _)| *fill == PALETTE.bar && r.width() > 100.0),
        "the bar"
    );
}

/// Typed letters set the bar's verb (`P R E S F D`, either case, as typed or as an input method
/// commits them) and never start Find: no typed character reaches the panel's key lookup.
#[test]
fn typed_letters_set_verbs_and_never_start_find() {
    let mut fx = with_planner(model(), &repo());
    fx.type_text("r");
    assert_eq!(
        ops().last(),
        Some(&Op::Verb {
            commit: oid('e'),
            verb: Verb::Reword
        })
    );
    assert_eq!(fx.top_of("reword"), fx.top_of("eeeeeeee"));
    fx.type_text("x");
    fx.type_text(" ");
    assert_eq!(ops().len(), 1, "x and a space set nothing");
    fx.press(Key::ArrowDown);
    fx.type_text("D");
    assert_eq!(
        ops().last(),
        Some(&Op::Verb {
            commit: oid('d'),
            verb: Verb::Drop
        })
    );
    fx.drv.frame(
        &mut fx.host,
        vec![egui::Event::Ime(egui::ImeEvent::Commit("f".into()))],
        Modifiers::NONE,
    );
    fx.idle();
    assert_eq!(
        ops().last(),
        Some(&Op::Verb {
            commit: oid('d'),
            verb: Verb::Fixup
        })
    );
    for letter in ["p", "e", "s"] {
        fx.type_text(letter);
    }
    let verbs: Vec<Verb> = ops()
        .iter()
        .filter_map(|o| match o {
            Op::Verb { verb, .. } => Some(*verb),
            _ => None,
        })
        .collect();
    assert_eq!(
        verbs,
        [
            Verb::Reword,
            Verb::Drop,
            Verb::Fixup,
            Verb::Pick,
            Verb::Edit,
            Verb::Squash
        ]
    );
    assert!(fx.host.leaked.is_empty(), "leaked: {:?}", fx.host.leaked);
}

/// ↑ ↓ move the bar; Cmd/Ctrl+↑/↓ move the bar's row, the bar going with it (rows are named by
/// their commit); a refused op is said in the error colour and changes nothing.
#[test]
fn the_bar_and_the_rows_move_with_their_keys() {
    let mut fx = with_planner(model(), &repo());
    fx.press(Key::ArrowDown);
    assert_eq!(fx.pane().bar(), Some(&oid('d')));
    let (key, mods) = cmd(Key::ArrowUp);
    fx.key(key, mods);
    assert_eq!(
        ops().last(),
        Some(&Op::Move {
            commit: oid('d'),
            toward: Toward::Up
        })
    );
    assert_eq!(
        fx.pane().bar(),
        Some(&oid('d')),
        "the bar goes with its row"
    );
    assert!(fx.top_of("dddddddd") < fx.top_of("eeeeeeee"));
    fx.key(key, mods);
    let why = "dddddddd: nothing to move past";
    fx.assert_shows(why);
    assert_eq!(fx.drv.colors_of(why), Some(vec![LOOK.error]));
    fx.press(Key::End);
    assert_eq!(fx.pane().bar(), Some(&oid('b')));
    fx.press(Key::Home);
    assert_eq!(fx.pane().bar(), Some(&oid('d')));
    let (key, mods) = cmd(Key::ArrowDown);
    fx.key(key, mods);
    assert_eq!(
        ops().last(),
        Some(&Op::Move {
            commit: oid('d'),
            toward: Toward::Down
        })
    );
    assert!(!fx.shows(why), "a later op clears the refusal");
    assert!(fx.host.leaked.is_empty(), "leaked: {:?}", fx.host.leaked);
}

/// Enter edits the message of a `reword` or `squash` row (a squash row's is its group's), in a
/// dialog where Shift+Enter is a new line and Enter keeps it; on any other row it only says what
/// Enter does. An untouched message comes back byte for byte.
#[test]
fn enter_edits_the_message_of_a_reword_or_squash_row() {
    let mut fx = with_planner(model(), &repo());
    fx.press(Key::Enter);
    assert!(!fx.pane().dialog_up(), "a pick row has no message to edit");
    fx.assert_shows(planner::ENTER_HINT);
    fx.type_text("r");
    fx.press(Key::Enter);
    assert!(fx.pane().dialog_up());
    assert!(fx.dialog_box());
    fx.retype(&find('e').message, "new");
    fx.key(Key::Enter, Modifiers::SHIFT);
    fx.type_text("body");
    fx.press(Key::Enter);
    assert_eq!(
        ops().last(),
        Some(&Op::Message {
            commit: oid('e'),
            message: "new\nbody".into()
        })
    );
    assert!(
        !fx.pane().dialog_up(),
        "Enter closed it and did not open it again"
    );
    // Untouched: the same bytes.
    fx.press(Key::Enter);
    fx.press(Key::Enter);
    assert_eq!(
        ops().last(),
        Some(&Op::Message {
            commit: oid('e'),
            message: "new\nbody".into()
        })
    );
    // A squash row: its group's message, the older first; Esc keeps the pane and changes nothing.
    fx.press(Key::ArrowDown);
    fx.press(Key::ArrowDown);
    fx.type_text("s");
    let count = ops().len();
    fx.press(Key::Enter);
    let group = rebase::squash_message(&find('b').message, &find('c').message);
    fx.assert_shows(&group);
    fx.press(Key::Escape);
    assert!(!fx.pane().dialog_up());
    assert_eq!(ops().len(), count, "Esc changed nothing");
    assert!(
        fx.host.planner_ends.is_empty(),
        "Esc closed only the editor"
    );
    assert!(fx.host.leaked.is_empty(), "leaked: {:?}", fx.host.leaked);
}

/// `A` edits the bar's author and dates in a dialog prefilled with the commit's own: a change
/// marks the row; "The commit's own" puts the commit's back and the mark goes.
#[test]
fn a_sets_the_author_and_dates_and_marks_the_row() {
    let mut fx = with_planner(model(), &repo());
    fx.press(Key::ArrowDown);
    fx.type_text("a");
    assert!(fx.pane().dialog_up());
    fx.assert_shows("Ada");
    fx.assert_shows("ada@example.com");
    fx.assert_shows("2026-11-11 18:24:00 +0900");
    fx.click("now");
    fx.press(Key::Enter);
    assert_eq!(
        ops().last(),
        Some(&Op::Author {
            commit: oid('d'),
            author: Some(Author {
                committer: CommitterDate::Now,
                ..ada()
            })
        })
    );
    assert_eq!(fx.top_of("A"), fx.top_of("dddddddd"));
    fx.type_text("a");
    fx.click(planner::OWN_AUTHOR);
    assert_eq!(
        ops().last(),
        Some(&Op::Author {
            commit: oid('d'),
            author: None
        })
    );
    assert!(!fx.shows("A"), "the mark is gone");
    // Unchanged and kept: no mark either.
    fx.type_text("a");
    fx.press(Key::Enter);
    assert_eq!(
        ops().last(),
        Some(&Op::Author {
            commit: oid('d'),
            author: None
        })
    );
    assert!(fx.host.leaked.is_empty(), "leaked: {:?}", fx.host.leaked);
}

/// Esc closes the planner at once when nothing changed, and asks when something did (03
/// §7.8.2, §9.2): Esc in the question keeps planning, Enter closes and loses the plan.
#[test]
fn esc_closes_and_asks_only_when_something_changed() {
    let mut fx = with_planner(model(), &repo());
    fx.press(Key::Escape);
    assert_eq!(fx.host.planner_ends, [PlannerEnd::Close]);

    let mut fx = with_planner(model(), &repo());
    fx.type_text("d");
    fx.press(Key::Escape);
    assert!(fx.host.planner_ends.is_empty(), "it asks first");
    assert!(fx.dialog_box());
    fx.assert_shows(planner::ASK_CLOSE);
    fx.press(Key::Escape);
    assert!(fx.host.planner_ends.is_empty(), "Esc keeps planning");
    assert!(
        !fx.pane().dialog_up(),
        "and does not ask again in the same pass"
    );
    fx.press(Key::Escape);
    assert!(fx.pane().dialog_up());
    fx.press(Key::Enter);
    assert_eq!(fx.host.planner_ends, [PlannerEnd::Close]);
}

/// Enter is the click of the button that has the focus (Tab moves it), as in S3-B's dialogs: in
/// the question, a focused "Keep planning" keeps planning and a focused Close closes; in the
/// message and author dialogs a focused Cancel changes nothing and a focused "The commit's own"
/// puts the commit's own back.
#[test]
fn enter_on_a_focused_button_is_its_click() {
    let asked = |tabs: usize| {
        let mut fx = with_planner(model(), &repo());
        fx.type_text("d");
        fx.press(Key::Escape);
        assert!(fx.pane().dialog_up());
        for _ in 0..tabs {
            fx.press(Key::Tab);
        }
        fx.press(Key::Enter);
        fx
    };
    let fx = asked(2);
    assert!(
        fx.host.planner_ends.is_empty(),
        "Keep planning had the focus"
    );
    assert!(!fx.pane().dialog_up(), "the question is gone");
    let fx = asked(1);
    assert_eq!(
        fx.host.planner_ends,
        [PlannerEnd::Close],
        "Close had the focus"
    );

    // The message editor: Tab from the field to OK, then Cancel.
    let mut fx = with_planner(model(), &repo());
    fx.type_text("r");
    fx.press(Key::Enter);
    let before = ops().len();
    fx.press(Key::Tab);
    fx.press(Key::Tab);
    fx.press(Key::Enter);
    assert_eq!(ops().len(), before, "Cancel had the focus: no message set");
    assert!(!fx.pane().dialog_up());

    // The author dialog, from its Email field: Author date, the three committer choices, OK,
    // the commit's own, Cancel.
    let author = |tabs: usize| {
        let mut fx = with_planner(model(), &repo());
        fx.type_text("a");
        fx.retype("ada@example.com", "grace@example.com");
        for _ in 0..tabs {
            fx.press(Key::Tab);
        }
        fx.press(Key::Enter);
        assert!(!fx.pane().dialog_up());
        fx
    };
    let before = ops().len();
    author(7);
    assert_eq!(ops().len(), before, "Cancel had the focus: nothing set");
    author(6);
    assert_eq!(
        ops().last(),
        Some(&Op::Author {
            commit: oid('e'),
            author: None
        }),
        "the commit's own had the focus"
    );
    author(5);
    assert!(
        matches!(ops().last(), Some(Op::Author { author: Some(a), .. }) if a.email == "grace@example.com"),
        "OK had the focus: {:?}",
        ops().last()
    );
}

/// Cmd/Ctrl+Enter shows the run's confirmation (03 §8.1): the checks that fail, the run's
/// options, the plan of the model as it is; Enter runs only a plan of exactly the model shown,
/// never while a check fails or the plan is refused.
#[test]
fn the_run_shows_its_confirmation_and_runs_only_what_it_shows() {
    // A squash with nothing below: the check fails in the pane and in the dialog.
    let mut fx = with_planner(model(), &repo());
    fx.press(Key::End);
    fx.type_text("s");
    let problem = "nothing below bbbbbbbb to fold it into";
    fx.assert_shows(problem);
    assert_eq!(fx.drv.colors_of(problem), Some(vec![LOOK.error]));
    let (key, mods) = cmd(Key::Enter);
    fx.key(key, mods);
    assert!(fx.pane().dialog_up());
    fx.assert_shows(problem);
    fx.press(Key::Enter);
    assert!(
        fx.host.planner_ends.is_empty(),
        "a check fails: nothing runs"
    );
    fx.press(Key::Escape);
    assert!(
        fx.host.planner_ends.is_empty(),
        "Esc closed only the dialog"
    );
    fx.type_text("p");
    assert!(!fx.shows(problem));

    // The options plan again; Enter runs the model as shown last.
    fx.key(key, mods);
    let mut m = fx.pane().model().clone();
    fx.assert_shows(&describe(&Edit::Plan(m.clone())));
    fx.click(planner::AUTOSTASH);
    m.autostash = true;
    let shown = describe(&Edit::Plan(m.clone()));
    fx.assert_shows(&shown);
    assert_eq!(edits().last(), Some(&Edit::Plan(m.clone())));
    fx.press(Key::Enter);
    assert_eq!(fx.host.planner_ends, [PlannerEnd::Run(Edit::Plan(m))]);

    // Uncommitted changes without --autostash: refused until it is on.
    let mut fx = with_planner(model(), &staged());
    let problem = "uncommitted changes: stash them for the run";
    fx.assert_shows(problem);
    fx.key(key, mods);
    fx.press(Key::Enter);
    assert!(fx.host.planner_ends.is_empty());
    fx.click(planner::AUTOSTASH);
    assert!(!fx.shows(problem));
    fx.press(Key::Enter);
    assert!(matches!(
        &fx.host.planner_ends[..],
        [PlannerEnd::Run(Edit::Plan(m))] if m.autostash
    ));

    // A refused plan: its reason, in the error colour; Enter does nothing.
    let mut fx = Fx::new(Host {
        planner: Some(PlannerView::new(model(), &repo())),
        seams: Seams {
            plan: refuse_plan,
            ..FAKE
        },
        ..Host::default()
    });
    fx.key(key, mods);
    let why = "an ignored file is in the way: build/out.o";
    fx.assert_shows(why);
    assert_eq!(
        fx.drv.painted_colors_of(why),
        Some(vec![PALETTE.dialog_error])
    );
    fx.press(Key::Enter);
    assert!(fx.host.planner_ends.is_empty());
}

/// Nothing panics on an empty range, a pane too small for a row, long or odd text, or a date at
/// the ends of its range; dates read back as written.
#[test]
fn nothing_panics_on_empty_small_long_or_odd_input() {
    let mut fx = with_planner(Model::new(Base::Commit(oid('e')), Vec::new()), &repo());
    fx.assert_shows(planner::NO_ROWS);
    for t in ["r", "a"] {
        fx.type_text(t);
    }
    fx.press(Key::Enter);
    fx.press(Key::End);
    let (key, mods) = cmd(Key::ArrowUp);
    fx.key(key, mods);
    let (key, mods) = cmd(Key::Enter);
    fx.key(key, mods);
    fx.press(Key::Escape);
    fx.press(Key::Escape);
    assert_eq!(fx.host.planner_ends, [PlannerEnd::Close]);

    let odd = "한글\u{301}\u{7}\t\u{1b}[0m ".repeat(60);
    let mut rows = chain();
    rows.pop();
    for k in &mut rows {
        k.row.subject = odd.clone();
        k.row.author_name = odd.clone();
        k.message = odd.clone();
    }
    let m = Model::new(Base::Root, rows.clone());
    for size in [egui::vec2(30.0, 20.0), egui::vec2(1.0, 1.0)] {
        let mut fx = Fx::new(Host {
            planner: Some(PlannerView::new(m.clone(), &repo())),
            body: Some(size),
            ..Host::default()
        });
        fx.type_text("r");
        fx.press(Key::Enter);
        fx.press(Key::Escape);
    }
    let mut fx = with_planner(m, &repo());
    fx.type_text("r");
    fx.press(Key::Enter);
    fx.press(Key::Enter);
    fx.type_text("a");
    fx.press(Key::Enter);
    let d = quick(QuickDialog::open(
        EditId::Squash,
        &rows[0],
        Some(&rows[1]),
        &repo(),
        fake_plan,
    ));
    let mut fx = with_quick(d, fake_plan);
    fx.press(Key::Enter);
    assert_eq!(fx.host.quick_ends.len(), 1);
    let target = Target::Commit {
        commit: rows[0].row.clone(),
        marked: Vec::new(),
    };
    let _ = with_menu(HistoryMenu::new(target, LINEAR, &repo()));

    // The pane's Date column at the ends of the range (the graph's local-time text).
    let mut m = model();
    for (k, seconds) in [(0, i64::MAX), (1, i64::MIN)] {
        m.rows[k].author = Some(Author {
            date: Date {
                seconds,
                offset_minutes: 0,
            },
            ..ada()
        });
    }
    let fx = with_planner(m, &repo());
    fx.assert_shows("eeeeeeee");
    fx.assert_shows("dddddddd");

    // Every date a commit can hold reads back as written: past the calendar's years as git's
    // own `@<unix> <±hhmm>`.
    for (seconds, offset_minutes) in [
        (i64::MAX, 0),
        (i64::MIN, 0),
        (0, -720),
        (-1, 840),
        (253_402_300_800, 60),
    ] {
        let d = Date {
            seconds,
            offset_minutes,
        };
        let text = planner::date_text(&d);
        assert_eq!(planner::parse_date(&text), Some(d), "{text}");
    }
    // An offset no zone has: shown without a panic (and refused if kept).
    for d in [
        Date {
            seconds: i64::MAX,
            offset_minutes: i32::MAX,
        },
        Date {
            seconds: i64::MIN,
            offset_minutes: i32::MIN,
        },
    ] {
        assert_eq!(planner::parse_date(&planner::date_text(&d)), None);
    }
    assert_eq!(planner::parse_date(&planner::date_text(&WHEN)), Some(WHEN));
    for bad in [
        "",
        "yesterday",
        "2026-02-30 00:00:00 +0000",
        "2026-11-11 24:00:00 +0900",
        "2026-11-11 18:24:00",
        "2026-11-11 18:24:00 +09",
        "2026-11-11 18:24:00 +0960",
        "２０２６-11-11 18:24:00 +0900",
        "@x +0000",
    ] {
        assert_eq!(planner::parse_date(bad), None, "{bad:?}");
    }
    assert_eq!(
        planner::parse_date("@1794389040 +0900"),
        Some(WHEN),
        "git's own form"
    );
    assert_eq!(
        planner::parse_date("2026-11-11 18:24 +0900"),
        Some(WHEN),
        "seconds may be left out"
    );
}

// ---------------------------------------------------------------------------------------------
// An edit stop on the banner (03 §8.5).

/// The probe of a rebase stopped by an `edit` line at `stopped` (`done`'s last line naming
/// `named`).
fn stopped_at(stopped: &str, named: &str) -> Banner {
    let in_progress = InProgress {
        operation: Some(Operation::Rebase),
        bisect: false,
        rebase: Some(RebaseProgress {
            msgnum: Some("2".into()),
            end: Some("4".into()),
            head_name: Some("refs/heads/main".into()),
            onto: Some(oid('a').as_str().into()),
            stopped_sha: Some(stopped.into()),
            done_last: Some(format!("edit {named} third")),
        }),
        todo_first: None,
        head: None,
    };
    Banner::of(
        &in_progress,
        &Status {
            branch: BranchStatus::default(),
            entries: Vec::new(),
        },
    )
    .expect("a banner")
}

/// An edit stop's banner offers Amend and Split beside S3-B's Continue and Abort, as keys
/// (proposed: Cmd/Ctrl+Shift+M, Cmd/Ctrl+Shift+P) and buttons: Amend opens §7.6's `amend`,
/// Split plans `Edit::SplitStop` of the stopped commit; S3-B's steps come back as S3-B's
/// `Pressed`. Elsewhere the keys do nothing and the status row says so; without the stopped
/// commit's full id, Split is not offered.
#[test]
fn an_edit_stop_offers_amend_and_split() {
    let c = oid('c');
    let banner = stopped_at(c.as_str(), c.as_str());
    let stop = EditStop::of(&banner).expect("an edit stop");
    assert_eq!(stop.commit(), Some(&c));
    let mut fx = Fx::new(Host {
        banner: Some(BannerView::of(&banner)),
        stop: Some(stop.clone()),
        ..Host::default()
    });
    fx.assert_shows("stopped to edit cccccccc");
    for word in ["Continue", "Abort", "Amend", "Split"] {
        fx.assert_shows(word);
    }
    fx.key(Key::M, testkit::command(true));
    assert_eq!(fx.host.stop_ends, [StopEnd::Stop(StopPressed::Amend)]);
    fx.host.stop_ends.clear();
    fx.key(Key::P, testkit::command(true));
    let split = StopPressed::Split(Edit::SplitStop { commit: c.clone() });
    assert_eq!(fx.host.stop_ends, [StopEnd::Stop(split.clone())]);
    fx.host.stop_ends.clear();
    fx.click("Split");
    assert_eq!(fx.host.stop_ends, [StopEnd::Stop(split)]);
    fx.host.stop_ends.clear();
    fx.click("Amend");
    assert_eq!(fx.host.stop_ends, [StopEnd::Stop(StopPressed::Amend)]);
    fx.host.stop_ends.clear();
    fx.click("Continue");
    assert_eq!(
        fx.host.stop_ends,
        [StopEnd::Banner(Pressed::Proceed(Action::Proceed(
            Proceed::Continue
        )))]
    );
    fx.host.stop_ends.clear();
    // Split's plan goes through a dialog of its own, as a quick edit's does.
    let split_stop = Edit::SplitStop { commit: c.clone() };
    let d = quick(QuickDialog::confirm(split_stop.clone(), &repo(), fake_plan));
    let mut dialog = with_quick(d, fake_plan);
    dialog.assert_shows("Fake split-stop");
    dialog.assert_shows("split-stop cccccccc");
    dialog.press(Key::Enter);
    assert_eq!(dialog.host.quick_ends, [QuickEnd::Run(split_stop)]);
    // Without Shift the letters are not the banner's.
    fx.key(Key::M, testkit::command(false));
    fx.key(Key::P, testkit::command(false));
    assert!(fx.host.stop_ends.is_empty());

    // Not an edit stop: the keys do nothing, and say so.
    let merge = BannerView::new(
        "MERGE · 1 conflict",
        vec![Proceed::Continue, Proceed::Abort],
    );
    let mut fx = Fx::new(Host {
        banner: Some(merge),
        ..Host::default()
    });
    assert!(!fx.shows("Amend") && !fx.shows("Split"));
    fx.key(Key::M, testkit::command(true));
    fx.key(Key::P, testkit::command(true));
    assert!(
        matches!(
            &fx.host.stop_ends[..],
            [
                StopEnd::Stop(StopPressed::NotOffered(a)),
                StopEnd::Stop(StopPressed::NotOffered(s))
            ] if a.starts_with("Amend: not offered while MERGE") && s.starts_with("Split: ")
        ),
        "{:?}",
        fx.host.stop_ends
    );

    // The stopped commit's id not read in full: Split is not offered, Amend is.
    let banner = stopped_at("cccccccc", "cccccccc");
    let stop = EditStop::of(&banner).expect("an edit stop");
    assert_eq!(stop.commit(), None);
    let mut fx = Fx::new(Host {
        banner: Some(BannerView::of(&banner)),
        stop: Some(stop),
        ..Host::default()
    });
    fx.assert_shows("Amend");
    assert!(!fx.shows("Split"));
    fx.key(Key::P, testkit::command(true));
    assert!(
        matches!(&fx.host.stop_ends[..], [StopEnd::Stop(StopPressed::NotOffered(s))] if s.starts_with("Split: ")),
        "{:?}",
        fx.host.stop_ends
    );
    // `done` names the commit in full where `stopped-sha` does not.
    let banner = stopped_at("cccccccc", c.as_str());
    assert_eq!(
        EditStop::of(&banner).and_then(|s| s.commit().cloned()),
        Some(c)
    );
    // Not a rebase at an edit stop: none.
    let banner = Banner::of(
        &InProgress {
            operation: Some(Operation::Merge),
            ..InProgress::default()
        },
        &Status {
            branch: BranchStatus::default(),
            entries: Vec::new(),
        },
    )
    .expect("a banner");
    assert!(EditStop::of(&banner).is_none());
}
