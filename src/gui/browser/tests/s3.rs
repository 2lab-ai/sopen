//! Slice S3's scenarios (02 §8.1's table) through the browser's own window: the action menu, the
//! F-keys and the banner's keys on each kind of row (03 §7, §9), each action's dialog and its
//! confirmation (§8.1), the run on a worker under the repository's write lock (02 §6), the refresh
//! after it (03 §5), backups and the window's undo (§8.2–§8.3), the Backups list, the command log,
//! the banner (§8.5) and `SOPEN_STATE dialog=` (02 §9).
//!
//! Each action's plan and executor are S3-A's, tested in `git::tests::{actions,backup}` through
//! `LocalGit` and `ShellGit`; the screens are S3-B's (`git_panel::tests_s3`). These tests drive the
//! wiring: real keys and clicks, the real planner, the real runner, a repository of the test's own
//! (`git::history::fixture`). They skip with a note where there is no git, and fail under
//! `SOPEN_REQUIRE_GIT=1` (CI).

use super::s2::{small, Gx};
use super::*;
use crate::git::actions::{Action, ActionId};
use crate::git::history::fixture::{self, Repo};
use std::path::Path;
use std::time::{Duration, Instant};

/// The fixture's git, or the test skips (fails under `SOPEN_REQUIRE_GIT=1`).
macro_rules! git_or_skip {
    () => {
        match fixture::git() {
            Some(git) => git,
            None => return,
        }
    };
}

impl Gx {
    /// The panel on the right with its rows and its actions' snapshot, the keyboard in it.
    pub(super) fn ready_in_git(&mut self) {
        self.wait_for_the_panel();
        self.tab_into_git();
        self.until_quiet("the snapshot");
    }

    /// Passes until the panel has its snapshot and nothing of S3 runs or reads, then settles.
    pub(super) fn until_quiet(&mut self, what: &str) {
        let top = self.top();
        self.until(what, move |b| {
            let Some(i) = b.git.panel(&top) else {
                return false;
            };
            let p = &b.git.panels[i];
            p.view.repo().is_some() && !p.acting() && p.read.is_none() && !p.dialog_reading()
        });
        self.settle();
    }

    pub(super) fn view(&self) -> &GitView {
        &self.panel().view
    }

    /// The open dialog's action id.
    pub(super) fn dialog_id(&self) -> Option<ActionId> {
        self.view().draft().map(Action::id)
    }

    /// A click on the control labelled `text` on top (a dialog's): a pass first, since a dialog
    /// whose reads came re-centres on the pass after it grew.
    pub(super) fn click(&mut self, text: &str) {
        self.idle(2);
        self.fx.drv.click_text_on_top(&mut self.fx.b, text);
        self.idle(2);
    }

    /// F10, then a click on the menu's `item`; passes until its dialog's reads are in.
    pub(super) fn menu(&mut self, item: &str) {
        self.key(Key::F10);
        self.idle(2);
        assert!(
            self.painted(item),
            "{item} in the menu: {:#?}",
            self.texts()
        );
        self.click(item);
        self.until_quiet(item);
    }

    /// Enter on the dialog: it runs on a worker; passes until the run ended and the refresh came.
    pub(super) fn run(&mut self) {
        self.key(Key::Enter);
        self.idle(2);
        self.until_quiet("the run and its refresh");
    }

    pub(super) fn status(&self) -> String {
        self.fx.b.status_line().text
    }

    pub(super) fn cmd_shift(&mut self, key: Key) {
        self.with(key, Modifiers::COMMAND | Modifiers::SHIFT);
        self.idle(2);
    }

    /// An F-key, then its dialog's reads.
    pub(super) fn f(&mut self, key: Key) {
        self.key(key);
        self.idle(2);
        self.until_quiet("the key's dialog");
    }

    /// Esc, then a pass.
    pub(super) fn esc(&mut self) {
        self.key(Key::Escape);
        self.idle(2);
    }

    /// Every text the last pass drew, whole.
    pub(super) fn texts(&self) -> Vec<String> {
        self.fx
            .drv
            .texts_starting_with("")
            .into_iter()
            .map(|(_, s)| s)
            .collect()
    }

    /// A text drawn that holds `part`.
    pub(super) fn shows(&self, part: &str) -> bool {
        self.texts().iter().any(|t| t.contains(part))
    }

    /// The F-key row the panel shows now, as `(slot, label)`.
    pub(super) fn slots(&self) -> Vec<(u8, &'static str)> {
        let i = self.b().git_focused().expect("the keyboard in git");
        self.b().git.panels[i]
            .view
            .fkeys(self.b().mode)
            .into_iter()
            .map(|k| (k.n, k.label))
            .collect()
    }

    /// The label cursor on the bar's row's label `text` (→ from the commit).
    pub(super) fn label_to(&mut self, text: &str) {
        for _ in 0..8 {
            if self.view().bar_label().is_some_and(|l| l.text == text) {
                return;
            }
            self.key(Key::ArrowRight);
        }
        panic!("no label {text} on the bar's row");
    }

    /// The bar on the uncommitted row, Enter: its details pane with the keyboard, the pane's bar
    /// on `path`.
    pub(super) fn pane_on(&mut self, path: &str) {
        let first = self.subjects()[0].clone();
        assert!(first.starts_with("Uncommitted changes"), "{first}");
        self.bar_to(&first);
        self.key(Key::Enter);
        let top = self.top();
        self.until("the uncommitted row's files", |b| {
            let i = b.git.panel(&top).unwrap();
            let p = &b.git.panels[i];
            p.details.is_none() && p.last_details.is_some()
        });
        self.settle();
        for _ in 0..30 {
            if self.view().pane_file().is_some_and(|f| f.path == path) {
                return;
            }
            self.key(Key::ArrowDown);
        }
        panic!("{path} not reached in the pane");
    }

    /// The backup refs in the repository.
    pub(super) fn backups(&self) -> String {
        self.repo
            .run(&["for-each-ref", "--format=%(refname)", "refs/sopen-backup/"])
    }
}

/// A repository on `main` — `base`, then `on main` — with a branch `side` whose `c.txt`
/// conflicts with main's; a clean work tree.
pub(super) fn conflicting(git: &Path) -> Repo {
    let repo = Repo::new(git);
    repo.write("c.txt", "base\n");
    repo.commit("base");
    repo.run(&["checkout", "-q", "-b", "side"]);
    repo.write("c.txt", "side\n");
    repo.commit("on side");
    repo.run(&["checkout", "-q", "main"]);
    repo.write("c.txt", "main\n");
    repo.commit("on main");
    repo
}

/// `origin`: a bare clone of `repo` beside it, added as its remote and fetched; its path.
pub(super) fn with_origin(repo: &Repo) -> std::path::PathBuf {
    let origin = repo.tmp.path().join("origin.git");
    let at = origin.to_str().unwrap();
    repo.run(&["clone", "-q", "--bare", repo.top(), at]);
    repo.run(&["remote", "add", "origin", at]);
    repo.run(&["fetch", "-q", "origin"]);
    origin
}

/// `path` written as an executable script (a hook, a stand-in).
pub(super) fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Whether process `pid` still exists.
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only asks.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The pid a stand-in wrote to `file`, waited for (at most 10 s).
fn pid_in(file: &Path) -> i32 {
    let t = Instant::now();
    loop {
        if let Some(pid) = std::fs::read_to_string(file)
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            return pid;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "no pid in {file:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether `pid` is gone within `within`.
fn gone_within(pid: i32, within: Duration) -> bool {
    let t = Instant::now();
    while alive(pid) {
        if t.elapsed() > within {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

/// G-A1 (the window's half; every action's plan and run are S3-A's `g_a1_*`): F10 on a commit
/// row opens its menu in the dialog look, the status row naming its object; Tag… opens its
/// dialog with the name field typing; Enter runs it on a worker; the refresh shows the new label.
#[test]
fn g_a1_a_tag_from_a_commits_menu() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    g.bar_to("second");
    g.key(Key::F10);
    g.idle(2);
    assert!(g.painted("Tag…") && g.painted("Reset the current branch here…"));
    assert!(
        g.status().starts_with("Menu of commit "),
        "the status row names the menu's object: {}",
        g.status()
    );
    g.click("Tag…");
    g.until_quiet("the tag dialog");
    assert_eq!(g.dialog_id(), Some(ActionId::Tag));
    g.fx.typed("v9");
    g.idle(2);
    assert!(g.shows("v9"), "the name typed: {:#?}", g.texts());
    g.run();
    assert_eq!(g.repo.rev("v9^{commit}"), g.repo.rev("light"));
    assert!(g.on_right("v9"), "the refresh drew the new tag");
    assert!(g.dialog_id().is_none(), "the dialog is gone");
}

/// G-S1 through the window (03 §8.2–§8.3): Reset hard from a commit's menu — its confirmation
/// names the backup refs and the command — runs, writes a group, sends `WorkTreeChanged` (the
/// list re-reads the changed file) and the status row says `… · backup <stamp> · Cmd/Ctrl+Z
/// undoes`; Cmd/Ctrl+Z opens the restore of that group and Enter puts HEAD and the changed file
/// back; Cmd/Ctrl+Z again opens the restore of the undo's own group (redo).
#[test]
fn g_s1_reset_hard_is_backed_up_and_cmd_z_undoes_it() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    let third = g.repo.rev("HEAD");
    let second = g.repo.rev("light");
    let changed = std::fs::read(g.repo.top.join("a.txt")).unwrap();
    g.bar_to("second");
    // Room for the whole confirmation (it scrolls in a 700-point window).
    g.fx.drv.resize(1000.0, 1400.0);
    g.idle(2);
    g.menu("Reset the current branch here…");
    assert_eq!(g.dialog_id(), Some(ActionId::Reset));
    g.click("( ) hard");
    g.until_quiet("the hard reset's reads");
    // The dialog grew with its reads: its area takes the new size over the next passes.
    g.idle(4);
    assert!(
        g.shows("git reset --hard "),
        "the command: {:#?}",
        g.texts()
    );
    assert!(
        g.shows("backup heads/main, worktree"),
        "the backups: {:#?}",
        g.texts()
    );
    g.run();
    assert_eq!(g.repo.rev("HEAD"), second, "reset");
    let at_second = std::fs::read(g.repo.top.join("a.txt")).unwrap();
    assert!(g.backups().contains("/heads/main"), "{}", g.backups());
    let status = g.status();
    let undoes = format!("{}Z undoes", keys::Mods::Command.prefix());
    assert!(
        status.contains(" · backup ") && status.ends_with(&undoes),
        "{status}"
    );
    // WorkTreeChanged: the list shows a.txt's size now.
    let size = std::fs::metadata(g.repo.top.join("a.txt")).unwrap().len();
    g.until("the list re-read", |b| {
        b.panels[0]
            .entries
            .iter()
            .any(|e| e.name == "a.txt" && e.size == size)
    });
    // Undo: the restore dialog of the group this window wrote.
    g.cmd(Key::Z);
    g.until_quiet("the restore's entries");
    assert_eq!(g.dialog_id(), Some(ActionId::Restore));
    g.run();
    assert_eq!(g.repo.rev("HEAD"), third, "undone");
    assert_eq!(std::fs::read(g.repo.top.join("a.txt")).unwrap(), changed);
    // Redo: the restore wrote its own group, the newest now. Its work-tree item resets to HEAD
    // before the branch moves (03 §8.3's order), so the redo lands exactly where the reset did.
    g.cmd(Key::Z);
    g.until_quiet("the redo's entries");
    assert_eq!(g.dialog_id(), Some(ActionId::Restore));
    g.run();
    assert!(!g.painted("failed"), "the result dialog: {:#?}", g.texts());
    assert_eq!(g.repo.rev("HEAD"), second, "redone");
    assert_eq!(std::fs::read(g.repo.top.join("a.txt")).unwrap(), at_second);
    // And undo again: back to the change the reset discarded, exactly.
    g.cmd(Key::Z);
    g.until_quiet("the second undo's entries");
    assert_eq!(g.dialog_id(), Some(ActionId::Restore));
    g.run();
    assert_eq!(g.repo.rev("HEAD"), third, "undone again");
    assert_eq!(std::fs::read(g.repo.top.join("a.txt")).unwrap(), changed);
}

/// G-A2 through the window: a local branch's label — Check out runs at once (03 §8.1: no
/// confirmation); F8 deletes; git refuses a branch with a commit no other branch has, and the
/// second dialog (03 §7.2) asks again, forced; the backup holds the branch.
#[test]
fn g_a2_check_out_at_once_and_f8_deletes_after_the_second_dialog() {
    let git = git_or_skip!();
    let mut g = Gx::new(conflicting(&git));
    g.ready_in_git();
    g.bar_to("on side");
    g.label_to("side");
    g.menu("Check out");
    assert!(g.dialog_id().is_none(), "no confirmation");
    assert_eq!(
        g.repo.run(&["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/side"
    );
    g.bar_to("on main");
    g.label_to("main");
    g.menu("Check out");
    assert_eq!(
        g.repo.run(&["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main"
    );
    g.bar_to("on side");
    g.label_to("side");
    g.f(Key::F8);
    assert_eq!(g.dialog_id(), Some(ActionId::DeleteBranch));
    g.run();
    assert!(
        matches!(
            g.view().draft(),
            Some(Action::DeleteBranch { force: true, .. })
        ),
        "the second dialog: {:?} {:#?}",
        g.view().draft(),
        g.texts()
    );
    g.run();
    assert_eq!(g.repo.run(&["for-each-ref", "refs/heads/side"]), "");
    assert!(g.backups().contains("/heads/side"), "{}", g.backups());
}

/// G-A3 and G-R3's local half through the window: a pull whose fetch's upload-pack waits (a
/// stand-in that writes its pid and sleeps) is stopped by Esc — a network command, at once, no
/// question — within 6 s; nothing of its group is left; HEAD is where it was; the log says so.
#[test]
fn g_a3_g_r3_esc_stops_a_pull_during_its_fetch_and_leaves_head() {
    let git = git_or_skip!();
    let repo = conflicting(&git);
    with_origin(&repo);
    // `side` moves on, so `origin/side` is a label of its own on `on side`.
    repo.run(&["checkout", "-q", "side"]);
    repo.write("d.txt", "more\n");
    repo.commit("more on side");
    repo.run(&["checkout", "-q", "main"]);
    let pidfile = repo.tmp.path().join("upload-pack.pid");
    let pack = format!("echo $$ > '{}'; exec sleep 30 #", pidfile.display());
    repo.run(&["config", "remote.origin.uploadpack", &pack]);
    let head = repo.rev("HEAD");
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("on side");
    g.label_to("origin/side");
    g.menu("Pull into the current branch…");
    assert_eq!(g.dialog_id(), Some(ActionId::Pull));
    g.key(Key::Enter);
    let pid = pid_in(&pidfile);
    assert!(alive(pid));
    assert!(g.status().contains("git fetch") || g.status().contains("Pull"));
    let t = Instant::now();
    g.esc();
    assert!(gone_within(pid, Duration::from_secs(6)), "upload-pack left");
    g.until_quiet("the stop and the refresh");
    assert!(t.elapsed() < Duration::from_secs(6), "{:?}", t.elapsed());
    assert_eq!(g.repo.rev("HEAD"), head, "HEAD where it was");
    let log = g.b().git_runner().unwrap().log().entries();
    assert!(
        log.iter()
            .any(|e| e.line.starts_with("git fetch") && e.status_line().contains("stopped")),
        "{:#?}",
        log.iter().map(|e| e.status_line()).collect::<Vec<_>>()
    );
    let locks: Vec<_> = walk(&g.repo.top.join(".git"))
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "lock"))
        .collect();
    assert!(locks.is_empty(), "{locks:?}");
}

/// Every path under `dir`.
fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        }
        out.push(p);
    }
    out
}

/// G-A4 through the window: F3 on an annotated tag's label shows `cat-file -p` (03 §7.4); F8
/// deletes the tag after its dialog, backed up.
#[test]
fn g_a4_f3_shows_an_annotated_tag_and_f8_deletes_it() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    g.bar_to("Merge branch 'feature'");
    g.label_to("v1.0");
    g.f(Key::F3);
    assert!(
        g.painted("Tag v1.0") && g.shows("version one"),
        "{:#?}",
        g.texts()
    );
    g.esc();
    g.f(Key::F8);
    assert_eq!(g.dialog_id(), Some(ActionId::DeleteTag));
    g.run();
    assert_eq!(g.repo.run(&["for-each-ref", "refs/tags/v1.0"]), "");
    assert!(g.backups().contains("/tags/v1.0"), "{}", g.backups());
}

/// G-A5 and G-S1's stash round trip through the window: F8 on a stash row drops it after its
/// dialog, backed up; Cmd/Ctrl+Z brings it back, again drops it (redo), again brings it back —
/// one entry, not two.
#[test]
fn g_a5_f8_drops_a_stash_and_cmd_z_brings_it_back() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    g.bar_to("On main: saved");
    g.f(Key::F8);
    assert_eq!(g.dialog_id(), Some(ActionId::StashDrop));
    assert!(g.shows("backup stash"), "{:#?}", g.texts());
    g.run();
    assert_eq!(g.repo.run(&["stash", "list"]), "");
    g.cmd(Key::Z);
    g.until_quiet("the restore's entries");
    assert_eq!(g.dialog_id(), Some(ActionId::Restore));
    g.run();
    assert!(g.repo.run(&["stash", "list"]).contains("saved"));
    assert!(
        g.subjects().iter().any(|s| s == "On main: saved"),
        "the row"
    );
    // Redo (the undo's own group): dropped again; undo again: one entry, not two (03 G-S1).
    g.cmd(Key::Z);
    g.until_quiet("the redo's entries");
    g.run();
    assert_eq!(g.repo.run(&["stash", "list"]), "", "redone");
    g.cmd(Key::Z);
    g.until_quiet("the undo's entries");
    g.run();
    assert_eq!(
        g.repo.run(&["stash", "list"]).lines().count(),
        1,
        "one entry"
    );
}

/// G-A6 through the window: Space on a changed file of the uncommitted row stages it (no
/// confirmation, 03 §8.1); F7 there opens Commit; the message typed and Enter commit it.
#[test]
fn g_a6_space_stages_a_file_and_f7_commits_it() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    g.pane_on("a.txt");
    g.key(Key::Space);
    g.until_quiet("the stage");
    assert_eq!(
        g.repo.run(&["diff", "--cached", "--name-only"]).trim(),
        "a.txt"
    );
    assert!(g.slots().contains(&(7, "Commit")), "{:?}", g.slots());
    g.f(Key::F7);
    assert_eq!(g.dialog_id(), Some(ActionId::Commit));
    g.fx.typed("staged a");
    g.idle(2);
    g.run();
    assert_eq!(g.repo.run(&["log", "-1", "--format=%s"]).trim(), "staged a");
    assert_eq!(g.repo.run(&["diff", "--cached", "--name-only"]), "");
}

/// G-A7 and the notes' edit-remote prefill (03 §3.7): Edit a remote… from the repository's menu
/// shows the remote's URL with its password masked — never the secret — and leaving the field
/// alone writes nothing.
#[test]
fn g_a7_edit_a_remote_shows_its_url_masked() {
    let git = git_or_skip!();
    let repo = small(&git);
    let url = "https://ann:s3cret@example.com/x.git";
    repo.run(&["remote", "add", "origin", url]);
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.key(Key::F10);
    g.idle(2);
    g.click("Repository…");
    g.click("Edit a remote…");
    g.until_quiet("the remotes' URLs");
    assert_eq!(g.dialog_id(), Some(ActionId::EditRemote));
    assert!(
        g.shows("https://ann:***@example.com/x.git"),
        "{:#?}",
        g.texts()
    );
    assert!(!g.shows("s3cret"), "the secret is never in a dialog");
    g.esc();
    assert_eq!(g.repo.run(&["remote", "get-url", "origin"]).trim(), url);
}

/// G-A8 through the window: a cherry-pick that would write over an ignored file is refused by
/// its dialog, naming the file (03 §8.6); Enter runs nothing.
#[test]
fn g_a8_an_ignored_file_in_the_way_refuses_the_pick() {
    let git = git_or_skip!();
    let repo = Repo::new(&git);
    repo.write(".gitignore", "*.log\n");
    repo.commit("base");
    repo.run(&["checkout", "-q", "-b", "side"]);
    repo.write("x.log", "theirs\n");
    repo.run(&["add", "-f", "x.log"]);
    repo.run(&["commit", "-q", "-m", "adds a log"]);
    repo.run(&["checkout", "-q", "main"]);
    repo.write("x.log", "mine\n");
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("adds a log");
    g.f(Key::F5);
    assert_eq!(g.dialog_id(), Some(ActionId::Pick));
    assert!(g.shows("x.log"), "the refusal names it: {:#?}", g.texts());
    g.key(Key::Enter);
    g.idle(3);
    assert_eq!(g.dialog_id(), Some(ActionId::Pick), "Enter ran nothing");
    assert_eq!(
        std::fs::read_to_string(g.repo.top.join("x.log")).unwrap(),
        "mine\n"
    );
    g.esc();
}

/// G-A9 through the window: with `fetch.prune` and `fetch.pruneTags` in the config, Fetch all
/// (Cmd/Ctrl+Shift+R, no confirmation) prunes nothing — its line says `--no-prune
/// --no-prune-tags`.
#[test]
fn g_a9_fetch_all_never_prunes_behind_the_dialogs_back() {
    let git = git_or_skip!();
    let repo = conflicting(&git);
    let origin = with_origin(&repo);
    repo.run_in(&origin, &["branch", "-D", "side"]);
    repo.run(&["config", "fetch.prune", "true"]);
    repo.run(&["config", "fetch.pruneTags", "true"]);
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.cmd_shift(Key::R);
    g.until_quiet("the fetch");
    assert!(g.dialog_id().is_none(), "no confirmation");
    assert_ne!(
        g.repo.run(&["for-each-ref", "refs/remotes/origin/side"]),
        ""
    );
    let log = g.b().git_runner().unwrap().log().entries();
    let fetch = log
        .iter()
        .find(|e| e.line.starts_with("git fetch"))
        .expect("the fetch's entry");
    assert!(
        fetch.line.contains("--no-prune --no-prune-tags"),
        "{}",
        fetch.line
    );
}

/// G-S3 through the window: a merge that conflicts — its result dialog says the step failed —
/// then the banner `MERGE of side · 1 conflict` with its Continue and Abort; Skip, which a merge
/// does not offer, says so on the status row; Abort through its dialog ends it.
#[test]
fn g_s3_a_merge_conflict_shows_the_banner_and_abort_ends_it() {
    let git = git_or_skip!();
    let mut g = Gx::new(conflicting(&git));
    g.ready_in_git();
    g.bar_to("on side");
    g.label_to("side");
    g.menu("Merge into the current branch…");
    assert_eq!(g.dialog_id(), Some(ActionId::Merge));
    g.run();
    assert!(g.painted("failed"), "the result dialog: {:#?}", g.texts());
    g.key(Key::Enter);
    g.idle(2);
    assert!(g.shows("MERGE of side · 1 conflict"), "{:#?}", g.texts());
    assert!(g.painted("[ Continue ]") && g.painted("[ Abort ]") && !g.painted("[ Skip ]"));
    g.cmd_shift(Key::S);
    assert!(
        g.status()
            .starts_with("Skip: not offered while MERGE of side"),
        "{}",
        g.status()
    );
    g.cmd_shift(Key::A);
    g.until_quiet("the abort's dialog");
    assert_eq!(g.dialog_id(), Some(ActionId::Abort));
    g.run();
    assert!(!g.shows("MERGE of side"), "the banner is gone");
    assert!(!g.repo.top.join(".git/MERGE_HEAD").exists());
}

/// G-S5 through the window: `index.lock` held by another process — Stage all (no confirmation)
/// fails with git's message in the result dialog, and the lock is still there.
#[test]
fn g_s5_a_held_lock_fails_with_gits_message_and_stays() {
    let git = git_or_skip!();
    let repo = small(&git);
    let lock = repo.top.join(".git/index.lock");
    std::fs::write(&lock, "").unwrap();
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.menu("Stage all");
    assert!(
        // The lock's path wraps by cells, anywhere (10 §6): its rows read as one text.
        g.painted("failed") && g.texts().concat().contains("index.lock"),
        "{:#?}",
        g.texts()
    );
    assert!(lock.exists(), "never removed");
}

/// G-S8 through the window: bisect started in a terminal shows on its own line after the load
/// (the probe), the status row names its key; Continue, which bisect does not offer, says so;
/// Cmd/Ctrl+Shift+B's Reset through its dialog ends it.
#[test]
fn g_s8_bisect_is_its_own_line_and_reset_ends_it() {
    let git = git_or_skip!();
    let repo = small(&git);
    repo.run(&["bisect", "start"]);
    let mut g = Gx::new(repo);
    g.ready_in_git();
    assert!(g.painted("BISECT · Reset"), "{:#?}", g.texts());
    let reset = format!("{}Shift+B Reset", keys::Mods::Command.prefix());
    assert!(g.status().contains(&reset), "{}", g.status());
    g.cmd_shift(Key::C);
    assert!(
        g.status().starts_with("Continue: not offered while BISECT"),
        "{}",
        g.status()
    );
    g.cmd_shift(Key::B);
    g.until_quiet("the reset's dialog");
    assert_eq!(g.dialog_id(), Some(ActionId::BisectReset));
    g.run();
    assert!(!g.painted("BISECT · Reset"));
    assert!(!g.repo.top.join(".git/BISECT_LOG").exists());
}

/// G-R5 through the window: a `pre-commit` hook that waits for ever — the status row says what
/// git may be waiting for once it is silent (the 10 s shortened); Esc on the write asks "Stop
/// git?"; Yes stops it within 6 s; nothing of its group is left, no `*.lock`, the commit not
/// made, its change still staged.
#[test]
fn g_r5_a_waiting_hook_is_said_and_stopped_after_the_question() {
    let git = git_or_skip!();
    let repo = small(&git);
    let pidfile = repo.tmp.path().join("hook.pid");
    script(
        &repo.top.join(".git/hooks/pre-commit"),
        &format!("echo $$ > '{}'\nexec sleep 30", pidfile.display()),
    );
    repo.run(&["add", "a.txt"]);
    let head = repo.rev("HEAD");
    let mut g = Gx::new(repo);
    g.fx.b.git.quiet_after = Duration::from_millis(300);
    g.fx.b.git.stop_after = Duration::from_millis(300);
    g.ready_in_git();
    g.f(Key::F7);
    assert_eq!(g.dialog_id(), Some(ActionId::Commit));
    g.fx.typed("held");
    g.idle(2);
    g.key(Key::Enter);
    let pid = pid_in(&pidfile);
    g.until("the status row's warning", |b| {
        b.status_line()
            .text
            .contains("a hook, a signer or a credential helper may be waiting for you")
    });
    g.esc();
    assert!(g.painted("Stop git?"), "{:#?}", g.texts());
    let t = Instant::now();
    g.key(Key::Enter);
    assert!(gone_within(pid, Duration::from_secs(6)), "the hook left");
    g.until_quiet("the stop");
    assert!(t.elapsed() < Duration::from_secs(6), "{:?}", t.elapsed());
    assert_eq!(g.repo.rev("HEAD"), head, "no commit");
    assert_eq!(
        g.repo.run(&["diff", "--cached", "--name-only"]).trim(),
        "a.txt"
    );
    assert!(!g.repo.top.join(".git/index.lock").exists());
}

/// 02 §6's write lock through the window: a second action while a write runs waits in line,
/// and the status row says so; it runs once the first has ended.
#[test]
fn a_second_write_waits_for_the_first() {
    let git = git_or_skip!();
    let repo = small(&git);
    let pidfile = repo.tmp.path().join("hook.pid");
    script(
        &repo.top.join(".git/hooks/pre-commit"),
        &format!("echo $$ > '{}'\nexec sleep 30", pidfile.display()),
    );
    repo.run(&["add", "a.txt"]);
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.f(Key::F7);
    g.fx.typed("held");
    g.idle(2);
    g.key(Key::Enter);
    pid_in(&pidfile);
    let ctx = g.fx.drv.ctx.clone();
    let i = g.b().git.panel(g.repo.top()).unwrap();
    let stage = Action::Stage {
        paths: vec!["u.txt".into()],
    };
    g.fx.b.start_action(&ctx, i, stage);
    g.idle(3);
    assert_eq!(g.panel().running.len(), 2);
    assert!(
        g.status().contains("waits for the other write"),
        "{}",
        g.status()
    );
    assert_eq!(
        g.repo.run(&["diff", "--cached", "--name-only"]).trim(),
        "a.txt"
    );
    // The first stopped: the second runs.
    g.panel().running[0].handle.stop();
    g.until_quiet("both ended");
    assert!(g
        .repo
        .run(&["diff", "--cached", "--name-only"])
        .contains("u.txt"));
}

/// "Stop git?" goes with the write it asks about (X4-dialogs2, round 1): Esc over a commit A whose
/// hook waits asks; A ends without its answer (stopped by its handle) and a checkout B, waiting in
/// line behind it, starts (its hook waits too) — the question is gone, and the Enter that would
/// have been its Yes stops nothing.
#[test]
fn the_stop_question_goes_with_its_write() {
    let git = git_or_skip!();
    let repo = small(&git);
    let (a_pid, b_pid) = (repo.tmp.path().join("a.pid"), repo.tmp.path().join("b.pid"));
    for (hook, pid) in [("pre-commit", &a_pid), ("post-checkout", &b_pid)] {
        let body = format!("echo $$ > '{}'\nexec sleep 30", pid.display());
        script(&repo.top.join(".git/hooks").join(hook), &body);
    }
    repo.run(&["add", "a.txt"]);
    repo.run(&["branch", "side"]);
    let mut g = Gx::new(repo);
    g.fx.b.git.quiet_after = Duration::from_millis(300);
    g.fx.b.git.stop_after = Duration::from_millis(300);
    g.ready_in_git();
    g.f(Key::F7);
    g.fx.typed("held");
    g.idle(2);
    g.key(Key::Enter);
    pid_in(&a_pid);
    let ctx = g.fx.drv.ctx.clone();
    let i = g.b().git.panel(g.repo.top()).unwrap();
    let side = Action::CheckoutBranch {
        branch: "side".into(),
    };
    g.fx.b.start_action(&ctx, i, side);
    g.until("the status row's warning", |b| {
        b.status_line()
            .text
            .contains("a hook, a signer or a credential helper may be waiting for you")
    });
    g.esc();
    assert!(g.painted("Stop git?"), "{:#?}", g.texts());
    // A ends; B runs.
    g.panel().running[0].handle.stop();
    let b = pid_in(&b_pid);
    g.until("A's end", |b| b.git.panels[0].running.len() == 1);
    g.idle(3);
    // A's result, if its end shows one, first.
    if g.painted("[ OK ]") {
        g.esc();
    }
    assert!(!g.painted("Stop git?"), "asked over B: {:#?}", g.texts());
    g.key(Key::Enter);
    g.idle(3);
    assert!(!gone_within(b, Duration::from_millis(500)), "B was stopped");
    assert_eq!(g.panel().running.len(), 1, "B still runs");
    g.panel().running[0].handle.stop();
    assert!(gone_within(b, Duration::from_secs(6)));
    g.until_quiet("B's stop");
}

/// Two commits of the same words, A behind a hook that waits and B in line behind it (the
/// fix-check of X4-dialogs2): A from its dialog, B from the same dialog opened while A runs; then
/// Esc's question over A. A's pid (the hook writes each one's in order).
fn two_commits(g: &mut Gx, pids: &Path) -> i32 {
    g.f(Key::F7);
    g.fx.typed("first");
    g.idle(2);
    g.key(Key::Enter);
    let a = pid_in(pids);
    // B: Commit's dialog again (its prefill read would wait behind A's write lock: opened with
    // the read the integration gives an empty message), run from it, in line.
    let opened = (g.panel_mut().view).open_action(
        ActionId::Commit,
        git_panel::Target::Uncommitted,
        git_panel::Prefill::default(),
    );
    assert!(opened.is_empty(), "{opened:?}");
    g.idle(3);
    assert_eq!(g.dialog_id(), Some(ActionId::Commit));
    g.fx.typed("second");
    g.idle(2);
    g.key(Key::Enter);
    g.until("B in line", |b| b.git.panels[0].running.len() == 2);
    let titles: Vec<&str> = (g.panel().running.iter())
        .map(|r| r.title.as_str())
        .collect();
    assert_eq!(titles[0], titles[1], "the same words");
    g.until("the status row's warning", |b| {
        b.status_line()
            .text
            .contains("a hook, a signer or a credential helper may be waiting for you")
    });
    g.esc();
    assert!(g.painted("Stop git?"), "{:#?}", g.texts());
    a
}

/// The second pid the hook wrote: B's, once it runs.
fn second_pid(pids: &Path) -> i32 {
    let t = Instant::now();
    loop {
        let all = std::fs::read_to_string(pids).unwrap_or_default();
        if let Some(pid) = all.lines().nth(1).and_then(|l| l.trim().parse().ok()) {
            return pid;
        }
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "B never ran: {all:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A repository whose pre-commit hook writes its pid to `pids` and waits; `a.txt` staged.
fn hanging_commits(git: &Path) -> (Gx, std::path::PathBuf) {
    let repo = small(git);
    let pids = repo.tmp.path().join("pids");
    let body = format!("echo $$ >> '{}'\nexec sleep 30", pids.display());
    script(&repo.top.join(".git/hooks/pre-commit"), &body);
    repo.write("a.txt", "changed\n");
    repo.run(&["add", "a.txt"]);
    let mut g = Gx::new(repo);
    g.fx.b.git.quiet_after = Duration::from_millis(300);
    g.fx.b.git.stop_after = Duration::from_millis(300);
    g.ready_in_git();
    (g, pids)
}

/// "Stop git?" asks about one write, not about its words (the fix-check): two commits of the same
/// title, A running and B in line. A ends without the question's answer and B starts: the
/// question goes with A, and the Enter that would have been its Yes stops nothing.
#[test]
fn the_stop_question_over_a_commit_goes_with_it_not_its_twin() {
    let git = git_or_skip!();
    let (mut g, pids) = hanging_commits(&git);
    two_commits(&mut g, &pids);
    g.panel().running[0].handle.stop();
    let b = second_pid(&pids);
    g.until("A's end", |b| b.git.panels[0].running.len() == 1);
    g.idle(3);
    if g.painted("[ OK ]") {
        g.esc();
    }
    assert!(!g.painted("Stop git?"), "asked over B: {:#?}", g.texts());
    g.key(Key::Enter);
    g.idle(3);
    assert!(!gone_within(b, Duration::from_millis(500)), "B was stopped");
    g.panel().running[0].handle.stop();
    assert!(gone_within(b, Duration::from_secs(6)));
    g.until_quiet("B's stop");
}

/// Yes stops the write it asked about alone (the fix-check, 03 §3.4: "Yes stops it"): A stops,
/// and B, in line behind it with the same words, runs.
#[test]
fn yes_stops_the_write_it_asked_about_alone() {
    let git = git_or_skip!();
    let (mut g, pids) = hanging_commits(&git);
    let a = two_commits(&mut g, &pids);
    g.key(Key::Enter);
    assert!(gone_within(a, Duration::from_secs(6)), "A stopped");
    let b = second_pid(&pids);
    g.until("A's end", |b| b.git.panels[0].running.len() == 1);
    assert!(
        !gone_within(b, Duration::from_millis(500)),
        "B was stopped too"
    );
    g.panel().running[0].handle.stop();
    assert!(gone_within(b, Duration::from_secs(6)));
    g.until_quiet("B's stop");
}

/// G-K1: each slot of 03 §9.1 does its row's action on each kind of row, and the F-key row
/// shows the slots that act there — F7 says Commit on the uncommitted row; a commit's F2 and F8
/// (reword, drop: the history edits) are empty and say so.
#[test]
fn g_k1_each_slot_acts_on_its_kind_of_row() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    let slots = |g: &Gx| -> Vec<u8> { g.slots().iter().map(|s| s.0).collect() };
    // The uncommitted row.
    assert!(g.slots().contains(&(7, "Commit")), "{:?}", g.slots());
    assert_eq!(slots(&g), [1, 3, 4, 7, 8, 9, 10]);
    g.f(Key::F8);
    assert_eq!(g.dialog_id(), Some(ActionId::ResetChanges));
    g.esc();
    // A commit row.
    g.bar_to("second");
    assert_eq!(slots(&g), [1, 3, 4, 5, 6, 7, 9, 10]);
    for (key, id) in [
        (Key::F5, ActionId::Pick),
        (Key::F6, ActionId::Rebase),
        (Key::F7, ActionId::Branch),
    ] {
        g.f(key);
        assert_eq!(g.dialog_id(), Some(id), "{key:?}");
        g.esc();
    }
    // F2 and F8 on a commit are its reword and drop (S4); `second` is below the merge, so the
    // history row offers neither there and the status row says why (S4-C's `g_k1_*` in `s4.rs`
    // has them on a linear commit).
    g.f(Key::F2);
    assert!(
        g.status().starts_with("Reword: not offered on ") && g.status().contains("a merge"),
        "{}",
        g.status()
    );
    g.f(Key::F8);
    assert!(
        g.status().starts_with("Drop: not offered on "),
        "{}",
        g.status()
    );
    assert!(g.dialog_id().is_none() && !g.view().dialog_up());
    // A stash row.
    g.bar_to("On main: saved");
    for (key, id) in [
        (Key::F5, ActionId::StashApply),
        (Key::F7, ActionId::StashBranch),
        (Key::F8, ActionId::StashDrop),
    ] {
        g.f(key);
        assert_eq!(g.dialog_id(), Some(id), "{key:?}");
        g.esc();
    }
    // A local branch's label.
    g.bar_to("feature work");
    g.label_to("feature");
    assert!(g.slots().contains(&(2, "Reword")), "{:?}", g.slots());
    for (key, id) in [
        (Key::F2, ActionId::Rename),
        (Key::F5, ActionId::Pick),
        (Key::F6, ActionId::Rebase),
        (Key::F8, ActionId::DeleteBranch),
    ] {
        g.f(key);
        assert_eq!(g.dialog_id(), Some(id), "{key:?}");
        g.esc();
    }
}

/// G-K2: Esc walks 03 §9.2's chain — the menu, a dialog, the log's whole entry, the log, the
/// Backups list, the details pane's keyboard, the details pane — then unpins.
#[test]
fn g_k2_esc_walks_the_chain_to_the_unpin() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    g.cmd(Key::G);
    g.settle();
    g.tab_into_git();
    g.until_quiet("the snapshot");
    assert!(g.b().git.pin.is_some());
    g.key(Key::F10);
    g.idle(2);
    assert!(g.view().dialog_up(), "the menu");
    g.esc();
    assert!(!g.view().dialog_up());
    g.f(Key::F7);
    assert_eq!(g.dialog_id(), Some(ActionId::Commit));
    g.esc();
    assert!(g.dialog_id().is_none());
    g.cmd(Key::L);
    g.idle(2);
    assert!(g.view().log_newest().is_some(), "the log");
    g.key(Key::Enter);
    g.idle(2);
    assert!(g.view().dialog_up(), "an entry whole");
    g.esc();
    assert!(!g.view().dialog_up() && g.view().log_newest().is_some());
    g.esc();
    assert!(g.view().log_newest().is_none(), "the log closed");
    g.cmd(Key::B);
    g.until_quiet("the Backups list");
    assert!(g.view().backups().is_some());
    g.esc();
    assert!(g.view().backups().is_none());
    g.key(Key::Enter);
    g.until_quiet("the details");
    assert!(g.view().details_open());
    g.esc();
    assert!(g.view().details_open(), "the pane's keyboard went");
    g.esc();
    assert!(!g.view().details_open());
    assert!(g.b().git.pin.is_some());
    g.esc();
    assert!(g.b().git.pin.is_none(), "unpinned");
    assert!(g.b().git_focused().is_none());
}

/// G-W1's backups and log through the window: Cmd/Ctrl+B lists the group a drop wrote, under
/// its header; Enter shows it in the graph; F8 deletes the group through its dialog; Cmd/Ctrl+L
/// lists the commands, newest first, and Enter shows one whole.
#[test]
fn g_w1_the_backups_list_and_the_command_log() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    g.bar_to("On main: saved");
    g.f(Key::F8);
    g.run();
    g.cmd(Key::B);
    g.until_quiet("the Backups list");
    assert!(g.shows(git_panel_backups_header()), "{:#?}", g.texts());
    assert!(g.shows("stash"));
    g.key(Key::Enter);
    g.until_quiet("Show");
    assert!(g.view().backups().is_none(), "Show leaves the list");
    assert!(g.status().starts_with("showing stash "), "{}", g.status());
    g.cmd(Key::B);
    g.until_quiet("the Backups list");
    g.f(Key::F8);
    assert_eq!(g.dialog_id(), Some(ActionId::DeleteBackup));
    assert!(
        g.shows("Deleting a backup is the one removal"),
        "{:#?}",
        g.texts()
    );
    g.run();
    assert_eq!(g.backups(), "");
    g.esc();
    g.cmd(Key::L);
    g.idle(2);
    assert!(g.shows("git stash drop"), "{:#?}", g.texts());
    g.key(Key::Enter);
    g.idle(2);
    assert!(g.view().dialog_up());
    g.esc();
    g.esc();
    assert!(g.view().log_newest().is_none());
}

/// The Backups list's header, as S3-B's list draws it.
fn git_panel_backups_header() -> &'static str {
    "Backups under refs/sopen-backup/"
}

/// The notes' item 4 (S3-A's `Outcome::warning`): a run whose backup could not be completed
/// (a `reference-transaction` hook refuses its `after` record) says `backup <stamp> incomplete`
/// in its result dialog and on the status row in the error colour, in place of the undo text.
#[test]
fn an_incomplete_backup_is_said_in_place_of_the_undo_text() {
    let git = git_or_skip!();
    let repo = small(&git);
    script(
        &repo.top.join(".git/hooks/reference-transaction"),
        "[ \"$1\" = prepared ] || exit 0\nif grep -q '/after$'; then exit 1; fi\nexit 0",
    );
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("On main: saved");
    g.f(Key::F8);
    g.run();
    let status = g.fx.b.status_line();
    assert!(status.text.contains(" incomplete: "), "{}", status.text);
    assert!(status.error, "the error colour");
    assert!(!status.text.contains("undoes"), "{}", status.text);
    assert!(
        g.shows(" incomplete: "),
        "the result dialog: {:#?}",
        g.texts()
    );
}

/// A repository with one commit `base` on `main`, pushed to a bare `origin` beside it.
fn pushed(git: &Path) -> (Repo, std::path::PathBuf) {
    let repo = Repo::new(git);
    repo.write("README", "base\n");
    repo.commit("base");
    let origin = with_origin(&repo);
    repo.run(&["branch", "-q", "--set-upstream-to=origin/main", "main"]);
    (repo, origin)
}

/// G-S2 through the window: Push with "Overwrite the remote branch, with lease" after a rewrite,
/// while another clone pushes between the backup's fetch and the push (a `reference-transaction`
/// hook, when the backup commits) — refused with git's `stale info` in the result dialog, the
/// remote unchanged.
#[test]
fn g_s2_an_overwrite_push_after_another_push_is_refused_stale() {
    let git = git_or_skip!();
    let (repo, origin) = pushed(&git);
    let other = repo.tmp.path().join("other");
    repo.run(&[
        "clone",
        "-q",
        origin.to_str().unwrap(),
        other.to_str().unwrap(),
    ]);
    repo.run(&["commit", "-q", "--amend", "-m", "rewritten"]);
    let once = repo.tmp.path().join("once");
    script(
        &repo.top.join(".git/hooks/reference-transaction"),
        &format!(
            "[ \"$1\" = committed ] || exit 0\n\
             grep -q sopen-backup || exit 0\n\
             [ -e '{once}' ] && exit 0\n\
             touch '{once}'\n\
             unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX GIT_COMMON_DIR GIT_OBJECT_DIRECTORY\n\
             cd '{other}' && echo more >> README && git commit -q -am more && git push -q origin main >/dev/null 2>&1",
            once = once.display(),
            other = other.display(),
        ),
    );
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("rewritten");
    g.label_to("►main");
    g.menu("Push…");
    assert_eq!(g.dialog_id(), Some(ActionId::Push));
    g.click("[ ] Overwrite the remote branch, with lease");
    g.until_quiet("the overwrite's reads");
    assert!(
        matches!(
            g.view().draft(),
            Some(Action::Push {
                overwrite: true,
                ..
            })
        ),
        "{:?}",
        g.view().draft()
    );
    g.run();
    assert!(once.exists(), "the other clone pushed");
    assert!(g.shows("stale info"), "{:#?}", g.texts());
    let moved = g.repo.run_in(&other, &["rev-parse", "HEAD"]);
    let remote = g.repo.run_in(&origin, &["rev-parse", "refs/heads/main"]);
    assert_eq!(remote, moved, "the remote unchanged");
}

/// G-S7 through the window: a remote with two push URLs that disagree — the overwrite push is
/// refused at Run, naming both, and nothing is pushed.
#[test]
fn g_s7_two_push_urls_that_disagree_refuse_the_overwrite() {
    let git = git_or_skip!();
    let (repo, _origin) = pushed(&git);
    let one = repo.tmp.path().join("one.git");
    let two = repo.tmp.path().join("two.git");
    for bare in [&one, &two] {
        repo.run(&["clone", "-q", "--bare", repo.top(), bare.to_str().unwrap()]);
    }
    repo.write("only", "on two\n");
    repo.commit("only on two");
    repo.run(&["push", "-q", two.to_str().unwrap(), "main"]);
    repo.run(&["reset", "-q", "--hard", "HEAD~1"]);
    repo.run(&["commit", "-q", "--amend", "-m", "rewritten"]);
    for bare in [&one, &two] {
        repo.run(&[
            "remote",
            "set-url",
            "--add",
            "--push",
            "origin",
            bare.to_str().unwrap(),
        ]);
    }
    let before = [&one, &two].map(|b| repo.run_in(b, &["rev-parse", "refs/heads/main"]));
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("rewritten");
    g.label_to("►main");
    g.menu("Push…");
    g.click("[ ] Overwrite the remote branch, with lease");
    g.until_quiet("the overwrite's reads");
    g.run();
    assert!(g.painted("failed"), "{:#?}", g.texts());
    assert!(
        g.shows(one.to_str().unwrap()) && g.shows(two.to_str().unwrap()),
        "both named: {:#?}",
        g.texts()
    );
    let after = [&one, &two].map(|b| g.repo.run_in(b, &["rev-parse", "refs/heads/main"]));
    assert_eq!(after, before, "nothing pushed");
}

/// G-S6 through the window: Take theirs over a hand-edited conflicted file (from the pane's
/// file menu) writes a `files` backup first; Cmd/Ctrl+Z's restore lists the file as occupied —
/// left unless "replace" is chosen — and with it the hand-made bytes (CRLF) come back exactly,
/// the file still unmerged.
#[test]
fn g_s6_take_theirs_then_restore_with_replace() {
    let git = git_or_skip!();
    let repo = conflicting(&git);
    let out = std::process::Command::new(&repo.git)
        .current_dir(&repo.top)
        .envs(fixture::env(&repo.home()))
        .args(["merge", "side"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "a conflict");
    let hand = b"hand made\r\nby the owner\r\n";
    std::fs::write(repo.top.join("c.txt"), hand).unwrap();
    let mut g = Gx::new(repo);
    g.ready_in_git();
    assert!(g.shows("MERGE of side · 1 conflict"), "{:#?}", g.texts());
    g.pane_on("c.txt");
    g.menu("Take theirs…");
    assert_eq!(g.dialog_id(), Some(ActionId::Theirs));
    g.run();
    assert_eq!(std::fs::read(g.repo.top.join("c.txt")).unwrap(), b"side\n");
    assert!(g.backups().contains("/files"), "{}", g.backups());
    g.esc();
    g.cmd(Key::Z);
    g.until_quiet("the restore's entries");
    assert_eq!(g.dialog_id(), Some(ActionId::Restore));
    assert!(
        g.shows("occupied: left as it is unless replace is chosen"),
        "{:#?}",
        g.texts()
    );
    g.click("[ ] replace");
    g.until_quiet("the replace's reads");
    g.run();
    assert_eq!(std::fs::read(g.repo.top.join("c.txt")).unwrap(), hand);
    assert_ne!(
        g.repo.run(&["ls-files", "-u", "c.txt"]),
        "",
        "still unmerged"
    );
}

/// How `SOPEN_STATE dialog=<id>` ends for an id on the G-S4 fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opens {
    /// Its dialog, planned with every read it needs.
    Dialog,
    /// A view or the clipboard: no dialog, nothing refused.
    View,
    /// No confirmation (03 §8.1): it ran at once.
    Runs,
    /// Not on any object of this fixture (a conflict's steps, a branch with no local twin), or
    /// only on the Backups list (restore, show, delete): the status row says so.
    NotHere,
    /// A banner step with nothing in progress: the status row says so.
    NothingInProgress,
}

/// What each id does on the G-S4 fixture, and the row it is asked on (`select=`, `label=`).
fn g_s4_case(id: ActionId) -> (Opens, Option<&'static str>, Option<usize>) {
    use ActionId as A;
    use Opens::*;
    let commit = Some("light");
    let local = Some("feature");
    let remote = Some("origin/remote-only");
    let tag = Some("v1.0");
    let stash = Some("stash@{0}");
    match id {
        A::Tag
        | A::Branch
        | A::MoveBranch
        | A::Checkout
        | A::Pick
        | A::Revert
        | A::Merge
        | A::Rebase
        | A::Reset
        | A::Archive
        | A::MoveTag => (Dialog, commit, None),
        A::CopyHash | A::CopySubject => (View, commit, None),
        // Offered only with exactly one other row marked (03 §7.1).
        A::Compare => (NotHere, commit, None),
        A::Rename | A::DeleteBranch | A::Push => (Dialog, local, Some(1)),
        A::OnlyBranch | A::AllBranches | A::CopyName => (View, local, Some(1)),
        A::CheckoutBranch => (Runs, local, Some(1)),
        A::CheckoutRemote | A::DeleteRemoteBranch | A::Pull => (Dialog, remote, Some(1)),
        A::FetchInto => (NotHere, remote, Some(1)),
        A::DeleteTag | A::PushTag => (Dialog, tag, Some(1)),
        A::TagDetails => (View, tag, Some(1)),
        A::StashApply | A::StashPop | A::StashBranch | A::StashDrop => (Dialog, stash, None),
        A::Commit | A::Amend | A::Stash | A::ResetChanges | A::Clean | A::Discard => {
            (Dialog, None, None)
        }
        A::Stage | A::Unstage => (Runs, None, None),
        A::Editor => (View, None, None),
        A::Ours | A::Theirs | A::Resolved | A::RedoMerge => (NotHere, None, None),
        A::Fetch
        | A::FetchRemote
        | A::PruneRemote
        | A::AddRemote
        | A::EditRemote
        | A::DeleteRemote
        | A::User
        | A::UserRemove => (Dialog, None, None),
        A::RemoteVisibility
        | A::Refresh
        | A::Find
        | A::Search
        | A::Filters
        | A::Log
        | A::Backups
        | A::Undo => (View, None, None),
        A::Restore | A::ShowBackup | A::DeleteBackup => (NotHere, None, None),
        A::Continue | A::Skip | A::Abort | A::BisectReset => (NothingInProgress, None, None),
    }
}

/// G-S4 through `SOPEN_STATE` (02 §9): every id of 03 §7 (`ActionId::ALL`), asked with
/// `dialog=<id>` on its kind of row (`select=`, `label=`), ends as its kind of id should: its
/// dialog, planned with every read it needs (the prefilled ones included); a view; a run at once;
/// or the status row saying it is not offered here / nothing is in progress. The ids that run at
/// once go last (they change the repository). An unknown id is refused before the window opens.
#[test]
fn g_s4_dialog_opens_every_actions_dialog() {
    let git = git_or_skip!();
    let repo = small(&git);
    let origin = with_origin(&repo);
    let first = repo.rev("main~3");
    repo.run_in(&origin, &["branch", "remote-only", &first]);
    repo.run(&["fetch", "-q", "origin"]);
    let mut g = Gx::new(repo);
    g.fx.b.git.script = GitScript {
        pin: true,
        ..GitScript::default()
    };
    g.wait_for_the_panel();
    g.until_quiet("the pin");
    let mut ids: Vec<ActionId> = ActionId::ALL.to_vec();
    ids.sort_by_key(|&id| g_s4_case(id).0 == Opens::Runs);
    for id in ids {
        let (opens, select, label) = g_s4_case(id);
        let runs_before = g.b().git_runner().unwrap().log().entries().len();
        g.fx.b.git.script = GitScript {
            select: select.map(String::from),
            label,
            dialog: Some(crate::gui::DialogId::Action(id)),
            ..GitScript::default()
        };
        g.idle(2);
        g.until("the script", |b| b.git.script.done());
        g.until_quiet(id.as_str());
        let said = g.view().status_line();
        let what = format!(
            "{} ({opens:?}): {} {:?}",
            id.as_str(),
            said.text,
            g.dialog_id()
        );
        match opens {
            Opens::Dialog => {
                assert_eq!(g.dialog_id(), Some(id), "{what}");
                assert!(!g.shows("not read yet"), "{what}: its reads came");
            }
            Opens::View => {
                assert!(g.dialog_id().is_none(), "{what}");
                assert!(!said.text.contains("not offered"), "{what}");
            }
            Opens::Runs => {
                assert!(g.dialog_id().is_none(), "{what}");
                let runs = g.b().git_runner().unwrap().log().entries().len();
                assert!(runs > runs_before, "{what}: it ran");
            }
            Opens::NotHere => {
                assert!(g.dialog_id().is_none(), "{what}");
                assert!(said.text.contains("not offered"), "{what}");
            }
            Opens::NothingInProgress => {
                assert!(said.text.contains("nothing is in progress"), "{what}");
            }
        }
        // Whatever it opened closes (never an Esc with nothing open: that would unpin).
        for _ in 0..3 {
            let v = g.view();
            if v.dialog_up() || v.log_newest().is_some() || v.backups().is_some() {
                g.esc();
            }
        }
        g.until_quiet("closed");
        assert!(g.b().git.pin.is_some(), "{what}: still pinned");
    }
    let state = crate::gui::State::parse("git=pin;select=light;dialog=reset").unwrap();
    assert_eq!(
        state.dialog,
        Some(crate::gui::DialogId::Action(ActionId::Reset))
    );
    assert!(crate::gui::State::parse("dialog=nonsense").is_err());
}

/// The coordinator's rule for reads: a read whose command fails is never recorded — the plan
/// stays refused with `not read yet: …` and the dialog says so, the status row says why, and the
/// read is not asked again for that snapshot (no loop). Here the hard reset's `ls-tree` fails (a
/// stand-in git that fails that one read).
#[test]
fn a_failing_read_leaves_the_plan_refused() {
    let git = git_or_skip!();
    let mut repo = small(&git);
    let log = repo.tmp.path().join("ls-tree.log");
    let wrapper = repo.tmp.path().join("git-failing");
    script(
        &wrapper,
        &format!(
            "case \" $* \" in *\" ls-tree -r -z --name-only --full-tree \"*) echo x >> '{}'; echo 'fatal: a read that fails' >&2; exit 128;; esac\nexec '{}' \"$@\"",
            log.display(),
            git.display()
        ),
    );
    repo.git = wrapper;
    let head = repo.rev("HEAD");
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("second");
    g.fx.drv.resize(1000.0, 1400.0);
    g.idle(2);
    g.menu("Reset the current branch here…");
    g.click("( ) hard");
    g.until_quiet("the reads, one failing");
    g.idle(4);
    assert!(
        g.shows("not read yet: git ls-tree -r -z --name-only --full-tree"),
        "the dialog says so: {:#?}",
        g.texts()
    );
    assert!(
        g.status().contains("a read for the dialog failed")
            && g.status().contains("a read that fails"),
        "{}",
        g.status()
    );
    g.key(Key::Enter);
    g.idle(6);
    assert_eq!(g.dialog_id(), Some(ActionId::Reset), "Enter ran nothing");
    assert_eq!(g.repo.rev("HEAD"), head);
    let tries = std::fs::read_to_string(&log).unwrap().lines().count();
    assert_eq!(tries, 1, "asked once for this snapshot");
}

/// The coordinator's rule for the banner's steps: Continue goes through the same `needs` /
/// `inspect` loop before `plan` as every action (its dialog is `ActionDialog::confirm`'s, its
/// run the plan the dialog showed), never steps built directly. S4-A's plan reads
/// `core.commentChar` and refuses a rebase's Continue under anything but `#` (03 §7.8): with
/// `p`, the dialog says so and Enter runs nothing. On a base whose plan reads nothing for it
/// (S3-A's, before S4-A), the read half is not there: the dialog's plan is checked complete.
#[test]
fn the_banners_continue_reads_before_it_plans() {
    let git = git_or_skip!();
    let repo = conflicting(&git);
    repo.run(&["checkout", "-q", "side"]);
    let out = std::process::Command::new(&repo.git)
        .current_dir(&repo.top)
        .envs(fixture::env(&repo.home()))
        .args(["rebase", "main"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "a conflict");
    repo.run(&["config", "core.commentChar", "p"]);
    let mut g = Gx::new(repo);
    g.ready_in_git();
    assert!(g.shows("REBASE"), "{:#?}", g.texts());
    g.cmd_shift(Key::C);
    g.until_quiet("Continue's reads");
    assert_eq!(g.dialog_id(), Some(ActionId::Continue));
    assert!(!g.shows("not read yet"), "{:#?}", g.texts());
    let mut fresh = g.view().repo().unwrap().0.clone();
    fresh.seen = Default::default();
    let continue_ = Action::Proceed(crate::git::actions::Proceed::Continue);
    if crate::git::actions::needs(&continue_, &fresh).is_empty() {
        eprintln!("note: this base's plan of a rebase's Continue reads nothing (S4-A's commentChar check is not in it)");
        return;
    }
    assert!(
        g.shows("core.commentChar is 'p': start or finish this rebase in a terminal"),
        "{:#?}",
        g.texts()
    );
    g.key(Key::Enter);
    g.idle(4);
    assert_eq!(g.dialog_id(), Some(ActionId::Continue), "Enter ran nothing");
    assert!(
        g.repo.top.join(".git/rebase-merge").exists(),
        "still in progress"
    );
}

/// 02 §6, 03 §3.4: closing the window while a write runs waits for it — the run is handed over
/// when the browser goes, and `open`'s end waits for it and prints its result; the commit is
/// made.
#[test]
fn closing_the_window_waits_for_a_running_write() {
    let git = git_or_skip!();
    let repo = small(&git);
    let pidfile = repo.tmp.path().join("hook.pid");
    script(
        &repo.top.join(".git/hooks/pre-commit"),
        &format!("echo $$ > '{}'\nsleep 1", pidfile.display()),
    );
    repo.run(&["add", "a.txt"]);
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.f(Key::F7);
    g.fx.typed("made while closing");
    g.idle(2);
    g.key(Key::Enter);
    pid_in(&pidfile);
    assert_eq!(g.panel().running.len(), 1, "the commit runs");
    let Gx { repo, fx } = g;
    let leftover = fx.b.leftover.clone();
    drop(fx);
    leftover.borrow_mut().finish();
    assert_eq!(
        repo.run(&["log", "-1", "--format=%s"]).trim(),
        "made while closing"
    );
}

/// 03 §9.2: a right click on a row of the graph puts the bar on it and opens its menu.
#[test]
fn a_right_click_opens_the_rows_menu() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    let at =
        g.fx.drv
            .texts_starting_with("second")
            .into_iter()
            .find(|(r, _)| r.left() >= 500.0)
            .expect("the row on the right")
            .0
            .center();
    g.fx.drv.move_to(&mut g.fx.b, at);
    g.fx.drv.press(&mut g.fx.b, egui::PointerButton::Secondary);
    g.fx.drv
        .release(&mut g.fx.b, egui::PointerButton::Secondary);
    g.idle(2);
    assert!(g.painted("Tag…"), "the commit's menu: {:#?}", g.texts());
    assert!(g.status().starts_with("Menu of commit"), "{}", g.status());
    let row = g.view().bar_row().cloned();
    assert!(
        matches!(&row, Some(crate::git::graph::Row::Commit { commit, .. }) if commit.subject == "second"),
        "the bar on the row clicked"
    );
}

/// A command run on the live host: its exit code and what it printed.
type Ssh<'a> = dyn Fn(&[&str], &str) -> (i32, String) + 'a;

/// G-R3's live receipt (03 §11; S3-C), run by hand — sopen's own browser and runner
/// (`git_connect`: `SshGit` over a real ssh master), the window here (the test kit's), the
/// repository on the host:
///
/// ```text
/// SOPEN_TEST_GIT_HOST=<ssh destination> SOPEN_TEST_GIT_DIR=/tmp/<a new folder> \
///   cargo test --locked --lib live_g_r3 -- --ignored --nocapture
/// ```
///
/// It makes `<dir>/origin.git` and `<dir>/repo` there, the remote's upload-pack a stand-in that
/// writes its pid and sleeps (`sleep 30.4711`, a marker `pgrep` finds only here); Fetch a remote…
/// from the repository's menu, Esc while it waits → within 6 s the log says `stopped` and
/// `pgrep -f 'sleep 30.4711'` on the host prints nothing. Then again, the ssh master killed
/// (`-O exit`) before Esc → what the result says. It prints each line it checks, and removes
/// `<dir>` at the end.
#[test]
#[ignore = "needs SOPEN_TEST_GIT_HOST=<ssh destination> and SOPEN_TEST_GIT_DIR=/tmp/<new folder>"]
fn live_g_r3_esc_stops_a_remote_fetch() {
    use crate::remote::{connection_args, SftpRemote};
    use crate::target::RemoteSpec;
    let Ok(host) = std::env::var("SOPEN_TEST_GIT_HOST") else {
        eprintln!("skipping: SOPEN_TEST_GIT_HOST is not set");
        return;
    };
    let dir = std::env::var("SOPEN_TEST_GIT_DIR").expect("SOPEN_TEST_GIT_DIR");
    assert!(dir.starts_with("/tmp/"), "a folder under /tmp: {dir}");
    let (user, name) = match host.split_once('@') {
        Some((u, h)) => (Some(u.to_string()), h.to_string()),
        None => (None, host.clone()),
    };
    let marker = "sleep 30.4711";
    // What `pgrep -f` looks for: the marker, written so that the pattern matches the sleep but
    // not a command line holding the pattern itself (the host may be this machine).
    let pattern = "'slee[p] 30.4711'";
    let ssh = |args: &[&str], stdin: &str| -> (i32, String) {
        use std::io::Write as _;
        let mut child = std::process::Command::new("ssh")
            .arg(&host)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        (out.status.code().unwrap_or(-1), text)
    };
    let (code, said) = ssh(&["echo", "shell:", "$SHELL;", "uname", "-sr"], "");
    println!("host: {} ({code})", said.trim());
    // The fixture, by a plain `sh` reading its script on stdin (any login shell runs `sh -s`).
    let setup = format!(
        "set -e\nd='{dir}'\nmkdir \"$d\"\ncd \"$d\"\ngit init -q --bare origin.git\n\
         git init -q repo\ncd repo\ngit checkout -q -b main\n\
         git -c user.name=sopen -c user.email=sopen@example.invalid commit -q --allow-empty -m one\n\
         git remote add origin \"$d/origin.git\"\ngit push -q origin main\ngit fetch -q origin\n\
         git config remote.origin.uploadpack \"echo \\$\\$ > $d/up.pid; exec {marker} #\"\n\
         git --version\n"
    );
    let (code, said) = ssh(&["sh", "-s"], &setup);
    assert_eq!(code, 0, "the fixture: {said}");
    println!("fixture: {}", said.trim());
    let tmp = tempfile::Builder::new()
        .prefix("s3c-")
        .tempdir_in("/tmp")
        .unwrap();
    let state = tmp.path().join("s");
    std::fs::create_dir(&state).unwrap();
    let spec = RemoteSpec {
        user,
        host: name,
        path: format!("{dir}/repo"),
    };
    let (dest, label) = (spec.destination(), spec.label());
    let connect: Connect = {
        let (spec, state) = (spec.clone(), state.clone());
        Arc::new(move || {
            Ok(Box::new(SftpRemote::connect(&spec, &state)?) as Box<dyn RemoteFs + Send>)
        })
    };
    let fs = connect().expect("the ssh master");
    let git = git_connect(
        Some(dest.clone()),
        Some(label.clone()),
        state.clone(),
        connect.clone(),
    );
    let b = Browser::new(Start {
        fs,
        connect,
        label: Some(label.clone()),
        destination: Some(dest.clone()),
        abs: format!("{dir}/repo"),
        cache: tmp.path().join("c"),
        state_dir: state.clone(),
        launch: Arc::new(|_: &str, _: bool| Ok(String::new())),
        git: Some(git),
    });
    let mut fx = Fx {
        _tmp: tmp,
        root: format!("{dir}/repo"),
        cache: PathBuf::new(),
        drv: Driver::new(1000.0, 700.0),
        b,
        launched: Arc::default(),
    };
    let wait = |fx: &mut Fx, what: &str, secs: u64, done: &dyn Fn(&Browser) -> bool| {
        let t = Instant::now();
        while !done(&fx.b) {
            if t.elapsed() >= Duration::from_secs(secs) {
                let log =
                    fx.b.git_runner()
                        .map(|r| r.log().entries())
                        .unwrap_or_default();
                let lines: Vec<String> = log.iter().take(6).map(|e| e.debug_line()).collect();
                panic!(
                    "{what}: not within {secs} s; status {:?}; the log, newest first: {lines:#?}",
                    fx.b.status_line().text
                );
            }
            fx.drv.frame(&mut fx.b, Vec::new(), Modifiers::NONE);
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    // The panel shown (its top level as git says it: `/private/tmp/…` on macOS).
    let quiet = |b: &Browser| {
        b.git
            .panels
            .first()
            .is_some_and(|p| p.view.repo().is_some() && !p.acting() && p.read.is_none())
    };
    wait(&mut fx, "the panel", 60, &|b| {
        b.shown
            .as_ref()
            .is_some_and(|s| matches!(&s[1], view::Content::Git(_)))
    });
    wait(&mut fx, "the snapshot", 60, &quiet);
    fx.settle();
    fx.key(Key::Tab);
    wait(&mut fx, "the keyboard in git", 10, &|b| {
        b.git_focused().is_some()
    });
    let painted = |fx: &mut Fx, text: &str| {
        let t = Instant::now();
        while fx.drv.find_text(text).is_none() {
            assert!(
                t.elapsed() < Duration::from_secs(10),
                "{text} not on screen"
            );
            fx.drv.frame(&mut fx.b, Vec::new(), Modifiers::NONE);
        }
        fx.drv.idle(&mut fx.b, 2);
    };
    let fetch = |fx: &mut Fx| {
        fx.key(Key::F10);
        painted(fx, "Repository…");
        fx.drv.click_text_on_top(&mut fx.b, "Repository…");
        painted(fx, "Fetch a remote…");
        fx.drv.click_text_on_top(&mut fx.b, "Fetch a remote…");
        painted(fx, "Run");
        fx.drv.idle(&mut fx.b, 3);
        fx.key(Key::Enter);
        fx.drv.idle(&mut fx.b, 2);
    };
    let pid_there = |ssh: &Ssh<'_>| {
        let t = Instant::now();
        loop {
            let (code, out) = ssh(&["cat", &format!("{dir}/up.pid")], "");
            if code == 0 && !out.trim().is_empty() {
                return out.trim().to_string();
            }
            assert!(t.elapsed() < Duration::from_secs(30), "no upload-pack pid");
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    // 1. Esc during the fetch.
    fetch(&mut fx);
    let pid = pid_there(&ssh);
    println!("upload-pack waiting, pid {pid} on the host");
    let t = Instant::now();
    fx.key(Key::Escape);
    wait(&mut fx, "the stop", 20, &quiet);
    let took = t.elapsed();
    let log = fx.b.git_runner().unwrap().log().entries();
    let entry = log
        .iter()
        .find(|e| e.line.starts_with("git fetch"))
        .expect("the fetch's entry");
    println!(
        "Esc → ended in {:.2} s: {}",
        took.as_secs_f32(),
        entry.status_line()
    );
    let (code, out) = ssh(&["pgrep", "-f", pattern], "");
    println!("pgrep -f {pattern} → exit {code}, {:?}", out.trim());
    assert!(
        entry.status_line().contains("stopped"),
        "{}",
        entry.status_line()
    );
    assert!(took < Duration::from_secs(6), "{took:?}");
    assert_eq!(code, 1, "nothing left: {out}");
    // 2. The master killed before Esc. The first stop's result dialog closes first.
    ssh(&["rm", "-f", &format!("{dir}/up.pid")], "");
    for _ in 0..3 {
        if fx.b.git.panels.first().is_some_and(|p| p.view.dialog_up()) {
            fx.key(Key::Escape);
            fx.drv.idle(&mut fx.b, 2);
        }
    }
    fetch(&mut fx);
    let pid = pid_there(&ssh);
    println!("upload-pack waiting again, pid {pid}");
    let mut exit = std::process::Command::new("ssh");
    exit.args(connection_args(&dest, &state).unwrap())
        .args(["-O", "exit", &dest]);
    let killed = exit.output().unwrap();
    println!(
        "ssh -O exit → {} {}",
        killed.status,
        String::from_utf8_lossy(&killed.stderr).trim()
    );
    fx.key(Key::Escape);
    wait(&mut fx, "the end", 30, &quiet);
    let log = fx.b.git_runner().unwrap().log().entries();
    let entry = log
        .iter()
        .find(|e| e.line.starts_with("git fetch"))
        .expect("the second fetch's entry");
    println!("master killed, Esc → {}", entry.status_line());
    println!("status row: {}", fx.b.status_line().text);
    let (code, out) = ssh(&["pgrep", "-f", pattern], "");
    println!("pgrep -f {pattern} → exit {code}, {:?}", out.trim());
    // Clean up: what this test started there, and its folder.
    let (_, out) = ssh(
        &["sh", "-s"],
        &format!("pkill -f {pattern}; sleep 1; rm -rf '{dir}'; ls -d '{dir}' 2>&1; pgrep -f {pattern} || echo none left\n"),
    );
    println!("cleanup: {}", out.trim());
}

/// Review 1, must-fix 1: while the log or the Backups list shows, the action keys do nothing —
/// they would act on the graph's bar under the list, which is not on screen: no dialog, no menu,
/// nothing read or run, no Find; the F-key row shows only what the list does; Cmd/Ctrl+L and B
/// close their list.
#[test]
fn action_keys_do_nothing_while_a_list_shows() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    g.bar_to("second");
    let none = Modifiers::NONE;
    let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
    for (open, slots) in [(Key::L, vec![1, 3, 4, 9]), (Key::B, vec![1, 3, 4, 8, 9])] {
        g.cmd(open);
        g.until_quiet("the list");
        let shows = |g: &Gx| g.view().log_newest().is_some() || g.view().backups().is_some();
        assert!(shows(&g), "{open:?}: the list");
        let row: Vec<u8> = g.slots().iter().map(|s| s.0).collect();
        assert_eq!(row, slots, "{open:?}: the list's F-key row");
        for (key, mods) in [
            (Key::F2, none),
            (Key::F5, none),
            (Key::F6, none),
            (Key::F7, none),
            (Key::F8, none),
            (Key::F10, none),
            (Key::F10, Modifiers::SHIFT),
            (Key::Space, none),
            (Key::R, cmd_shift),
            (Key::C, cmd_shift),
            // S4: an edit stop's Amend and Split act on the banner too (none here).
            (Key::M, cmd_shift),
            (Key::P, cmd_shift),
        ] {
            g.with(key, mods);
            g.idle(2);
            assert!(
                g.dialog_id().is_none() && !g.view().dialog_up(),
                "{open:?} {key:?}: nothing opened"
            );
            assert!(
                !g.panel().acting() && g.panel().running.is_empty(),
                "{open:?} {key:?}: nothing read or run"
            );
            assert!(shows(&g), "{open:?} {key:?}: the list still shows");
        }
        g.fx.typed("x");
        assert!(g.view().find_text().is_none(), "{open:?}: no Find");
        g.cmd(open);
        g.idle(2);
        assert!(!shows(&g), "{open:?}: closed by its key");
    }
}

/// Review 1, must-fix 2: a run whose confirmed plan was lost (here: none was ever shown) and
/// whose plan made now asks for a confirmation does not run — HEAD stays — and the action's
/// dialog opens with the new confirmation; Enter there runs it.
#[test]
fn a_run_whose_confirmed_plan_was_lost_asks_again() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    g.fx.drv.resize(1000.0, 1400.0);
    g.idle(2);
    let head = g.repo.rev("HEAD");
    let light = g.repo.rev("light");
    let action = Action::Reset {
        to: crate::git::model::Oid::parse(&light).unwrap(),
        mode: crate::git::actions::ResetMode::Hard,
    };
    let ctx = g.fx.drv.ctx.clone();
    let i = g.b().git.panel(g.repo.top()).unwrap();
    g.fx.b.start_action(&ctx, i, action);
    g.until_quiet("the plan made again");
    assert_eq!(g.repo.rev("HEAD"), head, "nothing ran");
    assert_eq!(g.dialog_id(), Some(ActionId::Reset), "its dialog again");
    g.idle(4);
    assert!(
        g.shows("git reset --hard "),
        "the new confirmation: {:#?}",
        g.texts()
    );
    assert!(
        g.view()
            .status_line()
            .text
            .contains("not run — its plan asks for a confirmation"),
        "{}",
        g.view().status_line().text
    );
    g.run();
    assert_eq!(g.repo.rev("HEAD"), light, "confirmed, it ran");
}

/// Review 1, must-fix 3: Cmd/Ctrl+Z restores the newest group this window wrote by its stamp
/// (UTC, then `-2`, `-3`…), whatever order the runs ended in.
#[test]
fn undo_takes_the_newest_stamp_whatever_order_the_runs_end_in() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    let ctx = g.fx.drv.ctx.clone();
    let i = g.b().git.panel(g.repo.top()).unwrap();
    let top = g.top();
    let ran = |stamp: &str| RunEnd::Ran {
        outcome: Box::new(crate::git::actions::Outcome {
            steps: Vec::new(),
            backup: Some(stamp.to_string()),
        }),
        title: "a run".into(),
        what: "a run".into(),
        changes_files: false,
        squash: false,
        action: Box::new(Action::Fetch {
            prune: false,
            prune_tags: false,
        }),
    };
    let newest = |g: &Gx| g.b().git.undo[&top].newest().map(String::from);
    // The newer ended first.
    for stamp in [
        "20261123T120000Z-2",
        "20261123T120000Z",
        "20261123T115959Z-10",
    ] {
        g.fx.b.run_ended(&ctx, i, ran(stamp));
    }
    g.until_quiet("the refreshes");
    assert_eq!(newest(&g).as_deref(), Some("20261123T120000Z-2"));
    for stamp in ["20261123T120001Z", "20261123T120000Z-3"] {
        g.fx.b.run_ended(&ctx, i, ran(stamp));
    }
    g.until_quiet("the refreshes");
    assert_eq!(newest(&g).as_deref(), Some("20261123T120001Z"));
    // Cmd/Ctrl+Z asks for that group (none exists here: its read says so, naming it).
    g.esc();
    g.cmd(Key::Z);
    g.until_quiet("the restore's read");
    assert!(
        g.shows("no backup group 20261123T120001Z"),
        "{:#?}",
        g.texts()
    );
}

/// Two repositories' panels side by side (X4-menu): `a`'s (the test's `g.repo`) pinned on the
/// right, the right list in `b` and active, so `b`'s own panel stands on the left (02 §3.1 O6)
/// with the keyboard in it. Their indexes, `a`'s then `b`'s.
fn two_panels(g: &mut Gx, b: &Repo) -> (usize, usize) {
    g.wait_for_the_panel();
    g.cmd(Key::G);
    g.settle();
    let (a_top, b_top) = (g.top(), b.top().to_string());
    g.fx.b.list(Some(1), b_top.clone(), None, None);
    g.fx.b.active = 1;
    let both = [
        view::Content::Git(b_top.clone()),
        view::Content::Git(a_top.clone()),
    ];
    g.until("the two panels with their rows", |br| {
        br.shown.as_ref() == Some(&both)
            && [&a_top, &b_top].iter().all(|top| {
                br.git
                    .panel(top)
                    .is_some_and(|i| !br.git.panels[i].view.rows().is_empty())
            })
    });
    g.settle();
    g.fx.b.git.focus = true;
    g.settle();
    let (ia, ib) = (
        g.b().git.panel(&a_top).expect("a's panel"),
        g.b().git.panel(&b_top).expect("b's panel"),
    );
    assert_eq!(g.b().git_focused(), Some(ib), "the keyboard in b's panel");
    assert_eq!(g.shown(), both);
    (ia, ib)
}

/// A second repository for [`two_panels`]: one commit.
fn other_repo(git: &Path) -> Repo {
    let b = Repo::new(git);
    b.write("b.txt", "b\n");
    b.commit("only in b");
    b
}

/// The panels' grid of the browser's 1000×700 window: between the title row and the F-key and
/// status rows.
fn panels_grid() -> Grid {
    Grid::over(Rect::from_min_max(
        Pos2::new(0.0, 16.0),
        Pos2::new(1000.0, 700.0 - 32.0),
    ))
}

/// A press and a release of `button` at `p`, as one pass's events.
fn clicked_at(p: Pos2, button: egui::PointerButton) -> Vec<egui::Event> {
    let mut all = vec![egui::Event::PointerMoved(p)];
    all.extend([true, false].map(|pressed| egui::Event::PointerButton {
        pos: p,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    }));
    all
}

/// The centre of the row on the right that shows `subject`: `a`'s.
fn row_on_the_right(g: &Gx, subject: &str) -> Pos2 {
    (g.fx.drv.texts_starting_with(subject).into_iter())
        .find(|(r, _)| r.left() >= 500.0)
        .unwrap_or_else(|| panic!("{subject} on the right"))
        .0
        .center()
}

/// One git dialog at a time across two panels (X4-menu round 1): F10 opens `b`'s menu; a click
/// on the right half under its box moves the keyboard nowhere; a right click in `a`'s rows opens
/// no menu there; Enter runs `b`'s item. Once `b`'s dialog closes (fix-check), `a` is used as
/// ever: a click gives it the keyboard and its bar, a right click its menu, a letter its Find.
#[test]
fn x4_menu_one_dialog_at_a_time_across_two_panels() {
    let git = git_or_skip!();
    let b = other_repo(&git);
    let mut g = Gx::new(small(&git));
    let (ia, ib) = two_panels(&mut g, &b);
    g.key(Key::F10);
    g.idle(2);
    assert!(g.b().git.panels[ib].view.dialog_up(), "b's menu");
    // A click on the right half, inside b's box (its margin under the last line).
    let gr = panels_grid();
    let frame = g.b().git.panels[ib].view.covered(&gr)[0];
    let right_half = cells::halves(gr.all())[1];
    let x = frame.right().saturating_sub(4).max(right_half.x);
    let at = gr
        .rect(cells::Cells::new(x, frame.bottom() - 2, 1, 1))
        .center();
    g.fx.drv.click(&mut g.fx.b, at);
    g.idle(2);
    assert_eq!(
        g.b().git_focused(),
        Some(ib),
        "no keyboard move under b's box"
    );
    assert!(g.b().git.panels[ib].view.dialog_up(), "b's menu stays");
    // A right click on a's rows, on the right: no menu there.
    let p = row_on_the_right(&g, "third");
    g.fx.drv.frame(
        &mut g.fx.b,
        clicked_at(p, egui::PointerButton::Secondary),
        Modifiers::NONE,
    );
    g.idle(2);
    assert!(
        !g.b().git.panels[ia].view.dialog_up(),
        "no menu in a's panel"
    );
    assert_eq!(g.b().git_focused(), Some(ib));
    // Enter runs b's item, its first: Tag… on b's commit.
    g.key(Key::Enter);
    g.idle(2);
    assert_eq!(
        g.b().git.panels[ib].view.draft().map(Action::id),
        Some(ActionId::Tag)
    );
    assert!(g.b().git.panels[ia].view.draft().is_none());
    // b's dialog closes: a is used as ever.
    g.key(Key::Escape);
    g.idle(2);
    assert!(!g.b().git.panels[ib].view.dialog_up());
    let p = row_on_the_right(&g, "third");
    g.fx.drv.click(&mut g.fx.b, p);
    g.idle(2);
    assert_eq!(
        g.b().git_focused(),
        Some(ia),
        "a click gives a the keyboard"
    );
    let view = &g.b().git.panels[ia].view;
    let third =
        (0..view.rows().len()).find(|&k| view.line(k).is_some_and(|l| l.subject == "third"));
    assert_eq!(Some(view.bar_place().0), third, "the bar on a's row");
    g.fx.drv.frame(
        &mut g.fx.b,
        clicked_at(p, egui::PointerButton::Secondary),
        Modifiers::NONE,
    );
    g.idle(2);
    assert!(
        g.b().git.panels[ia].view.dialog_up(),
        "a right click: a's menu"
    );
    g.key(Key::Escape);
    g.idle(2);
    g.fx.typed("x");
    g.idle(2);
    assert_eq!(
        g.b().git.panels[ia].view.find_text(),
        Some("x"),
        "a letter: a's Find"
    );
}

/// A dialog the integration opens waits while another panel's is up (X4-menu fix-check, rule 2):
/// a's result — here a tag's details, read on a worker — comes while b's menu is open, and stays
/// in its channel; once b's menu closes it opens, with the keyboard (rule 1), and Esc closes it.
#[test]
fn x4_menu_a_result_waits_for_the_other_panels_dialog() {
    let git = git_or_skip!();
    let b = other_repo(&git);
    let mut g = Gx::new(small(&git));
    let (ia, ib) = two_panels(&mut g, &b);
    g.key(Key::F10);
    g.idle(2);
    assert!(g.b().git.panels[ib].view.dialog_up(), "b's menu");
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(("Tag v1.0".to_string(), Ok("tagged by Ann".to_string())))
        .unwrap();
    g.fx.b.git.panels[ia].text_read = Some(git_host::Pending {
        rx,
        handle: crate::git::runner::Handle::new(),
    });
    g.idle(3);
    assert!(!g.b().git.panels[ia].view.dialog_up(), "a's result waits");
    assert!(g.b().git.panels[ia].text_read.is_some(), "in its channel");
    assert!(g.b().git.panels[ib].view.dialog_up(), "b's menu stays");
    g.key(Key::Escape);
    g.idle(3);
    assert!(!g.b().git.panels[ib].view.dialog_up(), "b's menu closed");
    assert!(g.b().git.panels[ia].view.dialog_up(), "a's result opened");
    assert_eq!(g.b().git_focused(), Some(ia), "with the keyboard");
    g.key(Key::Escape);
    g.idle(2);
    assert!(!g.b().git.panels[ia].view.dialog_up(), "Esc closed it");
    assert!(!g.b().git.panels[ib].view.dialog_up());
}

/// A dialog that opens in a while the keyboard is in b takes the keyboard (X4-menu fix-check,
/// rule 1), and what is typed goes into it, not into b (whose typed letter would start Find).
#[test]
fn x4_menu_a_dialog_opening_elsewhere_takes_the_keyboard() {
    let git = git_or_skip!();
    let b = other_repo(&git);
    let mut g = Gx::new(small(&git));
    let (ia, ib) = two_panels(&mut g, &b);
    g.bar_to("third");
    let asked = g.fx.b.git.panels[ia]
        .view
        .open_by_id(crate::gui::DialogId::Action(ActionId::Tag));
    assert!(asked.is_empty(), "{asked:?}");
    g.idle(3);
    assert_eq!(
        g.b().git_focused(),
        Some(ia),
        "the keyboard follows the dialog"
    );
    g.fx.typed("v9");
    g.idle(2);
    let draft = g.b().git.panels[ia].view.draft().cloned();
    assert!(
        matches!(&draft, Some(Action::Tag { name, .. }) if name == "v9"),
        "{draft:?}"
    );
    assert_eq!(g.b().git.panels[ib].view.find_text(), None, "no Find in b");
    g.key(Key::Escape);
    g.idle(2);
    assert!(!g.b().git.panels[ia].view.dialog_up());
}

/// The panels' dialogs as the pass began are what each panel's draw holds to (X4-menu fix-check):
/// a click on `a`'s rows, on the right, outside `b`'s menu on the left, closes that menu during
/// `b`'s draw — which comes first — and acts on nothing of `a` in that pass: no keyboard, no bar.
#[test]
fn x4_menu_the_click_that_closes_the_left_dialog_acts_on_nothing_right() {
    let git = git_or_skip!();
    let b = other_repo(&git);
    let mut g = Gx::new(small(&git));
    let (ia, ib) = two_panels(&mut g, &b);
    g.key(Key::F10);
    g.idle(2);
    assert!(g.b().git.panels[ib].view.dialog_up(), "b's menu");
    let bar = g.b().git.panels[ia].view.bar_place().0;
    let p = row_on_the_right(&g, "third");
    g.fx.drv.frame(
        &mut g.fx.b,
        clicked_at(p, egui::PointerButton::Primary),
        Modifiers::NONE,
    );
    g.idle(2);
    assert!(
        !g.b().git.panels[ib].view.dialog_up(),
        "the click closed b's menu"
    );
    assert_eq!(
        g.b().git_focused(),
        Some(ib),
        "no keyboard to a in that pass"
    );
    assert_eq!(
        g.b().git.panels[ia].view.bar_place().0,
        bar,
        "a's bar stays"
    );
    // The next click is a's.
    g.fx.drv.click(&mut g.fx.b, p);
    g.idle(2);
    assert_eq!(g.b().git_focused(), Some(ia));
    assert_ne!(
        g.b().git.panels[ia].view.bar_place().0,
        bar,
        "a's bar on the row"
    );
}

/// A run of a's that ends while b's menu is open (X4-menu fix-check, rule 2): its end stays in
/// its channel — nothing happens in a, and the run reads as running — and once b's menu closes it
/// is heard: `running` empties and its result shows, with the keyboard.
#[test]
fn x4_menu_a_runs_end_waits_for_the_other_panels_dialog_and_is_heard() {
    let git = git_or_skip!();
    let b = other_repo(&git);
    let mut g = Gx::new(small(&git));
    let (ia, ib) = two_panels(&mut g, &b);
    g.key(Key::F10);
    g.idle(2);
    assert!(g.b().git.panels[ib].view.dialog_up(), "b's menu");
    let (tx, rx) = std::sync::mpsc::channel();
    // Its id as `spawn_run` gives one.
    g.fx.b.git.runs += 1;
    let id = g.fx.b.git.runs;
    g.fx.b.git.panels[ia].running.push(git_host::Running {
        id,
        title: "Push".into(),
        handle: crate::git::runner::Handle::new(),
        rx,
        started: Default::default(),
        since: std::time::Instant::now(),
        network: Vec::new(),
        worker: None,
    });
    // Its worker ends: the end is sent while b's menu is open.
    tx.send(git_host::RunEnd::NotRun {
        title: "Push".into(),
        why: "the remote refused it".into(),
        line: None,
    })
    .unwrap();
    g.idle(3);
    assert_eq!(g.b().git.panels[ia].running.len(), 1, "still running");
    assert!(!g.b().git.panels[ia].view.dialog_up(), "nothing in a yet");
    g.key(Key::Escape);
    g.idle(3);
    assert!(!g.b().git.panels[ib].view.dialog_up(), "b's menu closed");
    assert!(g.b().git.panels[ia].running.is_empty(), "its end heard");
    assert!(g.b().git.panels[ia].view.dialog_up(), "its result shows");
    assert_eq!(g.b().git_focused(), Some(ia), "with the keyboard");
    drop(tx);
}

// ---------------------------------------------------------------------------------------------
// F-git-listq: while the list's question or the tree has the keyboard, what the git panel reads
// and what it carries out agree.

/// A tree whose worker is never made while it is open: its side says it reads the tree, Enter,
/// Space, Esc and the letters are its keys (05 §1), and its connect returns once it closes (at
/// most 30 s).
fn a_tree_that_waits(g: &mut Gx) {
    let connect: TreeConnect = Arc::new(|closed: &AtomicBool| {
        let t = Instant::now();
        while !closed.load(Ordering::Acquire) && t.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(5));
        }
        bail!("the test's tree, closed")
    });
    g.fx.b.set_tree_connect(connect, "f-git-listq");
}

/// A press and a release of `key`, as one pass's events.
fn pressed(key: Key) -> Vec<egui::Event> {
    [true, false]
        .map(|pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        })
        .to_vec()
}

/// A hard reset to `light` started in panel `i` with no confirmation shown, as a run begun before
/// the list's question or the tree took the keyboard: its plan asks for one, so the run's end
/// opens it again (`RunEnd::Confirm`) — a dialog the integration opens. HEAD, and `light`.
fn a_reset_starts(g: &mut Gx, i: usize) -> (String, String) {
    g.fx.drv.resize(1000.0, 1400.0);
    g.idle(2);
    let (head, light) = (g.repo.rev("HEAD"), g.repo.rev("light"));
    let action = Action::Reset {
        to: crate::git::model::Oid::parse(&light).unwrap(),
        mode: crate::git::actions::ResetMode::Hard,
    };
    let ctx = g.fx.drv.ctx.clone();
    g.fx.b.start_action(&ctx, i, action);
    (head, light)
}

/// Passes until panel `i`'s first run has sent its end (its worker ended), or its end was heard.
fn its_end_sent(g: &mut Gx, i: usize) {
    g.until("the run's worker", move |b| {
        (b.git.panels[i].running.first())
            .is_none_or(|r| r.worker.as_ref().is_some_and(|w| w.is_finished()))
    });
    g.idle(2);
}

/// Passes until panel `i`'s confirmation of the reset is up with its reads in, and painted.
fn the_confirmation_is_up(g: &mut Gx, i: usize) {
    g.until("the confirmation again", move |b| {
        let p = &b.git.panels[i];
        p.running.is_empty()
            && !p.dialog_reading()
            && p.view.draft().map(Action::id) == Some(ActionId::Reset)
    });
    g.idle(4);
    assert!(
        g.shows("git reset --hard "),
        "the confirmation: {:#?}",
        g.texts()
    );
}

/// [`a_reset_starts`], then its confirmation is up ([`the_confirmation_is_up`]), nothing run.
fn a_confirmation_opens(g: &mut Gx, i: usize) -> (String, String) {
    let (head, light) = a_reset_starts(g, i);
    the_confirmation_is_up(g, i);
    assert_eq!(g.repo.rev("HEAD"), head, "nothing ran yet");
    (head, light)
}

/// Passes until panel `i` runs nothing; HEAD then, and the open dialog's action id.
fn after_the_run(g: &mut Gx, i: usize) -> (String, Option<ActionId>) {
    g.until("the run", move |b| !b.git.panels[i].acting());
    g.idle(2);
    let id = g.b().git.panels[i].view.draft().map(Action::id);
    (g.repo.rev("HEAD"), id)
}

/// F-git-listq: the tree has the keyboard and the other side stays as it was, frozen, until the
/// tree closes (05 §1), so the git panel beside it opens no dialog, as under the list's question
/// (X4-menu): a right click on its rows opens no menu — in the pass whose F10 opens the tree (the
/// list's keys come before the panels' draw) and after it. Its menu's choice and the dialog that
/// choice opened were what the panel's dropped actions lost. Once the tree closes, the right
/// click opens the row's menu as ever.
#[test]
fn f_git_listq_the_panel_beside_the_tree_opens_no_menu() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    a_tree_that_waits(&mut g);
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    let p = row_on_the_right(&g, "third");
    let mut events = pressed(Key::F10);
    events.extend(clicked_at(p, egui::PointerButton::Secondary));
    g.fx.drv.frame(&mut g.fx.b, events, Modifiers::NONE);
    g.idle(2);
    assert!(g.b().tree.is_some(), "F10 opened the tree");
    assert_eq!(g.shown()[1], g.git(), "the git panel beside it");
    assert!(
        !g.b().git.panels[i].view.dialog_up(),
        "no menu in the pass the tree opened: {:#?}",
        g.texts()
    );
    g.fx.drv.frame(
        &mut g.fx.b,
        clicked_at(p, egui::PointerButton::Secondary),
        Modifiers::NONE,
    );
    g.idle(2);
    assert!(
        !g.b().git.panels[i].view.dialog_up(),
        "no menu beside the tree: {:#?}",
        g.texts()
    );
    g.key(Key::Escape);
    g.idle(2);
    assert!(g.b().tree.is_none(), "Esc closed the tree");
    let p = row_on_the_right(&g, "third");
    g.fx.drv.frame(
        &mut g.fx.b,
        clicked_at(p, egui::PointerButton::Secondary),
        Modifiers::NONE,
    );
    g.idle(2);
    assert!(
        g.b().git.panels[i].view.dialog_up(),
        "the control: the row's menu once the tree closed"
    );
}

/// F-git-listq: a run's end that opens a dialog (its confirmation again; `a_reset_starts`) waits
/// in its channel while the tree has the keyboard, as for another panel's dialog (X4-menu's rule):
/// nothing opens beside the tree, whose keys — Enter, a letter and Space in one pass — stay its
/// own, and the run reads as running. Right after the tree closes the end is heard: the
/// confirmation opens with the keyboard, the status row says why, and a click on its `[ Run ]`
/// runs what it shows.
#[test]
fn f_git_listq_a_runs_end_waits_for_the_tree() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    a_tree_that_waits(&mut g);
    g.key(Key::F10);
    g.idle(2);
    assert!(g.b().tree.is_some(), "F10 opened the tree");
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    let (head, light) = a_reset_starts(&mut g, i);
    its_end_sent(&mut g, i);
    let mut events = pressed(Key::Enter);
    events.push(egui::Event::Text("x".into()));
    events.extend(pressed(Key::Space));
    g.fx.drv.frame(&mut g.fx.b, events, Modifiers::NONE);
    g.idle(2);
    let p = &g.b().git.panels[i];
    assert_eq!(
        (p.running.len(), p.view.dialog_up()),
        (1, false),
        "its end waits in its channel: no dialog beside the tree"
    );
    assert_eq!(g.repo.rev("HEAD"), head, "nothing ran");
    g.key(Key::Escape);
    g.idle(2);
    assert!(g.b().tree.is_none(), "Esc closed the tree");
    let p = &g.b().git.panels[i];
    assert_eq!(
        (p.running.len(), p.view.draft().map(Action::id)),
        (0, Some(ActionId::Reset)),
        "heard right after the tree closed"
    );
    the_confirmation_is_up(&mut g, i);
    assert_eq!(g.b().git_focused(), Some(i), "with the keyboard");
    let said = g.view().status_line().text;
    assert!(
        said.contains("not run — its plan asks for a confirmation"),
        "{said}"
    );
    g.click("[ Run ]");
    assert_eq!(
        after_the_run(&mut g, i),
        (light, None),
        "the click ran what the dialog showed, and the dialog closed with it ({})",
        g.status()
    );
}

/// F-git-listq: the same while the list's question has the keyboard (F7's make-folder dialog):
/// the run's end waits in its channel — no git dialog opens in front of the question, where it
/// would take the pointer while an Enter answered the question behind it — and is heard right
/// after the question closes (Esc): the confirmation opens with the keyboard, and its click runs.
#[test]
fn f_git_listq_a_runs_end_waits_for_the_lists_question() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    g.key(Key::F7);
    g.idle(2);
    assert!(g.b().ops.holds_keyboard(), "F7's dialog");
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    let (head, light) = a_reset_starts(&mut g, i);
    its_end_sent(&mut g, i);
    g.idle(2);
    let p = &g.b().git.panels[i];
    assert_eq!(
        (p.running.len(), p.view.dialog_up()),
        (1, false),
        "its end waits in its channel: no dialog in front of the question"
    );
    assert!(g.b().ops.holds_keyboard(), "F7's dialog stays");
    assert_eq!(g.repo.rev("HEAD"), head, "nothing ran");
    g.key(Key::Escape);
    g.idle(2);
    assert!(!g.b().ops.holds_keyboard(), "Esc closed F7's dialog");
    let p = &g.b().git.panels[i];
    assert_eq!(
        (p.running.len(), p.view.draft().map(Action::id)),
        (0, Some(ActionId::Reset)),
        "heard right after the question closed"
    );
    the_confirmation_is_up(&mut g, i);
    assert_eq!(g.b().git_focused(), Some(i), "with the keyboard");
    g.click("[ Run ]");
    assert_eq!(
        after_the_run(&mut g, i),
        (light, None),
        "the click ran what the dialog showed, and the dialog closed with it ({})",
        g.status()
    );
}

/// Where a stand-in copy stands: the thread it asks from, once it is about to ask, and whether
/// its question is queued for the window — `Link::ask` queues it, then wakes the window from that
/// thread, which the window's repaint observer sees ([`Asking::watch`]).
#[derive(Default)]
struct Asking {
    thread: Mutex<Option<std::thread::ThreadId>>,
    queued: AtomicBool,
}

impl Asking {
    /// The window's repaint observer marks the question queued at the first wake-up from the
    /// copy's thread once it is about to ask; no pass is drawn meanwhile.
    fn watch(this: &Arc<Asking>, ctx: &egui::Context) {
        let seen = this.clone();
        ctx.set_repaint_observer(move |_| {
            if *seen.thread.lock().unwrap() == Some(std::thread::current().id()) {
                seen.queued.store(true, Ordering::Release);
            }
        });
    }

    /// Waits (at most 10 s, drawing no pass) until the question is queued.
    fn until_queued(&self) {
        let t = Instant::now();
        while !self.queued.load(Ordering::Acquire) {
            assert!(
                t.elapsed() < Duration::from_secs(10),
                "the copy never asked"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// A stand-in copy: held at `gate`, then — its thread noted in `asking` first — asking about its
/// first name, and done as answered.
fn a_copy_that_asks(gate: &Gate, asking: &Arc<Asking>) -> ops::Run {
    let (gate, asking) = (gate.clone(), asking.clone());
    Arc::new(move |job: ops::Job, link: &ops::Link| {
        let ops::Job::Transfer { names, .. } = job else {
            return Report::default();
        };
        gate.hold();
        let name = names[0].clone();
        *asking.thread.lock().unwrap() = Some(std::thread::current().id());
        let reply = link.ask(crate::fileops::Clash {
            name: name.clone(),
            free: "free.txt".to_string(),
        });
        let outcome = match reply.answer {
            crate::fileops::Answer::Skip => ItemOutcome::Skipped,
            crate::fileops::Answer::Rename(to) => ItemOutcome::Renamed(to),
            crate::fileops::Answer::Cancel => ItemOutcome::NotReached,
        };
        Report {
            items: vec![Item { name, outcome }],
            notes: Vec::new(),
        }
    })
}

/// The copy of `sub` to the repository's parent (F5, Enter) runs, held before its question;
/// then panel `i`'s confirmation opens — a copy that runs asks nothing yet, so nothing holds it
/// back — and the keyboard follows it. The panel, HEAD and `light`.
fn a_dialog_while_a_copy_runs(
    g: &mut Gx,
    gate: &Gate,
    asking: &Arc<Asking>,
) -> (usize, String, String) {
    g.fx.b.set_ops_run(a_copy_that_asks(gate, asking));
    g.wait_for_the_panel();
    let to = Path::new(g.repo.top()).parent().unwrap();
    let to = to.to_str().unwrap().to_string();
    g.fx.b.list(Some(1), to.clone(), None, None);
    g.until("the other list", move |b| {
        b.listing.is_empty() && b.panels[1].dir == to
    });
    g.fx.select("sub");
    g.key(Key::F5);
    g.key(Key::Enter);
    gate.reached();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    let (head, light) = a_confirmation_opens(g, i);
    assert_eq!(g.b().git_focused(), Some(i), "the keyboard follows it");
    assert!(!g.b().ops.holds_keyboard(), "no question yet");
    (i, head, light)
}

/// F-git-listq, guarding the residual's boundary (no fix of this row's makes it pass): a git
/// dialog that was up before the list's question came — a copy's name-in-the-way question arriving
/// while the panel's confirmation is up — stands behind it: from the pass after the question came,
/// a click on its `[ Run ]` reaches nothing (egui gives the pointer to the dialog in front), as no
/// key does. Once the question and its report are
/// answered, the confirmation is in front again and its click runs.
#[test]
fn f_git_listq_a_dialog_behind_the_lists_question_takes_no_click() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    let (gate, asking) = (Gate::default(), Arc::new(Asking::default()));
    let (i, head, light) = a_dialog_while_a_copy_runs(&mut g, &gate, &asking);
    gate.release();
    g.until("the question", |b| b.ops.holds_keyboard());
    g.idle(2);
    assert!(g.painted("Name exists"), "the question: {:#?}", g.texts());
    let run = g.fx.drv.find_text("[ Run ]").expect("[ Run ]").center();
    g.fx.drv.frame(
        &mut g.fx.b,
        clicked_at(run, egui::PointerButton::Primary),
        Modifiers::NONE,
    );
    g.idle(2);
    let id = g.b().git.panels[i].view.draft().map(Action::id);
    assert_eq!(
        (g.repo.rev("HEAD"), id),
        (head, Some(ActionId::Reset)),
        "the click behind the question reached nothing"
    );
    assert!(g.b().ops.holds_keyboard(), "the question stays");
    g.key(Key::Escape);
    g.until("the report", |b| !b.ops.busy());
    g.key(Key::Enter);
    g.idle(2);
    assert!(!g.b().ops.holds_keyboard(), "the question and its report");
    // The copy's end re-read the panel: the confirmation shows the plan made on the new snapshot.
    g.until_quiet("the panel after the copy");
    let runs = g.b().git.runs;
    g.click("[ Run ]");
    assert_eq!(
        after_the_run(&mut g, i),
        (light, None),
        "the control: in front again, its click runs ({}; runs {runs} → {})",
        g.status(),
        g.b().git.runs
    );
}

/// F-git-listq: the one pass in which a click still reaches a git dialog behind the list's
/// question is the pass the question arrives in — egui gives a pass's pointer by the pass before,
/// where the git dialog stood in front, and the question is drawn after the panels. A click on its
/// `[ Run ]` read in that pass runs what the dialog showed; the run's end then waits for the
/// question and its report, and is heard once they are answered.
#[test]
fn f_git_listq_a_click_in_the_pass_the_question_arrives_runs() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    let (gate, asking) = (Gate::default(), Arc::new(Asking::default()));
    let (i, _, light) = a_dialog_while_a_copy_runs(&mut g, &gate, &asking);
    let run = g.fx.drv.find_text("[ Run ]").expect("[ Run ]").center();
    Asking::watch(&asking, &g.fx.drv.ctx);
    gate.release();
    // Its question is queued for the window, no pass drawn since: the next pass hears it.
    asking.until_queued();
    g.fx.drv.ctx.set_repaint_observer(|_| {});
    g.fx.drv.frame(
        &mut g.fx.b,
        clicked_at(run, egui::PointerButton::Primary),
        Modifiers::NONE,
    );
    assert!(
        g.b().ops.holds_keyboard(),
        "the question arrived in the pass of the click"
    );
    let t = Instant::now();
    while g.repo.rev("HEAD") != light {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "the click ran nothing"
        );
        g.idle(1);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        g.b().git.panels[i].view.draft().is_none(),
        "the dialog closed with its run"
    );
    its_end_sent(&mut g, i);
    assert_eq!(
        g.b().git.panels[i].running.len(),
        1,
        "its end waits for the question"
    );
    g.key(Key::Escape);
    g.until("the report", |b| !b.ops.busy());
    g.key(Key::Enter);
    assert_eq!(
        after_the_run(&mut g, i),
        (light, None),
        "heard once the question and its report were answered"
    );
}

/// F-git-listq: the keyboard follows a dialog the git pass opens before any key is read (X4-menu
/// fix-check, rule 1), and the pass's keys are dropped (`Browser::ui`): they were pressed before
/// the dialog was on screen. A run's end heard in the pass of an F10 and an Enter in the list: F10
/// opens no tree beside the dialog, and the Enter is neither the list's nor the dialog's. Once its
/// confirmation shows, Enter runs it.
#[test]
fn f_git_listq_a_dialog_heard_in_a_pass_takes_its_keys() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    a_tree_that_waits(&mut g);
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    let (head, light) = a_reset_starts(&mut g, i);
    // Its end sent, and no pass since: the next pass hears it.
    let t = Instant::now();
    while !(g.b().git.panels[i].running.first())
        .is_some_and(|r| r.worker.as_ref().is_some_and(|w| w.is_finished()))
    {
        assert!(t.elapsed() < Duration::from_secs(20), "the run never ended");
        std::thread::sleep(Duration::from_millis(5));
    }
    let list = (g.b().panels[0].dir.clone(), g.b().panels[0].highlight);
    let mut events = pressed(Key::F10);
    events.extend(pressed(Key::Enter));
    g.fx.drv.frame(&mut g.fx.b, events, Modifiers::NONE);
    assert!(g.b().tree.is_none(), "F10 opened no tree beside the dialog");
    assert_eq!(
        g.b().git.panels[i].view.draft().map(Action::id),
        Some(ActionId::Reset),
        "its dialog opened in that pass"
    );
    assert_eq!(g.b().git_focused(), Some(i), "with that pass's keyboard");
    g.idle(2);
    assert_eq!(
        (g.b().panels[0].dir.clone(), g.b().panels[0].highlight),
        list,
        "the Enter was not the list's"
    );
    the_confirmation_is_up(&mut g, i);
    assert_eq!(g.repo.rev("HEAD"), head, "that Enter ran nothing unseen");
    g.key(Key::Enter);
    assert_eq!(
        after_the_run(&mut g, i),
        (light, None),
        "its Enter runs what it shows"
    );
    assert!(g.b().tree.is_none());
}

/// A run of panel `i`'s, as `spawn_run` gives one, whose worker already sent `end`: the next pass
/// hears it (as `x4_menu_a_runs_end_waits_for_the_other_panels_dialog_and_is_heard` builds one).
/// Its sender, to keep while it matters.
fn a_run_that_ended(
    g: &mut Gx,
    i: usize,
    end: git_host::RunEnd,
) -> std::sync::mpsc::Sender<git_host::RunEnd> {
    let (_, tx) = a_run_that_runs(g, i);
    tx.send(end).unwrap();
    tx
}

/// A run of panel `i`'s, as `spawn_run` gives one, its end not sent yet: its id and its sender.
fn a_run_that_runs(g: &mut Gx, i: usize) -> (u64, std::sync::mpsc::Sender<git_host::RunEnd>) {
    let (tx, rx) = std::sync::mpsc::channel();
    g.fx.b.git.runs += 1;
    let id = g.fx.b.git.runs;
    g.fx.b.git.panels[i].running.push(git_host::Running {
        id,
        title: "Push".into(),
        handle: crate::git::runner::Handle::new(),
        rx,
        started: Default::default(),
        since: std::time::Instant::now(),
        network: Vec::new(),
        worker: None,
    });
    (id, tx)
}

/// Waits, drawing no pass, until panel `i`'s first run's worker has sent its end: the next pass
/// hears it.
fn its_end_sent_unheard(g: &Gx, i: usize) {
    let t = Instant::now();
    while !(g.b().git.panels[i].running.first())
        .is_some_and(|r| r.worker.as_ref().is_some_and(|w| w.is_finished()))
    {
        assert!(t.elapsed() < Duration::from_secs(20), "the run never ended");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The list's folder and bar.
fn the_list(g: &Gx) -> (String, usize) {
    (g.b().panels[0].dir.clone(), g.b().panels[0].highlight)
}

/// F-git-listq review, must-fix 1: a run's end heard in the pass of the list's `key` (in `sopen
/// --tui`, any burst read with the end's wake-up) opens its result in that pass and the keyboard
/// follows it — but none of that pass's keys: the result was never on screen, so it does not close
/// on them (its reason would be lost unread), and the list does not move either. The next Esc
/// closes it.
fn a_result_heard_with_a_lists(key: Key) {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    assert_eq!(g.b().git_focused(), None, "the keyboard in the list");
    let list = the_list(&g);
    let ended = git_host::RunEnd::NotRun {
        title: "Push".into(),
        why: "the remote refused it".into(),
        line: None,
    };
    let tx = a_run_that_ended(&mut g, i, ended);
    g.fx.drv.frame(&mut g.fx.b, pressed(key), Modifiers::NONE);
    g.idle(2);
    let p = &g.b().git.panels[i];
    assert!(p.running.is_empty(), "its end heard");
    assert!(p.view.dialog_up(), "its result stays up");
    assert_eq!(g.b().git_focused(), Some(i), "with the keyboard");
    assert_eq!(the_list(&g), list, "the list did not move");
    assert!(g.shows("the remote refused it"), "its reason");
    g.key(Key::Escape);
    g.idle(2);
    assert!(
        !g.b().git.panels[i].view.dialog_up(),
        "the next Esc closes it"
    );
    drop(tx);
}

#[test]
fn f_git_listq_a_result_heard_with_a_lists_enter_stays() {
    a_result_heard_with_a_lists(Key::Enter);
}

#[test]
fn f_git_listq_a_result_heard_with_a_lists_esc_stays() {
    a_result_heard_with_a_lists(Key::Escape);
}

/// F-git-listq review, must-fix 1: a run's end that opens its confirmation again, heard in the
/// pass of the list's Esc, is not cancelled unseen: it stays up with the keyboard, the list as it
/// was; once it shows, its click runs what it shows.
#[test]
fn f_git_listq_a_confirmation_heard_with_a_lists_esc_stays() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    let list = the_list(&g);
    let (head, light) = a_reset_starts(&mut g, i);
    its_end_sent_unheard(&g, i);
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    g.idle(2);
    assert_eq!(
        g.b().git.panels[i].view.draft().map(Action::id),
        Some(ActionId::Reset),
        "the unseen confirmation stays"
    );
    assert_eq!(g.b().git_focused(), Some(i), "with the keyboard");
    assert_eq!(the_list(&g), list, "the list did not move");
    the_confirmation_is_up(&mut g, i);
    assert_eq!(g.repo.rev("HEAD"), head);
    g.click("[ Run ]");
    assert_eq!(
        after_the_run(&mut g, i),
        (light, None),
        "its click runs what it shows"
    );
}

/// F-git-listq review (paging): a page the graph asks for while the panel is `elsewhere` — here
/// the tree open beside it — is read. `ask_more` marks the page asked as it asks for it, so the
/// old clear, which dropped the request, left the graph without its next page until a refresh.
/// The bar is put on the last loaded row while the tree is open, as a refresh that kept it there
/// would.
#[test]
fn f_git_listq_a_page_asked_beside_the_tree_is_read() {
    let git = git_or_skip!();
    let mut g = Gx::new(fixture::big(&git, 1000));
    g.wait_for_the_panel();
    let before = g.view().rows().len();
    assert!(
        matches!(g.view().paging(), git_panel::Paging::More(_)),
        "{:?}",
        g.view().paging()
    );
    a_tree_that_waits(&mut g);
    g.key(Key::F10);
    g.idle(2);
    assert!(g.b().tree.is_some(), "F10 opened the tree");
    let last = git_panel::RowKey::of(&g.view().rows()[before - 1]);
    assert!(g.panel_mut().view.select(last), "the bar on the last row");
    let t = Instant::now();
    while g.view().rows().len() == before && t.elapsed() < Duration::from_secs(10) {
        g.idle(1);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        g.view().rows().len() > before,
        "the page asked for beside the tree came: {} rows, {:?}",
        g.view().rows().len(),
        g.view().paging()
    );
    assert!(g.b().tree.is_some(), "the tree stays");
    g.key(Key::Escape);
    g.idle(2);
}

/// F-git-listq review, item 6: Find's `[ ] Aa` takes no click while the panel is `elsewhere` —
/// here the tree open beside it, its other side frozen — as the rest of the panel takes none. Once
/// the tree closes, a click turns it.
#[test]
fn f_git_listq_finds_toggle_takes_no_click_beside_the_tree() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    g.fx.typed("t");
    g.idle(2);
    assert_eq!(g.view().find_text(), Some("t"), "Find open");
    // A click on the list's `sub` gives the list the keyboard back; F10 opens the tree.
    let sub = (g.fx.drv.texts_starting_with("sub").into_iter())
        .find(|(r, _)| r.left() < 500.0)
        .expect("sub on the left")
        .0
        .center();
    g.fx.drv.click(&mut g.fx.b, sub);
    g.idle(2);
    assert_eq!(g.b().git_focused(), None, "the keyboard in the list");
    a_tree_that_waits(&mut g);
    g.key(Key::F10);
    g.idle(2);
    assert!(g.b().tree.is_some(), "F10 opened the tree");
    let aa =
        g.fx.drv
            .find_text("[ ] Aa")
            .expect("Find's toggle beside the tree");
    g.fx.drv.click(&mut g.fx.b, aa.center());
    g.idle(2);
    assert!(
        g.painted("[ ] Aa") && !g.painted("[X] Aa"),
        "the toggle stays off: {:#?}",
        g.texts()
    );
    g.key(Key::Escape);
    g.idle(2);
    assert!(g.b().tree.is_none(), "Esc closed the tree");
    g.fx.drv.click(&mut g.fx.b, aa.center());
    g.idle(2);
    assert!(g.painted("[X] Aa"), "the control: the click turns it");
}

/// "Stop git?" up and painted over a running write of panel `i`'s (`ask_stop`, as Esc after
/// 10 s asks it), the keyboard in the panel: the write's sender, to end it with.
fn stop_git_is_up(g: &mut Gx, i: usize) -> std::sync::mpsc::Sender<git_host::RunEnd> {
    let (id, tx) = a_run_that_runs(g, i);
    g.fx.b.git.panels[i].view.ask_stop("Push".into(), id);
    g.idle(2);
    assert!(
        g.shows("Push is still running."),
        "Stop git?: {:#?}",
        g.texts()
    );
    assert_eq!(g.b().git_focused(), Some(i), "the keyboard in the panel");
    tx
}

/// The details as they stand: a read on its way, one come, the pane open.
fn details_of(g: &Gx, i: usize) -> (bool, bool, bool) {
    let p = &g.b().git.panels[i];
    (
        p.details.is_some(),
        p.last_details.is_some(),
        p.view.details_open(),
    )
}

/// F-git-listq fix-check: "Stop git?" is up and painted; its write ends with nothing to say (a
/// success, no result) in the pass of the owner's Enter, meant for "Stop git?". The dialog goes
/// with its run, and the Enter is dropped with the pass (its front dialog changed under it): it
/// does not fall through to the graph, whose Enter opens the bar's details. The next Enter is the
/// graph's.
#[test]
fn f_git_listq_stop_git_gone_with_its_run_leaves_the_enter_to_nothing() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    let details = details_of(&g, i);
    let tx = stop_git_is_up(&mut g, i);
    let light = g.repo.rev("light");
    tx.send(git_host::RunEnd::Ran {
        outcome: Box::new(crate::git::actions::Outcome {
            steps: Vec::new(),
            backup: None,
        }),
        title: "Push".into(),
        what: "pushed".into(),
        changes_files: false,
        squash: false,
        action: Box::new(Action::Reset {
            to: crate::git::model::Oid::parse(&light).unwrap(),
            mode: crate::git::actions::ResetMode::Hard,
        }),
    })
    .unwrap();
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Enter), Modifiers::NONE);
    g.idle(2);
    let p = &g.b().git.panels[i];
    assert!(p.running.is_empty(), "its end heard");
    assert!(!p.view.dialog_up(), "Stop git? went with its run");
    assert_eq!(
        details_of(&g, i),
        details,
        "the Enter did not reach the graph"
    );
    g.until_quiet("the refresh after the run");
    g.key(Key::Enter);
    g.idle(2);
    assert_ne!(
        details_of(&g, i),
        details,
        "the control: the next Enter is the graph's"
    );
    drop(tx);
}

/// F-git-listq fix-check: "Stop git?" up and painted; its write's end puts its confirmation in
/// front again (`RunEnd::Confirm`) in the pass of the owner's Esc, meant for "Stop git?" (No). The
/// confirmation, never on screen, is not cancelled by it; once it shows, its click runs.
#[test]
fn f_git_listq_a_confirmation_in_place_of_stop_git_stays_with_its_esc() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    g.fx.drv.resize(1000.0, 1400.0);
    g.idle(2);
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    let tx = stop_git_is_up(&mut g, i);
    let (head, light) = (g.repo.rev("HEAD"), g.repo.rev("light"));
    tx.send(git_host::RunEnd::Confirm {
        title: "Reset".into(),
        action: Box::new(Action::Reset {
            to: crate::git::model::Oid::parse(&light).unwrap(),
            mode: crate::git::actions::ResetMode::Hard,
        }),
    })
    .unwrap();
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    g.idle(2);
    assert_eq!(
        g.b().git.panels[i].view.draft().map(Action::id),
        Some(ActionId::Reset),
        "the unseen confirmation stays"
    );
    the_confirmation_is_up(&mut g, i);
    assert_eq!(g.repo.rev("HEAD"), head);
    g.click("[ Run ]");
    assert_eq!(
        after_the_run(&mut g, i),
        (light, None),
        "its click runs what it shows"
    );
    drop(tx);
}

/// F-git-listq fix-check: a confirmation up and painted is replaced by another of its kind — a
/// run's end puts another action's confirmation in its place — in the pass of the owner's Esc,
/// meant for the first. The front dialog's serial changed, so the Esc is dropped with the pass:
/// the new confirmation, never on screen, stays.
#[test]
fn f_git_listq_a_confirmation_replaced_by_another_keeps_the_esc_from_it() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.wait_for_the_panel();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    a_confirmation_opens(&mut g, i);
    assert_eq!(g.b().git_focused(), Some(i), "the keyboard in the panel");
    let first = g.repo.rev("main~3");
    let other = Action::Reset {
        to: crate::git::model::Oid::parse(&first).unwrap(),
        mode: crate::git::actions::ResetMode::Hard,
    };
    let tx = a_run_that_ended(
        &mut g,
        i,
        git_host::RunEnd::Confirm {
            title: "Reset".into(),
            action: Box::new(other.clone()),
        },
    );
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    g.idle(2);
    assert_eq!(
        g.b().git.panels[i].view.draft(),
        Some(&other),
        "the unseen confirmation in its place stays"
    );
    g.idle(4);
    g.key(Key::Escape);
    g.idle(2);
    assert!(
        !g.b().git.panels[i].view.dialog_up(),
        "the control: once shown, Esc cancels it"
    );
    drop(tx);
}

/// F-git-listq fix-check, guarding #181's own rule too: "Stop git?" up and painted; its write's
/// failure (a result) comes in the pass of the owner's `key`. The result stays, its reason
/// readable; the next Esc closes it.
fn a_result_in_place_of_stop_git(key: Key) {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    let tx = stop_git_is_up(&mut g, i);
    tx.send(git_host::RunEnd::NotRun {
        title: "Push".into(),
        why: "the remote refused it".into(),
        line: None,
    })
    .unwrap();
    g.fx.drv.frame(&mut g.fx.b, pressed(key), Modifiers::NONE);
    g.idle(2);
    assert!(g.b().git.panels[i].view.dialog_up(), "its result stays");
    assert!(g.shows("the remote refused it"), "its reason");
    g.key(Key::Escape);
    g.idle(2);
    assert!(
        !g.b().git.panels[i].view.dialog_up(),
        "the next Esc closes it"
    );
    drop(tx);
}

#[test]
fn f_git_listq_a_result_in_place_of_stop_git_stays_with_its_enter() {
    a_result_in_place_of_stop_git(Key::Enter);
}

#[test]
fn f_git_listq_a_result_in_place_of_stop_git_stays_with_its_esc() {
    a_result_in_place_of_stop_git(Key::Escape);
}

/// F-git-listq fix-check 2: every slot counts, not the one first in some order. The planner's
/// question (a reword's message, the planner's own dialog in the body) is up and painted; a run's
/// failure comes in the pass of the owner's Enter, meant for that question, and puts a result up
/// over it. The pass's keys are dropped: the question does not keep the message on an Enter
/// pressed before the result was there, and the result does not close on it either — both stay.
#[test]
fn f_git_listq_a_result_over_the_planners_question_takes_no_key_from_it() {
    let git = git_or_skip!();
    let repo = Repo::new(&git);
    repo.write("a.txt", "a\n");
    repo.commit("base");
    repo.write("b.txt", "b\n");
    repo.commit("one");
    repo.write("c.txt", "c\n");
    repo.commit("two");
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("base");
    g.cmd(Key::E);
    g.idle(2);
    g.until_quiet("the planner's range");
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    let message = |g: &Gx| {
        let model = g.view().planner_model().expect("the planner");
        model.rows[0].message.clone()
    };
    g.fx.typed("r");
    g.idle(1);
    g.key(Key::Enter);
    g.idle(2);
    assert!(
        g.shows("Enter keeps it"),
        "the planner's question: {:#?}",
        g.texts()
    );
    assert_eq!(message(&g), None, "no message kept yet");
    let tx = a_run_that_ended(
        &mut g,
        i,
        git_host::RunEnd::NotRun {
            title: "Push".into(),
            why: "the remote refused it".into(),
            line: None,
        },
    );
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Enter), Modifiers::NONE);
    g.idle(2);
    assert!(g.b().git.panels[i].running.is_empty(), "its end heard");
    assert!(g.shows("the remote refused it"), "the result up");
    assert!(g.shows("Enter keeps it"), "the planner's question stays");
    assert_eq!(message(&g), None, "the Enter kept no message");
    drop(tx);
}
