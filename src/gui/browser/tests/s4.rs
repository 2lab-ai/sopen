//! Slice S4's scenarios (02 §8.1's table: 03 G-H1–G-H5, G-W1's planner) through the browser's own
//! window: the commit menu's history row as the loaded graph allows it (03 §7.8), F2 and F8 on a
//! commit (§9.1), Cmd/Ctrl+E and the planner over the git panel (§7.8.2) taking its keys before
//! the git context's rows, the quick edits' dialogs (§7.8.1), the run on a worker under the
//! repository's write lock with the plan its confirmation showed (02 §6, 03 §8.1), the refresh
//! after it (03 §5), an edit stop's Amend and Split on the banner (§8.5, Cmd/Ctrl+Shift+M and P),
//! the split kept per window and its Continue, the window's undo (§8.3) and `SOPEN_STATE dialog=`
//! for §7.8's ids (02 §9).
//!
//! The plans are S4-A's (`git::tests::rebase`, through `LocalGit` and `ShellGit`), the screens
//! S4-B's (`git_panel::tests_s4`). These drive the wiring: real keys and typing, the real
//! planner and runner, a repository of the test's own (`git::history::fixture`). They skip with
//! a note where there is no git, and fail under `SOPEN_REQUIRE_GIT=1` (CI).

use super::s2::{small, Gx};
use super::s3::script;
use super::*;
use crate::git::actions::ActionId;
use crate::git::history::fixture::{self, Repo};
use crate::git::rebase::{Edit, EditId, Model, Verb};
use crate::gui::testkit;

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
    /// The open quick edit's id (03 §7.8.1).
    fn edit_id(&self) -> Option<EditId> {
        match self.view().edit_draft()? {
            Edit::Quick(q) => Some(q.id()),
            _ => None,
        }
    }

    /// The planner's model while it is open.
    fn plan_model(&self) -> Option<&Model> {
        self.view().planner_model()
    }

    /// The planner's rows, newest first: subject and verb.
    fn plan_rows(&self) -> Vec<(String, Verb)> {
        self.plan_model().map_or_else(Vec::new, |m| {
            m.rows
                .iter()
                .map(|r| (r.commit.row.subject.clone(), r.verb))
                .collect()
        })
    }

    /// Typing `text` into the field that has the typing: all of it selected first (Cmd/Ctrl+A).
    fn replace_typed(&mut self, text: &str) {
        self.fx
            .drv
            .key(&mut self.fx.b, Key::A, testkit::command(false));
        self.idle(1);
        self.fx.typed(text);
        self.idle(2);
    }

    /// Shift+Enter: a new line in a message field.
    fn new_line(&mut self) {
        self.with(Key::Enter, Modifiers::SHIFT);
        self.idle(1);
    }

    /// Cmd/Ctrl+E on the bar's commit, then the range's reads: the planner.
    fn open_planner_here(&mut self) {
        self.cmd(Key::E);
        self.idle(2);
        self.until_quiet("the planner's range");
        assert!(
            self.plan_model().is_some(),
            "the planner: {} {:#?}",
            self.status(),
            self.texts()
        );
    }

    /// `git log --format=<format>` of `rev`, newest first, a line each.
    fn log_of(&self, format: &str, rev: &str) -> Vec<String> {
        self.repo
            .run(&["log", &format!("--format={format}"), rev])
            .lines()
            .map(String::from)
            .collect()
    }

    /// A commit's message as stored (`cat-file commit`), byte for byte.
    fn stored_message(&self, rev: &str) -> Vec<u8> {
        let out = std::process::Command::new(&self.repo.git)
            .current_dir(&self.repo.top)
            .envs(fixture::env(&self.repo.home()))
            .args(["cat-file", "commit", rev])
            .output()
            .unwrap();
        let raw = out.stdout;
        let at = raw.windows(2).position(|w| w == b"\n\n").unwrap();
        raw[at + 2..].to_vec()
    }
}

/// G-H1's history: `base`, `one`, `two`, `three` (empty), `four`, `five` on `main`; a clean
/// work tree.
fn five(git: &std::path::Path) -> Repo {
    let repo = Repo::new(git);
    repo.write("base.txt", "base\n");
    repo.commit("base");
    for (file, subject) in [("one.txt", "one"), ("two.txt", "two")] {
        repo.write(file, format!("{subject}\n"));
        repo.commit(subject);
    }
    repo.commit("three");
    for (file, subject) in [("four.txt", "four"), ("five.txt", "five")] {
        repo.write(file, format!("{subject}\n"));
        repo.commit(subject);
    }
    repo
}

/// What the history row may offer, as the loaded graph says (S4-B review 1's `Eligibility`),
/// with a merge in range: on `small`'s history — `first`, `second`, the merge of `feature`
/// (`feature work` its second parent), `third` (HEAD) — HEAD's line holds the merge, so
/// everything below it offers only the planner, and `third`, right above it, no edit that
/// rewrites from below it. The menu draws exactly that; the status row counts the history row.
#[test]
fn the_history_row_follows_the_loaded_graph_with_a_merge_in_range() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    let eligible = |g: &Gx, subject: &str| {
        let oid = crate::git::model::Oid::parse(&g.repo.rev(&format!(":/^{subject}"))).unwrap();
        let e = g.view().eligibility(&oid);
        (e.ancestor, e.linear, e.below_merge)
    };
    assert_eq!(
        eligible(&g, "third"),
        (true, true, true),
        "HEAD, above the merge"
    );
    assert_eq!(
        eligible(&g, "Merge branch"),
        (true, false, false),
        "the merge itself"
    );
    assert_eq!(
        eligible(&g, "second"),
        (true, false, false),
        "below the merge"
    );
    assert_eq!(
        eligible(&g, "feature work"),
        (true, false, false),
        "the merge's second parent"
    );
    assert_eq!(eligible(&g, "first"), (true, false, false), "the root");
    // `third`: its quick edits (03 §7.8.1), none that rewrites from below the merge (squash,
    // fixup, move down), nothing above HEAD (move up, plan), no fold with nothing staged.
    g.bar_to("third");
    g.key(Key::F10);
    g.idle(2);
    for item in [
        "Reword…",
        "Author and date…",
        "Drop…",
        "Stop here to edit…",
        "Split…",
    ] {
        assert!(g.painted(item), "{item}: {:#?}", g.texts());
    }
    for item in [
        "Squash into the commit below…",
        "Fix up into the commit below",
        "Move up",
        "Move down",
        "Edit history from here…",
        "Fold the staged changes into this commit…",
    ] {
        assert!(!g.painted(item), "{item} is not offered on third");
    }
    assert!(g.painted("Tag…"), "S3's items stay first");
    assert!(
        g.status().contains("18 items"),
        "13 of S3's and 5 history edits: {}",
        g.status()
    );
    g.esc();
    // The merge, and below it: only the planner, which names the merge (G-H4).
    for subject in ["Merge branch 'feature'", "second"] {
        g.bar_to(subject);
        g.key(Key::F10);
        g.idle(2);
        assert!(g.painted("Edit history from here…"), "{subject}");
        assert!(!g.painted("Reword…") && !g.painted("Drop…"), "{subject}");
        g.esc();
    }
}

/// G-K1's S4 half (03 §9.1): on a commit the history row offers them, F2 opens `reword`'s
/// dialog — HEAD's form, the message prefilled — and F8 `drop`'s, each with its plan; the F-key
/// row shows both slots there.
#[test]
fn g_k1_f2_and_f8_on_a_commit_are_reword_and_drop() {
    let git = git_or_skip!();
    let mut g = Gx::new(five(&git));
    g.ready_in_git();
    g.bar_to("four");
    assert!(g.slots().contains(&(2, "Reword")), "{:?}", g.slots());
    assert!(g.slots().contains(&(8, "Delete")), "{:?}", g.slots());
    g.f(Key::F2);
    assert_eq!(g.edit_id(), Some(EditId::Reword));
    g.idle(4);
    assert!(g.shows("four"), "the message prefilled: {:#?}", g.texts());
    assert!(
        g.shows("rebase -i --reschedule-failed-exec"),
        "an older commit's reword is a run: {:#?}",
        g.texts()
    );
    g.esc();
    assert!(g.edit_id().is_none() && !g.view().dialog_up());
    g.f(Key::F8);
    assert_eq!(g.edit_id(), Some(EditId::Drop));
    g.idle(4);
    assert!(g.shows("backup heads/main"), "{:#?}", g.texts());
    g.esc();
    // HEAD: reword is an amend, no rebase.
    g.bar_to("five");
    g.f(Key::F2);
    assert_eq!(g.edit_id(), Some(EditId::Reword));
    g.idle(4);
    assert!(
        g.shows("git commit --amend --only --allow-empty --cleanup=verbatim -F -"),
        "{:#?}",
        g.texts()
    );
    g.esc();
}

/// G-H1 through the window (03 §11; S4-A's `g_h1_*` runs every byte through `LocalGit` and
/// `ShellGit`): Cmd/Ctrl+E on `base` opens the planner over the five commits; typed letters set
/// verbs and never start Find; drop the 5th; move the 4th below the 3rd; change the 4th's author
/// (its email) and committer date; reword the empty 3rd with a message of quotes, `\n` as text,
/// `%`, `$`, backquotes, Hangul, a `#` line, a blank line and trailing spaces; squash the 2nd into
/// the 1st with a new message → Cmd/Ctrl+Enter shows the run's confirmation and its reads, Enter
/// runs it once: the log shows exactly that, each message byte for byte; the planner closes; the
/// status row names the backup and Cmd/Ctrl+Z. `sequence.editor = false` in the repository's
/// config does not stop it.
#[test]
fn g_h1_one_planner_run_of_every_verb_through_the_window() {
    let git = git_or_skip!();
    let repo = five(&git);
    repo.run(&["config", "sequence.editor", "false"]);
    repo.run(&["config", "core.editor", "false"]);
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("base");
    g.open_planner_here();
    let subjects: Vec<String> = g.plan_rows().into_iter().map(|(s, _)| s).collect();
    assert_eq!(subjects, ["five", "four", "three", "two", "one"]);
    assert!(g.shows("pick"), "the verbs drawn: {:#?}", g.texts());
    // The F-key row while the planner is open: the window's slots only.
    let row: Vec<u8> = g.slots().iter().map(|s| s.0).collect();
    assert_eq!(row, [1, 9], "{:?}", g.slots());
    // `d` on five: a verb, not Find.
    g.fx.typed("d");
    g.idle(2);
    assert!(g.view().find_text().is_none(), "no Find");
    assert_eq!(g.plan_rows()[0].1, Verb::Drop);
    // four below three.
    g.key(Key::ArrowDown);
    g.with(Key::ArrowDown, Modifiers::COMMAND);
    g.idle(2);
    let subjects: Vec<String> = g.plan_rows().into_iter().map(|(s, _)| s).collect();
    assert_eq!(subjects, ["five", "three", "four", "two", "one"]);
    // four's author: its email, and the committer date "now".
    g.fx.typed("a");
    g.idle(2);
    assert!(g.view().dialog_up(), "the author dialog");
    g.key(Key::Tab);
    g.idle(1);
    g.replace_typed("bea@example.com");
    g.click("now");
    g.key(Key::Enter);
    g.idle(2);
    assert!(!g.view().dialog_up(), "{:#?}", g.texts());
    let four = &g.plan_model().unwrap().rows[2];
    assert_eq!(
        four.author.as_ref().map(|a| a.email.as_str()),
        Some("bea@example.com")
    );
    // three (empty): reword with a hard message.
    g.key(Key::ArrowUp);
    g.fx.typed("r");
    g.idle(1);
    g.key(Key::Enter);
    g.idle(2);
    assert!(g.view().dialog_up(), "the message editor");
    let first_line = "a \"quoted\" \\n 100% $HOME `x` 한글";
    g.replace_typed(first_line);
    g.new_line();
    g.new_line();
    g.fx.typed("# hash");
    g.new_line();
    g.new_line();
    g.fx.typed("trailing   ");
    g.idle(1);
    g.key(Key::Enter);
    g.idle(2);
    let hard = format!("{first_line}\n\n# hash\n\ntrailing   ");
    assert_eq!(
        g.plan_model().unwrap().rows[1].message.as_deref(),
        Some(hard.as_str())
    );
    // two squashed into one, the group's message new.
    g.key(Key::ArrowDown);
    g.key(Key::ArrowDown);
    g.fx.typed("s");
    g.idle(1);
    g.key(Key::Enter);
    g.idle(2);
    assert!(g.view().dialog_up(), "the group's message");
    g.replace_typed("one and two");
    g.key(Key::Enter);
    g.idle(2);
    let verbs: Vec<Verb> = g.plan_rows().into_iter().map(|(_, v)| v).collect();
    assert_eq!(
        verbs,
        [
            Verb::Drop,
            Verb::Reword,
            Verb::Pick,
            Verb::Squash,
            Verb::Pick
        ]
    );
    // The run's confirmation with its reads, then Enter.
    g.fx.drv.resize(1000.0, 1400.0);
    g.with(Key::Enter, Modifiers::COMMAND);
    g.idle(2);
    g.until_quiet("the run's reads");
    g.idle(4);
    assert!(
        g.shows("rebase -i --reschedule-failed-exec"),
        "the run's command: {:#?}",
        g.texts()
    );
    assert!(!g.shows("not read yet"), "{:#?}", g.texts());
    let before = g.repo.rev("HEAD");
    g.run();
    assert!(g.plan_model().is_none(), "the planner closed");
    assert_ne!(g.repo.rev("HEAD"), before, "it ran");
    assert_eq!(
        g.log_of("%s", "main"),
        [first_line, "four", "one and two", "base"],
        "{}",
        g.status()
    );
    assert_eq!(g.stored_message("main"), hard.as_bytes(), "byte for byte");
    assert_eq!(g.log_of("%ae", "main~1")[0], "bea@example.com");
    assert!(!g.repo.top.join("five.txt").exists(), "five dropped");
    // `GitEvent::WorkTreeChanged`: the list on the left reads the folder again.
    g.until("the list re-read", |b| {
        !b.panels[0].entries.iter().any(|e| e.name == "five.txt")
    });
    assert!(g.repo.top.join("two.txt").exists() && g.repo.top.join("one.txt").exists());
    assert_eq!(
        g.repo.run(&["diff-tree", "--no-commit-id", "-r", "main"]),
        "",
        "the empty commit stays empty"
    );
    let undoes = format!("{}Z undoes", keys::Mods::Command.prefix());
    assert!(
        g.status().contains(" · backup ") && g.status().ends_with(&undoes),
        "{}",
        g.status()
    );
}

/// The newest row moved (S4-C round 2): Cmd/Ctrl+↓ twice on the top row of the planner puts
/// `five` below `three`; the run's confirmation plans it (it is the owner's plan, not a moved
/// HEAD) and Enter runs it; the history is exactly that order, every file kept.
#[test]
fn the_newest_row_moved_down_runs_and_the_history_is_exact() {
    let git = git_or_skip!();
    let mut g = Gx::new(five(&git));
    g.fx.drv.resize(1000.0, 1400.0);
    g.ready_in_git();
    g.bar_to("base");
    g.open_planner_here();
    g.with(Key::ArrowDown, Modifiers::COMMAND);
    g.with(Key::ArrowDown, Modifiers::COMMAND);
    g.idle(2);
    let subjects: Vec<String> = g.plan_rows().into_iter().map(|(s, _)| s).collect();
    assert_eq!(subjects, ["four", "three", "five", "two", "one"]);
    g.with(Key::Enter, Modifiers::COMMAND);
    g.idle(2);
    g.until_quiet("the run's reads");
    g.idle(2);
    assert!(!g.shows("not the plan's newest commit"), "{:#?}", g.texts());
    g.key(Key::Enter);
    g.idle(2);
    assert!(g.plan_model().is_none(), "Enter ran it: {:#?}", g.texts());
    g.until_quiet("the run and its refresh");
    assert_eq!(
        g.log_of("%s", "main"),
        ["four", "three", "five", "two", "one", "base"],
        "{}",
        g.status()
    );
    for file in ["one.txt", "two.txt", "four.txt", "five.txt"] {
        assert!(g.repo.top.join(file).exists(), "{file} kept");
    }
}

/// G-H2 through the window: a plan that drops a commit another one needs stops on a conflict —
/// the banner `REBASE … conflict`; the file resolved and marked resolved from its menu, the
/// banner's Continue (Cmd/Ctrl+Shift+C, through its dialog) finishes the plan and its message
/// line still applies. A second plan whose reorder conflicts → the banner's Abort → HEAD back.
#[test]
fn g_h2_a_conflict_then_continue_then_a_reorder_aborted() {
    let git = git_or_skip!();
    let repo = Repo::new(&git);
    repo.write("c.txt", "1\n");
    repo.commit("base");
    repo.write("c.txt", "2\n");
    repo.commit("A");
    repo.write("c.txt", "3\n");
    repo.commit("B");
    repo.write("c.txt", "4\n");
    repo.commit("C");
    let mut g = Gx::new(repo);
    // Wide enough for the banner's whole line beside its buttons.
    g.fx.drv.resize(1800.0, 1400.0);
    g.ready_in_git();
    g.bar_to("base");
    g.open_planner_here();
    // rows: C, B, A — reword C, drop A.
    g.fx.typed("r");
    g.idle(1);
    g.key(Key::Enter);
    g.idle(2);
    g.replace_typed("C, reworded");
    g.key(Key::Enter);
    g.idle(2);
    g.key(Key::End);
    g.fx.typed("d");
    g.idle(2);
    g.with(Key::Enter, Modifiers::COMMAND);
    g.idle(2);
    g.until_quiet("the run's reads");
    g.run();
    assert!(g.painted("failed"), "the result dialog: {:#?}", g.texts());
    g.key(Key::Enter);
    g.idle(2);
    assert!(g.shows("REBASE"), "the banner: {:#?}", g.texts());
    assert!(g.shows("1 conflict"), "{:#?}", g.texts());
    // Resolve, mark resolved from the file's menu, Continue.
    g.repo.write("c.txt", "3\n");
    g.cmd(Key::R);
    g.until_quiet("the refresh");
    g.pane_on("c.txt");
    g.menu("Mark resolved…");
    assert_eq!(g.dialog_id(), Some(ActionId::Resolved));
    g.run();
    g.esc();
    g.cmd_shift(Key::C);
    g.until_quiet("Continue's dialog");
    assert_eq!(g.dialog_id(), Some(ActionId::Continue));
    g.run();
    assert!(!g.repo.top.join(".git/rebase-merge").exists(), "finished");
    assert_eq!(g.log_of("%s", "main"), ["C, reworded", "B", "base"]);
    // A second plan: C, the newest row, moved below B conflicts; Abort puts HEAD back.
    let head = g.repo.rev("HEAD");
    g.bar_to("base");
    g.open_planner_here();
    g.with(Key::ArrowDown, Modifiers::COMMAND);
    g.idle(2);
    let subjects: Vec<String> = g.plan_rows().into_iter().map(|(s, _)| s).collect();
    assert_eq!(subjects, ["B", "C, reworded"]);
    g.with(Key::Enter, Modifiers::COMMAND);
    g.idle(2);
    g.until_quiet("the run's reads");
    let generation = g.view().repo().map(|r| r.1);
    let refreshed = g.panel().refreshed;
    g.key(Key::Enter);
    g.idle(2);
    assert!(
        g.plan_model().is_none(),
        "Enter ran the plan: snapshot {generation:?} -> {:?}, refreshed since: {}, {} {:#?}",
        g.view().repo().map(|r| r.1),
        refreshed != g.panel().refreshed,
        g.status(),
        g.texts()
    );
    g.until_quiet("the run and its refresh");
    assert!(g.painted("failed"), "the result dialog: {:#?}", g.texts());
    g.key(Key::Enter);
    g.idle(2);
    assert!(g.shows("REBASE"), "the banner: {:#?}", g.texts());
    g.cmd_shift(Key::A);
    g.until_quiet("Abort's dialog");
    assert_eq!(g.dialog_id(), Some(ActionId::Abort));
    g.run();
    assert_eq!(g.repo.rev("HEAD"), head, "HEAD back");
    assert!(!g.repo.top.join(".git/rebase-merge").exists());
}

/// A commit that modifies `a.txt` and adds `new.txt`, between `base` and `top`; a clean tree.
fn splittable(git: &std::path::Path) -> Repo {
    let repo = Repo::new(git);
    repo.write("a.txt", "a\n");
    repo.commit("base");
    repo.write("a.txt", "a, changed\n");
    repo.write("new.txt", "new\n");
    repo.commit("both");
    repo.write("z.txt", "z\n");
    repo.commit("top");
    repo
}

/// G-H3's split through the window (03 §7.8.1 `split`, §8.5): `split` from the commit's menu
/// runs to an edit stop, where its Split's confirmation opens by itself (the paths recorded,
/// `reset --mixed`); the banner offers Amend and Split; Continue goes through
/// `Edit::ContinueSplit` and is refused while a recorded file still differs from HEAD — the
/// added file untracked included — and Enter runs nothing; committed file by file, Continue
/// finishes with both files in history, in two commits.
#[test]
fn g_h3_split_a_commit_and_continue_once_both_are_committed() {
    let git = git_or_skip!();
    let mut g = Gx::new(splittable(&git));
    g.fx.drv.resize(1000.0, 1400.0);
    g.ready_in_git();
    g.bar_to("both");
    g.menu("Split…");
    assert_eq!(g.edit_id(), Some(EditId::Split));
    g.run();
    // The run stopped to edit `both`; the Split's confirmation opened at the stop.
    g.until_quiet("the Split's reads");
    assert!(
        matches!(g.view().edit_draft(), Some(Edit::SplitStop { .. })),
        "{:?} {}",
        g.view().edit_draft(),
        g.status()
    );
    g.idle(4);
    assert!(g.shows("git reset --mixed "), "{:#?}", g.texts());
    g.run();
    assert!(g.shows("stopped to edit"), "{:#?}", g.texts());
    for button in ["Amend", "Split", "Continue", "Abort"] {
        assert!(g.painted(button), "{button}: {:#?}", g.texts());
    }
    let top = g.top();
    assert!(g.b().git.splits.contains_key(&top), "the split kept");
    // Continue: refused while a.txt and new.txt differ from HEAD.
    g.cmd_shift(Key::C);
    g.until_quiet("Continue's reads");
    assert!(
        matches!(g.view().edit_draft(), Some(Edit::ContinueSplit(_))),
        "{:?}",
        g.view().edit_draft()
    );
    g.idle(4);
    assert!(
        g.shows("Continue is refused while 2 files still differ from HEAD"),
        "{:#?}",
        g.texts()
    );
    g.key(Key::Enter);
    g.idle(4);
    assert!(g.repo.top.join(".git/rebase-merge").exists(), "nothing ran");
    g.esc();
    // a.txt committed alone: still refused (new.txt, untracked).
    g.repo.run(&["add", "a.txt"]);
    g.repo.run(&["commit", "-q", "-m", "the change"]);
    g.cmd(Key::R);
    g.until_quiet("the refresh");
    g.cmd_shift(Key::C);
    g.until_quiet("Continue's reads");
    g.idle(4);
    assert!(
        g.shows("Continue is refused while 1 file still differs from HEAD: new.txt"),
        "{:#?}",
        g.texts()
    );
    g.esc();
    g.repo.run(&["add", "new.txt"]);
    g.repo.run(&["commit", "-q", "-m", "the new file"]);
    g.cmd(Key::R);
    g.until_quiet("the refresh");
    g.cmd_shift(Key::C);
    g.until_quiet("Continue's reads");
    g.idle(4);
    assert!(!g.shows("Continue is refused"), "{:#?}", g.texts());
    g.run();
    assert!(!g.repo.top.join(".git/rebase-merge").exists(), "finished");
    assert_eq!(
        g.log_of("%s", "main"),
        ["top", "the new file", "the change", "base"]
    );
    assert!(!g.b().git.splits.contains_key(&top), "the split dropped");
}

/// The split is kept only while its own stop lasts (03 §7.8.1): split `both`, commit its parts
/// with other bytes, Continue — the next pick (`top`, which changes `a.txt` again) conflicts on a
/// recorded path; the banner shows a conflict, not the edit stop, so the kept split goes, and once
/// the file is resolved the banner's Continue is the plain one and finishes, never refused as the
/// split's ("commit it first").
#[test]
fn a_kept_split_goes_once_its_stop_is_left() {
    let git = git_or_skip!();
    let repo = Repo::new(&git);
    repo.write("a.txt", "a\n");
    repo.commit("base");
    repo.write("a.txt", "a, changed\n");
    repo.write("new.txt", "new\n");
    repo.commit("both");
    repo.write("a.txt", "a, changed, top\n");
    repo.commit("top");
    let mut g = Gx::new(repo);
    g.fx.drv.resize(1800.0, 1400.0);
    g.ready_in_git();
    g.bar_to("both");
    g.menu("Split…");
    g.run();
    g.until_quiet("the Split's reads");
    g.run();
    let top = g.top();
    assert!(g.b().git.splits.contains_key(&top), "the split kept");
    // Its parts, a.txt with other bytes, then Continue: `top` conflicts on a.txt.
    g.repo.write("a.txt", "a, other\n");
    g.repo.run(&["add", "a.txt", "new.txt"]);
    g.repo.run(&["commit", "-q", "-m", "the parts"]);
    g.cmd(Key::R);
    g.until_quiet("the refresh");
    g.cmd_shift(Key::C);
    g.until_quiet("Continue's reads");
    assert!(
        matches!(g.view().edit_draft(), Some(Edit::ContinueSplit(_))),
        "{:?}",
        g.view().edit_draft()
    );
    g.run();
    assert!(g.painted("failed"), "the result dialog: {:#?}", g.texts());
    g.key(Key::Enter);
    g.idle(2);
    assert!(g.shows("1 conflict"), "{:#?}", g.texts());
    assert!(
        !g.b().git.splits.contains_key(&top),
        "the split went with its stop"
    );
    // Resolved and marked: the plain Continue, which finishes.
    g.repo.write("a.txt", "a, other, top\n");
    g.repo.run(&["add", "a.txt"]);
    g.cmd(Key::R);
    g.until_quiet("the refresh");
    g.cmd_shift(Key::C);
    g.until_quiet("Continue's dialog");
    assert_eq!(g.dialog_id(), Some(ActionId::Continue), "{:#?}", g.texts());
    g.run();
    assert!(!g.repo.top.join(".git/rebase-merge").exists(), "finished");
    assert_eq!(g.log_of("%s", "main"), ["top", "the parts", "base"]);
}

/// A conflict at the split's own stop keeps the split (S4-C review): a stash applied there
/// conflicts, the banner shows `· 1 conflict` (no edit stop on it) — the rebase is still stopped
/// at the split's `edit` line, so the kept split stays and the banner's Continue is still
/// `Edit::ContinueSplit`'s, refused while a recorded file differs.
#[test]
fn a_conflict_at_the_splits_stop_keeps_the_split() {
    let git = git_or_skip!();
    let mut g = Gx::new(splittable(&git));
    g.fx.drv.resize(1800.0, 1400.0);
    g.ready_in_git();
    g.bar_to("both");
    g.menu("Split…");
    g.run();
    g.until_quiet("the Split's reads");
    g.run();
    let top = g.top();
    assert!(g.b().git.splits.contains_key(&top), "the split kept");
    // a.txt committed, then a stash of it that conflicts with a later commit, applied.
    g.repo.run(&["add", "a.txt"]);
    g.repo.run(&["commit", "-q", "-m", "the change"]);
    g.repo.write("a.txt", "stashed\n");
    g.repo.run(&["stash", "push", "-q", "--", "a.txt"]);
    g.repo.write("a.txt", "committed\n");
    g.repo.run(&["commit", "-q", "-am", "another"]);
    let applied = std::process::Command::new(&g.repo.git)
        .current_dir(&g.repo.top)
        .envs(fixture::env(&g.repo.home()))
        .args(["stash", "apply"])
        .output()
        .unwrap();
    assert!(!applied.status.success(), "the stash conflicts");
    g.cmd(Key::R);
    g.until_quiet("the refresh");
    assert!(g.shows("1 conflict"), "{:#?}", g.texts());
    assert!(
        g.b().git.splits.contains_key(&top),
        "still at the split's stop: the split kept"
    );
    g.cmd_shift(Key::C);
    g.until_quiet("Continue's reads");
    assert!(
        matches!(g.view().edit_draft(), Some(Edit::ContinueSplit(_))),
        "{:?}",
        g.view().edit_draft()
    );
    g.idle(4);
    assert!(g.shows("Continue is refused while"), "{:#?}", g.texts());
    g.key(Key::Enter);
    g.idle(4);
    assert!(g.repo.top.join(".git/rebase-merge").exists(), "nothing ran");
}

/// The edit stop's two identities (S4-C review, round 3): a plan rewords an older commit and
/// stops to edit a later one, so git's `stopped-sha` and `done` line name the original commit
/// while HEAD is its replayed copy (measured, git 2.56.1). Split targets HEAD — the commit
/// checked out — and runs; the kept split survives a refresh (it remembers the stop by the
/// `done` line's commit); the banner's Continue is `Edit::ContinueSplit`'s, refused while a
/// recorded file differs, then finishes; the history holds the split's parts and every file is
/// tracked.
#[test]
fn split_at_a_stop_after_an_earlier_rewrite() {
    let git = git_or_skip!();
    let repo = Repo::new(&git);
    repo.write("a.txt", "a\n");
    repo.commit("base");
    repo.write("one.txt", "1\n");
    repo.commit("one");
    repo.write("a.txt", "a, changed\n");
    repo.write("new.txt", "new\n");
    let both = repo.commit("both");
    repo.write("z.txt", "z\n");
    repo.commit("top");
    let mut g = Gx::new(repo);
    g.fx.drv.resize(1800.0, 1400.0);
    g.ready_in_git();
    g.bar_to("base");
    g.open_planner_here();
    // rows: top, both, one — reword one, stop to edit both.
    g.key(Key::End);
    g.fx.typed("r");
    g.idle(1);
    g.key(Key::Enter);
    g.idle(2);
    g.replace_typed("one, reworded");
    g.key(Key::Enter);
    g.idle(2);
    g.key(Key::ArrowUp);
    g.fx.typed("e");
    g.idle(2);
    g.with(Key::Enter, Modifiers::COMMAND);
    g.idle(2);
    g.until_quiet("the run's reads");
    g.run();
    assert!(g.shows("stopped to edit"), "{:#?}", g.texts());
    let head = g.repo.rev("HEAD");
    assert_ne!(head, both, "both was replayed onto the reworded one");
    assert_eq!(
        std::fs::read_to_string(g.repo.top.join(".git/rebase-merge/stopped-sha"))
            .unwrap()
            .trim(),
        both,
        "git names the original"
    );
    // Split: HEAD's, planned and run.
    g.cmd_shift(Key::P);
    g.until_quiet("the Split's reads");
    g.idle(4);
    assert!(
        matches!(g.view().edit_draft(), Some(Edit::SplitStop { commit }) if commit.as_str() == head),
        "{:?}",
        g.view().edit_draft()
    );
    assert!(
        g.shows(&format!("Split {} both", &head[..8])),
        "the confirmation names HEAD: {:#?}",
        g.texts()
    );
    assert!(!g.shows("the edit stop is at"), "{:#?}", g.texts());
    g.run();
    assert_eq!(g.repo.rev("HEAD"), g.repo.rev(&format!("{head}^")), "reset");
    let top = g.top();
    assert!(g.b().git.splits.contains_key(&top), "the split kept");
    // A refresh keeps it; Continue is the split's.
    g.cmd(Key::R);
    g.until_quiet("the refresh");
    assert!(g.b().git.splits.contains_key(&top), "kept across a refresh");
    g.cmd_shift(Key::C);
    g.until_quiet("Continue's reads");
    assert!(
        matches!(g.view().edit_draft(), Some(Edit::ContinueSplit(_))),
        "{:?}",
        g.view().edit_draft()
    );
    g.idle(4);
    assert!(
        g.shows("Continue is refused while 2 files still differ from HEAD"),
        "{:#?}",
        g.texts()
    );
    g.esc();
    // Committed in parts, then Continue finishes.
    g.repo.run(&["add", "a.txt"]);
    g.repo.run(&["commit", "-q", "-m", "the change"]);
    g.repo.run(&["add", "new.txt"]);
    g.repo.run(&["commit", "-q", "-m", "the new file"]);
    g.cmd(Key::R);
    g.until_quiet("the refresh");
    g.cmd_shift(Key::C);
    g.until_quiet("Continue's reads");
    assert!(
        matches!(g.view().edit_draft(), Some(Edit::ContinueSplit(_))),
        "{:?}",
        g.view().edit_draft()
    );
    g.idle(4);
    assert!(!g.shows("Continue is refused"), "{:#?}", g.texts());
    g.run();
    assert!(!g.repo.top.join(".git/rebase-merge").exists(), "finished");
    assert_eq!(
        g.log_of("%s", "main"),
        ["top", "the new file", "the change", "one, reworded", "base"]
    );
    assert_eq!(
        g.repo.run(&["status", "--porcelain"]),
        "",
        "every file tracked"
    );
    assert!(g.repo.run(&["ls-files"]).lines().any(|l| l == "new.txt"));
}

/// G-H3's other rows through the window: `split` on the root commit is refused in its dialog
/// and Enter runs nothing; `edit` a commit, then the banner's Amend (Cmd/Ctrl+Shift+M: §7.6's
/// `amend` dialog, the stopped commit's message prefilled) amends it with a new message, and
/// Continue finishes; Cmd/Ctrl+Shift+P without an edit stop says so.
#[test]
fn g_h3_root_split_refused_and_edit_then_amend() {
    let git = git_or_skip!();
    let mut g = Gx::new(splittable(&git));
    g.fx.drv.resize(1000.0, 1400.0);
    g.ready_in_git();
    // Root: Split… refused.
    let head = g.repo.rev("HEAD");
    g.bar_to("base");
    g.menu("Split…");
    assert_eq!(g.edit_id(), Some(EditId::Split));
    g.idle(4);
    assert!(
        g.shows("is the root commit: it has no HEAD^ to split it from"),
        "{:#?}",
        g.texts()
    );
    g.key(Key::Enter);
    g.idle(4);
    assert_eq!(g.repo.rev("HEAD"), head, "nothing ran");
    g.esc();
    // Cmd/Ctrl+Shift+P with nothing in progress.
    g.cmd_shift(Key::P);
    assert!(
        g.view()
            .status_line()
            .text
            .contains("Split: nothing is in progress"),
        "{}",
        g.view().status_line().text
    );
    // edit `both`, then Amend at the stop.
    g.bar_to("both");
    g.menu("Stop here to edit…");
    assert_eq!(g.edit_id(), Some(EditId::Edit));
    g.run();
    assert!(g.shows("stopped to edit"), "{:#?}", g.texts());
    g.cmd_shift(Key::M);
    g.until_quiet("Amend's prefill");
    assert_eq!(g.dialog_id(), Some(ActionId::Amend));
    // The message field takes the typing once it shows (the dialog's own focus rule).
    g.click("edit it");
    g.idle(2);
    g.replace_typed("both, amended");
    g.idle(2);
    g.until_quiet("the amend's plan");
    g.run();
    assert_eq!(g.log_of("%s", "HEAD")[0], "both, amended");
    g.cmd_shift(Key::C);
    g.until_quiet("Continue's dialog");
    g.run();
    assert!(!g.repo.top.join(".git/rebase-merge").exists(), "finished");
    assert_eq!(g.log_of("%s", "main"), ["top", "both, amended", "base"]);
}

/// G-H3's last rows through the window: `fold` the staged change into an older commit (from its
/// menu, offered only with something staged); a quick `reword` of HEAD (F2) keeps a staged file
/// staged; a reword of a commit whose message is not UTF-8 is refused with the reason, and Enter
/// runs nothing.
#[test]
fn g_h3_fold_reword_head_and_a_message_not_utf8() {
    let git = git_or_skip!();
    let repo = splittable(&git);
    let mut g = Gx::new(repo);
    g.fx.drv.resize(1000.0, 1400.0);
    g.ready_in_git();
    // Nothing staged: no fold.
    g.bar_to("both");
    g.key(Key::F10);
    g.idle(2);
    assert!(!g.painted("Fold the staged changes into this commit…"));
    g.esc();
    // Stage a change to a.txt, fold it into `both`.
    g.repo.write("a.txt", "a, changed, folded\n");
    g.repo.run(&["add", "a.txt"]);
    g.cmd(Key::R);
    g.until_quiet("the refresh");
    g.bar_to("both");
    g.menu("Fold the staged changes into this commit…");
    assert_eq!(g.edit_id(), Some(EditId::Fold));
    // What a run that did not happen leaves, for the report (seen once under load, S4-C): the
    // dialog still open (Enter while a refresh had taken its reads) or closed with nothing run.
    let refreshed = g.panel().refreshed;
    let generation = g.view().repo().map(|r| r.1);
    g.run();
    assert!(
        g.edit_id().is_none(),
        "Enter ran the fold: snapshot {generation:?} -> {:?}, refreshed since: {}, {} {:#?}",
        g.view().repo().map(|r| r.1),
        refreshed != g.panel().refreshed,
        g.status(),
        g.texts()
    );
    assert_eq!(
        g.log_of("%s", "main"),
        ["top", "both", "base"],
        "{} {:#?}",
        g.status(),
        g.texts()
    );
    assert_eq!(
        g.repo.run(&["show", "main~1:a.txt"]),
        "a, changed, folded\n",
        "folded into both: {} running {}",
        g.status(),
        g.panel().running.len()
    );
    assert_eq!(g.repo.run(&["status", "--porcelain"]), "", "nothing left");
    // Reword of HEAD with a staged file: it stays staged.
    g.repo.write("z.txt", "z, staged\n");
    g.repo.run(&["add", "z.txt"]);
    g.cmd(Key::R);
    g.until_quiet("the refresh");
    g.bar_to("top");
    g.f(Key::F2);
    assert_eq!(g.edit_id(), Some(EditId::Reword));
    g.replace_typed("top, reworded");
    g.until_quiet("the reword's plan");
    g.run();
    assert_eq!(g.log_of("%s", "main")[0], "top, reworded");
    assert_eq!(g.repo.run(&["status", "--porcelain"]), "M  z.txt\n");
    // A message that is not UTF-8 (ISO-8859-1 bytes under its encoding header).
    let msg = g.repo.tmp.path().join("msg");
    std::fs::write(&msg, b"caf\xe9\n").unwrap();
    g.repo.run(&[
        "-c",
        "i18n.commitEncoding=ISO-8859-1",
        "commit",
        "-q",
        "-F",
        msg.to_str().unwrap(),
    ]);
    let head = g.repo.rev("HEAD");
    g.cmd(Key::R);
    g.until_quiet("the refresh");
    let subjects = g.subjects();
    g.bar_to(&subjects[0]);
    g.f(Key::F2);
    assert_eq!(g.edit_id(), Some(EditId::Reword));
    g.idle(4);
    assert!(
        g.shows("sopen rewrites UTF-8 only") || g.shows("its message is not valid UTF-8"),
        "{:#?}",
        g.texts()
    );
    g.key(Key::Enter);
    g.idle(4);
    assert_eq!(g.repo.rev("HEAD"), head, "nothing ran");
}

/// G-H4 through the window: from below a merge, Edit history from here… opens the planner, which
/// names the merge as a check that fails, and its run is refused (Enter runs nothing); plain
/// Rebase over the same range lists the merge as flattened, and its "Plan the commits first…"
/// opens the planner over the commits it would move instead of running.
#[test]
fn g_h4_a_merge_in_the_range_refused_and_rebase_lists_it() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.fx.drv.resize(1000.0, 1400.0);
    g.ready_in_git();
    let merge = g.repo.rev("v1.0^{commit}");
    let short = &merge[..8];
    g.bar_to("first");
    g.open_planner_here();
    g.idle(2);
    let named = format!("the range holds 1 merge commit: {short}");
    assert!(g.shows(&named), "{named}: {:#?}", g.texts());
    let head = g.repo.rev("HEAD");
    g.with(Key::Enter, Modifiers::COMMAND);
    g.idle(2);
    g.until_quiet("the run's reads");
    g.key(Key::Enter);
    g.idle(4);
    assert_eq!(g.repo.rev("HEAD"), head, "refused: nothing ran");
    g.esc();
    g.esc();
    g.idle(2);
    assert!(g.plan_model().is_none(), "closed: {:#?}", g.texts());
    // Plain Rebase onto `first`: the merge is listed as flattened.
    g.bar_to("first");
    g.f(Key::F6);
    assert_eq!(g.dialog_id(), Some(ActionId::Rebase));
    g.idle(4);
    assert!(g.shows(short), "the merge listed: {:#?}", g.texts());
    g.click(git_panel::PLAN_FIRST);
    g.until_quiet("the planner's range");
    assert!(g.dialog_id().is_none(), "nothing ran");
    let subjects: Vec<String> = g.plan_rows().into_iter().map(|(s, _)| s).collect();
    assert!(
        subjects.contains(&"third".to_string()) && subjects.contains(&"second".to_string()),
        "{subjects:?}"
    );
    assert!(g.shows(&named), "{:#?}", g.texts());
    assert_eq!(g.repo.rev("HEAD"), head);
    // By the keyboard: Enter while "Plan the commits first…" has the focus is its click, never
    // the rebase's Run (03 §9.2: inside a dialog Enter runs; the focused button decides).
    g.esc();
    g.idle(2);
    assert!(g.plan_model().is_none(), "closed");
    g.bar_to("first");
    g.f(Key::F6);
    assert_eq!(g.dialog_id(), Some(ActionId::Rebase));
    g.idle(2);
    let button = g.view().plan_first_button().expect("the button drawn");
    g.fx.drv.ctx.memory_mut(|m| m.request_focus(button));
    g.idle(2);
    g.key(Key::Enter);
    g.idle(2);
    g.until_quiet("the planner's range");
    assert!(g.dialog_id().is_none(), "the rebase dialog ended");
    assert!(
        g.plan_model().is_some(),
        "the planner opened: {:#?}",
        g.texts()
    );
    assert_eq!(g.repo.rev("HEAD"), head, "no rebase ran");
}

/// G-H5 through the window: with "Move other branches along" (`--update-refs`) on in the run's
/// confirmation, a plan that drops a commit below another branch's tip moves that branch to its
/// rewritten commit, and the group backs it up; a branch checked out in a linked worktree is
/// named in the plan view and stays; Cmd/Ctrl+Z restores both branches.
#[test]
fn g_h5_update_refs_moves_a_branch_and_undo_puts_both_back() {
    let git = git_or_skip!();
    let repo = Repo::new(&git);
    repo.write("base.txt", "base\n");
    repo.commit("base");
    repo.write("one.txt", "1\n");
    repo.commit("one");
    repo.write("two.txt", "2\n");
    repo.commit("two");
    repo.run(&["branch", "mid"]);
    repo.run(&["branch", "wt"]);
    let elsewhere = repo.tmp.path().join("linked");
    repo.run(&["worktree", "add", "-q", elsewhere.to_str().unwrap(), "wt"]);
    repo.write("three.txt", "3\n");
    repo.commit("three");
    let (main, mid, wt) = (repo.rev("main"), repo.rev("mid"), repo.rev("wt"));
    let mut g = Gx::new(repo);
    g.fx.drv.resize(1000.0, 1400.0);
    g.ready_in_git();
    g.bar_to("base");
    g.open_planner_here();
    g.key(Key::End);
    g.fx.typed("d");
    g.idle(2);
    g.with(Key::Enter, Modifiers::COMMAND);
    g.idle(2);
    g.until_quiet("the run's reads");
    g.click(git_panel::UPDATE_REFS);
    g.until_quiet("the run's reads, branches moved");
    g.idle(4);
    assert!(
        g.shows("wt is checked out in another worktree; it stays where it is"),
        "{:#?}",
        g.texts()
    );
    g.run();
    assert_eq!(g.log_of("%s", "main"), ["three", "two", "base"]);
    assert_eq!(g.log_of("%s", "mid"), ["two", "base"], "mid moved along");
    assert_eq!(g.repo.rev("wt"), wt, "wt stays");
    let backups = g.backups();
    assert!(
        backups.contains("/heads/main") && backups.contains("/heads/mid"),
        "{backups}"
    );
    g.cmd(Key::Z);
    g.until_quiet("the restore's entries");
    assert_eq!(g.dialog_id(), Some(ActionId::Restore));
    g.run();
    assert_eq!(g.repo.rev("main"), main, "main back");
    assert_eq!(g.repo.rev("mid"), mid, "mid back");
}

/// `SOPEN_STATE dialog=` for 03 §7.8.1's ids (02 §9, G-S4's history half): each id asked on a
/// commit whose history row offers it opens its quick edit's dialog with its reads in (`plan`:
/// the planner); asked where it is not offered (`move-up` on HEAD), the status row says so; the
/// parser takes the ids after `ActionId`'s.
#[test]
fn sopen_state_dialog_takes_the_history_edit_ids() {
    let git = git_or_skip!();
    let mut g = Gx::new(five(&git));
    g.fx.b.git.script = GitScript {
        pin: true,
        ..GitScript::default()
    };
    g.wait_for_the_panel();
    g.until_quiet("the pin");
    for id in EditId::ALL {
        // `two` is HEAD~3 (a parent and a grandparent below it), `four` HEAD~1.
        let select = match id {
            EditId::Fold => continue,
            EditId::Plan | EditId::MoveDown | EditId::Squash | EditId::Fixup => "HEAD~3",
            _ => "HEAD~1",
        };
        g.fx.b.git.script = GitScript {
            select: Some(select.into()),
            dialog: Some(crate::gui::DialogId::Edit(id)),
            ..GitScript::default()
        };
        g.idle(2);
        g.until("the script", |b| b.git.script.done());
        g.until_quiet(id.as_str());
        let said = g.view().status_line().text;
        match id {
            EditId::Plan => assert!(g.plan_model().is_some(), "plan: {said}"),
            _ => {
                assert_eq!(g.edit_id(), Some(id), "{}: {said}", id.as_str());
                assert!(!g.shows("not read yet"), "{}: its reads came", id.as_str());
            }
        }
        for _ in 0..3 {
            if g.view().dialog_up() || g.plan_model().is_some() {
                g.esc();
            }
        }
        g.until_quiet("closed");
        assert!(g.b().git.pin.is_some(), "{}: still pinned", id.as_str());
    }
    g.fx.b.git.script = GitScript {
        select: Some("HEAD".into()),
        dialog: Some(crate::gui::DialogId::Edit(EditId::MoveUp)),
        ..GitScript::default()
    };
    g.idle(2);
    g.until("the script", |b| b.git.script.done());
    g.until_quiet("move-up on HEAD");
    assert!(
        g.view()
            .status_line()
            .text
            .contains("dialog=move-up: not offered"),
        "{}",
        g.view().status_line().text
    );
    let state = crate::gui::State::parse("git=pin;select=light;dialog=drop").unwrap();
    assert_eq!(state.dialog, Some(crate::gui::DialogId::Edit(EditId::Drop)));
    assert_eq!(
        crate::gui::State::parse("dialog=reset").unwrap().dialog,
        Some(crate::gui::DialogId::Action(ActionId::Reset))
    );
}

/// S3-C's read rule for S4's reads (a stand-in git that fails two reads): a read that fails is
/// never recorded and never asked again for the snapshot. The planner's range (`log … <base>..HEAD`)
/// failing: Cmd/Ctrl+E opens no planner and says why. A reword's `cat-file commit` failing: its
/// dialog says `not read yet: …`, the status row says why, Enter runs nothing; a run planned on
/// the worker (no plan shown) is not run either.
#[test]
fn a_failing_read_opens_nothing_and_runs_nothing() {
    let git = git_or_skip!();
    let mut repo = five(&git);
    let base = repo.rev("HEAD~5");
    let (range_log, raw_log) = (
        repo.tmp.path().join("range.log"),
        repo.tmp.path().join("raw.log"),
    );
    let wrapper = repo.tmp.path().join("git-failing");
    script(
        &wrapper,
        &format!(
            "case \" $* \" in *\" log -z --topo-order \"*\" {base}..HEAD \"*) echo x >> '{}'; echo 'fatal: a range that fails' >&2; exit 128;; *\" cat-file commit \"*) echo x >> '{}'; echo 'fatal: a raw read that fails' >&2; exit 128;; esac\nexec '{}' \"$@\"",
            range_log.display(),
            raw_log.display(),
            git.display()
        ),
    );
    repo.git = wrapper;
    let head = repo.rev("HEAD");
    let mut g = Gx::new(repo);
    g.fx.drv.resize(1000.0, 1400.0);
    g.ready_in_git();
    let tries = |file: &std::path::Path| {
        std::fs::read_to_string(file)
            .map(|t| t.lines().count())
            .unwrap_or(0)
    };
    // The planner's range.
    g.bar_to("base");
    g.cmd(Key::E);
    g.idle(2);
    g.until_quiet("the range's read");
    assert!(g.plan_model().is_none(), "no planner");
    assert!(g.shows("a range that fails"), "said: {:#?}", g.texts());
    assert_eq!(tries(&range_log), 1);
    g.esc();
    // A reword's raw read.
    g.bar_to("four");
    g.f(Key::F2);
    assert_eq!(g.edit_id(), Some(EditId::Reword));
    g.idle(4);
    assert!(
        g.shows("not read yet: git cat-file commit"),
        "the dialog says so: {:#?}",
        g.texts()
    );
    assert!(
        g.status().contains("a read for the dialog failed")
            && g.status().contains("a raw read that fails"),
        "{}",
        g.status()
    );
    g.key(Key::Enter);
    g.idle(4);
    assert_eq!(g.edit_id(), Some(EditId::Reword), "Enter ran nothing");
    assert_eq!(g.repo.rev("HEAD"), head);
    assert_eq!(tries(&raw_log), 1, "asked once for this snapshot");
    let edit = g.view().edit_draft().unwrap();
    g.esc();
    // Planned on the worker, no plan shown: the read fails there too, and nothing runs.
    let ctx = g.fx.drv.ctx.clone();
    let i = g.b().git.panel(g.repo.top()).unwrap();
    g.fx.b.start_edit(&ctx, i, edit);
    g.until_quiet("the run's reads");
    assert!(g.shows("a raw read that fails"), "{:#?}", g.texts());
    assert!(g.status().ends_with(": not run"), "{}", g.status());
    assert_eq!(g.repo.rev("HEAD"), head, "nothing ran");
}

/// A refresh while the git version is not known (Cmd/Ctrl+R forgets it until detection reads it
/// again) keeps the version the panel last had, never the 2.20 stand-in: with it every planner
/// run was refused ("history edits need git 2.22 … 2.20 (not known)", seen in G-H2 under load).
#[test]
fn a_refresh_before_detection_answers_keeps_the_git_version() {
    let git = git_or_skip!();
    let mut g = Gx::new(five(&git));
    g.ready_in_git();
    let known = g.view().repo().unwrap().0.version.clone();
    assert!(known.at_least(2, 22), "{}", known.text);
    // What Cmd/Ctrl+R does, the snapshot read landing before detection answers again.
    let host = g.b().git.host.clone();
    g.fx.b.git.detector.forget_host(&host);
    let ctx = g.fx.drv.ctx.clone();
    let i = g.b().git.panel(g.repo.top()).unwrap();
    g.fx.b.start_read(&ctx, i, true, 0);
    g.until_quiet("the refresh");
    let version = &g.view().repo().unwrap().0.version;
    assert_eq!(version.text, known.text, "the version kept");
    // A planner run plans.
    g.bar_to("base");
    g.open_planner_here();
    g.fx.typed("d");
    g.idle(2);
    g.with(Key::Enter, Modifiers::COMMAND);
    g.idle(2);
    g.until_quiet("the run's reads");
    g.idle(2);
    assert!(!g.shows("history edits need git 2.22"), "{:#?}", g.texts());
}

/// A range read for an older snapshot is dropped (S4-C review, round 3): the planner asked for,
/// then the repository refreshed before the range came — nothing opens, and the status row says
/// to open it again; asked again, it opens.
#[test]
fn a_range_read_before_a_refresh_opens_nothing() {
    let git = git_or_skip!();
    let mut g = Gx::new(five(&git));
    g.ready_in_git();
    let ctx = g.fx.drv.ctx.clone();
    let i = g.b().git.panel(g.repo.top()).unwrap();
    let base = crate::git::model::Oid::parse(&g.repo.rev("HEAD~5")).unwrap();
    let open = GitAction::OpenPlanner(crate::git::rebase::Base::Commit(base));
    g.fx.b.git_actions(&ctx, i, vec![open.clone()]);
    // A refresh's snapshot lands while the range is read.
    let repo = g.view().repo().unwrap().0.clone();
    g.panel_mut().view.set_repo(repo);
    g.until_quiet("the range's read");
    assert!(g.plan_model().is_none(), "nothing opened on the old range");
    assert!(
        g.view()
            .status_line()
            .text
            .contains("the repository changed while it was read — open it again"),
        "{}",
        g.view().status_line().text
    );
    g.fx.b.git_actions(&ctx, i, vec![open]);
    g.until_quiet("the range's read");
    assert!(g.plan_model().is_some(), "asked again, it opens");
}

/// S3-C's review rules for history edits: a run whose shown plan was lost (none was shown here)
/// and whose plan asks for a confirmation does not run — HEAD stays — and its confirmation
/// opens again; Enter there runs it; Cmd/Ctrl+Z then takes that run's group, the newest.
#[test]
fn a_history_edit_whose_shown_plan_was_lost_asks_again() {
    let git = git_or_skip!();
    let mut g = Gx::new(five(&git));
    g.fx.drv.resize(1000.0, 1400.0);
    g.ready_in_git();
    let head = g.repo.rev("HEAD");
    let four = crate::git::model::Oid::parse(&g.repo.rev("HEAD~1")).unwrap();
    let edit = Edit::Quick(crate::git::rebase::Quick::Drop { commit: four });
    let ctx = g.fx.drv.ctx.clone();
    let i = g.b().git.panel(g.repo.top()).unwrap();
    g.fx.b.start_edit(&ctx, i, edit.clone());
    g.until_quiet("the plan made again");
    assert_eq!(g.repo.rev("HEAD"), head, "nothing ran");
    assert_eq!(g.view().edit_draft(), Some(edit), "its confirmation again");
    assert!(
        g.view()
            .status_line()
            .text
            .contains("not run — its plan asks for a confirmation"),
        "{}",
        g.view().status_line().text
    );
    g.run();
    assert_eq!(g.log_of("%s", "main")[..2], ["five", "three"], "dropped");
    let top = g.top();
    let newest = g.b().git.undo[&top].newest().map(String::from);
    assert!(newest.is_some());
    g.cmd(Key::Z);
    g.until_quiet("the restore's entries");
    assert_eq!(g.dialog_id(), Some(ActionId::Restore));
    g.run();
    assert_eq!(g.repo.rev("HEAD"), head, "undone");
}

/// PR #90 review (P2): the uncommitted row's details pane follows the status of the refresh that
/// read it — Space stages `a.txt` from the pane, and after the run's refresh the pane lists it
/// under Staged, not under the changes not staged (the pane was built from the snapshot before
/// it was replaced).
#[test]
fn the_uncommitted_pane_follows_the_new_status() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    g.pane_on("a.txt");
    let group_of = |g: &Gx, path: &str| {
        g.panel().last_details.as_ref().and_then(|d| {
            d.groups
                .iter()
                .find(|fg| fg.files.iter().any(|f| f.path == path))
                .map(|fg| fg.group)
        })
    };
    assert_eq!(group_of(&g, "a.txt"), Some(git_panel::Group::NotStaged));
    g.key(Key::Space);
    g.idle(2);
    g.until_quiet("the stage and its refresh");
    let top = g.top();
    g.until("the pane's details", move |b| {
        b.git
            .panel(&top)
            .is_some_and(|i| b.git.panels[i].details.is_none())
    });
    assert_eq!(
        g.repo.run(&["diff", "--cached", "--name-only"]).trim(),
        "a.txt",
        "staged"
    );
    assert_eq!(
        group_of(&g, "a.txt"),
        Some(git_panel::Group::Staged),
        "{:?}",
        g.panel().last_details.as_ref().map(|d| &d.groups)
    );
}

/// PR #90 review (P2): a right click on a commit row is that commit's menu, even while the
/// details pane has the keyboard on one of the uncommitted row's files — the click takes the
/// keyboard back to the graph first, as a left click does.
#[test]
fn a_right_click_is_the_clicked_rows_menu_whatever_the_pane_held() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    g.pane_on("a.txt");
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
    assert!(
        g.status().starts_with("Menu of commit "),
        "the clicked commit's menu: {}",
        g.status()
    );
    assert!(g.painted("Tag…"), "{:#?}", g.texts());
}

/// The planner takes its keys before the git context's rows (03 §7.8.2, S4-0 keys §C): ↓ moves
/// its bar, not the graph's; a typed letter that is no verb starts no Find; F5 and F10 act on
/// nothing under it; F1 and Tab still do what they do; Esc closes it (nothing changed: no
/// question). The banner's keys — an edit stop's Cmd/Ctrl+Shift+M and P among them — do nothing
/// while a viewer covers the window or a git dialog is up (S3-C review): no dialog opens and
/// nothing is read or run.
#[test]
fn the_planner_takes_its_keys_first_and_the_banner_keys_wait() {
    let git = git_or_skip!();
    let mut g = Gx::new(five(&git));
    g.ready_in_git();
    g.bar_to("base");
    let graph_bar = g.view().bar_place();
    g.open_planner_here();
    g.key(Key::ArrowDown);
    g.idle(2);
    assert_eq!(g.view().bar_place(), graph_bar, "the graph's bar stayed");
    g.fx.typed("x");
    g.idle(2);
    assert!(g.view().find_text().is_none(), "no Find");
    for key in [Key::F5, Key::F10, Key::F8] {
        g.key(key);
        g.idle(2);
        assert!(
            g.dialog_id().is_none() && g.edit_id().is_none() && !g.view().dialog_up(),
            "{key:?}: nothing opened"
        );
    }
    // Enter on a pick row is the pane's: its hint, no details pane.
    g.key(Key::Enter);
    g.idle(2);
    assert_eq!(
        g.view().planner_note(),
        Some("Enter edits the message of a reword or squash row")
    );
    assert!(!g.view().details_open(), "the graph's Enter did not act");
    g.key(Key::Escape);
    g.idle(2);
    assert!(g.plan_model().is_none(), "Esc closed it");

    // The banner's keys at an edit stop, with a dialog up and with a viewer up.
    let stopped = Repo::new(&git);
    stopped.write("a.txt", "a\n");
    stopped.commit("base");
    stopped.write("b.txt", "b\n");
    let b = stopped.commit("second");
    stopped.write("c.txt", "c\n");
    stopped.commit("third");
    let editor = stopped.tmp.path().join("edit-todo");
    script(
        &editor,
        &format!("sed -i.bak 's/^pick {}/edit {}/' \"$1\"", &b[..7], &b[..7]),
    );
    let out = std::process::Command::new(&stopped.git)
        .current_dir(&stopped.top)
        .envs(fixture::env(&stopped.home()))
        .env("GIT_SEQUENCE_EDITOR", &editor)
        .args(["rebase", "-i", "HEAD~2"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut g = Gx::new(stopped);
    g.ready_in_git();
    assert!(g.shows("stopped to edit"), "{:#?}", g.texts());
    for button in ["Amend", "Split", "Continue", "Abort"] {
        assert!(g.painted(button), "{button}");
    }
    let cmd_shift = keys::Mods::CommandShift.prefix();
    assert!(
        g.status().starts_with(&format!(
            "{cmd_shift}M Amend · {cmd_shift}P Split · {cmd_shift}C Continue · {cmd_shift}A Abort"
        )),
        "the banner's keys on the status row: {}",
        g.status()
    );
    let banner_keys = [Key::C, Key::S, Key::A, Key::M, Key::P];
    // A dialog up: the history row's menu.
    g.key(Key::F10);
    g.idle(2);
    assert!(g.view().dialog_up());
    for key in banner_keys {
        g.cmd_shift(key);
        assert!(
            g.dialog_id().is_none() && g.edit_id().is_none(),
            "{key:?}: no dialog under the menu"
        );
        assert!(!g.panel().acting(), "{key:?}: nothing read");
    }
    g.esc();
    // A viewer over both sides: F3 on a file of the list.
    g.key(Key::Tab);
    g.settle();
    let at = g.b().panels[g.b().active]
        .entries
        .iter()
        .position(|e| e.name == "a.txt")
        .unwrap();
    g.fx.b.panels[g.fx.b.active].highlight = at;
    g.key(Key::F3);
    g.until("the viewer", |b| b.viewer.is_some());
    for key in banner_keys {
        g.cmd_shift(key);
        assert!(
            g.dialog_id().is_none() && g.edit_id().is_none(),
            "{key:?}: nothing under the viewer"
        );
        assert!(!g.panel().acting(), "{key:?}: nothing read");
    }
    assert!(g.b().viewer.is_some(), "the viewer stays");
    // Back in the panel, Cmd/Ctrl+Shift+P is the stop's Split, through its confirmation.
    g.key(Key::F3);
    g.until("the list", |b| b.viewer.is_none());
    let active = g.fx.b.active;
    g.fx.b.panels[active].highlight = 0;
    g.rest();
    g.wait_for_the_panel();
    g.tab_into_git();
    g.cmd_shift(Key::P);
    g.until_quiet("the Split's reads");
    assert!(
        matches!(g.view().edit_draft(), Some(Edit::SplitStop { .. })),
        "{:?}",
        g.view().edit_draft()
    );
    g.esc();
}
