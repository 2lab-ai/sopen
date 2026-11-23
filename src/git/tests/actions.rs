//! The actions' tests (03 §7.1–§7.7, §8.1, §8.4–§8.6; G-A1–G-A9, G-S2, G-S3, G-S5, G-S7, G-S8,
//! G-R5), each run through `LocalGit` and `ShellGit`, asserting the refs and files after, the
//! confirmation's command lines and the backup refs. The fixture here is shared with
//! `tests/backup.rs`.
//!
//! Every git here runs isolated from the account's own: a fixture `GIT_CONFIG_GLOBAL`, no system
//! config, a temporary `HOME`, none of the variables that point git at a repository. git missing
//! is a skip with a note, a failure under `SOPEN_REQUIRE_GIT=1` (CI).

use crate::git::actions::{
    execute, inspect, needs, parse_probe, plan, probe_command, probe_paths, probe_paths_command,
    valid_name, Action, ActionId, ArchiveFormat, Banner, ConfigScope, FastForward, MergeOptions,
    Outcome, Plan, Proceed, PullHow, PushOptions, RebaseOptions, Refused, Remote, Repo, ResetMode,
    Rev, Seen, StepOutcome,
};
use crate::git::backup;
use crate::git::model::{Oid, Operation, Sequence};
use crate::git::parse;
use crate::git::runner::{
    quote, Class, Ending, GitCommand, GitRunner, Handle, LocalGit, ShellGit, LOCAL_REPO_ENV,
};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// Fixtures (shared with tests/backup.rs)

/// git for the tests, or `None` with a note (a failure under `SOPEN_REQUIRE_GIT=1`).
pub(super) fn git_or_skip() -> Option<PathBuf> {
    match LocalGit::find() {
        Some(git) if Command::new(git.git()).arg("--version").output().is_ok() => {
            Some(git.git().to_path_buf())
        }
        _ if std::env::var_os("SOPEN_REQUIRE_GIT").is_some_and(|v| v == "1") => {
            panic!("SOPEN_REQUIRE_GIT=1 but git is not on PATH")
        }
        _ => {
            eprintln!("note: git is not on PATH; the actions' tests are skipped");
            None
        }
    }
}

/// A folder of repositories, with its own git config and home.
pub(super) struct Fx {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub config: PathBuf,
    pub git: PathBuf,
}

const CONFIG: &str = "[user]\n\tname = sopen test\n\temail = test@example.invalid\n\
                      [init]\n\tdefaultBranch = main\n[commit]\n\tgpgSign = false\n\
                      [tag]\n\tgpgSign = false\n[advice]\n\tdetachedHead = false\n";

pub(super) fn fixture() -> Option<Fx> {
    let git = git_or_skip()?;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let config = root.join("gitconfig");
    std::fs::write(&config, CONFIG).unwrap();
    Some(Fx {
        _tmp: tmp,
        root,
        home,
        config,
        git,
    })
}

impl Fx {
    pub fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(&self.git);
        c.arg("-C").arg(dir).args(args);
        for key in LOCAL_REPO_ENV {
            c.env_remove(key);
        }
        c.env("HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", &self.config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::null());
        c
    }

    /// Plain git in `dir`, for setting up and checking (not through the runner): its stdout.
    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.output(dir, args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Plain git, whatever its exit.
    pub fn output(&self, dir: &Path, args: &[&str]) -> Output {
        self.command(dir, args).output().unwrap()
    }

    /// A rev's full hash, or `None`.
    pub fn rev(&self, dir: &Path, rev: &str) -> Option<String> {
        let out = self.output(dir, &["rev-parse", "-q", "--verify", rev]);
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn oid(&self, dir: &Path, rev: &str) -> Oid {
        Oid::parse(&self.rev(dir, rev).unwrap_or_else(|| panic!("no {rev}"))).unwrap()
    }

    /// A new repository `name` with one commit of `README`.
    pub fn repo(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        self.git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("README"), "fixture\n").unwrap();
        self.git(&dir, &["add", "README"]);
        self.git(&dir, &["commit", "-q", "-m", "first"]);
        dir
    }

    /// A bare repository `name`.
    pub fn bare(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        self.git(&self.root, &["init", "-q", "--bare", dir.to_str().unwrap()]);
        dir
    }

    /// A clone of `from` as `name`.
    pub fn clone_of(&self, from: &Path, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        self.git(
            &self.root,
            &["clone", "-q", from.to_str().unwrap(), dir.to_str().unwrap()],
        );
        dir
    }

    /// Writes `path` with `content` and commits it (`add -f`, so an ignored path too): the new
    /// commit.
    pub fn commit(&self, dir: &Path, path: &str, content: &str, message: &str) -> Oid {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, content).unwrap();
        self.git(dir, &["add", "-f", "--", path]);
        self.git(dir, &["commit", "-q", "-m", message]);
        self.oid(dir, "HEAD")
    }

    /// `local` and `ShellGit` under `sh`; and where `/bin/dash` is not `/bin/sh` (macOS), a
    /// `ShellGit` whose login shell and `sh` are dash, so `<locate>`, the probe and the files
    /// snippets run under dash as on a Debian remote.
    pub fn lanes(&self) -> Vec<(String, GitRunner)> {
        let mut lanes = vec![
            ("local".to_string(), GitRunner::local(self.local_git())),
            ("sh".to_string(), GitRunner::shell(self.shell_git())),
        ];
        let dash = Path::new("/bin/dash");
        let sh = Path::new("/bin/sh").canonicalize().ok();
        if dash.exists() && dash.canonicalize().ok() != sh {
            let bin = self.root.join("dash-bin");
            if !bin.exists() {
                std::fs::create_dir_all(&bin).unwrap();
                std::os::unix::fs::symlink(dash, bin.join("sh")).unwrap();
            }
            let mut path = std::ffi::OsString::from(&bin);
            path.push(":");
            path.push(std::env::var_os("PATH").unwrap_or_default());
            let git = ShellGit::new(dash, "box")
                .with_env("HOME", &self.home)
                .with_env("GIT_CONFIG_GLOBAL", &self.config)
                .with_env("GIT_CONFIG_NOSYSTEM", "1")
                .with_env("PATH", path);
            lanes.push(("dash".to_string(), GitRunner::shell(git)));
        }
        lanes
    }

    pub fn local_git(&self) -> LocalGit {
        LocalGit::new(&self.git)
            .with_env("HOME", &self.home)
            .with_env("GIT_CONFIG_GLOBAL", &self.config)
            .with_env("GIT_CONFIG_NOSYSTEM", "1")
    }

    pub fn shell_git(&self) -> ShellGit {
        ShellGit::new("/bin/sh", "box")
            .with_env("HOME", &self.home)
            .with_env("GIT_CONFIG_GLOBAL", &self.config)
            .with_env("GIT_CONFIG_NOSYSTEM", "1")
    }

    /// The backup groups of `dir`: each stamp and its names under it (the `after` record left
    /// out: it is not a kind of 03 §8.2's table).
    pub fn groups(&self, dir: &Path) -> Vec<(String, Vec<String>)> {
        let out = self.git(
            dir,
            &["for-each-ref", "--format=%(refname)", "refs/sopen-backup/"],
        );
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        for line in out.lines() {
            let rest = line.strip_prefix("refs/sopen-backup/").unwrap();
            let (stamp, name) = rest.split_once('/').unwrap();
            if name == "after" {
                continue;
            }
            match groups.iter_mut().find(|(s, _)| s == stamp) {
                Some((_, names)) => names.push(name.to_string()),
                None => groups.push((stamp.to_string(), vec![name.to_string()])),
            }
        }
        groups
    }

    /// The newest group's stamp and names, sorted.
    pub fn newest(&self, dir: &Path) -> (String, Vec<String>) {
        let mut groups = self.groups(dir);
        groups.sort_by_key(|(s, _)| stamp_order(s));
        let (stamp, mut names) = groups.pop().expect("a backup group");
        names.sort();
        (stamp, names)
    }

    /// No staging ref is left.
    pub fn no_staging(&self, dir: &Path) {
        let left = self.git(dir, &["for-each-ref", "refs/sopen-staging/"]);
        assert_eq!(left, "", "staging refs left");
    }
}

/// A stamp's order: its time, then its `-N`.
pub(super) fn stamp_order(stamp: &str) -> (String, u32) {
    match stamp.split_once('-') {
        Some((t, n)) => (t.to_string(), n.parse().unwrap()),
        None => (stamp.to_string(), 1),
    }
}

/// A shell word for a fixture path in a test's own scripts.
pub(super) fn sh_word(path: &Path) -> String {
    String::from_utf8(quote(path.as_os_str().as_encoded_bytes())).unwrap()
}

/// Writes an executable script.
pub(super) fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn run_read(runner: &GitRunner, top: &str, args: &[&str]) -> Vec<u8> {
    let done = runner.run(
        &GitCommand::new(Class::Read, top, args.iter().copied()),
        &Handle::new(),
    );
    assert!(done.ok(), "{}: {}", done.entry.line, done.stderr);
    done.stdout
}

fn run_code(runner: &GitRunner, top: &str, args: &[&str]) -> (Option<i32>, Vec<u8>) {
    let done = runner.run(
        &GitCommand::new(Class::Read, top, args.iter().copied()),
        &Handle::new(),
    );
    let code = match done.ending {
        Ending::Exit(c) => Some(c),
        _ => None,
    };
    (code, done.stdout)
}

/// The repository as the panel's refresh reads it (03 §4.1's reads, §8.5's probe), through the
/// runner under test.
pub(super) fn repo_of(runner: &GitRunner, top: &Path) -> Repo {
    let top_s = top.to_str().unwrap().to_string();
    let t = top_s.as_str();
    let git_dir = String::from_utf8(run_read(runner, t, &["rev-parse", "--absolute-git-dir"]))
        .unwrap()
        .trim()
        .to_string();
    let version = parse::git_version(&run_read(runner, t, &["version"])).unwrap();
    let refs = parse::refs(&run_read(
        runner,
        t,
        &[
            "for-each-ref",
            "--format=%(objectname)%00%(objecttype)%00%(*objectname)%00%(refname)%00%(upstream)%00%(upstream:track)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    ))
    .unwrap();
    let symbolic = run_code(runner, t, &["symbolic-ref", "-q", "HEAD"]);
    let verify = run_code(runner, t, &["rev-parse", "-q", "--verify", "HEAD"]);
    let head = parse::head((symbolic.0, &symbolic.1), (verify.0, &verify.1)).unwrap();
    let stashes = parse::stashes(&run_read(
        runner,
        t,
        &[
            "stash",
            "list",
            "-z",
            "--format=%H%x1f%P%x1f%gd%x1f%ct%x1f%s",
        ],
    ))
    .unwrap();
    let status = parse::status(&run_read(
        runner,
        t,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=normal",
        ],
    ))
    .unwrap();
    let paths_done = runner.run(&probe_paths_command(t), &Handle::new());
    assert!(paths_done.ok(), "{}", paths_done.stderr);
    let paths = probe_paths(&paths_done.stdout).expect("seven paths");
    let probe = runner.run(&probe_command(t, &paths), &Handle::new());
    assert!(probe.ok(), "the probe: {}", probe.stderr);
    let in_progress = parse_probe(&probe.stdout);
    let remotes = String::from_utf8(run_read(runner, t, &["remote"]))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    Repo {
        top: top_s.clone(),
        git_dir,
        version,
        head,
        refs,
        stashes,
        status,
        in_progress,
        remotes,
        seen: Seen::default(),
    }
}

/// `repo` with every read `action` needs (a few rounds at most).
pub(super) fn ready(runner: &GitRunner, repo: &mut Repo, action: &Action) {
    for _ in 0..6 {
        let missing = needs(action, repo);
        if missing.is_empty() {
            return;
        }
        inspect(runner, &Handle::new(), repo, &missing)
            .unwrap_or_else(|e| panic!("inspect: {}", e.reason));
    }
    panic!("needs did not settle for {:?}", action.id());
}

/// The plan of `action` on `top` as it is now.
pub(super) fn planned(runner: &GitRunner, top: &Path, action: &Action) -> Result<Plan, Refused> {
    let mut repo = repo_of(runner, top);
    ready(runner, &mut repo, action);
    plan(action, &repo)
}

/// Plans and runs `action`: the plan and the outcome.
pub(super) fn run(runner: &GitRunner, top: &Path, action: &Action) -> (Plan, Outcome) {
    let plan = planned(runner, top, action)
        .unwrap_or_else(|r| panic!("{:?} refused: {}", action.id(), r.0));
    let outcome = execute(&plan, top.to_str().unwrap(), runner, &Handle::new());
    (plan, outcome)
}

/// Every step done.
#[track_caller]
pub(super) fn done(lane: &str, outcome: &Outcome) {
    for step in &outcome.steps {
        assert_eq!(
            step.outcome,
            StepOutcome::Done,
            "{lane}: {} — {}",
            step.line,
            step.ran
                .iter()
                .map(|d| d.error_text())
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }
}

/// The step `index` failed, those before it done, those after it not run: its error texts.
#[track_caller]
pub(super) fn failed_at(lane: &str, outcome: &Outcome, index: usize) -> String {
    for (i, step) in outcome.steps.iter().enumerate() {
        let want = match i.cmp(&index) {
            std::cmp::Ordering::Less => StepOutcome::Done,
            std::cmp::Ordering::Equal => StepOutcome::Failed,
            std::cmp::Ordering::Greater => StepOutcome::NotRun,
        };
        assert_eq!(step.outcome, want, "{lane}: step {i} {}", step.line);
    }
    outcome.steps[index]
        .ran
        .iter()
        .map(|d| d.error_text())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The confirmation's command lines.
pub(super) fn commands(plan: &Plan) -> Vec<String> {
    plan.confirm
        .as_ref()
        .map(|c| c.commands.clone())
        .unwrap_or_default()
}

/// The confirmation's backup names.
pub(super) fn backup_names(plan: &Plan) -> Vec<String> {
    plan.confirm
        .as_ref()
        .map(|c| c.backups.clone())
        .unwrap_or_default()
}

#[track_caller]
pub(super) fn has_line(plan: &Plan, line: &str) {
    let lines = commands(plan);
    assert!(
        lines.iter().any(|l| l == line),
        "no line {line:?} in {lines:#?}"
    );
}

/// A remote `origin` (bare, `name`.git) for `dir`, `main` pushed to it.
pub(super) fn with_origin(fx: &Fx, dir: &Path, name: &str) -> PathBuf {
    let bare = fx.bare(&format!("{name}.git"));
    fx.git(dir, &["remote", "add", "origin", bare.to_str().unwrap()]);
    fx.git(dir, &["push", "-q", "-u", "origin", "main"]);
    fx.git(dir, &["fetch", "-q", "origin"]);
    bare
}

fn files_of(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != ".git")
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------------------------
// The ids and names

/// 03 §7: every id is unique, spelled as 03 spells it, and comes back through `parse`.
#[test]
fn every_id_round_trips() {
    let mut seen = std::collections::HashSet::new();
    for id in ActionId::ALL {
        assert!(seen.insert(id.as_str()), "{} twice", id.as_str());
        assert_eq!(ActionId::parse(id.as_str()), Some(id));
    }
    assert_eq!(ActionId::parse("drop"), None, "S4's ids are not S3's");
    assert_eq!(
        Action::Proceed(Proceed::BisectReset).id().as_str(),
        "bisect-reset"
    );
}

/// G-R1 (the dialog half): a new branch name is refused as `check-ref-format --branch` refuses
/// it, before anything runs.
#[test]
fn names_git_refuses_are_refused_first() {
    for bad in [
        "x\\y", "a b", "a..b", "-x", "x.lock", "", "a/", "/a", "a//b", ".a", "a.", "a@{b", "HEAD",
    ] {
        assert!(valid_name(bad).is_err(), "{bad:?} accepted");
    }
    for good in ["it's", "a$b", "ü-한", "@", "a/b", "feature-1"] {
        assert_eq!(valid_name(good), Ok(()), "{good:?}");
    }
    let Some(fx) = fixture() else { return };
    let dir = fx.repo("names");
    let (_, runner) = fx.lanes().remove(0);
    let at = fx.oid(&dir, "HEAD");
    let refused = planned(
        &runner,
        &dir,
        &Action::Branch {
            at,
            name: "x\\y".into(),
            checkout: false,
        },
    )
    .unwrap_err();
    assert!(refused.0.contains("x\\y"), "{}", refused.0);
}

// ---------------------------------------------------------------------------------------------
// G-A1: a commit row

/// G-A1: `tag` annotated with a push to a local bare remote, and its "name taken" move form
/// (`-f`, backup `tags/<t>`).
#[test]
fn g_a1_tag_and_its_move_form() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("tag-{lane}"));
        let bare = with_origin(&fx, &dir, &format!("tag-{lane}"));
        let c1 = fx.commit(&dir, "a", "a\n", "second");
        let c2 = fx.commit(&dir, "b", "b\n", "third");
        let action = Action::Tag {
            at: c1.clone(),
            name: "v1".into(),
            message: Some("release one\n".into()),
            push: Some("origin".into()),
        };
        let (plan, outcome) = run(&runner, &dir, &action);
        done(&lane, &outcome);
        has_line(&plan, &format!("git tag -a -F - v1 {}", c1.as_str()));
        has_line(&plan, "git push --progress origin refs/tags/v1");
        assert!(backup_names(&plan).is_empty(), "{lane}");
        assert_eq!(fx.git(&dir, &["cat-file", "-t", "v1"]).trim(), "tag");
        assert_eq!(fx.rev(&dir, "v1^{}").unwrap(), c1.as_str());
        assert!(fx.rev(&bare, "refs/tags/v1").is_some(), "{lane}: pushed");
        let old = fx.rev(&dir, "refs/tags/v1").unwrap();
        // The name is taken: the move form.
        let action = Action::Tag {
            at: c2.clone(),
            name: "v1".into(),
            message: Some("moved\n".into()),
            push: None,
        };
        let (plan, outcome) = run(&runner, &dir, &action);
        done(&lane, &outcome);
        has_line(&plan, &format!("git tag -f -a -F - v1 {}", c2.as_str()));
        assert_eq!(backup_names(&plan), vec!["tags/v1".to_string()]);
        assert_eq!(fx.rev(&dir, "v1^{}").unwrap(), c2.as_str());
        let (stamp, names) = fx.newest(&dir);
        assert_eq!(names, vec!["manifest", "tags/v1"], "{lane}");
        assert_eq!(
            fx.rev(&dir, &format!("refs/sopen-backup/{stamp}/tags/v1"))
                .unwrap(),
            old,
            "{lane}: the old tag object"
        );
        assert_eq!(outcome.backup.as_deref(), Some(stamp.as_str()));
    }
}

/// G-A1: `branch` (and checking it out), its "name taken" move form, refused for the checked-out
/// branch; `move-branch`'s `update-ref` with the value read.
#[test]
fn g_a1_branch_and_move_branch() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("branch-{lane}"));
        let c1 = fx.oid(&dir, "HEAD");
        let c2 = fx.commit(&dir, "a", "a\n", "second");
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Branch {
                at: c1.clone(),
                name: "b1".into(),
                checkout: false,
            },
        );
        done(&lane, &outcome);
        has_line(&plan, &format!("git branch b1 {}", c1.as_str()));
        assert_eq!(fx.rev(&dir, "refs/heads/b1").unwrap(), c1.as_str());
        // Taken: moved, backed up.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Branch {
                at: c2.clone(),
                name: "b1".into(),
                checkout: false,
            },
        );
        done(&lane, &outcome);
        has_line(&plan, &format!("git branch -f b1 {}", c2.as_str()));
        assert_eq!(backup_names(&plan), vec!["heads/b1".to_string()]);
        assert_eq!(fx.rev(&dir, "refs/heads/b1").unwrap(), c2.as_str());
        // The checked-out branch: refused, Reset offered.
        let refused = planned(
            &runner,
            &dir,
            &Action::Branch {
                at: c1.clone(),
                name: "main".into(),
                checkout: false,
            },
        )
        .unwrap_err();
        assert!(refused.0.contains("Reset"), "{lane}: {}", refused.0);
        // move-branch: back to c1, with the value read.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::MoveBranch {
                branch: "b1".into(),
                to: c1.clone(),
            },
        );
        done(&lane, &outcome);
        has_line(
            &plan,
            &format!(
                "git update-ref -m 'sopen: move b1' refs/heads/b1 {} {}",
                c1.as_str(),
                c2.as_str()
            ),
        );
        assert_eq!(backup_names(&plan), vec!["heads/b1".to_string()]);
        assert_eq!(fx.rev(&dir, "refs/heads/b1").unwrap(), c1.as_str());
        // New branch, checked out.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Branch {
                at: c1.clone(),
                name: "b2".into(),
                checkout: true,
            },
        );
        done(&lane, &outcome);
        has_line(
            &plan,
            &format!("git checkout --no-overwrite-ignore -b b2 {}", c1.as_str()),
        );
        assert_eq!(
            fx.git(&dir, &["symbolic-ref", "HEAD"]).trim(),
            "refs/heads/b2"
        );
    }
}

/// G-A1: `checkout` (detached), `pick` of two marked commits, `revert`.
#[test]
fn g_a1_checkout_pick_and_revert() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("pick-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        fx.git(&dir, &["checkout", "-q", "-b", "side"]);
        let s1 = fx.commit(&dir, "x", "x\n", "side one");
        let s2 = fx.commit(&dir, "y", "y\n", "side two");
        fx.git(&dir, &["checkout", "-q", "main"]);
        let c2 = fx.commit(&dir, "b", "b\n", "main two");
        // Check out detached, then back.
        let (plan, outcome) = run(&runner, &dir, &Action::Checkout { at: base.clone() });
        done(&lane, &outcome);
        has_line(
            &plan,
            &format!(
                "git checkout --no-overwrite-ignore --detach {}",
                base.as_str()
            ),
        );
        assert!(plan.changes_files);
        assert_eq!(fx.rev(&dir, "HEAD").unwrap(), base.as_str());
        assert!(!fx
            .output(&dir, &["symbolic-ref", "-q", "HEAD"])
            .status
            .success());
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::CheckoutBranch {
                branch: "main".into(),
            },
        );
        done(&lane, &outcome);
        assert!(
            plan.confirm.is_none(),
            "{lane}: check out of a branch runs at once"
        );
        // Pick both, oldest first.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Pick {
                commits: vec![s1.clone(), s2.clone()],
                mainline: None,
                record_origin: true,
                no_commit: false,
            },
        );
        done(&lane, &outcome);
        has_line(
            &plan,
            &format!("git cherry-pick -x {} {}", s1.as_str(), s2.as_str()),
        );
        assert_eq!(files_of(&dir), vec!["README", "b", "x", "y"], "{lane}");
        let log = fx.git(&dir, &["log", "--format=%s", "-3"]);
        assert_eq!(log, "side two\nside one\nmain two\n", "{lane}");
        // Revert main two.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Revert {
                commits: vec![c2.clone()],
                mainline: None,
            },
        );
        done(&lane, &outcome);
        has_line(&plan, &format!("git revert --no-edit {}", c2.as_str()));
        assert_eq!(files_of(&dir), vec!["README", "x", "y"], "{lane}");
    }
}

/// G-A1: `merge` with each fast-forward choice: when possible (a fast-forward), never (a merge
/// commit), only (refused by git when the histories diverged: the step fails).
#[test]
fn g_a1_merge_with_each_fast_forward_choice() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("merge-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        fx.git(&dir, &["checkout", "-q", "-b", "f1"]);
        fx.commit(&dir, "f1", "1\n", "f one");
        fx.git(&dir, &["checkout", "-q", "main"]);
        let merge = |from: Rev, ff: FastForward| Action::Merge {
            from,
            options: MergeOptions {
                fast_forward: ff,
                ..MergeOptions::default()
            },
        };
        let (plan, outcome) = run(
            &runner,
            &dir,
            &merge(Rev::Branch("f1".into()), FastForward::Never),
        );
        done(&lane, &outcome);
        has_line(
            &plan,
            "git merge --no-edit --no-overwrite-ignore --no-ff f1",
        );
        let parents = fx.git(&dir, &["log", "-1", "--format=%P"]);
        assert_eq!(parents.split_whitespace().count(), 2, "{lane}");
        assert_eq!(
            fx.git(&dir, &["log", "-1", "--format=%s"]).trim(),
            "Merge branch 'f1'",
            "{lane}"
        );
        // When possible: a fast-forward.
        fx.git(&dir, &["checkout", "-q", "-b", "f2"]);
        let f2 = fx.commit(&dir, "f2", "2\n", "f two");
        fx.git(&dir, &["checkout", "-q", "main"]);
        let (plan, outcome) = run(
            &runner,
            &dir,
            &merge(Rev::Commit(f2.clone()), FastForward::WhenPossible),
        );
        done(&lane, &outcome);
        has_line(
            &plan,
            &format!("git merge --no-edit --no-overwrite-ignore {}", f2.as_str()),
        );
        assert_eq!(fx.rev(&dir, "main").unwrap(), f2.as_str(), "{lane}");
        // Only: refused by git on a diverged branch.
        fx.git(&dir, &["checkout", "-q", "-b", "d", base.as_str()]);
        fx.commit(&dir, "d", "d\n", "diverged");
        fx.git(&dir, &["checkout", "-q", "main"]);
        let (plan, outcome) = run(
            &runner,
            &dir,
            &merge(Rev::Branch("d".into()), FastForward::Only),
        );
        has_line(
            &plan,
            "git merge --no-edit --no-overwrite-ignore --ff-only d",
        );
        let text = failed_at(&lane, &outcome, 0);
        assert!(text.contains("orward"), "{lane}: {text}");
        assert_eq!(fx.rev(&dir, "main").unwrap(), f2.as_str());
    }
}

/// G-A1: `rebase` (backup `heads/<current>`) and `reset` in all four modes.
#[test]
fn g_a1_rebase_and_reset_in_four_modes() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("rebase-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        fx.git(&dir, &["checkout", "-q", "-b", "topic"]);
        let t1 = fx.commit(&dir, "t", "t\n", "topic one");
        fx.git(&dir, &["checkout", "-q", "main"]);
        let m1 = fx.commit(&dir, "m", "m\n", "main one");
        fx.git(&dir, &["checkout", "-q", "topic"]);
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Rebase {
                onto: Rev::Branch("main".into()),
                options: RebaseOptions::default(),
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git rebase refs/heads/main");
        assert_eq!(backup_names(&plan), vec!["heads/topic".to_string()]);
        assert_eq!(fx.rev(&dir, "topic^").unwrap(), m1.as_str(), "{lane}");
        let (stamp, names) = fx.newest(&dir);
        assert_eq!(names, vec!["heads/topic", "manifest"]);
        assert_eq!(
            fx.rev(&dir, &format!("refs/sopen-backup/{stamp}/heads/topic"))
                .unwrap(),
            t1.as_str()
        );
        // reset in four modes, on topic.
        let tip = fx.oid(&dir, "HEAD");
        for (mode, flag) in [
            (ResetMode::Soft, "--soft"),
            (ResetMode::Mixed, "--mixed"),
            (ResetMode::Keep, "--keep"),
            (ResetMode::Hard, "--hard"),
        ] {
            fx.git(&dir, &["reset", "-q", "--hard", tip.as_str()]);
            let (plan, outcome) = run(
                &runner,
                &dir,
                &Action::Reset {
                    to: base.clone(),
                    mode,
                },
            );
            done(&lane, &outcome);
            has_line(&plan, &format!("git reset {flag} {}", base.as_str()));
            let mut want = vec!["heads/topic".to_string()];
            if mode == ResetMode::Hard {
                want.push("worktree".into());
            }
            assert_eq!(backup_names(&plan), want, "{lane} {flag}");
            assert_eq!(
                fx.rev(&dir, "HEAD").unwrap(),
                base.as_str(),
                "{lane} {flag}"
            );
            let confirm = plan.confirm.as_ref().unwrap();
            assert!(
                confirm
                    .loses
                    .as_deref()
                    .is_some_and(|l| l.contains("topic") && l.contains("1 commit")),
                "{lane}: {:?}",
                confirm.loses
            );
        }
    }
}

/// G-A1: `archive` into a chosen folder, zip and tar, streamed to the path; a taken name is
/// refused and the file there left alone (01 §6a).
#[test]
fn g_a1_archive_never_over_anything() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("archive-{lane}"));
        fx.commit(&dir, "sub/a.txt", "a\n", "second");
        let out = fx.root.join(format!("out-{lane}"));
        std::fs::create_dir_all(&out).unwrap();
        let zip = out.join("main.zip");
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Archive {
                rev: Rev::Branch("main".into()),
                format: ArchiveFormat::Zip,
                to: zip.clone(),
            },
        );
        done(&lane, &outcome);
        assert!(
            commands(&plan)[0].starts_with("git archive --format=zip refs/heads/main"),
            "{:?}",
            commands(&plan)
        );
        let bytes = std::fs::read(&zip).unwrap();
        assert_eq!(&bytes[..2], b"PK", "{lane}");
        let tar = out.join("main.tar");
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Archive {
                rev: Rev::Branch("main".into()),
                format: ArchiveFormat::Tar,
                to: tar.clone(),
            },
        );
        done(&lane, &outcome);
        let want = fx
            .command(&dir, &["archive", "--format=tar", "refs/heads/main"])
            .output()
            .unwrap()
            .stdout;
        assert_eq!(std::fs::read(&tar).unwrap(), want, "{lane}: byte for byte");
        // Taken: refused, the file there unchanged.
        std::fs::write(&zip, "mine").unwrap();
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Archive {
                rev: Rev::Branch("main".into()),
                format: ArchiveFormat::Zip,
                to: zip.clone(),
            },
        );
        let text = failed_at(&lane, &outcome, 0);
        assert!(text.contains("exists"), "{lane}: {text}");
        assert_eq!(std::fs::read(&zip).unwrap(), b"mine");
    }
}

// ---------------------------------------------------------------------------------------------
// G-A2: a local branch

/// G-A2: `checkout-branch`, `rename`, `delete-branch` merged, not merged (git's refusal, then the
/// forced form) and with its remote (§8.4's lease delete), `push` with set-upstream, and with
/// overwrite after a rewrite.
#[test]
fn g_a2_local_branch() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("local-{lane}"));
        let bare = with_origin(&fx, &dir, &format!("local-{lane}"));
        fx.git(&dir, &["branch", "merged"]);
        fx.git(&dir, &["checkout", "-q", "-b", "work"]);
        let w1 = fx.commit(&dir, "w", "w\n", "work one");
        fx.git(&dir, &["checkout", "-q", "main"]);
        // checkout-branch, no dialog.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::CheckoutBranch {
                branch: "work".into(),
            },
        );
        done(&lane, &outcome);
        assert!(plan.confirm.is_none());
        assert_eq!(
            fx.git(&dir, &["symbolic-ref", "HEAD"]).trim(),
            "refs/heads/work"
        );
        fx.git(&dir, &["checkout", "-q", "main"]);
        // rename.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Rename {
                branch: "work".into(),
                to: "wip".into(),
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git branch -m work wip");
        assert_eq!(fx.rev(&dir, "refs/heads/wip").unwrap(), w1.as_str());
        // delete-branch, merged.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::DeleteBranch {
                branch: "merged".into(),
                force: false,
                remotes: vec![],
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git branch -d merged");
        assert_eq!(backup_names(&plan), vec!["heads/merged".to_string()]);
        assert!(fx.rev(&dir, "refs/heads/merged").is_none());
        // Not merged: git refuses; the second dialog's forced form deletes it.
        let delete = |force| Action::DeleteBranch {
            branch: "wip".into(),
            force,
            remotes: vec![],
        };
        let plan = planned(&runner, &dir, &delete(false)).unwrap();
        assert!(
            plan.confirm
                .as_ref()
                .and_then(|c| c.loses.clone())
                .is_some_and(|l| l.contains("wip") && l.contains("1 commit")),
            "{lane}: {:?}",
            plan.confirm
        );
        let outcome = execute(&plan, dir.to_str().unwrap(), &runner, &Handle::new());
        let text = failed_at(&lane, &outcome, 1);
        assert!(text.contains("not fully merged"), "{lane}: {text}");
        let (plan, outcome) = run(&runner, &dir, &delete(true));
        done(&lane, &outcome);
        has_line(&plan, "git branch -D wip");
        assert!(fx.rev(&dir, "refs/heads/wip").is_none());
        // push with set-upstream.
        fx.git(&dir, &["checkout", "-q", "-b", "pub"]);
        let p1 = fx.commit(&dir, "p", "p\n", "pub one");
        let push = |overwrite| Action::Push {
            branch: "pub".into(),
            remotes: vec!["origin".into()],
            options: PushOptions {
                set_upstream: true,
                no_verify: false,
            },
            overwrite,
        };
        let (plan, outcome) = run(&runner, &dir, &push(false));
        done(&lane, &outcome);
        has_line(
            &plan,
            "git push --progress --set-upstream origin refs/heads/pub:refs/heads/pub",
        );
        assert_eq!(fx.rev(&bare, "refs/heads/pub").unwrap(), p1.as_str());
        assert_eq!(
            fx.git(&dir, &["rev-parse", "--symbolic-full-name", "pub@{u}"])
                .trim(),
            "refs/remotes/origin/pub"
        );
        // A rewrite, then the overwrite with lease.
        fx.git(&dir, &["commit", "-q", "--amend", "-m", "pub one, again"]);
        let p2 = fx.oid(&dir, "HEAD");
        let (plan, outcome) = run(&runner, &dir, &push(true));
        done(&lane, &outcome);
        assert_eq!(plan.remote_changes().count(), 1);
        assert_eq!(
            backup_names(&plan),
            vec!["pushed/origin/heads/pub".to_string()]
        );
        has_line(
            &plan,
            "git push --progress --force-with-lease=refs/heads/pub:<value at Run> --set-upstream origin refs/heads/pub:refs/heads/pub",
        );
        assert_eq!(fx.rev(&bare, "refs/heads/pub").unwrap(), p2.as_str());
        let (stamp, names) = fx.newest(&dir);
        assert_eq!(names, vec!["manifest", "pushed/origin/heads/pub"]);
        assert_eq!(
            fx.rev(
                &dir,
                &format!("refs/sopen-backup/{stamp}/pushed/origin/heads/pub")
            )
            .unwrap(),
            p1.as_str()
        );
        fx.no_staging(&dir);
        // delete-branch with its remote: the lease delete first, then the local one.
        fx.git(&dir, &["checkout", "-q", "main"]);
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::DeleteBranch {
                branch: "pub".into(),
                force: true,
                remotes: vec!["origin".into()],
            },
        );
        done(&lane, &outcome);
        assert_eq!(
            backup_names(&plan),
            vec![
                "heads/pub".to_string(),
                "pushed/origin/heads/pub".to_string()
            ]
        );
        has_line(
            &plan,
            "git push --progress --force-with-lease=refs/heads/pub:<value at Run> origin --delete refs/heads/pub",
        );
        assert!(fx.rev(&bare, "refs/heads/pub").is_none(), "{lane}");
        assert!(fx.rev(&dir, "refs/heads/pub").is_none(), "{lane}");
        fx.no_staging(&dir);
    }
}

// ---------------------------------------------------------------------------------------------
// G-A3: a remote branch

/// G-A3: `checkout-remote` (a new name, and the existing-name pull), `delete-remote-branch`,
/// `fetch-into` (forced: backup), `pull` with merge, rebase and fast-forward only.
#[test]
fn g_a3_remote_branch() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("remote-{lane}"));
        let bare = with_origin(&fx, &dir, &format!("remote-{lane}"));
        let other = fx.clone_of(&bare, &format!("remote-other-{lane}"));
        fx.git(&other, &["checkout", "-q", "-b", "feat"]);
        let o1 = fx.commit(&other, "o", "o\n", "other one");
        fx.git(&other, &["push", "-q", "origin", "feat"]);
        fx.git(&dir, &["fetch", "-q", "origin"]);
        // A new local branch tracking origin/feat.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::CheckoutRemote {
                remote: "origin".into(),
                branch: "feat".into(),
                name: "feat".into(),
                pull: None,
            },
        );
        done(&lane, &outcome);
        has_line(
            &plan,
            "git checkout --no-overwrite-ignore -b feat --track origin/feat",
        );
        assert_eq!(fx.rev(&dir, "HEAD").unwrap(), o1.as_str());
        // The name exists: check it out and pull into it.
        fx.git(&dir, &["checkout", "-q", "main"]);
        let o2 = fx.commit(&other, "o", "o2\n", "other two");
        fx.git(&other, &["push", "-q", "origin", "feat"]);
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::CheckoutRemote {
                remote: "origin".into(),
                branch: "feat".into(),
                name: "feat".into(),
                pull: Some(PullHow::default()),
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git checkout --no-overwrite-ignore feat --");
        has_line(
            &plan,
            "git fetch --progress --no-prune --no-prune-tags origin refs/heads/feat",
        );
        has_line(
            &plan,
            "git merge --no-edit --no-overwrite-ignore FETCH_HEAD",
        );
        assert_eq!(fx.rev(&dir, "HEAD").unwrap(), o2.as_str(), "{lane}");
        // pull with each how.
        for (how, line) in [
            (
                PullHow::FastForwardOnly,
                "git merge --no-edit --no-overwrite-ignore --ff-only FETCH_HEAD",
            ),
            (PullHow::Rebase, "git rebase FETCH_HEAD"),
            (
                PullHow::Merge {
                    no_ff: true,
                    squash: false,
                },
                "git merge --no-edit --no-overwrite-ignore --no-ff FETCH_HEAD",
            ),
        ] {
            let tip = fx.commit(&other, "o", &format!("{line}\n"), line);
            fx.git(&other, &["push", "-q", "origin", "feat"]);
            if how == PullHow::Rebase {
                fx.commit(&dir, "mine", "mine\n", "mine");
            }
            let (plan, outcome) = run(
                &runner,
                &dir,
                &Action::Pull {
                    remote: "origin".into(),
                    branch: "feat".into(),
                    how,
                },
            );
            done(&lane, &outcome);
            has_line(&plan, line);
            if how == PullHow::Rebase {
                assert_eq!(backup_names(&plan), vec!["heads/feat".to_string()]);
                assert_eq!(fx.rev(&dir, "HEAD^").unwrap(), tip.as_str(), "{lane}");
            } else {
                let merged =
                    fx.output(&dir, &["merge-base", "--is-ancestor", tip.as_str(), "HEAD"]);
                assert!(merged.status.success(), "{lane}: {line}");
            }
        }
        // fetch-into, forced: main moved on the remote in a non-fast-forward way.
        fx.git(&other, &["checkout", "-q", "main"]);
        fx.git(
            &other,
            &["commit", "-q", "--amend", "-m", "first, rewritten"],
        );
        fx.git(&other, &["push", "-q", "-f", "origin", "main"]);
        let old_main = fx.rev(&dir, "refs/heads/main").unwrap();
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::FetchInto {
                remote: "origin".into(),
                branch: "main".into(),
                force: true,
            },
        );
        done(&lane, &outcome);
        has_line(
            &plan,
            "git fetch --progress --no-prune --no-prune-tags origin +refs/heads/main:refs/heads/main",
        );
        assert_eq!(backup_names(&plan), vec!["heads/main".to_string()]);
        assert_eq!(
            fx.rev(&dir, "refs/heads/main"),
            fx.rev(&bare, "refs/heads/main")
        );
        let (stamp, _) = fx.newest(&dir);
        assert_eq!(
            fx.rev(&dir, &format!("refs/sopen-backup/{stamp}/heads/main"))
                .unwrap(),
            old_main
        );
        // delete-remote-branch: the lease delete, the tracking ref gone too.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::DeleteRemoteBranch {
                remote: "origin".into(),
                branch: "feat".into(),
            },
        );
        done(&lane, &outcome);
        assert_eq!(
            backup_names(&plan),
            vec!["pushed/origin/heads/feat".to_string()]
        );
        assert!(fx.rev(&bare, "refs/heads/feat").is_none());
        assert!(fx.rev(&dir, "refs/remotes/origin/feat").is_none());
        fx.no_staging(&dir);
    }
}

/// G-A3: stopping a pull during its fetch (an upload-pack that waits) leaves HEAD where it was;
/// the merge step is not run.
#[test]
fn g_a3_a_stopped_pull_leaves_head() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("stop-pull-{lane}"));
        let bare = with_origin(&fx, &dir, &format!("stop-pull-{lane}"));
        let other = fx.clone_of(&bare, &format!("stop-pull-other-{lane}"));
        fx.commit(&other, "o", "o\n", "other");
        fx.git(&other, &["push", "-q", "origin", "main"]);
        let marker = fx.root.join(format!("waiting-{lane}"));
        let upload = format!("touch {}; sleep 30; git-upload-pack", sh_word(&marker));
        fx.git(&dir, &["config", "remote.origin.uploadpack", &upload]);
        let head = fx.rev(&dir, "HEAD").unwrap();
        let action = Action::Pull {
            remote: "origin".into(),
            branch: "main".into(),
            how: PullHow::default(),
        };
        let plan = planned(&runner, &dir, &action).unwrap();
        let handle = Handle::new();
        let top = dir.to_str().unwrap().to_string();
        let (r, h) = (runner.clone(), handle.clone());
        let worker = std::thread::spawn(move || execute(&plan, &top, &r, &h));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "{lane}: the fetch never waited");
            std::thread::sleep(Duration::from_millis(20));
        }
        let t = Instant::now();
        handle.stop();
        let outcome = worker.join().unwrap();
        assert!(
            t.elapsed() < Duration::from_secs(6),
            "{lane}: {:?}",
            t.elapsed()
        );
        let text = failed_at(&lane, &outcome, 0);
        assert!(text.contains("stopped"), "{lane}: {text}");
        assert_eq!(fx.rev(&dir, "HEAD").unwrap(), head, "{lane}");
    }
}

// ---------------------------------------------------------------------------------------------
// G-A4: a tag

/// G-A4: `delete-tag` with the remote (lease delete first), `push-tag` plain and replacing a
/// tag that differs there, `move-tag` keeping it annotated with its message. (`tag-details` is
/// a view: `cat-file -p <t>` in the details pane, no plan.)
#[test]
fn g_a4_tags() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("tags-{lane}"));
        let bare = with_origin(&fx, &dir, &format!("tags-{lane}"));
        let c1 = fx.oid(&dir, "HEAD");
        let c2 = fx.commit(&dir, "a", "a\n", "second");
        fx.git(
            &dir,
            &["tag", "-a", "-m", "the message\n\nbody", "t1", c1.as_str()],
        );
        // push-tag, plain.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::PushTag {
                tag: "t1".into(),
                remotes: vec!["origin".into()],
                replace: false,
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git push --progress origin refs/tags/t1");
        assert_eq!(fx.rev(&bare, "refs/tags/t1"), fx.rev(&dir, "refs/tags/t1"));
        // move-tag, keeping its message.
        let old = fx.rev(&dir, "refs/tags/t1").unwrap();
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::MoveTag {
                tag: "t1".into(),
                to: c2.clone(),
                message: Some("the message\n\nbody\n".into()),
            },
        );
        done(&lane, &outcome);
        has_line(&plan, &format!("git tag -f -a -F - t1 {}", c2.as_str()));
        assert_eq!(backup_names(&plan), vec!["tags/t1".to_string()]);
        assert_eq!(fx.rev(&dir, "t1^{}").unwrap(), c2.as_str());
        assert_eq!(
            fx.git(&dir, &["cat-file", "tag", "t1"])
                .split_once("\n\n")
                .unwrap()
                .1,
            "the message\n\nbody\n"
        );
        let (stamp, _) = fx.newest(&dir);
        assert_eq!(
            fx.rev(&dir, &format!("refs/sopen-backup/{stamp}/tags/t1"))
                .unwrap(),
            old
        );
        // push-tag replacing the remote's, through the lease.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::PushTag {
                tag: "t1".into(),
                remotes: vec!["origin".into()],
                replace: true,
            },
        );
        done(&lane, &outcome);
        assert_eq!(
            backup_names(&plan),
            vec!["pushed/origin/tags/t1".to_string()]
        );
        assert_eq!(fx.rev(&bare, "refs/tags/t1"), fx.rev(&dir, "refs/tags/t1"));
        // delete-tag with the remote.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::DeleteTag {
                tag: "t1".into(),
                remote: Some("origin".into()),
            },
        );
        done(&lane, &outcome);
        assert_eq!(
            backup_names(&plan),
            vec!["tags/t1".to_string(), "pushed/origin/tags/t1".to_string()]
        );
        assert_eq!(commands(&plan).last().unwrap(), "git tag -d t1");
        assert!(fx.rev(&bare, "refs/tags/t1").is_none(), "{lane}");
        assert!(fx.rev(&dir, "refs/tags/t1").is_none(), "{lane}");
        fx.no_staging(&dir);
    }
}

// ---------------------------------------------------------------------------------------------
// G-A5: stashes

fn stash_list(fx: &Fx, dir: &Path) -> Vec<String> {
    fx.git(dir, &["stash", "list", "--format=%H %s"])
        .lines()
        .map(str::to_string)
        .collect()
}

/// G-A5: `stash` (with untracked files), `stash-apply` with and without the index, `stash-pop`,
/// `stash-branch`, `stash-drop`; a stash list changed under the dialog → nothing done, and read
/// again the drop takes the right entry.
#[test]
fn g_a5_stashes() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("stash-{lane}"));
        std::fs::write(dir.join("README"), "staged\n").unwrap();
        fx.git(&dir, &["add", "README"]);
        std::fs::write(dir.join("README"), "staged and more\n").unwrap();
        std::fs::write(dir.join("new.txt"), "untracked\n").unwrap();
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Stash {
                message: "two\nlines".into(),
                include_untracked: true,
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git stash push --include-untracked -m 'two lines'");
        assert_eq!(files_of(&dir), vec!["README"]);
        let repo = repo_of(&runner, &dir);
        let stash = repo.stashes[0].clone();
        assert_eq!(stash.subject, "On main: two lines");
        // apply with the index.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::StashApply {
                stash: stash.clone(),
                index: true,
            },
        );
        done(&lane, &outcome);
        has_line(
            &plan,
            &format!("git stash apply --index {}", stash.oid.as_str()),
        );
        assert_eq!(
            fx.git(&dir, &["diff", "--cached", "--name-only"]),
            "README\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("new.txt")).unwrap(),
            "untracked\n"
        );
        fx.git(&dir, &["reset", "-q", "--hard"]);
        fx.git(&dir, &["clean", "-q", "-f"]);
        // without the index.
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::StashApply {
                stash: stash.clone(),
                index: false,
            },
        );
        done(&lane, &outcome);
        assert_eq!(fx.git(&dir, &["diff", "--cached", "--name-only"]), "");
        fx.git(&dir, &["reset", "-q", "--hard"]);
        fx.git(&dir, &["clean", "-q", "-f"]);
        // pop: backup stash.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::StashPop {
                stash: stash.clone(),
                index: false,
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git stash pop 'stash@{0}'");
        assert_eq!(backup_names(&plan), vec!["stash".to_string()]);
        assert!(stash_list(&fx, &dir).is_empty());
        let (stamp, _) = fx.newest(&dir);
        assert_eq!(
            fx.rev(&dir, &format!("refs/sopen-backup/{stamp}/stash"))
                .unwrap(),
            stash.oid.as_str()
        );
        fx.git(&dir, &["reset", "-q", "--hard"]);
        fx.git(&dir, &["clean", "-q", "-f"]);
        // branch from a stash.
        std::fs::write(dir.join("README"), "for a branch\n").unwrap();
        fx.git(&dir, &["stash", "push", "-q", "-m", "for a branch"]);
        let repo = repo_of(&runner, &dir);
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::StashBranch {
                stash: repo.stashes[0].clone(),
                name: "from-stash".into(),
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git stash branch from-stash 'stash@{0}'");
        assert_eq!(
            fx.git(&dir, &["symbolic-ref", "HEAD"]).trim(),
            "refs/heads/from-stash"
        );
        fx.git(&dir, &["reset", "-q", "--hard"]);
        fx.git(&dir, &["checkout", "-q", "main"]);
        // drop; then a list changed under the dialog.
        std::fs::write(dir.join("README"), "one\n").unwrap();
        fx.git(&dir, &["stash", "push", "-q", "-m", "one"]);
        std::fs::write(dir.join("README"), "two\n").unwrap();
        fx.git(&dir, &["stash", "push", "-q", "-m", "two"]);
        let mut repo = repo_of(&runner, &dir);
        let older = repo.stashes[1].clone();
        let drop = Action::StashDrop {
            stash: older.clone(),
        };
        ready(&runner, &mut repo, &drop);
        let plan = crate::git::actions::plan(&drop, &repo).unwrap();
        has_line(&plan, "git stash drop 'stash@{1}'");
        // Another stash arrives before Run: stash@{1} is now another entry.
        std::fs::write(dir.join("README"), "three\n").unwrap();
        fx.git(&dir, &["stash", "push", "-q", "-m", "three"]);
        let before = stash_list(&fx, &dir);
        let outcome = execute(&plan, dir.to_str().unwrap(), &runner, &Handle::new());
        let at = outcome
            .steps
            .iter()
            .position(|s| s.outcome == StepOutcome::Failed)
            .unwrap_or_else(|| panic!("{lane}: nothing failed"));
        let text = failed_at(&lane, &outcome, at);
        assert!(text.contains("stash list changed"), "{lane}: {text}");
        assert_eq!(stash_list(&fx, &dir), before, "{lane}: nothing dropped");
        // Refreshed: the dialog's row for the same stash is now stash@{2}.
        let again = repo_of(&runner, &dir)
            .stashes
            .into_iter()
            .find(|s| s.oid == older.oid)
            .unwrap();
        let (plan, outcome) = run(&runner, &dir, &Action::StashDrop { stash: again });
        done(&lane, &outcome);
        has_line(&plan, "git stash drop 'stash@{2}'");
        let left = stash_list(&fx, &dir);
        assert_eq!(left.len(), 2);
        assert!(
            left.iter().all(|l| !l.starts_with(older.oid.as_str())),
            "{lane}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// G-A6: the uncommitted row

/// A repository whose `.gitattributes` says `* text eol=lf` and runs a filter on `f.dat` that
/// deletes `x` when cleaning and adds `!` when smudging; with a CRLF file, the filtered file, an
/// executable and a symlink untracked — the byte-for-byte fixtures of G-A6 and G-A8.
pub(super) fn awkward(fx: &Fx, name: &str) -> (PathBuf, Vec<(&'static str, Vec<u8>)>) {
    let dir = fx.repo(name);
    std::fs::write(
        dir.join(".gitattributes"),
        "* text eol=lf\nf.dat filter=del\n",
    )
    .unwrap();
    fx.git(&dir, &["config", "filter.del.clean", "tr -d x"]);
    fx.git(&dir, &["config", "filter.del.smudge", "sed s/^/!/"]);
    fx.git(&dir, &["add", ".gitattributes"]);
    fx.git(&dir, &["commit", "-q", "-m", "attributes"]);
    let files: Vec<(&'static str, Vec<u8>)> = vec![
        ("crlf.txt", b"one\r\ntwo\r\n".to_vec()),
        ("f.dat", b"xaxbx\n".to_vec()),
        ("run.sh", b"#!/bin/sh\necho hi\n".to_vec()),
    ];
    for (name, bytes) in &files {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    std::fs::set_permissions(dir.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("crlf.txt", dir.join("link")).unwrap();
    (dir, files)
}

/// The mode and bytes of `path` in `commit`.
pub(super) fn blob(fx: &Fx, dir: &Path, commit: &str, path: &str) -> (String, Vec<u8>) {
    let line = fx.git(dir, &["ls-tree", "--full-tree", commit, "--", path]);
    let mode = line.split(' ').next().unwrap_or("").to_string();
    let bytes = fx
        .command(dir, &["cat-file", "blob", &format!("{commit}:{path}")])
        .output()
        .unwrap()
        .stdout;
    (mode, bytes)
}

/// G-A6: stage / unstage (no dialog), `commit`, `amend` keeping its message, `discard` of a
/// tracked and an untracked file, `reset-changes` hard.
#[test]
fn g_a6_the_uncommitted_row() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("uncommitted-{lane}"));
        std::fs::write(dir.join("README"), "changed\n").unwrap();
        std::fs::write(dir.join("a*"), "star\n").unwrap();
        std::fs::write(dir.join("ab"), "not me\n").unwrap();
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Stage {
                paths: vec!["README".into(), "a*".into()],
            },
        );
        done(&lane, &outcome);
        assert!(plan.confirm.is_none());
        assert_eq!(
            fx.git(&dir, &["diff", "--cached", "--name-only"]),
            "README\na*\n"
        );
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Unstage {
                paths: vec!["a*".into()],
            },
        );
        done(&lane, &outcome);
        assert!(plan.confirm.is_none());
        assert_eq!(
            fx.git(&dir, &["diff", "--cached", "--name-only"]),
            "README\n"
        );
        // commit, byte for byte.
        let message = "\n subject \t\r\n\n# kept\n\n";
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Commit {
                message: message.into(),
                amend: false,
                no_verify: false,
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git commit --cleanup=verbatim -F -");
        let raw = fx.git(&dir, &["cat-file", "commit", "HEAD"]);
        assert!(raw.ends_with(message), "{lane}: {raw:?}");
        // amend, keeping its message.
        let before = fx.rev(&dir, "HEAD").unwrap();
        fx.git(&dir, &["add", "ab"]);
        let (plan, outcome) = run(&runner, &dir, &Action::Amend { message: None });
        done(&lane, &outcome);
        has_line(&plan, "git commit --amend --no-edit --cleanup=verbatim");
        assert_eq!(backup_names(&plan), vec!["heads/main".to_string()]);
        assert!(fx
            .git(&dir, &["cat-file", "commit", "HEAD"])
            .ends_with(message));
        let (stamp, _) = fx.newest(&dir);
        assert_eq!(
            fx.rev(&dir, &format!("refs/sopen-backup/{stamp}/heads/main"))
                .unwrap(),
            before
        );
        // discard a tracked file and an untracked one.
        std::fs::write(dir.join("README"), "edited\n").unwrap();
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Discard {
                paths: vec!["README".into(), "a*".into()],
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git checkout HEAD -- README");
        has_line(&plan, "git clean -f -d -- 'a*'");
        assert_eq!(
            backup_names(&plan),
            vec!["worktree".to_string(), "files".to_string()]
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("README")).unwrap(),
            "changed\n"
        );
        assert!(!dir.join("a*").exists());
        let (stamp, _) = fx.newest(&dir);
        assert_eq!(
            blob(&fx, &dir, &format!("refs/sopen-backup/{stamp}/files"), "a*").1,
            b"star\n"
        );
        // reset-changes hard.
        std::fs::write(dir.join("README"), "edited again\n").unwrap();
        let (plan, outcome) = run(&runner, &dir, &Action::ResetChanges { hard: true });
        done(&lane, &outcome);
        has_line(&plan, "git reset --hard HEAD");
        assert_eq!(backup_names(&plan), vec!["worktree".to_string()]);
        assert_eq!(
            std::fs::read_to_string(dir.join("README")).unwrap(),
            "changed\n"
        );
    }
}

/// G-A6: `clean` with folders and the untracked `discard` of a CRLF file under `* text eol=lf`,
/// a filtered file, an executable and a symlink → the backup's blobs are the bytes as they were,
/// and Restore writes them back exactly.
#[test]
fn g_a6_clean_and_discard_keep_raw_bytes() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        for discard in [false, true] {
            let (dir, files) = awkward(&fx, &format!("raw-{lane}-{discard}"));
            std::fs::create_dir_all(dir.join("sub/deeper")).unwrap();
            std::fs::write(dir.join("sub/deeper/n.txt"), "nested\n").unwrap();
            let action = if discard {
                Action::Discard {
                    paths: vec![
                        "crlf.txt".into(),
                        "f.dat".into(),
                        "run.sh".into(),
                        "link".into(),
                        "sub/".into(),
                    ],
                }
            } else {
                Action::Clean { folders: true }
            };
            let (plan, outcome) = run(&runner, &dir, &action);
            done(&lane, &outcome);
            if !discard {
                has_line(&plan, "git clean -f -d");
            }
            assert_eq!(backup_names(&plan), vec!["files".to_string()], "{lane}");
            assert_eq!(files_of(&dir), vec![".gitattributes", "README"], "{lane}");
            let (stamp, _) = fx.newest(&dir);
            let files_ref = format!("refs/sopen-backup/{stamp}/files");
            for (name, bytes) in &files {
                let (mode, got) = blob(&fx, &dir, &files_ref, name);
                assert_eq!(&got, bytes, "{lane}: {name}");
                let want = if *name == "run.sh" {
                    "100755"
                } else {
                    "100644"
                };
                assert_eq!(mode, want, "{lane}: {name}");
            }
            assert_eq!(
                blob(&fx, &dir, &files_ref, "link"),
                ("120000".into(), b"crlf.txt".to_vec())
            );
            assert_eq!(
                blob(&fx, &dir, &files_ref, "sub/deeper/n.txt").1,
                b"nested\n"
            );
            // Restore: every file back, byte for byte.
            let top = dir.to_str().unwrap();
            let record = backup::group(&runner, &Handle::new(), top, &stamp)
                .unwrap()
                .unwrap();
            let repo = repo_of(&runner, &dir);
            let entries =
                backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
            assert!(entries.iter().all(|e| e.chosen), "{lane}: {entries:#?}");
            let (_, outcome) = run(&runner, &dir, &Action::Restore { stamp, entries });
            done(&lane, &outcome);
            for (name, bytes) in &files {
                assert_eq!(
                    &std::fs::read(dir.join(name)).unwrap(),
                    bytes,
                    "{lane}: {name}"
                );
            }
            let mode = std::fs::metadata(dir.join("run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert!(mode & 0o111 != 0, "{lane}: executable");
            assert_eq!(
                std::fs::read_link(dir.join("link")).unwrap(),
                PathBuf::from("crlf.txt")
            );
            assert_eq!(
                std::fs::read_to_string(dir.join("sub/deeper/n.txt")).unwrap(),
                "nested\n"
            );
        }
    }
}

/// G-A6: ours / theirs / resolved in a conflict, each of ours, theirs and redo backing up the
/// file as it is first.
#[test]
fn g_a6_ours_theirs_resolved() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = conflicted(&fx, &format!("ours-{lane}"));
        std::fs::write(dir.join("c.txt"), "hand-made\n").unwrap();
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Ours {
                paths: vec!["c.txt".into()],
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git checkout --ours -- c.txt");
        assert_eq!(backup_names(&plan), vec!["files".to_string()]);
        assert_eq!(
            std::fs::read_to_string(dir.join("c.txt")).unwrap(),
            "main\n"
        );
        let (stamp, _) = fx.newest(&dir);
        assert_eq!(
            blob(
                &fx,
                &dir,
                &format!("refs/sopen-backup/{stamp}/files"),
                "c.txt"
            )
            .1,
            b"hand-made\n"
        );
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Theirs {
                paths: vec!["c.txt".into()],
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git checkout --theirs -- c.txt");
        assert_eq!(
            std::fs::read_to_string(dir.join("c.txt")).unwrap(),
            "side\n"
        );
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::RedoMerge {
                paths: vec!["c.txt".into()],
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git checkout --merge -- c.txt");
        assert!(std::fs::read_to_string(dir.join("c.txt"))
            .unwrap()
            .contains("<<<<<<<"));
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Resolved {
                paths: vec!["c.txt".into()],
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git add -- c.txt");
        assert_eq!(
            fx.git(&dir, &["diff", "--name-only", "--diff-filter=U"]),
            ""
        );
    }
}

/// A repository stopped in a merge conflict on `c.txt` (`main\n` against `side\n`).
pub(super) fn conflicted(fx: &Fx, name: &str) -> PathBuf {
    let dir = fx.repo(name);
    fx.commit(&dir, "c.txt", "base\n", "base");
    fx.git(&dir, &["checkout", "-q", "-b", "side"]);
    fx.commit(&dir, "c.txt", "side\n", "side");
    fx.git(&dir, &["checkout", "-q", "main"]);
    fx.commit(&dir, "c.txt", "main\n", "main");
    let out = fx.output(&dir, &["merge", "side"]);
    assert!(!out.status.success(), "a conflict");
    dir
}

// ---------------------------------------------------------------------------------------------
// G-A7: the repository

/// G-A7: `fetch` all, `fetch-remote` with prune, `prune-remote`, `add-remote`, `edit-remote`,
/// `delete-remote`, `user` local and global (the fixture's own global config), `user-remove`.
#[test]
fn g_a7_the_repository() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("repo-{lane}"));
        let bare = with_origin(&fx, &dir, &format!("repo-{lane}"));
        let up = fx.bare(&format!("repo-up-{lane}.git"));
        // add-remote with fetch now and a push URL.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::AddRemote {
                name: "up".into(),
                url: up.to_str().unwrap().into(),
                push_url: Some(bare.to_str().unwrap().into()),
                fetch_now: true,
            },
        );
        done(&lane, &outcome);
        has_line(
            &plan,
            &format!("git remote add up {}", up.to_str().unwrap()),
        );
        has_line(
            &plan,
            &format!("git remote set-url --push up {}", bare.to_str().unwrap()),
        );
        has_line(&plan, "git fetch --progress --no-prune --no-prune-tags up");
        assert_eq!(
            fx.git(&dir, &["config", "remote.up.pushurl"]).trim(),
            bare.to_str().unwrap()
        );
        // fetch all, at once.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Fetch {
                prune: false,
                prune_tags: false,
            },
        );
        done(&lane, &outcome);
        assert!(plan.confirm.is_none(), "{lane}: Fetch all runs at once");
        assert_eq!(
            outcome.steps[0].line,
            "git fetch --progress --no-prune --no-prune-tags --all --no-prune --no-prune-tags"
        );
        // edit-remote: rename and a new URL.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::EditRemote {
                was: Remote {
                    name: "up".into(),
                    url: up.to_str().unwrap().into(),
                    push_url: Some(bare.to_str().unwrap().into()),
                },
                now: Remote {
                    name: "upstream".into(),
                    url: bare.to_str().unwrap().into(),
                    push_url: Some(bare.to_str().unwrap().into()),
                },
            },
        );
        done(&lane, &outcome);
        assert_eq!(
            commands(&plan),
            vec![
                "git remote rename up upstream".to_string(),
                format!("git remote set-url upstream {}", bare.to_str().unwrap()),
            ]
        );
        // fetch-remote with prune; prune-remote.
        let other = fx.clone_of(&bare, &format!("repo-other-{lane}"));
        fx.git(&other, &["push", "-q", "origin", "main:gone"]);
        fx.git(&dir, &["fetch", "-q", "upstream"]);
        fx.git(&dir, &["fetch", "-q", "origin"]);
        fx.git(&other, &["push", "-q", "origin", "--delete", "gone"]);
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::FetchRemote {
                remote: "origin".into(),
                prune: true,
                prune_tags: false,
            },
        );
        done(&lane, &outcome);
        has_line(
            &plan,
            "git fetch --progress --no-prune --no-prune-tags origin --prune --no-prune-tags",
        );
        assert!(fx.rev(&dir, "refs/remotes/origin/gone").is_none());
        let (_, names) = fx.newest(&dir);
        assert_eq!(
            names,
            vec!["manifest", "remotes/origin/gone"],
            "{lane}: exactly what was pruned"
        );
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::PruneRemote {
                remote: "upstream".into(),
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git remote prune upstream");
        assert!(fx.rev(&dir, "refs/remotes/upstream/gone").is_none());
        let (_, names) = fx.newest(&dir);
        assert_eq!(names, vec!["manifest", "remotes/upstream/gone"], "{lane}");
        // delete-remote: its remote-tracking refs backed up.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::DeleteRemote {
                remote: "upstream".into(),
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git remote remove upstream");
        assert_eq!(
            backup_names(&plan),
            vec!["remotes/upstream/main".to_string()]
        );
        assert_eq!(fx.git(&dir, &["remote"]), "origin\n");
        // user: local, then global (the fixture's config), then removed.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::User {
                name: "Local One".into(),
                email: "local@example.invalid".into(),
                scope: ConfigScope::Local,
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git config --local user.name 'Local One'");
        assert_eq!(
            fx.git(&dir, &["config", "--local", "user.name"]),
            "Local One\n"
        );
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::User {
                name: "Global One".into(),
                email: "global@example.invalid".into(),
                scope: ConfigScope::Global,
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git config --global user.name 'Global One'");
        has_line(&plan, "git config --local --unset-all user.name");
        assert!(std::fs::read_to_string(&fx.config)
            .unwrap()
            .contains("Global One"));
        assert!(!fx
            .output(&dir, &["config", "--local", "user.name"])
            .status
            .success());
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::UserRemove {
                scope: ConfigScope::Global,
            },
        );
        done(&lane, &outcome);
        has_line(&plan, "git config --global --unset-all user.email");
        assert!(!std::fs::read_to_string(&fx.config)
            .unwrap()
            .contains("Global One"));
        std::fs::write(&fx.config, CONFIG).unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// G-A8: files in the way

/// A repository with `*.log` ignored.
fn ignoring(fx: &Fx, name: &str) -> PathBuf {
    let dir = fx.repo(name);
    std::fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
    fx.git(&dir, &["add", ".gitignore"]);
    fx.git(&dir, &["commit", "-q", "-m", "ignore"]);
    dir
}

/// G-A8: an ignored file in the way of `checkout` and `merge` (git refuses both: no
/// `--overwrite-ignore`), and of `cherry-pick`, `revert`, `rebase` and `stash pop` (sopen
/// refuses, listing the path) — the file is never changed.
#[test]
fn g_a8_files_in_the_way_are_refused() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = ignoring(&fx, &format!("way-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        fx.git(&dir, &["checkout", "-q", "-b", "brings"]);
        let brings = fx.commit(&dir, "x.log", "from git\n", "brings x.log");
        fx.git(&dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("x.log"), "mine\n").unwrap();
        let mine = |dir: &Path| std::fs::read_to_string(dir.join("x.log")).unwrap();
        // checkout and merge: git refuses (--no-overwrite-ignore).
        let (_, outcome) = run(&runner, &dir, &Action::Checkout { at: brings.clone() });
        failed_at(&lane, &outcome, 0);
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Merge {
                from: Rev::Branch("brings".into()),
                options: MergeOptions::default(),
            },
        );
        failed_at(&lane, &outcome, 0);
        assert_eq!(mine(&dir), "mine\n");
        // cherry-pick and rebase: refused by sopen, naming the path.
        for action in [
            Action::Pick {
                commits: vec![brings.clone()],
                mainline: None,
                record_origin: false,
                no_commit: false,
            },
            Action::Rebase {
                onto: Rev::Branch("brings".into()),
                options: RebaseOptions::default(),
            },
        ] {
            let refused = planned(&runner, &dir, &action).unwrap_err();
            assert!(refused.0.contains("x.log"), "{lane}: {}", refused.0);
        }
        // revert of a commit that deleted x.log brings it back.
        std::fs::remove_file(dir.join("x.log")).unwrap();
        fx.git(&dir, &["checkout", "-q", "-b", "had", base.as_str()]);
        fx.commit(&dir, "x.log", "tracked\n", "track x.log");
        fx.git(&dir, &["rm", "-q", "x.log"]);
        fx.git(&dir, &["commit", "-q", "-m", "delete x.log"]);
        let deleted = fx.oid(&dir, "HEAD");
        std::fs::write(dir.join("x.log"), "mine\n").unwrap();
        let refused = planned(
            &runner,
            &dir,
            &Action::Revert {
                commits: vec![deleted],
                mainline: None,
            },
        )
        .unwrap_err();
        assert!(refused.0.contains("x.log"), "{lane}: {}", refused.0);
        // stash pop of a stash that holds x.log.
        std::fs::remove_file(dir.join("x.log")).unwrap();
        fx.git(&dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("x.log"), "stashed\n").unwrap();
        fx.git(&dir, &["add", "-f", "x.log"]);
        fx.git(&dir, &["stash", "push", "-q"]);
        std::fs::write(dir.join("x.log"), "mine\n").unwrap();
        let repo = repo_of(&runner, &dir);
        let refused = planned(
            &runner,
            &dir,
            &Action::StashPop {
                stash: repo.stashes[0].clone(),
                index: false,
            },
        )
        .unwrap_err();
        assert!(refused.0.contains("x.log"), "{lane}: {}", refused.0);
        assert_eq!(mine(&dir), "mine\n");
    }
}

/// `action` changed nothing at `path`: sopen refused it, or git did (a step failed).
fn untouched(
    fx: &Fx,
    runner: &GitRunner,
    lane: &str,
    dir: &Path,
    action: &Action,
    path: &str,
    bytes: &[u8],
) {
    let _ = fx;
    match planned(runner, dir, action) {
        Err(_) => {}
        Ok(plan) => {
            let outcome = execute(&plan, dir.to_str().unwrap(), runner, &Handle::new());
            assert!(
                outcome
                    .steps
                    .iter()
                    .any(|s| s.outcome == StepOutcome::Failed),
                "{lane}: {:?} ran over {path}",
                action.id()
            );
        }
    }
    assert_eq!(
        std::fs::read(dir.join(path)).unwrap(),
        bytes,
        "{lane}: {:?} changed {path}",
        action.id()
    );
}

/// G-A8: an untracked file, and an untracked file where the target has a folder, in the way of
/// `checkout`, `merge`, `cherry-pick`, `revert`, `rebase` and `stash pop` → each is refused, by
/// git or by sopen, and the file is never changed.
#[test]
fn g_a8_untracked_in_the_way_is_never_changed() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        // (the untracked path, what the other side writes there)
        for (way, brought) in [("u.txt", "u.txt"), ("d", "d/f.txt")] {
            let name = format!("untracked-way-{lane}-{}", way.replace('.', "-"));
            let dir = fx.repo(&name);
            let base = fx.oid(&dir, "HEAD");
            fx.git(&dir, &["checkout", "-q", "-b", "brings"]);
            let brings = fx.commit(&dir, brought, "from git\n", "brings it");
            fx.git(&dir, &["checkout", "-q", "-b", "had", base.as_str()]);
            fx.commit(&dir, brought, "tracked\n", "track it");
            fx.git(&dir, &["rm", "-q", "-r", "--", way]);
            fx.git(&dir, &["commit", "-q", "-m", "delete it"]);
            let deleted = fx.oid(&dir, "HEAD");
            fx.git(&dir, &["checkout", "-q", "main"]);
            // A stash that brings it back.
            std::fs::create_dir_all(dir.join(brought).parent().unwrap()).unwrap();
            std::fs::write(dir.join(brought), "stashed\n").unwrap();
            fx.git(&dir, &["add", "--", brought]);
            fx.git(&dir, &["stash", "push", "-q"]);
            let bytes = b"mine, untracked\n";
            std::fs::write(dir.join(way), bytes).unwrap();
            let stash = repo_of(&runner, &dir).stashes[0].clone();
            let on_main = [
                Action::Checkout { at: brings.clone() },
                Action::Merge {
                    from: Rev::Branch("brings".into()),
                    options: MergeOptions::default(),
                },
                Action::Pick {
                    commits: vec![brings.clone()],
                    mainline: None,
                    record_origin: false,
                    no_commit: false,
                },
                Action::Rebase {
                    onto: Rev::Branch("brings".into()),
                    options: RebaseOptions::default(),
                },
                Action::StashPop {
                    stash,
                    index: false,
                },
            ];
            for action in &on_main {
                untouched(&fx, &runner, &lane, &dir, action, way, bytes);
                assert_eq!(
                    fx.rev(&dir, "HEAD").unwrap(),
                    base.as_str(),
                    "{lane}: {:?}",
                    action.id()
                );
            }
            // revert on `had`, where the deletion is HEAD.
            std::fs::remove_file(dir.join(way)).unwrap();
            fx.git(&dir, &["checkout", "-q", "had"]);
            std::fs::write(dir.join(way), bytes).unwrap();
            let revert = Action::Revert {
                commits: vec![deleted.clone()],
                mainline: None,
            };
            untouched(&fx, &runner, &lane, &dir, &revert, way, bytes);
            assert_eq!(
                fx.rev(&dir, "HEAD").unwrap(),
                deleted.as_str(),
                "{lane}: revert"
            );
        }
    }
}

/// G-A8: Reset hard backs up every file in the way first — the ignored file at the exact path
/// the target writes, an untracked file, an untracked folder where the target has a file — its
/// blobs equal to the original bytes, with their modes, before the reset runs; Restore writes
/// them back. A fifo in the way refuses it before it runs, naming the fifo.
#[test]
fn g_a8_reset_hard_backs_up_what_is_in_the_way() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = ignoring(&fx, &format!("hard-{lane}"));
        let start = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "x.log", "from git\n", "x.log");
        fx.commit(&dir, "u.txt", "tracked\n", "u.txt");
        fx.commit(&dir, "d", "a file\n", "d");
        let target = fx.oid(&dir, "HEAD");
        fx.git(&dir, &["reset", "-q", "--hard", start.as_str()]);
        std::fs::write(dir.join("x.log"), "mine, ignored\n").unwrap();
        std::fs::write(dir.join("u.txt"), "mine, untracked\r\n").unwrap();
        std::fs::set_permissions(dir.join("u.txt"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::create_dir_all(dir.join("d/e")).unwrap();
        std::fs::write(dir.join("d/e/f.txt"), "deep\n").unwrap();
        std::fs::write(dir.join("d/g.log"), "deep, ignored\n").unwrap();
        let action = Action::Reset {
            to: target.clone(),
            mode: ResetMode::Hard,
        };
        let plan = planned(&runner, &dir, &action).unwrap();
        assert_eq!(
            backup_names(&plan),
            vec!["heads/main".to_string(), "worktree".into(), "files".into()],
            "{lane}"
        );
        let changes = plan.confirm.as_ref().unwrap().changes.join("\n");
        assert!(
            changes.contains("x.log") && changes.contains("u.txt"),
            "{lane}: {changes}"
        );
        let outcome = execute(&plan, dir.to_str().unwrap(), &runner, &Handle::new());
        done(&lane, &outcome);
        let (stamp, _) = fx.newest(&dir);
        let files_ref = format!("refs/sopen-backup/{stamp}/files");
        assert_eq!(
            blob(&fx, &dir, &files_ref, "x.log"),
            ("100644".into(), b"mine, ignored\n".to_vec())
        );
        assert_eq!(
            blob(&fx, &dir, &files_ref, "u.txt"),
            ("100755".into(), b"mine, untracked\r\n".to_vec())
        );
        assert_eq!(blob(&fx, &dir, &files_ref, "d/e/f.txt").1, b"deep\n");
        assert_eq!(blob(&fx, &dir, &files_ref, "d/g.log").1, b"deep, ignored\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("x.log")).unwrap(),
            "from git\n"
        );
        // Restore the files: the target's own files are at those paths now, so "replace".
        let top = dir.to_str().unwrap();
        let record = backup::group(&runner, &Handle::new(), top, &stamp)
            .unwrap()
            .unwrap();
        let repo = repo_of(&runner, &dir);
        let mut entries =
            backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
        for e in &mut entries {
            let path = matches!(e.subject, backup::Subject::Path(_));
            e.chosen = path;
            e.replace = path;
        }
        let (_, outcome) = run(&runner, &dir, &Action::Restore { stamp, entries });
        done(&lane, &outcome);
        assert_eq!(
            std::fs::read_to_string(dir.join("x.log")).unwrap(),
            "mine, ignored\n"
        );
        assert_eq!(
            std::fs::read(dir.join("u.txt")).unwrap(),
            b"mine, untracked\r\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("d/e/f.txt")).unwrap(),
            "deep\n"
        );
        // A fifo in the way: refused before it runs.
        fx.git(&dir, &["reset", "-q", "--hard", start.as_str()]);
        fx.git(&dir, &["clean", "-q", "-f", "-d", "-x"]);
        let fifo = std::ffi::CString::new(dir.join("x.log").to_str().unwrap()).unwrap();
        // SAFETY: a path the test owns.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
        let (_, outcome) = run(&runner, &dir, &action);
        let text = failed_at(&lane, &outcome, 0);
        assert!(text.contains("x.log"), "{lane}: {text}");
        assert_eq!(
            fx.rev(&dir, "HEAD").unwrap(),
            start.as_str(),
            "{lane}: not run"
        );
    }
}

/// G-A8: the deletion-only case — HEAD tracks `p`, the work tree replaced it with a folder `p/`
/// holding an untracked file, a nested one and an ignored one, and the target deletes `p` →
/// Reset hard backs up all three first; afterwards each is present or restorable byte for byte.
#[test]
fn g_a8_reset_hard_over_a_folder_where_a_file_was() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = ignoring(&fx, &format!("deletion-{lane}"));
        let target = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "p", "a file\n", "p");
        std::fs::remove_file(dir.join("p")).unwrap();
        std::fs::create_dir_all(dir.join("p/q")).unwrap();
        std::fs::write(dir.join("p/u.txt"), "u\n").unwrap();
        std::fs::write(dir.join("p/q/n.txt"), "n\n").unwrap();
        std::fs::write(dir.join("p/i.log"), "i\n").unwrap();
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Reset {
                to: target,
                mode: ResetMode::Hard,
            },
        );
        done(&lane, &outcome);
        assert!(backup_names(&plan).contains(&"files".to_string()), "{lane}");
        let (stamp, _) = fx.newest(&dir);
        let files_ref = format!("refs/sopen-backup/{stamp}/files");
        for (path, bytes) in [("p/u.txt", "u\n"), ("p/q/n.txt", "n\n"), ("p/i.log", "i\n")] {
            assert_eq!(
                blob(&fx, &dir, &files_ref, path).1,
                bytes.as_bytes(),
                "{lane}: {path}"
            );
            let there = std::fs::read_to_string(dir.join(path)).ok();
            assert!(
                there.is_none() || there.as_deref() == Some(bytes),
                "{lane}: {path}"
            );
        }
    }
}

/// G-A8: Restore's paths never write outside the work tree or through a link — the backed-up
/// folder replaced by a symlink to a folder outside is refused naming the component; a real
/// folder where a symlink is to be restored and a dangling symlink where a file is to be
/// restored are refused as occupied, nothing created inside the folder or through the link; with
/// "replace" each is backed up, removed and restored.
#[test]
fn g_a8_restore_never_through_a_link() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("links-{lane}"));
        std::fs::create_dir_all(dir.join("dir/sub")).unwrap();
        std::fs::write(dir.join("dir/sub/n.dat"), "n\n").unwrap();
        std::os::unix::fs::symlink("README", dir.join("ln")).unwrap();
        std::fs::write(dir.join("plain"), "plain\n").unwrap();
        let (_, outcome) = run(&runner, &dir, &Action::Clean { folders: true });
        done(&lane, &outcome);
        let (stamp, _) = fx.newest(&dir);
        // The traps.
        let outside = fx.root.join(format!("outside-{lane}"));
        std::fs::create_dir_all(outside.join("sub")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("dir")).unwrap();
        std::fs::create_dir_all(dir.join("ln")).unwrap();
        std::os::unix::fs::symlink("nowhere", dir.join("plain")).unwrap();
        let top = dir.to_str().unwrap();
        let record = backup::group(&runner, &Handle::new(), top, &stamp)
            .unwrap()
            .unwrap();
        let repo = repo_of(&runner, &dir);
        let mut entries =
            backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
        assert!(entries.iter().all(backup::blocked), "{lane}: {entries:#?}");
        assert!(
            entries.iter().all(|e| !e.chosen),
            "{lane}: left unless chosen"
        );
        for e in &mut entries {
            e.chosen = true;
        }
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Restore {
                stamp: stamp.clone(),
                entries: entries.clone(),
            },
        );
        let text = outcome
            .steps
            .iter()
            .flat_map(|s| {
                s.ran
                    .iter()
                    .map(|d| String::from_utf8_lossy(&d.stdout).replace('\0', " "))
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("notfolder dir/sub/n.dat dir"),
            "{lane}: {text}"
        );
        assert!(text.contains("occupied ln"), "{lane}: {text}");
        assert!(text.contains("occupied plain"), "{lane}: {text}");
        assert!(
            !outside.join("sub/n.dat").exists(),
            "{lane}: written through the link"
        );
        assert_eq!(std::fs::read_dir(dir.join("ln")).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_link(dir.join("plain")).unwrap(),
            PathBuf::from("nowhere")
        );
        // With replace: backed up, removed, restored.
        for e in &mut entries {
            e.replace = true;
        }
        let (plan, outcome) = run(&runner, &dir, &Action::Restore { stamp, entries });
        done(&lane, &outcome);
        assert!(backup_names(&plan).contains(&"files".to_string()));
        assert_eq!(
            std::fs::read_to_string(dir.join("dir/sub/n.dat")).unwrap(),
            "n\n"
        );
        assert!(std::fs::symlink_metadata(dir.join("dir")).unwrap().is_dir());
        assert_eq!(
            std::fs::read_link(dir.join("ln")).unwrap(),
            PathBuf::from("README")
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("plain")).unwrap(),
            "plain\n"
        );
        assert!(outside.exists(), "{lane}: a link's target is never removed");
        let (stamp, _) = fx.newest(&dir);
        let saved = format!("refs/sopen-backup/{stamp}/files");
        assert_eq!(
            blob(&fx, &dir, &saved, "plain").0,
            "120000",
            "{lane}: the link backed up"
        );
        assert_eq!(
            blob(&fx, &dir, &saved, "dir").0,
            "120000",
            "{lane}: the link backed up"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// G-A9: pruning

/// G-A9: with `fetch.prune` and `fetch.pruneTags` set, a fetch with the dialog's options off
/// prunes nothing — nor does Add remote's fetch now, a pull or Fetch into, each log line showing
/// `--no-prune --no-prune-tags`; on → the group holds exactly the pruned refs (a remote branch
/// and a local-only tag) and Restore brings them back; a mirror refspec → refused.
#[test]
fn g_a9_pruning() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("prune-{lane}"));
        let bare = with_origin(&fx, &dir, &format!("prune-{lane}"));
        let other = fx.clone_of(&bare, &format!("prune-other-{lane}"));
        fx.git(&other, &["push", "-q", "origin", "main:gone"]);
        fx.git(&dir, &["branch", "side"]);
        fx.git(&dir, &["push", "-q", "origin", "side"]);
        fx.git(&dir, &["fetch", "-q", "origin"]);
        fx.git(&other, &["push", "-q", "origin", "--delete", "gone"]);
        fx.git(&dir, &["tag", "local-only"]);
        fx.git(&dir, &["config", "fetch.prune", "true"]);
        fx.git(&dir, &["config", "fetch.pruneTags", "true"]);
        let kept = || {
            fx.rev(&dir, "refs/remotes/origin/gone").is_some()
                && fx.rev(&dir, "refs/tags/local-only").is_some()
        };
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Fetch {
                prune: false,
                prune_tags: false,
            },
        );
        done(&lane, &outcome);
        assert!(outcome.steps[0].line.contains("--no-prune --no-prune-tags"));
        assert!(kept(), "{lane}: fetch pruned");
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Pull {
                remote: "origin".into(),
                branch: "main".into(),
                how: PullHow::default(),
            },
        );
        done(&lane, &outcome);
        assert!(outcome.steps[0].line.contains("--no-prune --no-prune-tags"));
        assert!(kept(), "{lane}: pull pruned");
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::FetchInto {
                remote: "origin".into(),
                branch: "side".into(),
                force: false,
            },
        );
        done(&lane, &outcome);
        assert!(outcome.steps[0].line.contains("--no-prune --no-prune-tags"));
        assert!(kept(), "{lane}: fetch-into pruned");
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::AddRemote {
                name: "again".into(),
                url: bare.to_str().unwrap().into(),
                push_url: None,
                fetch_now: true,
            },
        );
        done(&lane, &outcome);
        assert!(outcome
            .steps
            .last()
            .unwrap()
            .line
            .contains("--no-prune --no-prune-tags"));
        assert!(kept(), "{lane}: add-remote pruned");
        // On: exactly the pruned refs, restored after.
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Action::Fetch {
                prune: true,
                prune_tags: true,
            },
        );
        done(&lane, &outcome);
        assert!(plan.confirm.is_some());
        assert!(!kept());
        let (stamp, names) = fx.newest(&dir);
        assert_eq!(
            names,
            vec!["manifest", "remotes/origin/gone", "tags/local-only"],
            "{lane}"
        );
        let top = dir.to_str().unwrap();
        let record = backup::group(&runner, &Handle::new(), top, &stamp)
            .unwrap()
            .unwrap();
        let repo = repo_of(&runner, &dir);
        let entries = backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
        let (_, outcome) = run(&runner, &dir, &Action::Restore { stamp, entries });
        done(&lane, &outcome);
        assert!(kept(), "{lane}: restored");
        // A mirror refspec: refused.
        fx.git(
            &dir,
            &[
                "config",
                "--add",
                "remote.again.fetch",
                "+refs/heads/*:refs/heads/*",
            ],
        );
        let refused = planned(
            &runner,
            &dir,
            &Action::FetchRemote {
                remote: "again".into(),
                prune: true,
                prune_tags: false,
            },
        )
        .unwrap_err();
        assert!(refused.0.contains("refs/heads/"), "{lane}: {}", refused.0);
    }
}

// ---------------------------------------------------------------------------------------------
// §8.4, G-S2 and G-S7: overwriting on a remote

/// G-S2: an overwrite push after another clone pushed meanwhile — between the backup's fetch of
/// the remote's value and the push (a `reference-transaction` hook moves it when the backup
/// commits) — is refused with `stale info`, the remote unchanged; a delete the same.
#[test]
fn g_s2_a_remote_that_moved_is_never_overwritten() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        for delete in [false, true] {
            let name = format!("stale-{lane}-{delete}");
            let dir = fx.repo(&name);
            let bare = with_origin(&fx, &dir, &name);
            let other = fx.clone_of(&bare, &format!("{name}-other"));
            fx.git(&dir, &["commit", "-q", "--amend", "-m", "rewritten"]);
            // When the backup's transaction commits, another clone pushes.
            let hook = dir.join(".git/hooks/reference-transaction");
            let once = fx.root.join(format!("{name}-once"));
            script(
                &hook,
                &format!(
                    "[ \"$1\" = committed ] || exit 0\n\
                     grep -q sopen-backup || exit 0\n\
                     [ -e {once} ] && exit 0\n\
                     touch {once}\n\
                     unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX GIT_COMMON_DIR GIT_OBJECT_DIRECTORY\n\
                     cd {other} && echo more >> README && git commit -q -am more && git push -q origin main >/dev/null 2>&1\n",
                    once = sh_word(&once),
                    other = sh_word(&other),
                ),
            );
            let action = if delete {
                Action::DeleteRemoteBranch {
                    remote: "origin".into(),
                    branch: "main".into(),
                }
            } else {
                Action::Push {
                    branch: "main".into(),
                    remotes: vec!["origin".into()],
                    options: PushOptions {
                        set_upstream: false,
                        no_verify: false,
                    },
                    overwrite: true,
                }
            };
            let (_, outcome) = run(&runner, &dir, &action);
            assert!(once.exists(), "{lane}: the hook ran");
            let moved = fx.rev(&other, "HEAD").unwrap();
            let text = failed_at(&lane, &outcome, 1);
            assert!(text.contains("stale info"), "{lane}: {text}");
            assert_eq!(
                fx.rev(&bare, "refs/heads/main").unwrap(),
                moved,
                "{lane}: unchanged"
            );
            fx.no_staging(&dir);
        }
    }
}

/// G-S7: a remote whose `pushurl` differs from its URL → the backup and the lease come from the
/// push URL; two push URLs that disagree → refused, naming both.
#[test]
fn g_s7_the_lease_comes_from_the_push_urls() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("pushurl-{lane}"));
        let fetch = with_origin(&fx, &dir, &format!("pushurl-fetch-{lane}"));
        let push = fx.bare(&format!("pushurl-push-{lane}.git"));
        let first = fx.rev(&dir, "HEAD").unwrap();
        fx.commit(&dir, "p", "p\n", "only on the push URL");
        fx.git(&dir, &["push", "-q", push.to_str().unwrap(), "main"]);
        fx.git(
            &dir,
            &[
                "remote",
                "set-url",
                "--push",
                "origin",
                push.to_str().unwrap(),
            ],
        );
        let pushed = fx.rev(&dir, "HEAD").unwrap();
        fx.git(&dir, &["commit", "-q", "--amend", "-m", "rewritten"]);
        let action = Action::Push {
            branch: "main".into(),
            remotes: vec!["origin".into()],
            options: PushOptions {
                set_upstream: false,
                no_verify: false,
            },
            overwrite: true,
        };
        let (_, outcome) = run(&runner, &dir, &action);
        done(&lane, &outcome);
        let (stamp, _) = fx.newest(&dir);
        assert_eq!(
            fx.rev(
                &dir,
                &format!("refs/sopen-backup/{stamp}/pushed/origin/heads/main")
            )
            .unwrap(),
            pushed,
            "{lane}: the push URL's value, not the fetch URL's"
        );
        assert_eq!(fx.rev(&push, "main"), fx.rev(&dir, "HEAD"));
        assert_eq!(
            fx.rev(&fetch, "main").unwrap(),
            first,
            "{lane}: the fetch URL untouched"
        );
        // Two push URLs that disagree.
        fx.git(
            &dir,
            &[
                "remote",
                "set-url",
                "--add",
                "--push",
                "origin",
                fetch.to_str().unwrap(),
            ],
        );
        fx.git(&dir, &["commit", "-q", "--amend", "-m", "again"]);
        let (_, outcome) = run(&runner, &dir, &action);
        let text = failed_at(&lane, &outcome, 0);
        assert!(
            text.contains(push.to_str().unwrap()) && text.contains(fetch.to_str().unwrap()),
            "{lane}: {text}"
        );
        assert!(text.contains("disagree"), "{lane}: {text}");
        fx.no_staging(&dir);
    }
}

// ---------------------------------------------------------------------------------------------
// §8.5: the probe and the banner (G-S3, G-S8)

fn banner(runner: &GitRunner, dir: &Path) -> Option<Banner> {
    let repo = repo_of(runner, dir);
    Banner::of(&repo.in_progress, &repo.status)
}

fn proceed(runner: &GitRunner, dir: &Path, p: Proceed) -> (Plan, Outcome) {
    run(runner, dir, &Action::Proceed(p))
}

/// G-S3: merge, cherry-pick and revert conflicts → the banner for each; Continue (with
/// `GIT_EDITOR=true` in the log line), Skip and Abort each do what §8.5's table says; a rebase
/// started in a terminal shows the same banner after a refresh.
#[test]
fn g_s3_conflicts_and_the_banner() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        // A merge: the banner, no Skip, Continue after Mark resolved.
        let dir = conflicted(&fx, &format!("bmerge-{lane}"));
        let b = banner(&runner, &dir).expect("a banner");
        assert_eq!(b.state.operation, Some(Operation::Merge));
        assert_eq!(b.conflicts, 1);
        assert!(b.text().starts_with("MERGE"), "{lane}: {}", b.text());
        assert!(b.text().ends_with(" · 1 conflict"), "{lane}: {}", b.text());
        assert_eq!(b.offers(), vec![Proceed::Continue, Proceed::Abort]);
        assert!(planned(&runner, &dir, &Action::Proceed(Proceed::Skip)).is_err());
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Resolved {
                paths: vec!["c.txt".into()],
            },
        );
        done(&lane, &outcome);
        let (plan, outcome) = proceed(&runner, &dir, Proceed::Continue);
        done(&lane, &outcome);
        has_line(&plan, "GIT_EDITOR=true git merge --continue");
        assert!(banner(&runner, &dir).is_none());
        assert_eq!(
            fx.git(&dir, &["log", "-1", "--format=%P"])
                .split_whitespace()
                .count(),
            2
        );
        // A cherry-pick: Abort puts HEAD back.
        fx.git(&dir, &["checkout", "-q", "-b", "pick-here", "HEAD~1"]);
        let head = fx.rev(&dir, "HEAD").unwrap();
        let side = fx.rev(&dir, "side").unwrap();
        assert!(!fx.output(&dir, &["cherry-pick", &side]).status.success());
        let b = banner(&runner, &dir).unwrap();
        assert_eq!(b.state.operation, Some(Operation::CherryPick));
        assert!(b.text().starts_with("CHERRY-PICK"), "{lane}: {}", b.text());
        assert_eq!(
            b.offers(),
            vec![Proceed::Continue, Proceed::Skip, Proceed::Abort]
        );
        let (plan, outcome) = proceed(&runner, &dir, Proceed::Abort);
        done(&lane, &outcome);
        has_line(&plan, "git cherry-pick --abort");
        assert_eq!(fx.rev(&dir, "HEAD").unwrap(), head);
        assert!(banner(&runner, &dir).is_none());
        // A revert: Skip.
        let main_commit = fx.rev(&dir, "main~1").unwrap();
        fx.commit(&dir, "c.txt", "later\n", "later");
        assert!(!fx
            .output(&dir, &["revert", "--no-edit", &main_commit])
            .status
            .success());
        let b = banner(&runner, &dir).unwrap();
        assert_eq!(b.state.operation, Some(Operation::Revert));
        assert!(b.text().starts_with("REVERT"), "{lane}: {}", b.text());
        let (plan, outcome) = proceed(&runner, &dir, Proceed::Skip);
        done(&lane, &outcome);
        has_line(&plan, "git revert --skip");
        assert!(banner(&runner, &dir).is_none());
        // A rebase started in a terminal: the same banner after a refresh; Abort.
        fx.git(&dir, &["checkout", "-q", "-b", "r1", "side~1"]);
        fx.commit(&dir, "c.txt", "r1\n", "rebased one");
        let before = fx.rev(&dir, "HEAD").unwrap();
        assert!(!fx.output(&dir, &["rebase", "main"]).status.success());
        let b = banner(&runner, &dir).unwrap();
        assert_eq!(b.state.operation, Some(Operation::Rebase));
        assert!(
            b.text().starts_with("REBASE 1/1 · stopped at "),
            "{lane}: {}",
            b.text()
        );
        assert!(b.text().contains("rebased one"), "{lane}: {}", b.text());
        let (plan, outcome) = proceed(&runner, &dir, Proceed::Abort);
        done(&lane, &outcome);
        has_line(&plan, "git rebase --abort");
        assert_eq!(fx.rev(&dir, "HEAD").unwrap(), before);
    }
}

/// G-S8: am, a rebase of the apply backend, a sequence left with only `sequencer/`, and bisect →
/// the banner names each and its Continue / Skip / Abort (bisect: Reset) work; an edit stop
/// says `stopped to edit`.
#[test]
fn g_s8_every_state_of_the_table() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = conflicted(&fx, &format!("states-{lane}"));
        fx.git(&dir, &["merge", "--abort"]);
        // am: a patch that does not apply.
        let patch = fx.git(&dir, &["format-patch", "-1", "--stdout", "side"]);
        let patch_file = fx.root.join(format!("p-{lane}.patch"));
        std::fs::write(&patch_file, patch).unwrap();
        assert!(!fx
            .output(&dir, &["am", patch_file.to_str().unwrap()])
            .status
            .success());
        let b = banner(&runner, &dir).unwrap();
        assert_eq!(b.state.operation, Some(Operation::Am));
        assert!(b.text().starts_with("AM"), "{lane}: {}", b.text());
        let (plan, outcome) = proceed(&runner, &dir, Proceed::Skip);
        done(&lane, &outcome);
        has_line(&plan, "git am --skip");
        assert!(banner(&runner, &dir).is_none());
        // A rebase, apply backend.
        fx.git(&dir, &["checkout", "-q", "side"]);
        assert!(!fx
            .output(&dir, &["rebase", "--apply", "main"])
            .status
            .success());
        let b = banner(&runner, &dir).unwrap();
        assert_eq!(b.state.operation, Some(Operation::RebaseApply));
        assert!(b.text().starts_with("REBASE"), "{lane}: {}", b.text());
        let (plan, outcome) = proceed(&runner, &dir, Proceed::Abort);
        done(&lane, &outcome);
        has_line(&plan, "git rebase --abort");
        // A sequence left with only sequencer/.
        fx.git(&dir, &["checkout", "-q", "main"]);
        fx.git(&dir, &["checkout", "-q", "-b", "two"]);
        let p1 = fx.commit(&dir, "c.txt", "p1\n", "p1");
        let p2 = fx.commit(&dir, "q.txt", "q\n", "p2");
        fx.git(&dir, &["checkout", "-q", "main"]);
        fx.commit(&dir, "c.txt", "main again\n", "main again");
        assert!(!fx
            .output(&dir, &["cherry-pick", p1.as_str(), p2.as_str()])
            .status
            .success());
        std::fs::write(dir.join("c.txt"), "resolved\n").unwrap();
        fx.git(&dir, &["add", "c.txt"]);
        fx.git(&dir, &["commit", "-q", "-m", "by hand"]);
        let b = banner(&runner, &dir).unwrap();
        assert_eq!(b.state.operation, Some(Operation::Sequence(Sequence::Pick)));
        assert!(b.text().starts_with("CHERRY-PICK"), "{lane}: {}", b.text());
        let (plan, outcome) = proceed(&runner, &dir, Proceed::Continue);
        done(&lane, &outcome);
        has_line(&plan, "GIT_EDITOR=true git cherry-pick --continue");
        assert!(dir.join("q.txt").exists());
        assert!(banner(&runner, &dir).is_none());
        // bisect.
        fx.git(&dir, &["bisect", "start", "HEAD", "HEAD~2"]);
        let b = banner(&runner, &dir).unwrap();
        assert!(b.state.bisect);
        assert_eq!(b.text(), "BISECT · Reset");
        assert_eq!(b.offers(), vec![Proceed::BisectReset]);
        let (plan, outcome) = proceed(&runner, &dir, Proceed::BisectReset);
        done(&lane, &outcome);
        has_line(&plan, "git bisect reset");
        assert!(banner(&runner, &dir).is_none());
        // An edit stop.
        let mut edit = fx.command(&dir, &["rebase", "-i", "HEAD~1"]);
        edit.env("GIT_SEQUENCE_EDITOR", "sed -i.bak -e 's/^pick/edit/'");
        let out = edit.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let b = banner(&runner, &dir).unwrap();
        let stop = b.edit_stop().expect("an edit stop").to_string();
        assert_eq!(b.text(), format!("stopped to edit {stop}"));
        assert_eq!(b.offers(), vec![Proceed::Continue, Proceed::Abort]);
        let (_, outcome) = proceed(&runner, &dir, Proceed::Continue);
        done(&lane, &outcome);
        assert!(banner(&runner, &dir).is_none());
    }
}

/// The probe's parser on its own records (03 §8.5's table, the first match wins) and the
/// banner's text from them.
#[test]
fn the_probe_parses_each_state() {
    let p = |records: &[&str]| {
        let mut bytes = Vec::new();
        for r in records {
            bytes.extend_from_slice(r.as_bytes());
            bytes.push(0);
        }
        parse_probe(&bytes)
    };
    assert_eq!(p(&[]).operation, None);
    let r = p(&[
        "exists rebase-merge",
        "exists MERGE_HEAD",
        "msgnum 3",
        "end 7",
        "stopped-sha 0123456789abcdef0123456789abcdef01234567",
        "done-last pick 0123456789abcdef0123456789abcdef01234567 # the subject",
    ]);
    assert_eq!(r.operation, Some(Operation::Rebase));
    assert_eq!(r.rebase.as_ref().unwrap().msgnum.as_deref(), Some("3"));
    let b = Banner {
        state: r,
        conflicts: 2,
    };
    assert_eq!(
        b.text(),
        "REBASE 3/7 · stopped at 01234567 the subject · 2 conflicts"
    );
    assert_eq!(
        p(&["exists rebase-apply", "exists applying"]).operation,
        Some(Operation::Am)
    );
    assert_eq!(
        p(&["exists rebase-apply", "exists rebasing"]).operation,
        Some(Operation::RebaseApply)
    );
    assert_eq!(
        p(&["exists sequencer", "todo-first revert 0123abc x"]).operation,
        Some(Operation::Sequence(Sequence::Revert))
    );
    let both = p(&["exists CHERRY_PICK_HEAD", "exists BISECT_LOG"]);
    assert_eq!(both.operation, Some(Operation::CherryPick));
    assert!(both.bisect);
    let b = Banner {
        state: both,
        conflicts: 0,
    };
    assert_eq!(b.text(), "CHERRY-PICK\nBISECT · Reset");
    assert_eq!(
        b.offers(),
        vec![
            Proceed::Continue,
            Proceed::Skip,
            Proceed::Abort,
            Proceed::BisectReset
        ]
    );
    assert_eq!(
        probe_paths(b"a\nb\nc\nd\ne\nf\ng\n").map(|v| v.len()),
        Some(7)
    );
    assert_eq!(probe_paths(b"a\nb\n"), None);
}

// ---------------------------------------------------------------------------------------------
// G-S5, G-R5: locks and waits

/// G-S5: `index.lock` held by another process → a write fails with git's message; the lock is
/// still there.
#[test]
fn g_s5_a_held_lock_is_never_removed() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("lock-{lane}"));
        let lock = dir.join(".git/index.lock");
        std::fs::write(&lock, "").unwrap();
        std::fs::write(dir.join("README"), "changed\n").unwrap();
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Stage {
                paths: vec!["README".into()],
            },
        );
        let text = failed_at(&lane, &outcome, 0);
        assert!(text.contains("index.lock"), "{lane}: {text}");
        assert!(lock.exists(), "{lane}: the lock stays");
        std::fs::remove_file(&lock).unwrap();
    }
}

/// A local HTTP server that answers every request with 401 and a Basic challenge, for 30 s.
fn unauthorized() -> u16 {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut s, _)) => {
                    s.set_nonblocking(false).unwrap();
                    s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf);
                    let _ = s.write_all(
                        b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"x\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    });
    port
}

/// Is `pid` alive?
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                dirs.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

/// G-R5 (S3-A's half): a `pre-commit` hook, a signing program and a credential helper that each
/// wait forever → the handle's silence grows while the step runs; Esc stops it within 6 s, the
/// step `failed` and the rest `not run`, no process of the stand-in left and no `*.lock`; no
/// commit was made and the staged change is still staged. (The 10 s status-row wording is the
/// panel's.)
#[test]
fn g_r5_a_wait_that_never_ends_is_stopped() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        for waiter in ["hook", "signer", "credential"] {
            let dir = fx.repo(&format!("wait-{lane}-{waiter}"));
            let pid_file = fx.root.join(format!("pid-{lane}-{waiter}"));
            let stand_in = fx.root.join(format!("stand-in-{lane}-{waiter}"));
            script(
                &stand_in,
                &format!("echo $$ > {}\nexec sleep 1000", sh_word(&pid_file)),
            );
            let action = match waiter {
                "hook" => {
                    let hook = dir.join(".git/hooks/pre-commit");
                    std::fs::copy(&stand_in, &hook).unwrap();
                    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
                        .unwrap();
                    None
                }
                "signer" => {
                    fx.git(&dir, &["config", "commit.gpgSign", "true"]);
                    fx.git(&dir, &["config", "gpg.program", stand_in.to_str().unwrap()]);
                    None
                }
                _ => {
                    let port = unauthorized();
                    fx.git(
                        &dir,
                        &[
                            "remote",
                            "add",
                            "web",
                            &format!("http://127.0.0.1:{port}/x.git"),
                        ],
                    );
                    fx.git(
                        &dir,
                        &[
                            "config",
                            "credential.helper",
                            &format!("!{}", stand_in.to_str().unwrap()),
                        ],
                    );
                    Some(Action::FetchRemote {
                        remote: "web".into(),
                        prune: false,
                        prune_tags: false,
                    })
                }
            };
            std::fs::write(dir.join("README"), "staged\n").unwrap();
            fx.git(&dir, &["add", "README"]);
            let head = fx.rev(&dir, "HEAD").unwrap();
            let action = action.unwrap_or(Action::Commit {
                message: "never made\n".into(),
                amend: false,
                no_verify: false,
            });
            let plan = planned(&runner, &dir, &action).unwrap();
            let handle = Handle::new();
            let (r, h, top) = (
                runner.clone(),
                handle.clone(),
                dir.to_str().unwrap().to_string(),
            );
            let worker = std::thread::spawn(move || execute(&plan, &top, &r, &h));
            let deadline = Instant::now() + Duration::from_secs(20);
            while std::fs::read_to_string(&pid_file).map_or(true, |s| s.trim().is_empty()) {
                assert!(
                    Instant::now() < deadline,
                    "{lane} {waiter}: the stand-in never ran"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            let pid: i32 = std::fs::read_to_string(&pid_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            std::thread::sleep(Duration::from_millis(1200));
            assert!(
                handle
                    .silent_for()
                    .is_some_and(|d| d >= Duration::from_secs(1)),
                "{lane} {waiter}: {:?}",
                handle.silent_for()
            );
            let t = Instant::now();
            handle.stop();
            let outcome = worker.join().unwrap();
            assert!(
                t.elapsed() < Duration::from_secs(6),
                "{lane} {waiter}: {:?}",
                t.elapsed()
            );
            let at = outcome
                .steps
                .iter()
                .position(|s| s.outcome == StepOutcome::Failed)
                .expect("a failed step");
            failed_at(&lane, &outcome, at);
            let deadline = Instant::now() + Duration::from_secs(3);
            while alive(pid) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(!alive(pid), "{lane} {waiter}: the stand-in is left");
            let locks: Vec<_> = walk(&dir.join(".git"))
                .into_iter()
                .filter(|p| p.extension().is_some_and(|e| e == "lock"))
                .collect();
            assert!(locks.is_empty(), "{lane} {waiter}: {locks:?}");
            assert_eq!(
                fx.rev(&dir, "HEAD").unwrap(),
                head,
                "{lane} {waiter}: no commit"
            );
            assert_eq!(
                fx.git(&dir, &["diff", "--cached", "--name-only"]),
                "README\n",
                "{lane} {waiter}: still staged"
            );
        }
    }
}
