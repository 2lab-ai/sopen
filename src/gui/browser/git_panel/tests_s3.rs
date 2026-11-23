//! S3-B's tests (03 §7, §8.1–§8.5, §3.7–§3.8): the menu, the dialogs and their confirmations, the
//! Backups list, the command log and the banner, each drawn through the test kit with real egui
//! keys and clicks. No git runs and the real `actions::plan` is never called: every dialog is
//! given a planner of the test's own, whose plans are built here.

use super::backups::{self, BackupsEnd, BackupsView};
use super::dialogs::{self, ActionDialog, DialogEnd, Opened, Planner, Prefill, ResultDialog};
use super::in_progress::{self, BannerView, Pressed};
use super::log_view::{self, LogEnd, LogView};
use super::menu::{self, Menu, MenuEnd, Target};
use crate::git::actions::{
    Action, ActionId, Confirmation, Outcome, Overwrite, Plan, Proceed, PushOptions, Refused,
    Remote, RemoteChange, Repo, ResetMode, Seen, Step, StepOutcome, StepResult,
};
use crate::git::backup::{self as backup, Manifest, Record, RestoreEntry, RestoreHow, Value};
use crate::git::model::{
    Backup, BackupKind, BackupRef, BranchStatus, Change, CommitRow, GitVersion, Head, InProgress,
    ObjectType, Oid, RefKind, RefLabel, StashRow, Status, StatusEntry, StatusState,
};
use crate::git::runner::{Class, Done, Ending, GitCommand, LogEntry};
use crate::gui::browser::keys::{self, Context};
use crate::gui::chrome::{self, PALETTE};
use crate::gui::testkit::{self, Driver};
use crate::gui::Window;
use eframe::egui::{self, Key, Modifiers};
use std::cell::RefCell;
use std::time::{Duration, SystemTime};

use ActionId as A;

/// The slices the panel reads once S3-C has landed slice 3: the landed set and 3.
const WITH_SLICE_3: keys::Slices = keys::LANDED_SLICES.with(3);

// ---------------------------------------------------------------------------------------------
// Fixtures.

/// A full object id of one repeated hex digit.
fn oid(c: char) -> Oid {
    Oid::parse(&c.to_string().repeat(40)).expect("a hex digit")
}

fn commit(c: char, parents: &[char], subject: &str) -> CommitRow {
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

fn reference(refname: &str, at: char, kind: RefKind) -> RefLabel {
    RefLabel {
        object: oid(at),
        object_type: ObjectType::Commit,
        peeled: None,
        refname: refname.into(),
        kind,
        upstream: None,
        track: String::new(),
        lossy: false,
    }
}

fn local(name: &str, at: char) -> RefLabel {
    reference(&format!("refs/heads/{name}"), at, RefKind::LocalBranch)
}

fn remote(name: &str, at: char) -> RefLabel {
    reference(&format!("refs/remotes/{name}"), at, RefKind::RemoteBranch)
}

fn tag(name: &str, at: char, annotated: bool) -> RefLabel {
    let mut r = reference(&format!("refs/tags/{name}"), at, RefKind::Tag);
    if annotated {
        r.object = oid('9');
        r.object_type = ObjectType::Tag;
        r.peeled = Some(oid(at));
    }
    r
}

fn stash(n: usize, at: char, base: char) -> StashRow {
    StashRow {
        oid: oid(at),
        base: oid(base),
        index: Some(oid('8')),
        untracked: None,
        selector: format!("stash@{{{n}}}"),
        commit_time: 1_794_389_040,
        subject: "WIP on main: first".into(),
    }
}

fn changed(path: &str, staged: Option<Change>, unstaged: Option<Change>) -> StatusEntry {
    StatusEntry {
        path: path.into(),
        old_path: None,
        state: StatusState::Changed { staged, unstaged },
        lossy: false,
    }
}

fn untracked(path: &str) -> StatusEntry {
    StatusEntry {
        path: path.into(),
        old_path: None,
        state: StatusState::Untracked,
        lossy: false,
    }
}

fn conflict(path: &str) -> StatusEntry {
    StatusEntry {
        path: path.into(),
        old_path: None,
        state: StatusState::Conflict("UU".into()),
        lossy: false,
    }
}

/// `main` checked out at `a`, `feature` at `b`, both on `origin` too, tags `v1` (lightweight) and
/// `v2` (annotated), one stash, a clean work tree. The one place a `Repo` is built.
fn repo() -> Repo {
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
            oid: oid('a'),
        },
        refs: vec![
            local("main", 'a'),
            local("feature", 'b'),
            remote("origin/main", 'a'),
            remote("origin/feature", 'b'),
            tag("v1", 'a', false),
            tag("v2", 'b', true),
        ],
        stashes: vec![stash(0, 'd', 'a')],
        status: Status {
            branch: BranchStatus::default(),
            entries: Vec::new(),
        },
        in_progress: InProgress::default(),
        remotes: vec!["origin".into()],
        seen: Seen::default(),
    }
}

/// The same repository with a staged, an unstaged and an untracked file.
fn dirty() -> Repo {
    let mut r = repo();
    r.status.entries = vec![
        changed("s.txt", Some(Change::Modified), None),
        changed("u.txt", None, Some(Change::Modified)),
        untracked("n.txt"),
    ];
    r
}

/// The one place a `Plan` is built.
fn plan(confirm: Option<Confirmation>, steps: Vec<Step>) -> Plan {
    Plan {
        confirm,
        backups: Vec::new(),
        message: String::new(),
        steps,
        changes_files: false,
    }
}

fn git(words: &[&str]) -> GitCommand {
    GitCommand::new(Class::Write, "/srv/sopen", words.iter().copied())
}

thread_local! {
    /// Every action a test's planner was asked for, in order (each test runs on its own thread).
    static ASKED: RefCell<Vec<Action>> = const { RefCell::new(Vec::new()) };
}

fn asked() -> Vec<Action> {
    ASKED.with(|a| a.borrow().clone())
}

fn record(action: &Action) {
    ASKED.with(|a| a.borrow_mut().push(action.clone()));
}

/// What the fake planner's confirmation says the command is.
fn fake_command(action: &Action) -> String {
    match action {
        Action::Reset { to, mode } => {
            let mode = format!("{mode:?}").to_lowercase();
            format!("git reset --{mode} {}", to.short())
        }
        other => format!("git {}", other.id().as_str()),
    }
}

/// A planner whose every plan has a confirmation naming the action in each part.
fn fake(action: &Action, _: &Repo) -> Result<Plan, Refused> {
    record(action);
    let id = action.id().as_str();
    let confirm = Confirmation {
        title: format!("Fake {id}"),
        loses: Some(format!("{id} loses this")),
        changes: vec![format!("{id}: 11111111 → 22222222")],
        backups: vec![format!("heads/{id}")],
        commands: vec![fake_command(action)],
        notes: vec![format!("{id} note")],
    };
    Ok(plan(Some(confirm), Vec::new()))
}

/// A planner with no confirmation: the action runs at once (03 §8.1).
fn quiet(action: &Action, _: &Repo) -> Result<Plan, Refused> {
    record(action);
    let step = Step::Git(git(&["checkout", action.id().as_str()]));
    Ok(plan(None, vec![step]))
}

/// A planner that refuses everything (03 §8.6).
fn refuse(action: &Action, _: &Repo) -> Result<Plan, Refused> {
    record(action);
    Err(Refused("an ignored file is in the way: build/out.o".into()))
}

/// A planner whose plan overwrites a branch and deletes a tag on a remote (03 §8.4).
fn overwrite(action: &Action, repo: &Repo) -> Result<Plan, Refused> {
    let mut p = fake(action, repo)?;
    let options = PushOptions {
        set_upstream: true,
        no_verify: false,
    };
    p.steps = vec![
        Step::Backup,
        Step::Remote(RemoteChange {
            remote: "origin".into(),
            refname: "refs/heads/feature".into(),
            change: Overwrite::Replace {
                src: "refs/heads/feature".into(),
            },
            options,
        }),
        Step::Remote(RemoteChange {
            remote: "origin".into(),
            refname: "refs/tags/v1".into(),
            change: Overwrite::Delete,
            options: PushOptions {
                set_upstream: false,
                no_verify: false,
            },
        }),
    ];
    Ok(p)
}

fn dialog(opened: Opened) -> ActionDialog {
    match opened {
        Opened::Dialog(d) => *d,
        other => panic!("not a dialog: {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// The host: every screen of S3-B drawn as the git panel will draw it, with what each returned.

struct Host {
    menu: Option<Menu>,
    menu_ends: Vec<MenuEnd>,
    dialog: Option<ActionDialog>,
    planner: Planner,
    dialog_ends: Vec<DialogEnd>,
    result: Option<ResultDialog>,
    results_closed: usize,
    backups: Option<BackupsView>,
    backups_ends: Vec<BackupsEnd>,
    log: Option<LogView>,
    log_ends: Vec<LogEnd>,
    banner: Option<BannerView>,
    pressed: Vec<Pressed>,
}

impl Default for Host {
    fn default() -> Self {
        Host {
            menu: None,
            menu_ends: Vec::new(),
            dialog: None,
            planner: fake,
            dialog_ends: Vec::new(),
            result: None,
            results_closed: 0,
            backups: None,
            backups_ends: Vec::new(),
            log: None,
            log_ends: Vec::new(),
            banner: None,
            pressed: Vec::new(),
        }
    }
}

impl Window for Host {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let modal = self.menu.is_some() || self.dialog.is_some() || self.result.is_some();
        // The git context's rows, as the panel reads them while a list is open: the landed set
        // and slice 3, as S3-C will land it (until then the panel reads `LANDED_SLICES`).
        if !modal && (self.backups.is_some() || self.log.is_some()) {
            while let Some(c) =
                ctx.input_mut(|i| keys::find(&mut i.events, Context::Git, WITH_SLICE_3))
            {
                if let Some(v) = &mut self.backups {
                    self.backups_ends.extend(v.run(c));
                } else if let Some(v) = &mut self.log {
                    self.log_ends.extend(v.run(c));
                }
            }
        }
        if let (false, Some(b)) = (modal, &self.banner) {
            self.pressed.extend(in_progress::keys(&ctx, b));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.max_rect().shrink(2.0);
                let mut body = rect;
                if let Some(b) = &self.banner {
                    let h = chrome::CELL.y * b.rows() as f32;
                    let top = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), h));
                    body.min.y += h;
                    self.pressed.extend(in_progress::draw(ui, top, b));
                }
                if let Some(v) = &mut self.backups {
                    self.backups_ends.extend(backups::draw(ui, body, v, !modal));
                } else if let Some(v) = &mut self.log {
                    log_view::draw(ui, body, v, !modal);
                }
            });
        if let Some(m) = &mut self.menu {
            if let Some(end) = m.ui(&ctx) {
                self.menu = None;
                self.menu_ends.push(end);
            }
        }
        if let Some(d) = &mut self.dialog {
            if let Some(end) = d.ui(&ctx, self.planner) {
                self.dialog = None;
                self.dialog_ends.push(end);
            }
        }
        if let Some(r) = &mut self.result {
            if r.ui(&ctx) {
                self.result = None;
                self.results_closed += 1;
            }
        }
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
    /// A window wider than any dialog, `host` laid out twice (a new modal is laid out unseen
    /// first).
    fn new(host: Host) -> Fx {
        let mut fx = Fx {
            drv: Driver::new(1200.0, 900.0),
            host,
        };
        fx.idle();
        fx.idle();
        fx
    }

    fn idle(&mut self) {
        self.drv.idle(&mut self.host, 1);
    }

    fn key(&mut self, key: Key, mods: Modifiers) {
        self.drv.key(&mut self.host, key, mods);
    }

    fn press(&mut self, key: Key) {
        self.key(key, Modifiers::NONE);
    }

    fn click(&mut self, text: &str) {
        self.drv.click_text_on_top(&mut self.host, text);
        self.idle();
    }

    fn shows(&self, text: &str) -> bool {
        self.drv.find_text(text).is_some()
    }

    fn assert_shows(&self, text: &str) {
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
    fn dialog_box(&self) -> bool {
        self.drv
            .rects()
            .iter()
            .any(|(r, fill, _)| *fill == PALETTE.dialog_back && r.width() > 100.0)
    }
}

fn with_menu(target: Target, repo: &Repo) -> Fx {
    Fx::new(Host {
        menu: Some(Menu::new(target, repo)),
        ..Host::default()
    })
}

fn with_dialog(d: ActionDialog, planner: Planner) -> Fx {
    Fx::new(Host {
        dialog: Some(d),
        planner,
        ..Host::default()
    })
}

fn ids(target: &Target, repo: &Repo) -> Vec<ActionId> {
    menu::items(target, repo).iter().map(|i| i.id).collect()
}

// ---------------------------------------------------------------------------------------------
// The menu (03 §7, §9.2).

/// Each object's menu offers 03 §7.1–§7.7's ids for it, in the tables' order, and only where the
/// tables say it applies.
#[test]
fn each_object_offers_its_tables_ids() {
    let r = repo();
    let bar = commit('b', &['a'], "feature work");
    assert_eq!(
        ids(
            &Target::Commit {
                commit: bar.clone(),
                marked: Vec::new()
            },
            &r
        ),
        [
            A::Tag,
            A::Branch,
            A::MoveBranch,
            A::Checkout,
            A::Pick,
            A::Revert,
            A::Merge,
            A::Rebase,
            A::Reset,
            A::Archive,
            A::CopyHash,
            A::CopySubject,
            A::MoveTag
        ],
        "§7.1, then §7.4's move-tag (from a commit's menu)"
    );
    let one_marked = Target::Commit {
        commit: bar.clone(),
        marked: vec![commit('c', &['b'], "later")],
    };
    assert!(
        ids(&one_marked, &r).contains(&A::Compare),
        "with the marked commit"
    );
    assert_eq!(
        ids(&Target::Label(local("feature", 'b')), &r),
        [
            A::CheckoutBranch,
            A::Rename,
            A::DeleteBranch,
            A::Merge,
            A::Rebase,
            A::Push,
            A::Archive,
            A::OnlyBranch,
            A::AllBranches,
            A::CopyName
        ],
        "§7.2"
    );
    assert!(
        !ids(&Target::Label(local("main", 'a')), &r).contains(&A::CheckoutBranch),
        "not on the checked-out branch"
    );
    assert_eq!(
        ids(&Target::Label(remote("origin/feature", 'b')), &r),
        [
            A::CheckoutRemote,
            A::DeleteRemoteBranch,
            A::FetchInto,
            A::Merge,
            A::Pull,
            A::Archive,
            A::OnlyBranch,
            A::AllBranches,
            A::CopyName
        ],
        "§7.3"
    );
    assert!(
        !ids(&Target::Label(remote("origin/main", 'a')), &r).contains(&A::FetchInto),
        "fetch-into only when the local branch is not checked out"
    );
    assert_eq!(
        ids(&Target::Label(tag("v1", 'a', false)), &r),
        [A::DeleteTag, A::PushTag, A::Archive, A::CopyName],
        "§7.4, a lightweight tag"
    );
    assert_eq!(
        ids(&Target::Label(tag("v2", 'b', true)), &r)[0],
        A::TagDetails,
        "an annotated tag's details"
    );
    assert_eq!(
        ids(&Target::Stash(stash(0, 'd', 'a')), &r),
        [
            A::StashApply,
            A::StashPop,
            A::StashBranch,
            A::StashDrop,
            A::CopyName,
            A::CopyHash
        ],
        "§7.5"
    );
    let d = dirty();
    assert_eq!(
        ids(&Target::Uncommitted, &d),
        [
            A::Stage,
            A::Unstage,
            A::Commit,
            A::Amend,
            A::Stash,
            A::ResetChanges,
            A::Clean,
            A::Editor
        ],
        "§7.6, the uncommitted row"
    );
    assert_eq!(
        ids(
            &Target::Files(vec![changed("u.txt", None, Some(Change::Modified))]),
            &d
        ),
        [A::Stage, A::Discard, A::Editor],
        "§7.6, a file"
    );
    assert_eq!(
        ids(&Target::Files(vec![conflict("c.txt")]), &d),
        [A::Ours, A::Theirs, A::Resolved, A::RedoMerge, A::Editor],
        "§7.6, a conflicted file"
    );
    assert_eq!(
        ids(&Target::Repository, &r),
        [
            A::Fetch,
            A::FetchRemote,
            A::PruneRemote,
            A::AddRemote,
            A::EditRemote,
            A::DeleteRemote,
            A::RemoteVisibility,
            A::User,
            A::UserRemove,
            A::Refresh,
            A::Find,
            A::Search,
            A::Filters,
            A::Log,
            A::Backups,
            A::Undo
        ],
        "§7.7"
    );
    let mut alone = r.clone();
    alone.remotes.clear();
    alone.refs.retain(|l| l.kind != RefKind::RemoteBranch);
    assert_eq!(
        ids(&Target::Repository, &alone),
        [
            A::AddRemote,
            A::User,
            A::UserRemove,
            A::Refresh,
            A::Find,
            A::Search,
            A::Filters,
            A::Log,
            A::Backups,
            A::Undo
        ],
        "no remote: nothing that names one"
    );
}

/// A name that is not UTF-8 names nothing (`RefLabel::lossy`, `StatusEntry::lossy`): its menu
/// offers no action, and no dialog lists it as a choice.
#[test]
fn a_name_not_utf8_is_offered_nowhere() {
    let mut r = repo();
    let mut bad = local("b\u{fffd}d", 'c');
    bad.lossy = true;
    r.refs.insert(1, bad.clone());
    assert!(ids(&Target::Label(bad), &r).is_empty());
    let mut file = untracked("n\u{fffd}.txt");
    file.lossy = true;
    assert!(ids(&Target::Files(vec![file]), &r).is_empty());
    // Move a branch here: the choice is `feature`, never the lossy one listed before it.
    let bar = Target::Commit {
        commit: commit('c', &['b'], "c"),
        marked: Vec::new(),
    };
    let d = dialog(ActionDialog::open(
        A::MoveBranch,
        &bar,
        &r,
        &Prefill::default(),
        fake,
    ));
    assert_eq!(
        d.draft(),
        &Action::MoveBranch {
            branch: "feature".into(),
            to: oid('c')
        }
    );
}

/// The menu (F10, Shift+F10, a right click): the dialog look, each item with its key; ↓ and
/// Enter choose, Esc cancels, "Repository…" turns it into the repository's menu.
#[test]
fn the_menu_draws_in_the_dialog_look_and_its_keys_choose() {
    let r = repo();
    let target = Target::Label(local("feature", 'b'));
    let mut fx = with_menu(target.clone(), &r);
    assert!(fx.dialog_box(), "the cyan box");
    fx.assert_shows("feature");
    fx.assert_shows("Check out");
    fx.assert_shows("Rename…");
    fx.assert_shows("Delete…");
    fx.assert_shows("F2");
    fx.assert_shows("F8");
    fx.assert_shows("Repository…");
    fx.press(Key::ArrowDown);
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.menu_ends,
        [MenuEnd::Chose {
            id: A::Rename,
            target: target.clone()
        }]
    );

    let mut fx = with_menu(target.clone(), &r);
    fx.press(Key::Escape);
    assert_eq!(fx.host.menu_ends, [MenuEnd::Cancel]);

    let mut fx = with_menu(target, &r);
    fx.click("Delete…");
    assert!(matches!(
        &fx.host.menu_ends[..],
        [MenuEnd::Chose {
            id: A::DeleteBranch,
            ..
        }]
    ));

    let mut fx = with_menu(Target::Stash(stash(0, 'd', 'a')), &r);
    fx.click("Repository…");
    fx.idle();
    fx.assert_shows("Fetch all remotes…");
    fx.assert_shows(&format!("{}+Shift+R", keys::command_name()));
    fx.press(Key::End);
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.menu_ends,
        [MenuEnd::Chose {
            id: A::Undo,
            target: Target::Repository
        }]
    );
}

// ---------------------------------------------------------------------------------------------
// The action dialogs (03 §7, §8.1, §8.4).

/// Every id is a view the panel carries out (no plan), or opens a dialog that shows its plan's
/// title, command lines and backup names — none lacks a form.
#[test]
fn every_id_is_a_view_or_a_dialog_with_its_plan() {
    let d = dirty();
    let targets = [
        Target::Commit {
            commit: commit('b', &['a'], "feature work"),
            marked: vec![commit('c', &['b'], "later")],
        },
        Target::Label(local("feature", 'b')),
        Target::Label(remote("origin/feature", 'b')),
        Target::Label(tag("v2", 'b', true)),
        Target::Stash(stash(0, 'd', 'a')),
        Target::Uncommitted,
        Target::Files(vec![
            changed("s.txt", Some(Change::Modified), Some(Change::Modified)),
            untracked("n.txt"),
        ]),
        Target::Files(vec![conflict("c.txt")]),
        Target::Repository,
    ];
    let stamp = "20261123T102405Z".to_string();
    for id in ActionId::ALL {
        let opened = match id {
            A::Restore => ActionDialog::restore(stamp.clone(), Vec::new(), None, &d, fake),
            A::DeleteBackup => ActionDialog::confirm(
                Action::DeleteBackup {
                    stamp: stamp.clone(),
                },
                &d,
                fake,
            ),
            A::Continue | A::Skip | A::Abort | A::BisectReset => {
                let p = match id {
                    A::Continue => Proceed::Continue,
                    A::Skip => Proceed::Skip,
                    A::Abort => Proceed::Abort,
                    _ => Proceed::BisectReset,
                };
                ActionDialog::confirm(Action::Proceed(p), &d, fake)
            }
            A::ShowBackup => {
                assert!(menu::is_view(id), "{id:?}");
                continue;
            }
            _ => {
                let target = targets
                    .iter()
                    .find(|t| ids(t, &d).contains(&id))
                    .unwrap_or_else(|| panic!("no menu offers {id:?}"));
                ActionDialog::open(id, target, &d, &Prefill::default(), fake)
            }
        };
        if menu::is_view(id) {
            assert!(matches!(opened, Opened::View(v) if v == id), "{id:?}");
            continue;
        }
        let d = dialog(opened);
        let command = fake_command(d.draft());
        let fx = with_dialog(d, fake);
        let name = id.as_str();
        assert!(fx.dialog_box(), "{name}");
        fx.assert_shows(&format!("Fake {name}"));
        fx.assert_shows(&command);
        fx.assert_shows(&format!("heads/{name}"));
    }
}

/// The confirmation (03 §8.1): the lost line first, then the options, what changes, the backups
/// written first, the exact command lines and the notes; the §8.4 warning for each overwrite or
/// delete on a remote.
#[test]
fn the_dialog_shows_every_part_of_the_confirmation() {
    let r = repo();
    let label = Target::Label(local("feature", 'b'));
    let d = dialog(ActionDialog::open(
        A::Push,
        &label,
        &r,
        &Prefill::default(),
        overwrite,
    ));
    let fx = with_dialog(d, overwrite);
    fx.assert_shows("Fake push");
    for text in [
        "push loses this",
        "push: 11111111 → 22222222",
        "heads/push",
        "git push",
        "push note",
    ] {
        fx.assert_shows(text);
    }
    let lost = fx.drv.find_text("push loses this").unwrap();
    let option = fx.drv.find_text("Set upstream").unwrap();
    let command = fx.drv.find_text("git push").unwrap();
    assert!(lost.top() < option.top(), "the lost line first (§8.1)");
    assert!(
        option.top() < command.top(),
        "the options before the commands"
    );
    fx.assert_shows(&dialogs::remote_warning(&RemoteChange {
        remote: "origin".into(),
        refname: "refs/heads/feature".into(),
        change: Overwrite::Replace {
            src: "refs/heads/feature".into(),
        },
        options: PushOptions {
            set_upstream: true,
            no_verify: false,
        },
    }));
    assert_eq!(
        dialogs::remote_warning(&RemoteChange {
            remote: "origin".into(),
            refname: "refs/tags/v1".into(),
            change: Overwrite::Delete,
            options: PushOptions {
                set_upstream: false,
                no_verify: false,
            },
        }),
        "deletes refs/tags/v1 on origin, with a lease on the value read at Run",
    );
    assert_eq!(
        fx.drv.painted_colors_of(
            "deletes refs/tags/v1 on origin, with a lease on the value read at Run"
        ),
        Some(vec![PALETTE.dialog_error]),
        "the warning in the dialog's error colour"
    );
}

/// The confirmation updates as options change (03 §8.1): each change builds the action again and
/// asks the planner, and the dialog shows the new plan.
#[test]
fn every_option_change_plans_again() {
    let r = repo();
    let bar = Target::Commit {
        commit: commit('b', &['a'], "feature work"),
        marked: Vec::new(),
    };
    let d = dialog(ActionDialog::open(
        A::Reset,
        &bar,
        &r,
        &Prefill::default(),
        fake,
    ));
    let mut fx = with_dialog(d, fake);
    fx.assert_shows("git reset --mixed bbbbbbbb");
    assert_eq!(
        asked().last(),
        Some(&Action::Reset {
            to: oid('b'),
            mode: ResetMode::Mixed
        })
    );
    let before = asked().len();
    fx.idle();
    assert_eq!(asked().len(), before, "nothing changed: not planned again");
    fx.click("hard");
    assert_eq!(
        asked().last(),
        Some(&Action::Reset {
            to: oid('b'),
            mode: ResetMode::Hard
        })
    );
    fx.assert_shows("git reset --hard bbbbbbbb");
    assert!(!fx.shows("git reset --mixed bbbbbbbb"));
    // Enter returns the action as chosen; the dialog runs nothing itself.
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.dialog_ends,
        [DialogEnd::Run {
            action: Action::Reset {
                to: oid('b'),
                mode: ResetMode::Hard
            },
            dont_ask_again: false
        }]
    );

    // A text option: each typed character plans again.
    let d = dialog(ActionDialog::open(
        A::Branch,
        &bar,
        &r,
        &Prefill::default(),
        fake,
    ));
    let mut fx = with_dialog(d, fake);
    fx.drv.frame(
        &mut fx.host,
        vec![egui::Event::Text("topic".into())],
        Modifiers::NONE,
    );
    fx.idle();
    assert_eq!(
        asked().last(),
        Some(&Action::Branch {
            at: oid('b'),
            name: "topic".into(),
            checkout: false
        })
    );
    fx.press(Key::Escape);
    assert_eq!(fx.host.dialog_ends, [DialogEnd::Cancel]);
}

/// A refusal (03 §8.6, §8.5) is the dialog's text, in the error colour, and Enter does nothing.
#[test]
fn a_refused_action_says_why_and_enter_does_nothing() {
    let r = repo();
    let bar = Target::Commit {
        commit: commit('b', &['a'], "feature work"),
        marked: Vec::new(),
    };
    let d = dialog(ActionDialog::open(
        A::Pick,
        &bar,
        &r,
        &Prefill::default(),
        refuse,
    ));
    let mut fx = with_dialog(d, refuse);
    let why = "an ignored file is in the way: build/out.o";
    fx.assert_shows(why);
    assert_eq!(
        fx.drv.painted_colors_of(why),
        Some(vec![PALETTE.dialog_error])
    );
    fx.press(Key::Enter);
    assert!(fx.host.dialog_ends.is_empty(), "nothing to run");
    assert!(fx.host.dialog.is_some(), "the dialog stays");
}

/// `Plan::confirm` `None` runs at once (03 §8.1's "No confirmation"): an action without options
/// returns it from `open`; a form with options still shows, with its steps' command lines.
#[test]
fn no_confirmation_runs_at_once() {
    let r = dirty();
    let label = Target::Label(local("feature", 'b'));
    match ActionDialog::open(A::CheckoutBranch, &label, &r, &Prefill::default(), quiet) {
        Opened::Run(action) => assert_eq!(
            action,
            Action::CheckoutBranch {
                branch: "feature".into()
            }
        ),
        other => panic!("{other:?}"),
    }
    match ActionDialog::open(
        A::Stage,
        &Target::Uncommitted,
        &r,
        &Prefill::default(),
        quiet,
    ) {
        Opened::Run(action) => assert_eq!(
            action,
            Action::Stage {
                paths: vec!["u.txt".into(), "n.txt".into()]
            }
        ),
        other => panic!("{other:?}"),
    }
    match ActionDialog::confirm(
        Action::Fetch {
            prune: false,
            prune_tags: false,
        },
        &r,
        quiet,
    ) {
        Opened::Run(_) => {}
        other => panic!("Fetch all (Cmd/Ctrl+Shift+R) runs at once: {other:?}"),
    }
    // The menu's Fetch is the dialog form (prune, prune tags).
    let d = dialog(ActionDialog::open(
        A::Fetch,
        &Target::Repository,
        &r,
        &Prefill::default(),
        quiet,
    ));
    let fx = with_dialog(d, quiet);
    fx.assert_shows("Prune");
    fx.assert_shows("git checkout fetch");
    // Check out (detached) with "Don't ask again in this window" chosen earlier runs at once.
    let bar = Target::Commit {
        commit: commit('b', &['a'], "b"),
        marked: Vec::new(),
    };
    let unasked = Prefill {
        checkout_unasked: true,
        ..Prefill::default()
    };
    assert!(matches!(
        ActionDialog::open(A::Checkout, &bar, &r, &unasked, fake),
        Opened::Run(Action::Checkout { .. })
    ));
    let d = dialog(ActionDialog::open(
        A::Checkout,
        &bar,
        &r,
        &Prefill::default(),
        fake,
    ));
    let mut fx = with_dialog(d, fake);
    fx.click("Don't ask again in this window");
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.dialog_ends,
        [DialogEnd::Run {
            action: Action::Checkout { at: oid('b') },
            dont_ask_again: true
        }]
    );
}

/// A restore never writes over anything (03 §8.3): an entry that moved since is listed as moved
/// and left unchosen, an occupied path is left unless "replace" is chosen; one item of a group
/// restores alone.
#[test]
fn a_restore_never_writes_over_anything_unless_chosen() {
    let r = repo();
    let stamp = "20261123T102405Z".to_string();
    let backup_ref = |kind: BackupKind, name: &str, at: char| BackupRef {
        kind,
        refname: format!("refs/sopen-backup/{stamp}/{name}"),
        oid: oid(at),
        subject: "first".into(),
    };
    let entries = vec![
        RestoreEntry {
            subject: backup::Subject::Ref("refs/heads/main".into()),
            how: RestoreHow::CheckedOut,
            backup: Some(backup_ref(
                BackupKind::Head("main".into()),
                "heads/main",
                'a',
            )),
            before: Value::Object(oid('a')),
            now: Value::Object(oid('b')),
            moved_since: false,
            chosen: false,
            replace: true,
        },
        RestoreEntry {
            subject: backup::Subject::Ref("refs/heads/feature".into()),
            how: RestoreHow::Ref,
            backup: Some(backup_ref(
                BackupKind::Head("feature".into()),
                "heads/feature",
                'b',
            )),
            before: Value::Object(oid('b')),
            now: Value::Object(oid('c')),
            moved_since: true,
            chosen: true,
            replace: false,
        },
        RestoreEntry {
            subject: backup::Subject::Path("build/out.txt".into()),
            how: RestoreHow::Files,
            backup: Some(backup_ref(BackupKind::Files, "files", 'e')),
            before: Value::Object(oid('f')),
            now: Value::Occupied,
            moved_since: false,
            chosen: false,
            replace: false,
        },
    ];
    let d = dialog(ActionDialog::restore(
        stamp.clone(),
        entries.clone(),
        None,
        &r,
        fake,
    ));
    let Action::Restore {
        entries: drafted, ..
    } = d.draft().clone()
    else {
        panic!("{:?}", d.draft());
    };
    let chosen: Vec<(bool, bool)> = drafted.iter().map(|e| (e.chosen, e.replace)).collect();
    assert_eq!(
        chosen,
        [(true, false), (false, false), (true, false)],
        "moved since: unchosen; nothing replaced"
    );
    let mut fx = with_dialog(d, fake);
    fx.assert_shows("moved since (now cccccccc)");
    fx.assert_shows("occupied: left as it is unless replace is chosen");
    fx.click("replace");
    let Some(Action::Restore {
        entries: planned, ..
    }) = asked().last().cloned()
    else {
        panic!("{:?}", asked());
    };
    assert!(planned[2].replace, "replace chosen for the occupied path");
    fx.press(Key::Enter);
    assert!(matches!(
        &fx.host.dialog_ends[..],
        [DialogEnd::Run {
            action: Action::Restore { .. },
            ..
        }]
    ));

    // One item of the group (a ref line of the Backups list): that entry alone is chosen — and
    // one that moved since still waits for its explicit choice in the dialog.
    let chosen = |only: &str| -> Vec<bool> {
        let only = format!("refs/sopen-backup/{stamp}/{only}");
        let d = dialog(ActionDialog::restore(
            stamp.clone(),
            entries.clone(),
            Some(&only),
            &r,
            fake,
        ));
        let Action::Restore { entries, .. } = d.draft() else {
            panic!("{:?}", d.draft());
        };
        entries.iter().map(|e| e.chosen).collect()
    };
    assert_eq!(chosen("heads/main"), [true, false, false], "the item alone");
    assert_eq!(
        chosen("heads/feature"),
        [false, false, false],
        "moved since: still a second, explicit choice"
    );
}

// ---------------------------------------------------------------------------------------------
// The result dialog (03 §3.8, §8.3).

fn entry(line: &str, ending: Ending, stderr: &str, secs: u64) -> LogEntry {
    LogEntry {
        time: SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_389_040 + secs),
        host: "box".into(),
        repo: "/srv/sopen".into(),
        line: line.into(),
        stdin: None,
        ending,
        duration: Duration::from_millis(1200),
        stdout_bytes: 0,
        stderr: stderr.into(),
    }
}

fn done(line: &str, code: i32, stderr: &str) -> Done {
    Done {
        ending: Ending::Exit(code),
        stdout: Vec::new(),
        stdout_cut: false,
        stderr: stderr.into(),
        entry: entry(line, Ending::Exit(code), stderr, 0),
    }
}

fn outcome(failed: bool) -> Outcome {
    let push = "git push --progress origin refs/heads/b:refs/heads/b";
    let error = "error: failed to push some refs to 'origin'\nhint: fetch first";
    let (code, stderr) = if failed { (1, error) } else { (0, "") };
    let (second, third) = if failed {
        (StepOutcome::Failed, StepOutcome::NotRun)
    } else {
        (StepOutcome::Done, StepOutcome::Done)
    };
    Outcome {
        steps: vec![
            StepResult {
                line: "backup".into(),
                outcome: StepOutcome::Done,
                ran: vec![done("git update-ref --stdin", 0, "")],
            },
            StepResult {
                line: push.into(),
                outcome: second,
                ran: vec![done(push, code, stderr)],
            },
            StepResult {
                line: "git branch -d b".into(),
                outcome: third,
                ran: Vec::new(),
            },
        ],
        backup: Some("20261123T102405Z".into()),
    }
}

/// A failed step (03 §3.8): the dialog titled with the action lists each step `done`, `failed`
/// or `not run`, holds git's stderr whole and the log entry's command line, and the backup line;
/// Enter or Esc closes it. Every step done: no dialog, the status row's line.
#[test]
fn the_result_dialog_lists_each_step_and_the_error() {
    assert!(ResultDialog::of("Delete branch b", &outcome(false)).is_none());
    assert_eq!(
        dialogs::status_text("Deleted branch b", &outcome(false)),
        format!(
            "Deleted branch b · backup 20261123T102405Z · {}+Z undoes",
            keys::command_name()
        )
    );
    let mut quiet_outcome = outcome(false);
    quiet_outcome.backup = None;
    assert_eq!(
        dialogs::status_text("Fetched", &quiet_outcome),
        "Fetched",
        "no backup, no undo"
    );

    let result = ResultDialog::of("Delete branch b", &outcome(true)).expect("a step failed");
    let mut fx = Fx::new(Host {
        result: Some(result),
        ..Host::default()
    });
    assert!(fx.dialog_box());
    fx.assert_shows("Delete branch b");
    for word in ["done", "failed", "not run"] {
        fx.assert_shows(word);
    }
    fx.assert_shows("git branch -d b");
    fx.assert_shows("error: failed to push some refs to 'origin'\nhint: fetch first");
    fx.assert_shows("git push --progress origin refs/heads/b:refs/heads/b");
    fx.assert_shows(&format!(
        "backup 20261123T102405Z · {}+Z undoes",
        keys::command_name()
    ));
    fx.press(Key::Enter);
    assert_eq!(fx.host.results_closed, 1);

    let mut fx = Fx::new(Host {
        result: Some(ResultDialog::error(
            "Backups",
            "fatal: not a git repository",
            Some("git for-each-ref refs/sopen-backup/"),
        )),
        ..Host::default()
    });
    fx.assert_shows("fatal: not a git repository");
    fx.assert_shows("git for-each-ref refs/sopen-backup/");
    fx.press(Key::Escape);
    assert_eq!(fx.host.results_closed, 1);
}

// ---------------------------------------------------------------------------------------------
// The Backups list (03 §8.2, §9.2).

fn records() -> Vec<Record> {
    let group = |stamp: &str, message: &str, refs: Vec<(BackupKind, &str, char, &str)>| Record {
        group: Backup {
            stamp: stamp.into(),
            message: message.into(),
            refs: refs
                .into_iter()
                .map(|(kind, name, at, subject)| BackupRef {
                    kind,
                    refname: format!("refs/sopen-backup/{stamp}/{name}"),
                    oid: oid(at),
                    subject: subject.into(),
                })
                .collect(),
        },
        manifest: Manifest::default(),
    };
    vec![
        group(
            "20261123T102405Z",
            "sopen: reset main to aaaaaaaa",
            vec![
                (BackupKind::Head("main".into()), "heads/main", 'a', "first"),
                (BackupKind::Manifest, "manifest", 'e', "sopen: reset main"),
            ],
        ),
        group(
            "20261122T080000Z-2",
            "sopen: drop stash@{0}",
            vec![(BackupKind::Stash, "stash", 'd', "WIP on main: first")],
        ),
    ]
}

/// The Backups list: groups newest first with the time and the reflog message, each ref with its
/// kind, short hash and subject, the header's warning; the git context's rows act on it — Enter
/// Shows, Cmd/Ctrl+Z restores, F8 deletes, Esc closes — and its buttons do the same.
#[test]
fn the_backups_list_shows_groups_and_its_keys_and_buttons_act() {
    let mut fx = Fx::new(Host {
        backups: Some(BackupsView::new(records())),
        ..Host::default()
    });
    fx.assert_shows(backups::HEADER);
    fx.assert_shows("2026-11-23 10:24:05 UTC");
    fx.assert_shows("sopen: reset main to aaaaaaaa");
    fx.assert_shows("2026-11-22 08:00:00 UTC -2");
    fx.assert_shows("heads/main");
    fx.assert_shows("aaaaaaaa");
    fx.assert_shows("WIP on main: first");
    let group_y = fx.drv.find_text("2026-11-23 10:24:05 UTC").unwrap().top();
    let other_y = fx
        .drv
        .find_text("2026-11-22 08:00:00 UTC -2")
        .unwrap()
        .top();
    assert!(group_y < other_y, "newest first");
    let refs = &records()[0].group.refs;

    // Enter on a group: Show its refs (not the manifest).
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.backups_ends.pop(),
        Some(BackupsEnd::Show(vec![refs[0].clone()]))
    );
    // ↓ to its first ref: Cmd/Ctrl+Z restores that item, F8 deletes the group.
    fx.press(Key::ArrowDown);
    fx.key(Key::Z, testkit::command(false));
    assert_eq!(
        fx.host.backups_ends.pop(),
        Some(BackupsEnd::Restore {
            stamp: "20261123T102405Z".into(),
            only: Some(refs[0].clone())
        })
    );
    fx.press(Key::F8);
    assert_eq!(
        fx.host.backups_ends.pop(),
        Some(BackupsEnd::Delete(Action::DeleteBackup {
            stamp: "20261123T102405Z".into()
        }))
    );
    // The buttons do what the keys do.
    fx.click("Restore");
    assert_eq!(
        fx.host.backups_ends.pop(),
        Some(BackupsEnd::Restore {
            stamp: "20261123T102405Z".into(),
            only: Some(refs[0].clone())
        })
    );
    fx.click("Show");
    assert_eq!(
        fx.host.backups_ends.pop(),
        Some(BackupsEnd::Show(vec![refs[0].clone()]))
    );
    // The bar to the second group: Restore is the group.
    fx.press(Key::End);
    fx.press(Key::ArrowUp);
    fx.click("Restore");
    assert_eq!(
        fx.host.backups_ends.pop(),
        Some(BackupsEnd::Restore {
            stamp: "20261122T080000Z-2".into(),
            only: None
        })
    );
    fx.press(Key::Escape);
    assert_eq!(fx.host.backups_ends.pop(), Some(BackupsEnd::Close));

    let fx = Fx::new(Host {
        backups: Some(BackupsView::new(Vec::new())),
        ..Host::default()
    });
    fx.assert_shows("no backups");
}

// ---------------------------------------------------------------------------------------------
// The command log (03 §3.7).

/// The log in the panel, newest first, one line an entry; Enter shows the entry whole (its
/// stderr included), Esc closes that, then the log.
#[test]
fn the_command_log_lists_newest_first_and_enter_shows_an_entry_whole() {
    let mut newest = entry(
        "git push --progress origin main",
        Ending::Exit(1),
        "error: failed to push some refs\nhint: fetch first",
        60,
    );
    newest.stdin = Some("message, 3 lines".into());
    let older = entry(
        "git fetch --progress --no-prune --no-prune-tags --all",
        Ending::Exit(0),
        "",
        0,
    );
    let mut fx = Fx::new(Host {
        log: Some(LogView::new(vec![newest.clone(), older.clone()])),
        ..Host::default()
    });
    fx.assert_shows(log_view::HEADER);
    let first = log_view::row_text(&newest);
    let second = log_view::row_text(&older);
    assert!(first.contains("git push --progress origin main · error: failed to push some refs"));
    fx.assert_shows(&first);
    fx.assert_shows(&second);
    assert!(
        fx.drv.find_text(&first).unwrap().top() < fx.drv.find_text(&second).unwrap().top(),
        "newest first"
    );
    fx.press(Key::Enter);
    fx.idle();
    assert!(fx.host.log.as_ref().unwrap().whole().is_some());
    fx.assert_shows("error: failed to push some refs\nhint: fetch first");
    fx.assert_shows("message, 3 lines");
    fx.assert_shows("/srv/sopen");
    fx.press(Key::Escape);
    assert!(fx.host.log.as_ref().unwrap().whole().is_none());
    assert!(fx.host.log_ends.is_empty(), "the log stays");
    fx.press(Key::ArrowDown);
    fx.press(Key::Enter);
    fx.idle();
    fx.assert_shows("git fetch --progress --no-prune --no-prune-tags --all");
    fx.press(Key::Escape);
    fx.press(Key::Escape);
    assert_eq!(fx.host.log_ends, [LogEnd::Close]);
}

// ---------------------------------------------------------------------------------------------
// The banner (03 §8.5, §9.2).

/// The banner: its text in the warning colour, one row a line, the steps it offers as buttons;
/// Cmd/Ctrl+Shift+C, S, A, B give `Action::Proceed`, a step not offered says so instead.
#[test]
fn the_banner_shows_its_state_and_its_keys_and_buttons_proceed() {
    let text = "MERGE of feature · 1 conflict";
    let mut fx = Fx::new(Host {
        banner: Some(BannerView::new(
            text,
            vec![Proceed::Continue, Proceed::Abort],
        )),
        ..Host::default()
    });
    fx.assert_shows(text);
    assert_eq!(fx.drv.colors_of(text), Some(vec![PALETTE.warn]));
    fx.assert_shows("Continue");
    fx.assert_shows("Abort");
    assert!(!fx.shows("Skip"), "a merge has no Skip");
    fx.key(Key::C, testkit::command(true));
    assert_eq!(
        fx.host.pressed,
        [Pressed::Proceed(Action::Proceed(Proceed::Continue))]
    );
    fx.host.pressed.clear();
    fx.key(Key::S, testkit::command(true));
    assert!(
        matches!(&fx.host.pressed[..], [Pressed::NotOffered(why)] if why.contains("Skip")),
        "{:?}",
        fx.host.pressed
    );
    fx.host.pressed.clear();
    fx.click("Abort");
    assert_eq!(
        fx.host.pressed,
        [Pressed::Proceed(Action::Proceed(Proceed::Abort))]
    );
    fx.host.pressed.clear();
    // Cmd/Ctrl+C without Shift is not the banner's.
    fx.key(Key::C, testkit::command(false));
    assert!(fx.host.pressed.is_empty());

    let both = BannerView::new(
        "REBASE 3/7 · stopped at abcdef12 fix · 2 conflicts\nBISECT · Reset",
        vec![
            Proceed::Continue,
            Proceed::Skip,
            Proceed::Abort,
            Proceed::BisectReset,
        ],
    );
    assert_eq!(both.rows(), 2);
    let mut fx = Fx::new(Host {
        banner: Some(both),
        ..Host::default()
    });
    fx.assert_shows("REBASE 3/7 · stopped at abcdef12 fix · 2 conflicts");
    fx.assert_shows("BISECT · Reset");
    fx.key(Key::B, testkit::command(true));
    assert_eq!(
        fx.host.pressed,
        [Pressed::Proceed(Action::Proceed(Proceed::BisectReset))]
    );
}

/// A message field takes several lines (03 §7.6's commit message): Shift+Enter is a new line in
/// it, Enter still runs the dialog (03 §9.2: inside a dialog, Enter runs).
#[test]
fn shift_enter_is_a_new_line_in_a_message_and_enter_runs() {
    let d = dialog(ActionDialog::open(
        A::Commit,
        &Target::Uncommitted,
        &dirty(),
        &Prefill::default(),
        fake,
    ));
    let mut fx = with_dialog(d, fake);
    let typed = |fx: &mut Fx, text: &str| {
        fx.drv.frame(
            &mut fx.host,
            vec![egui::Event::Text(text.into())],
            Modifiers::NONE,
        );
        fx.idle();
    };
    typed(&mut fx, "a");
    fx.key(Key::Enter, Modifiers::SHIFT);
    typed(&mut fx, "b");
    let draft = fx.host.dialog.as_ref().expect("still open").draft().clone();
    assert_eq!(
        draft,
        Action::Commit {
            message: "a\nb".into(),
            amend: false,
            no_verify: false
        }
    );
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.dialog_ends,
        [DialogEnd::Run {
            action: draft,
            dont_ask_again: false
        }]
    );
}

// ---------------------------------------------------------------------------------------------
// edit-remote's masked URLs (03 §3.7; the coordinator's decision, 2026-11-23).

/// `edit-remote`'s dialog over `origin`, its URLs prefilled as the integration gives them (masked).
fn edit_remote(url: &str, push_url: Option<&str>) -> Fx {
    let prefill = Prefill {
        remotes: vec![Remote {
            name: "origin".into(),
            url: url.into(),
            push_url: push_url.map(str::to_string),
        }],
        ..Prefill::default()
    };
    let d = dialog(ActionDialog::open(
        A::EditRemote,
        &Target::Repository,
        &repo(),
        &prefill,
        fake,
    ));
    with_dialog(d, fake)
}

/// `text` typed as one event, then a pass.
fn type_text(fx: &mut Fx, text: &str) {
    fx.drv.frame(
        &mut fx.host,
        vec![egui::Event::Text(text.into())],
        Modifiers::NONE,
    );
    fx.idle();
}

/// A click into the field showing `shown`, then `text` typed there (`all`: over all of it).
fn type_into(fx: &mut Fx, shown: &str, all: bool, text: &str) {
    let at = fx
        .drv
        .find_text(shown)
        .unwrap_or_else(|| panic!("no field shows {shown:?}"))
        .center();
    fx.drv.click(&mut fx.host, at);
    if all {
        fx.key(Key::A, testkit::command(false));
    }
    type_text(fx, text);
}

/// What Enter returned: the action run.
fn ran(fx: &mut Fx) -> (Remote, Remote) {
    fx.press(Key::Enter);
    match &fx.host.dialog_ends[..] {
        [DialogEnd::Run {
            action: Action::EditRemote { was, now },
            ..
        }] => (was.clone(), now.clone()),
        other => panic!("{other:?}"),
    }
}

const MASKED: &str = "https://ada:***@example.com/x.git";
const MASKED_PUSH: &str = "https://***@example.com/push.git";

/// A URL field left alone changes nothing: `now`'s URL is `was`'s, so the masked prefill is
/// never written back, whatever else is edited.
#[test]
fn an_untouched_masked_url_is_not_changed() {
    let mut fx = edit_remote(MASKED, Some(MASKED_PUSH));
    fx.assert_shows(MASKED);
    // The name field has the typing: a rename alone.
    type_text(&mut fx, "2");
    let (was, now) = ran(&mut fx);
    assert_eq!(was.url, MASKED);
    assert_ne!(now.name, was.name, "the rename went in");
    assert_eq!(now.url, was.url, "fetch URL unchanged: nothing to set");
    assert_eq!(
        now.push_url, was.push_url,
        "push URL unchanged: nothing to set"
    );
}

/// A changed field that still holds the mask is refused (it would set the URL to `***`): the
/// dialog says so, Run is off, and the planner never sees it.
#[test]
fn an_edit_that_keeps_the_mask_is_refused() {
    let mut fx = edit_remote(MASKED, None);
    type_into(&mut fx, MASKED, false, "z");
    fx.assert_shows(dialogs::RETYPE);
    assert_eq!(
        fx.drv.painted_colors_of(dialogs::RETYPE),
        Some(vec![PALETTE.dialog_error])
    );
    fx.press(Key::Enter);
    assert!(fx.host.dialog_ends.is_empty(), "Run is off");
    assert!(fx.host.dialog.is_some());
    for action in asked() {
        if let Action::EditRemote { was, now } = action {
            assert_eq!(now.url, was.url, "a masked edit was planned: {now:?}");
        }
    }
    // The same for the push URL field, retyped with the mask in it.
    let mut fx = edit_remote(MASKED, Some(MASKED_PUSH));
    type_into(
        &mut fx,
        MASKED_PUSH,
        true,
        "https://bob:***@example.com/p.git",
    );
    fx.assert_shows(dialogs::RETYPE);
    assert!(dialogs::hides_password("https://***@example.com/p.git"));
    assert!(!dialogs::hides_password("https://example.com/a***b.git"));
}

/// A URL retyped whole goes into the action as typed; the field left alone stays unchanged.
#[test]
fn a_fully_retyped_url_is_passed() {
    let mut fx = edit_remote(MASKED, Some(MASKED_PUSH));
    let typed = "https://ada:n3w-pass@example.com/x.git";
    type_into(&mut fx, MASKED, true, typed);
    assert!(!fx.shows(dialogs::RETYPE));
    let (was, now) = ran(&mut fx);
    assert_eq!(now.url, typed);
    assert_eq!(now.push_url, was.push_url, "the push URL left alone");
}

/// A URL without credentials behaves as before: left alone it is unchanged, edited it goes in
/// as typed, never refused.
#[test]
fn a_url_without_credentials_behaves_as_before() {
    let plain = "https://example.com/x.git";
    let mut fx = edit_remote(plain, None);
    let (was, now) = ran(&mut fx);
    assert_eq!((now.url.as_str(), now.push_url), (plain, None));
    assert_eq!(was.url, plain);

    let mut fx = edit_remote(plain, None);
    type_into(&mut fx, plain, true, "https://example.com/y.git");
    assert!(!fx.shows(dialogs::RETYPE));
    let (_, now) = ran(&mut fx);
    assert_eq!(now.url, "https://example.com/y.git");
    assert_eq!(now.push_url, None);
}

// ---------------------------------------------------------------------------------------------
// Review round 1 (PR #81).

/// Move tag keeps the chosen tag's own message (03 §7.4): switching from annotated `alpha` to
/// annotated `beta` loads beta's, never alpha's; an annotated tag whose message is not loaded is
/// refused; a lightweight tag moves lightweight; an edit stays with its tag.
#[test]
fn move_tag_keeps_the_chosen_tags_own_message() {
    let mut r = repo();
    r.refs.retain(|l| l.kind != RefKind::Tag);
    r.refs.extend([
        tag("alpha", 'a', true),
        tag("beta", 'b', true),
        tag("gamma", 'a', true),
        tag("light", 'a', false),
    ]);
    let prefill = Prefill {
        tag_messages: [("alpha", "release alpha"), ("beta", "release beta")]
            .into_iter()
            .map(|(t, m)| (t.to_string(), m.to_string()))
            .collect(),
        // A commit message meant for another dialog is never a tag's.
        message: Some("a commit message".into()),
        ..Prefill::default()
    };
    let bar = Target::Commit {
        commit: commit('c', &['b'], "c"),
        marked: Vec::new(),
    };
    let open = || dialog(ActionDialog::open(A::MoveTag, &bar, &r, &prefill, fake));
    let move_tag = |tag: &str, message: Option<&str>| Action::MoveTag {
        tag: tag.into(),
        to: oid('c'),
        message: message.map(str::to_string),
    };
    let d = open();
    assert_eq!(d.draft(), &move_tag("alpha", Some("release alpha")));

    // alpha → beta: Run carries beta's message.
    let mut fx = with_dialog(open(), fake);
    fx.assert_shows("release alpha");
    fx.click("beta");
    fx.assert_shows("release beta");
    assert!(!fx.shows("release alpha"));
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.dialog_ends,
        [DialogEnd::Run {
            action: move_tag("beta", Some("release beta")),
            dont_ask_again: false
        }]
    );

    // gamma's message is not loaded: refused, never alpha's text.
    let mut fx = with_dialog(open(), fake);
    fx.click("gamma");
    let why = format!("gamma: {}", dialogs::NOT_LOADED);
    fx.assert_shows(&why);
    assert_eq!(
        fx.drv.painted_colors_of(&why),
        Some(vec![PALETTE.dialog_error])
    );
    fx.press(Key::Enter);
    assert!(fx.host.dialog_ends.is_empty(), "Run is off");
    assert_eq!(
        fx.host.dialog.as_ref().unwrap().draft(),
        &move_tag("gamma", Some(""))
    );
    // Moving it lightweight instead is not refused.
    fx.click("Keep it annotated with its message");
    assert!(!fx.shows(&why));
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.dialog_ends,
        [DialogEnd::Run {
            action: move_tag("gamma", None),
            dont_ask_again: false
        }]
    );

    // A lightweight tag moves lightweight; an edit to alpha stays alpha's across a switch.
    let mut fx = with_dialog(open(), fake);
    let field = fx.drv.find_text("release alpha").unwrap().right_center();
    fx.drv.click(&mut fx.host, field + egui::vec2(-1.0, 0.0));
    type_text(&mut fx, " v2");
    fx.click("light");
    assert_eq!(
        fx.host.dialog.as_ref().unwrap().draft(),
        &move_tag("light", None)
    );
    fx.click("alpha");
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.dialog_ends,
        [DialogEnd::Run {
            action: move_tag("alpha", Some("release alpha v2")),
            dont_ask_again: false
        }]
    );

    // A new tag starts with an empty message, not the prefill's commit message.
    let d = dialog(ActionDialog::open(A::Tag, &bar, &r, &prefill, fake));
    assert!(matches!(d.draft(), Action::Tag { message: Some(m), .. } if m.is_empty()));
}

/// The result dialog masks whatever it shows (03 §3.7), a URL the owner typed included.
#[test]
fn the_result_dialog_masks_credentials() {
    let secret = "https://ada:s3cret@example.com/x.git";
    let masked = "https://ada:***@example.com/x.git";
    let mut failed = outcome(true);
    failed.steps[1].line = format!("git remote add origin {secret}");
    failed.steps[1].ran[0].entry.line = format!("git remote add origin {secret}");
    failed.steps[1].ran[0].stderr = format!("fatal: remote origin already exists: {secret}");
    let fx = Fx::new(Host {
        result: Some(ResultDialog::of("Add a remote", &failed).unwrap()),
        ..Host::default()
    });
    fx.assert_shows(&format!("git remote add origin {masked}"));
    fx.assert_shows(&format!("fatal: remote origin already exists: {masked}"));
    let texts = fx.drv.texts_starting_with("");
    assert!(
        !texts.iter().any(|(_, t)| t.contains("s3cret")),
        "{texts:?}"
    );
    let fx = Fx::new(Host {
        result: Some(ResultDialog::error(
            "Edit a remote",
            &format!("fatal: could not read {secret}"),
            Some(&format!("git remote set-url origin {secret}")),
        )),
        ..Host::default()
    });
    fx.assert_shows(&format!("fatal: could not read {masked}"));
    fx.assert_shows(&format!("git remote set-url origin {masked}"));
}
