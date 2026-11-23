//! Detection's tests (03 §2; G-D1, G-D2): every row of §2.1's table, the cache. S2-B writes them.

use super::history::{real, sh, TestGit};
use crate::git::detect::{self, Detector};
use crate::git::history::{Bound, Git, Pipe, Ran, RunError};
use crate::git::model::{Detection, NoWorkTree, Unavailable};
use crate::git::parse;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::Mutex;

/// A repository at `dir` with one commit, and the folder `sub/deep` in it.
fn repo(dir: &Path) {
    std::fs::create_dir_all(dir.join("sub/deep")).unwrap();
    std::fs::write(dir.join("sub/deep/f.txt"), "f\n").unwrap();
    sh(dir, &["init", "-q", "-b", "main"]);
    sh(dir, &["add", "."]);
    sh(dir, &["commit", "-q", "-m", "one"]);
}

/// A work tree at `top` as detection reports it when run in a folder of it.
fn work_tree(top: &str, git_dir: &str, common_dir: &str) -> Detection {
    tree_with(top, git_dir, common_dir, false, None, false)
}

fn tree_with(
    top: &str,
    git_dir: &str,
    common_dir: &str,
    linked: bool,
    submodule_of: Option<&str>,
    shallow: bool,
) -> Detection {
    Detection::WorkTree {
        top: top.into(),
        git_dir: git_dir.into(),
        common_dir: common_dir.into(),
        linked,
        submodule_of: submodule_of.map(str::to_string),
        shallow,
    }
}

/// G-D1 (03 §2.1, 03:1091): a work tree's subfolder is a work tree whose top level is the
/// repository's; a bare repository, the inside of `.git`, a plain folder and a git that does not
/// exist (`SOPEN_GIT=/nonexistent`) keep the list, each with the line that says why — bare /
/// not a work tree / not a repository / no git.
#[test]
fn g_d1_a_work_tree_shows_the_panel_and_the_rest_say_why_not() {
    let Some(_) = TestGit::local() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let root = real(tmp.path());
    let git = TestGit::local()
        .unwrap()
        .env("GIT_CEILING_DIRECTORIES", &root);
    let at = |p: &str| format!("{root}/{p}");
    repo(&tmp.path().join("repo"));
    sh(tmp.path(), &["clone", "-q", "--bare", "repo", "bare.git"]);
    std::fs::create_dir(tmp.path().join("plain")).unwrap();
    let detector = Detector::new();

    let found = detector.detect("host", &at("repo/sub/deep"), &git);
    let top = at("repo");
    let dot_git = at("repo/.git");
    assert_eq!(found, work_tree(&top, &dot_git, &dot_git));
    assert!(found.shows_panel());
    assert_eq!(found.reason("host"), None);
    assert_eq!(found.title_notes(), "");

    let cases = [
        ("bare.git", Detection::NoWorkTree(NoWorkTree::Bare), "bare"),
        (
            "repo/.git",
            Detection::NoWorkTree(NoWorkTree::GitDir),
            "not a work tree",
        ),
        (
            "repo/.git/objects",
            Detection::NoWorkTree(NoWorkTree::GitDir),
            "not a work tree",
        ),
        ("plain", Detection::NotARepo(None), "not a repository"),
    ];
    for (folder, want, why) in cases {
        let got = detector.detect("host", &at(folder), &git);
        assert_eq!(got, want, "{folder}");
        assert!(!got.shows_panel());
        let reason = got.reason("host").unwrap();
        assert!(
            reason.starts_with("git panel: ") && reason.contains(why),
            "{reason}"
        );
        assert!(
            !got.on_status_row(),
            "{folder} says why only in the listing"
        );
    }

    // SOPEN_GIT=/nonexistent: §2.4 takes it as given, and running it finds no git.
    let program = detect::local_git(Some(OsStr::new("/nonexistent")), None, false, || true);
    assert_eq!(program.as_deref(), Ok(Path::new("/nonexistent")));
    let missing = TestGit::with_program("/nonexistent");
    let got = Detector::new().detect("host", &at("repo"), &missing);
    assert_eq!(got, Detection::Unavailable(Unavailable::NoGit));
    assert_eq!(got.reason("host").unwrap(), "git panel: no git on host");
    assert!(got.on_status_row());
}

/// G-D2 (03 §2.1, §2.3, 03:1095): a submodule, a linked worktree and a shallow clone are work
/// trees with `submodule_of`, `linked` and `shallow`, and the frame title's notes say so; a
/// folder reached through a symbolic link is not mistaken for a linked worktree (git prints the
/// common dir relative to the folder's real path).
#[test]
fn g_d2_submodule_worktree_and_shallow_clone() {
    let Some(_) = TestGit::local() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let root = real(tmp.path());
    let git = TestGit::local()
        .unwrap()
        .env("GIT_CEILING_DIRECTORIES", &root);
    let at = |p: &str| format!("{root}/{p}");
    repo(&tmp.path().join("repo"));
    let detector = Detector::new();

    // A submodule: the innermost repository, with its superproject.
    std::fs::create_dir(tmp.path().join("super")).unwrap();
    sh(&tmp.path().join("super"), &["init", "-q", "-b", "main"]);
    sh(
        &tmp.path().join("super"),
        &["submodule", "add", "-q", &at("repo"), "sm"],
    );
    let modules = at("super/.git/modules/sm");
    let got = detector.detect("host", &at("super/sm/sub"), &git);
    assert_eq!(
        got,
        Detection::WorkTree {
            top: at("super/sm"),
            git_dir: modules.clone(),
            common_dir: modules,
            linked: false,
            submodule_of: Some(at("super")),
            shallow: false,
        }
    );
    assert_eq!(
        got.title_notes(),
        format!(" (submodule of {})", at("super"))
    );

    // A linked worktree: its own git dir, the main repository's common dir.
    sh(
        &tmp.path().join("repo"),
        &["worktree", "add", "-q", &at("wt"), "-b", "wtb"],
    );
    let got = detector.detect("host", &at("wt/sub"), &git);
    assert_eq!(
        got,
        Detection::WorkTree {
            top: at("wt"),
            git_dir: at("repo/.git/worktrees/wt"),
            common_dir: at("repo/.git"),
            linked: true,
            submodule_of: None,
            shallow: false,
        }
    );
    assert_eq!(got.title_notes(), format!(" (worktree of {})", at("repo")));

    // A shallow clone.
    let url = format!("file://{}", at("repo"));
    sh(
        tmp.path(),
        &["clone", "-q", "--depth", "1", &url, "shallow"],
    );
    let got = detector.detect("host", &at("shallow"), &git);
    let dot_git = at("shallow/.git");
    let Detection::WorkTree {
        shallow, linked, ..
    } = &got
    else {
        panic!("{got:?}");
    };
    assert!(*shallow && !*linked);
    assert_eq!(
        got,
        tree_with(&at("shallow"), &dot_git, &dot_git, false, None, true)
    );
    assert_eq!(got.title_notes(), " (shallow)");

    // The main work tree through a symbolic link: git prints `../.git` from the real folder.
    std::os::unix::fs::symlink(at("repo"), tmp.path().join("link")).unwrap();
    let got = detector.detect("host", &at("link/sub"), &git);
    let Detection::WorkTree { top, linked, .. } = &got else {
        panic!("{got:?}");
    };
    assert_eq!(top, &at("repo"));
    assert!(!linked, "a symbolic link is not a linked worktree: {got:?}");
    assert_eq!(got.title_notes(), "");
}

/// Every row of 03 §2.1's table from the bytes git prints (verified with git 2.56.1), and what
/// the table does not have: each is an answer, never a panic.
#[test]
fn detection_reads_every_row_from_gits_bytes() {
    let wt = b"false\nfalse\nfalse\n/r/.git\n.git\n/r\n";
    let deep = b"false\nfalse\nfalse\n/r/.git\n../../.git\n/r\n";
    let sm = b"false\nfalse\nfalse\n/s/.git/modules/m\n/s/.git/modules/m\n/s/m\n/s\n";
    let linked = b"false\nfalse\nfalse\n/r/.git/worktrees/w\n/r/.git\n/w\n";
    let shallow = b"false\nfalse\ntrue\n/r/.git\n.git\n/r\n";
    let bare = b"true\ntrue\nfalse\n/b.git\n.\n";
    let gitdir = b"false\ntrue\nfalse\n/r/.git\n.\n";
    let work = b"fatal: this operation must be run in a work tree\n";
    let none = b"fatal: not a git repository (or any of the parent directories): .git\n";
    let mount = b"fatal: not a git repository (or any parent up to mount point /mnt)\n\
                  Stopping at filesystem boundary (GIT_DISCOVERY_ACROSS_FILESYSTEM not set).\n";
    let owner = b"fatal: detected dubious ownership in repository at '/r'\n\
                  To add an exception for this directory, call:\n\n\
                  \tgit config --global --add safe.directory /r\n";
    let lossy_top = b"false\nfalse\nfalse\n/r\xff/.git\n.git\n/r\xff\n";

    let tree = |top: &str, git_dir: &str, common: &str| work_tree(top, git_dir, common);
    let is = |code: Option<i32>, out: &[u8], err: &[u8], folder: &str, want: Detection| {
        let got = parse::detection(code, out, err, folder);
        assert_eq!(got, want, "{:?}", String::from_utf8_lossy(out));
    };
    is(Some(0), wt, b"", "/r", tree("/r", "/r/.git", "/r/.git"));
    is(
        Some(0),
        deep,
        b"",
        "/r/a/b",
        tree("/r", "/r/.git", "/r/.git"),
    );
    is(
        Some(0),
        sm,
        b"",
        "/s/m",
        tree_with(
            "/s/m",
            "/s/.git/modules/m",
            "/s/.git/modules/m",
            false,
            Some("/s"),
            false,
        ),
    );
    is(
        Some(0),
        linked,
        b"",
        "/w",
        tree_with("/w", "/r/.git/worktrees/w", "/r/.git", true, None, false),
    );
    is(
        Some(0),
        shallow,
        b"",
        "/r",
        tree_with("/r", "/r/.git", "/r/.git", false, None, true),
    );
    let bare_one = Detection::NoWorkTree(NoWorkTree::Bare);
    is(Some(128), bare, work, "/b.git", bare_one);
    let in_git_dir = Detection::NoWorkTree(NoWorkTree::GitDir);
    is(Some(128), gitdir, work, "/r/.git", in_git_dir);
    is(Some(128), b"", none, "/p", Detection::NotARepo(None));
    let line = "fatal: not a git repository (or any parent up to mount point /mnt)";
    is(
        Some(128),
        b"",
        mount,
        "/mnt/x",
        Detection::NotARepo(Some(line.into())),
    );
    let line = "fatal: detected dubious ownership in repository at '/r'";
    is(Some(128), b"", owner, "/r", Detection::Refused(line.into()));
    let shown = Detection::Unavailable(Unavailable::NotUtf8("/r\u{fffd}".into()));
    is(Some(0), lossy_top, b"", "/r", shown);

    // What the table does not have: a defined answer that keeps the list, with git's line.
    let odd = |code: Option<i32>, out: &[u8], err: &[u8], why: &str| {
        let got = parse::detection(code, out, err, "/r");
        assert_eq!(got, Detection::Unavailable(Unavailable::Failed(why.into())));
        assert!(!got.shows_panel());
    };
    let gone = "fatal: cannot change to '/x': No such file or directory";
    odd(Some(128), b"", format!("{gone}\n").as_bytes(), gone);
    let usage = "usage: git rev-parse --parseopt";
    odd(Some(129), b"", format!("{usage}\n").as_bytes(), usage);
    odd(None, b"", b"", "git ended without an exit status");
    odd(
        Some(0),
        b"false\nfalse\nfalse\n/r/.git\n",
        b"",
        "git's answer has 4 lines",
    );
    let maybe = b"maybe\nfalse\nfalse\n/r/.git\n.git\n/r\n";
    odd(
        Some(0),
        maybe,
        b"",
        "git's answer is not true or false: maybe",
    );
    let relative = b"false\nfalse\nfalse\n/r/.git\n.git\nrelative\n";
    odd(
        Some(0),
        relative,
        b"",
        "the top level is not absolute: relative",
    );
    odd(Some(0), b"", b"", "git's answer has 0 lines");
}

/// The live rows a CI host may lack, each guarded so another host cannot turn them red: git's
/// refusal of a repository another user owns (git's own `GIT_TEST_ASSUME_DIFFERENT_OWNER`), and
/// a folder that is a mount point (`/dev`, when it is one).
#[test]
fn refused_and_the_filesystem_boundary_from_a_live_git() {
    let Some(_) = TestGit::local() else { return };
    let tmp = tempfile::tempdir().unwrap();
    repo(&tmp.path().join("repo"));
    let folder = real(&tmp.path().join("repo"));
    let other = TestGit::local()
        .unwrap()
        .env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1");
    match detect::run(&other, &folder) {
        Detection::Refused(line) => {
            assert!(line.contains("dubious ownership"), "{line}");
            let why = Detection::Refused(line.clone()).reason("host").unwrap();
            assert_eq!(
                why,
                format!(
                    "git panel: git refuses this repository: {}",
                    line.trim_start_matches("fatal: ")
                )
            );
        }
        Detection::WorkTree { .. } => {
            eprintln!("skipping: this git has no GIT_TEST_ASSUME_DIFFERENT_OWNER")
        }
        got => panic!("{got:?}"),
    }

    use std::os::unix::fs::MetadataExt;
    let mount = std::fs::metadata("/dev").map(|m| m.dev()).ok()
        != std::fs::metadata("/").map(|m| m.dev()).ok();
    if !mount {
        eprintln!("skipping: /dev is not a mount point here");
        return;
    }
    let git = TestGit::local().unwrap();
    let got = detect::run(&git, "/dev");
    let Detection::NotARepo(Some(line)) = &got else {
        panic!("{got:?}");
    };
    assert!(line.contains("mount point"), "{line}");
    assert!(got.on_status_row());
    assert!(got.reason("host").unwrap().contains("not a repository"));
}

type Answer = dyn Fn(&str, &[String]) -> Result<Ran, RunError> + Send + Sync;

/// A runner that answers from a table and records what it was asked.
struct Fake {
    answer: Box<Answer>,
    calls: Mutex<Vec<String>>,
}

impl Fake {
    fn new(
        answer: impl Fn(&str, &[String]) -> Result<Ran, RunError> + Send + Sync + 'static,
    ) -> Fake {
        Fake {
            answer: Box::new(answer),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

impl Git for Fake {
    fn run(&self, dir: &str, args: &[String], _: &[u8], bound: Bound) -> Result<Ran, RunError> {
        assert_eq!(
            bound,
            Bound::Detection,
            "detection is bounded at 10 s in all"
        );
        self.calls
            .lock()
            .unwrap()
            .push(format!("{dir} {}", args[0]));
        (self.answer)(dir, args)
    }

    fn spawn(&self, _: &str, _: &[String], _: &[u8]) -> Result<Box<dyn Pipe>, RunError> {
        Err(RunError::Failed("detection starts no stream".into()))
    }
}

fn ok(stdout: &str) -> Result<Ran, RunError> {
    Ok(Ran {
        code: Some(0),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    })
}

/// 03 §2.2, §2.4: a folder's result is cached per `(host, folder)` until forgotten; `git version`
/// runs once per host; a missing git is cached per host; a git below 2.20 is refused before
/// detection; the 10 s bound passing is `TimedOut`; a failure is not cached.
#[test]
fn the_cache_and_the_version_floor() {
    let tree = "false\nfalse\nfalse\n/r/.git\n.git\n/r\n";
    let git = Fake::new(move |_, args| match args[0].as_str() {
        "version" => ok("git version 2.56.1\n"),
        _ => ok(tree),
    });
    let detector = Detector::new();
    let want = work_tree("/r", "/r/.git", "/r/.git");
    assert_eq!(detector.detect("h", "/r", &git), want);
    assert_eq!(git.calls(), ["/r version", "/r rev-parse"]);
    assert_eq!(detector.detect("h", "/r", &git), want);
    assert_eq!(detector.cached("h", "/r"), Some(want.clone()));
    assert_eq!(git.calls(), Vec::<String>::new(), "cached");
    assert_eq!(detector.version("h").unwrap().text, "2.56.1");
    detector.detect("h", "/r/a", &git);
    assert_eq!(
        git.calls(),
        ["/r/a rev-parse"],
        "the version is asked once per host"
    );
    detector.forget("h", "/r");
    assert_eq!(detector.cached("h", "/r"), None);
    detector.detect("h", "/r", &git);
    assert_eq!(git.calls(), ["/r rev-parse"]);
    detector.detect("other", "/r", &git);
    assert_eq!(git.calls(), ["/r version", "/r rev-parse"], "another host");
    detector.forget_host("h");
    detector.forget("h", "/r");
    detector.detect("h", "/r", &git);
    assert_eq!(
        git.calls(),
        ["/r version", "/r rev-parse"],
        "a hard refresh"
    );

    // No git: locally a spawn's NotFound, remotely `sopen: no git` with exit 127; per host.
    for answer in [
        Err(RunError::NoGit),
        Ok(Ran {
            code: Some(127),
            stdout: Vec::new(),
            stderr: b"sopen: no git\n".to_vec(),
        }),
    ] {
        let git = Fake::new(move |_, _| answer.clone());
        let detector = Detector::new();
        assert_eq!(
            detector.detect("h", "/a", &git),
            Detection::Unavailable(Unavailable::NoGit)
        );
        assert_eq!(git.calls(), ["/a version"]);
        assert_eq!(
            detector.detect("h", "/b", &git),
            Detection::Unavailable(Unavailable::NoGit)
        );
        assert_eq!(
            git.calls(),
            Vec::<String>::new(),
            "no git is cached per host"
        );
        // The list's re-read (03:71–72) detects again: git may have been installed since.
        detector.forget("h", "/b");
        detector.detect("h", "/b", &git);
        assert_eq!(git.calls(), ["/b version"], "a re-read asks for git again");
    }

    // Too old: refused before detection, with 03 §2.3's line; a re-read asks again.
    let git = Fake::new(|_, _| ok("git version 2.17.1\n"));
    let detector = Detector::new();
    let got = detector.detect("h", "/a", &git);
    assert_eq!(
        got,
        Detection::Unavailable(Unavailable::TooOld("2.17.1".into()))
    );
    assert_eq!(git.calls(), ["/a version"]);
    assert_eq!(
        got.reason("h").unwrap(),
        "git panel: git 2.17.1 on h; it needs 2.20"
    );
    detector.forget("h", "/a");
    detector.detect("h", "/a", &git);
    assert_eq!(
        git.calls(),
        ["/a version"],
        "a re-read asks for the version again"
    );
    let git = Fake::new(move |_, args| match args[0].as_str() {
        "version" => ok("git version 2.20.0\n"),
        _ => ok(tree),
    });
    assert!(
        Detector::new().detect("h", "/r", &git).shows_panel(),
        "2.20 is enough"
    );

    // The bound passed.
    let git = Fake::new(|_, args| match args[0].as_str() {
        "version" => ok("git version 2.39.5 (Apple Git-154)\n"),
        _ => Err(RunError::TimedOut),
    });
    let detector = Detector::new();
    assert_eq!(
        detector.detect("h", "/a", &git),
        Detection::Unavailable(Unavailable::TimedOut)
    );
    assert_eq!(
        detector.version("h").unwrap().text,
        "2.39.5 (Apple Git-154)"
    );

    // A failure is asked again next time.
    let git = Fake::new(|_, args| match args[0].as_str() {
        "version" => ok("git version 2.56.1\n"),
        _ => Err(RunError::Failed("the channel was lost".into())),
    });
    let detector = Detector::new();
    let got = detector.detect("h", "/a", &git);
    assert_eq!(
        got,
        Detection::Unavailable(Unavailable::Failed("the channel was lost".into()))
    );
    assert_eq!(detector.cached("h", "/a"), None);
    detector.detect("h", "/a", &git);
    assert_eq!(git.calls(), ["/a version", "/a rev-parse", "/a rev-parse"]);
}

/// 03 §2.3's three status-row texts, literally, and which results say why on the status row;
/// every other result's line names its reason.
#[test]
fn the_status_row_says_why_as_03_writes_it() {
    let refused =
        Detection::Refused("fatal: detected dubious ownership in repository at '/r'".into());
    assert_eq!(
        refused.reason("host").unwrap(),
        "git panel: git refuses this repository: detected dubious ownership in repository at '/r'"
    );
    let no_git = Detection::Unavailable(Unavailable::NoGit);
    assert_eq!(no_git.reason("host").unwrap(), "git panel: no git on host");
    let old = Detection::Unavailable(Unavailable::TooOld("2.17.1".into()));
    assert_eq!(
        old.reason("host").unwrap(),
        "git panel: git 2.17.1 on host; it needs 2.20"
    );
    let boundary = Detection::NotARepo(Some(
        "fatal: not a git repository (or any parent up to mount point /mnt)".into(),
    ));
    let timed_out = Detection::Unavailable(Unavailable::TimedOut);
    let lossy = Detection::Unavailable(Unavailable::NotUtf8("/r\u{fffd}".into()));
    let failed = Detection::Unavailable(Unavailable::Failed("the channel was lost".into()));
    for (d, has) in [
        (&refused, "refuses"),
        (&no_git, "no git"),
        (&old, "2.20"),
        (&boundary, "mount point /mnt"),
        (&timed_out, "10 s"),
        (&lossy, "not UTF-8"),
        (&failed, "the channel was lost"),
    ] {
        assert!(d.on_status_row(), "{d:?}");
        let why = d.reason("host").unwrap();
        assert!(why.starts_with("git panel: ") && why.contains(has), "{why}");
    }
    for d in [
        Detection::NoWorkTree(NoWorkTree::Bare),
        Detection::NoWorkTree(NoWorkTree::GitDir),
        Detection::NotARepo(None),
    ] {
        assert!(!d.on_status_row(), "{d:?}");
    }
}

/// Which git (03 §2.4): `SOPEN_GIT` when set (empty is ignored), else the first `git` on `PATH`;
/// on macOS `/usr/bin/git` counts only when `xcode-select -p` succeeds — and is never asked about
/// elsewhere.
#[test]
fn which_git_is_sopen_git_then_path_without_the_shim() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, c) = (
        tmp.path().join("a"),
        tmp.path().join("b"),
        tmp.path().join("c"),
    );
    for d in [&a, &b, &c] {
        std::fs::create_dir(d).unwrap();
    }
    for d in [&b, &c] {
        std::fs::write(d.join("git"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(d.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(a.join("git"), "not executable").unwrap();
    let path = std::env::join_paths([&a, &b, &c]).unwrap();
    let never = || panic!("xcode-select is asked only about /usr/bin/git on macOS");

    assert_eq!(
        detect::local_git(Some(OsStr::new("/opt/x/git")), Some(&path), true, never),
        Ok("/opt/x/git".into())
    );
    assert_eq!(
        detect::local_git(Some(OsStr::new("")), Some(&path), true, never),
        Ok(b.join("git"))
    );
    assert_eq!(
        detect::local_git(None, Some(&path), true, never),
        Ok(b.join("git"))
    );
    assert_eq!(
        detect::local_git(None, Some(a.as_os_str()), true, never),
        Err(Unavailable::NoGit)
    );
    assert_eq!(
        detect::local_git(None, None, true, never),
        Err(Unavailable::NoGit)
    );

    if !Path::new("/usr/bin/git").is_file() {
        eprintln!("skipping the shim rows: no /usr/bin/git here");
        return;
    }
    let usr = OsStr::new("/usr/bin");
    assert_eq!(
        detect::local_git(None, Some(usr), true, || false),
        Err(Unavailable::NoGit)
    );
    assert_eq!(
        detect::local_git(None, Some(usr), true, || true),
        Ok("/usr/bin/git".into())
    );
    assert_eq!(
        detect::local_git(None, Some(usr), false, never),
        Ok("/usr/bin/git".into())
    );
}

/// `git version`'s forms: plain, Apple's, Windows', and what is not one.
#[test]
fn the_version_line() {
    let v = parse::git_version(b"git version 2.56.1\n").unwrap();
    assert_eq!(
        (v.major, v.minor, v.patch, v.text.as_str()),
        (2, 56, 1, "2.56.1")
    );
    assert!(v.at_least(2, 20) && v.at_least(2, 56) && !v.at_least(2, 57));
    let apple = parse::git_version(b"git version 2.39.5 (Apple Git-154)\n").unwrap();
    assert_eq!((apple.major, apple.minor, apple.patch), (2, 39, 5));
    let win = parse::git_version(b"git version 2.45.1.windows.1\n").unwrap();
    assert_eq!((win.major, win.minor, win.patch), (2, 45, 1));
    let two = parse::git_version(b"git version 2.20\n").unwrap();
    assert_eq!((two.major, two.minor, two.patch), (2, 20, 0));
    for bad in [
        &b""[..],
        b"git version\n",
        b"hello 2.3.4\n",
        b"git version x.y\n",
        b"\xff\xfe",
    ] {
        assert_eq!(parse::git_version(bad), None, "{bad:?}");
    }
}
