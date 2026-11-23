//! The log stream's tests (03 §4.5; G-P1, G-P2, G-P4). S2-B writes them.
//!
//! Also the tests' git ([`TestGit`]: `std::process::Command` running this machine's git, isolated
//! from its config, with 03 §3.3's environment and options) and the fixture builders, which the
//! detection and parser tests use through `super::history`.

use crate::git::history::{
    self, Generation, Git, LogOptions, LogStream, PageEnd, Pipe, Ran, RunError, StartError, PAGE,
};
use crate::git::model::Oid;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A `git` (or a stand-in) with git's own config, the environment and the inherited `GIT_*`
/// that would point it elsewhere taken away.
fn isolated(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_NAMESPACE",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_TEST_ASSUME_DIFFERENT_OWNER",
    ] {
        cmd.env_remove(var);
    }
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_EDITOR", "false")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .stdin(Stdio::null());
    cmd
}

/// git for the tests: each read as the runner will run it (03 §3.3's options in front, `-C dir`,
/// `GIT_OPTIONAL_LOCKS=0`), counted, with the pids of the streams it started.
pub(super) struct TestGit {
    program: PathBuf,
    env: Vec<(String, String)>,
    /// Commands run or started.
    pub(super) runs: AtomicUsize,
    /// The log streams' processes.
    pub(super) pids: Mutex<Vec<u32>>,
    /// Called after each read from a stream's pipe (a load superseded mid-stream).
    on_read: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl TestGit {
    /// This machine's git; `None` (the test is skipped with a note) when there is none — a
    /// failure under `SOPEN_REQUIRE_GIT=1` (03 §11).
    pub(super) fn local() -> Option<TestGit> {
        let found = isolated(Path::new("git"))
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if found {
            return Some(TestGit::with_program("git"));
        }
        if std::env::var_os("SOPEN_REQUIRE_GIT").is_some_and(|v| v == "1") {
            panic!("SOPEN_REQUIRE_GIT=1 but git is not on PATH");
        }
        eprintln!("skipping: git is not on PATH");
        None
    }

    /// `program` in git's place: a stand-in, or a path that does not exist.
    pub(super) fn with_program(program: impl Into<PathBuf>) -> TestGit {
        TestGit {
            program: program.into(),
            env: Vec::new(),
            runs: AtomicUsize::new(0),
            pids: Mutex::new(Vec::new()),
            on_read: None,
        }
    }

    pub(super) fn env(mut self, key: &str, value: &str) -> TestGit {
        self.env.push((key.into(), value.into()));
        self
    }

    pub(super) fn on_read(mut self, hook: impl Fn() + Send + Sync + 'static) -> TestGit {
        self.on_read = Some(Arc::new(hook));
        self
    }

    pub(super) fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }

    fn command(&self, dir: &str, args: &[String]) -> Command {
        let mut cmd = isolated(&self.program);
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.arg("-C").arg(dir);
        for c in [
            "core.quotePath=false",
            "color.ui=false",
            "log.showSignature=false",
            "i18n.commitEncoding=UTF-8",
        ] {
            cmd.args(["-c", c]);
        }
        cmd.arg("--no-pager").args(args);
        cmd
    }

    fn start(&self, dir: &str, args: &[String], stdin: &[u8]) -> Result<Child, RunError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let mut child = self
            .command(dir, args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => RunError::NoGit,
                _ => RunError::Failed(e.to_string()),
            })?;
        let mut input = child.stdin.take().unwrap();
        // git reads its stdin whole before it prints (`--stdin`), and the rest read none.
        let _ = input.write_all(stdin);
        drop(input);
        Ok(child)
    }
}

/// Reads a child's stream to its end on a thread (at most 64 MiB kept).
fn drain(mut from: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut all = Vec::new();
        let _ = (&mut from).take(64 << 20).read_to_end(&mut all);
        all
    })
}

impl Git for TestGit {
    fn run(
        &self,
        dir: &str,
        args: &[String],
        stdin: &[u8],
        bound: history::Bound,
    ) -> Result<Ran, RunError> {
        let mut child = self.start(dir, args, stdin)?;
        let out = drain(child.stdout.take().unwrap());
        let err = drain(child.stderr.take().unwrap());
        let limit = match bound {
            history::Bound::Detection => Duration::from_secs(10),
            history::Bound::Read => Duration::from_secs(30),
        };
        let t0 = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if t0.elapsed() < limit => std::thread::sleep(Duration::from_millis(2)),
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunError::TimedOut);
                }
            }
        };
        Ok(Ran {
            code: status.code(),
            stdout: out.join().unwrap(),
            stderr: err.join().unwrap(),
        })
    }

    fn spawn(&self, dir: &str, args: &[String], stdin: &[u8]) -> Result<Box<dyn Pipe>, RunError> {
        let mut child = self.start(dir, args, stdin)?;
        self.pids.lock().unwrap().push(child.id());
        let stdout = child.stdout.take().unwrap();
        let stderr = drain(child.stderr.take().unwrap());
        Ok(Box::new(TestPipe {
            child,
            stdout,
            stderr: Some(stderr),
            on_read: self.on_read.clone(),
            reaped: None,
        }))
    }
}

struct TestPipe {
    child: Child,
    stdout: ChildStdout,
    stderr: Option<JoinHandle<Vec<u8>>>,
    on_read: Option<Arc<dyn Fn() + Send + Sync>>,
    reaped: Option<Option<i32>>,
}

impl TestPipe {
    fn reap(&mut self) -> Ran {
        let code = match self.reaped {
            Some(code) => code,
            None => {
                let code = self.child.wait().ok().and_then(|s| s.code());
                self.reaped = Some(code);
                code
            }
        };
        let stderr = self
            .stderr
            .take()
            .map(|h| h.join().unwrap())
            .unwrap_or_default();
        Ran {
            code,
            stdout: Vec::new(),
            stderr,
        }
    }
}

impl Pipe for TestPipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.stdout.read(buf)?;
        if let Some(hook) = &self.on_read {
            hook();
        }
        Ok(n)
    }

    fn running(&mut self) -> bool {
        self.reaped.is_none() && matches!(self.child.try_wait(), Ok(None))
    }

    fn stop(&mut self) {
        if self.reaped.is_none() {
            let _ = self.child.kill();
        }
        self.reap();
    }

    fn finish(&mut self) -> Ran {
        self.reap()
    }
}

/// The process `pid` is gone (reaped): nothing is left of a stopped stream.
pub(super) fn gone(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// Fixture-building git in `dir`: isolated, a fixed identity, author and committer date `t`;
/// stdout without its last newline. Panics when git fails.
pub(super) fn sh_at(dir: &Path, t: i64, args: &[&str]) -> String {
    sh_with(dir, t, &[], args, b"")
}

/// [`sh_at`] with `env` added and `stdin` fed.
pub(super) fn sh_with(
    dir: &Path,
    t: i64,
    env: &[(&str, &str)],
    args: &[&str],
    stdin: &[u8],
) -> String {
    let date = format!("{t} +0000");
    let mut cmd = isolated(Path::new("git"));
    cmd.current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Ann Author")
        .env("GIT_AUTHOR_EMAIL", "ann@example.invalid")
        .env("GIT_COMMITTER_NAME", "Ann Author")
        .env("GIT_COMMITTER_EMAIL", "ann@example.invalid")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .envs(env.iter().copied())
        .args([
            "-c",
            "protocol.file.allow=always",
            "-c",
            "advice.detachedHead=false",
        ])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let mut text = String::from_utf8(out.stdout).unwrap();
    if text.ends_with('\n') {
        text.pop();
    }
    text
}

/// [`sh_at`] at a fixed date.
pub(super) fn sh(dir: &Path, args: &[&str]) -> String {
    sh_at(dir, 1_700_000_000, args)
}

/// The path as git prints it (absolute, symbolic links resolved: a temp dir on macOS).
pub(super) fn real(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

pub(super) fn oid(text: &str) -> Oid {
    Oid::parse(text).unwrap_or_else(|| panic!("not an id: {text:?}"))
}

/// A fast-import stream of 03 §4.5's generator (G-P1): 50 commits on `main`, then `blocks - 1`
/// blocks of 47 on `main`, two on `side` from there and their merge into `main` — `blocks` = 2000
/// is 100 000 commits with 1 999 merges. Each commit is a minute after the one before.
fn fast_import_stream(blocks: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(blocks * 50 * 220);
    let mut n = 0usize;
    let mut commit = |out: &mut Vec<u8>,
                      branch: &str,
                      file: &str,
                      parent: Option<usize>,
                      merge: Option<usize>| {
        n += 1;
        let t = 1_700_000_000 + n as i64 * 60;
        let msg = format!("commit {n}\n");
        let body = format!("{n}\n");
        let _ = write!(
            out,
            "commit refs/heads/{branch}\nmark :{n}\n\
             author Fixture <fixture@example.invalid> {t} +0000\n\
             committer Fixture <fixture@example.invalid> {t} +0000\n\
             data {}\n{msg}",
            msg.len()
        );
        if let Some(p) = parent {
            let _ = writeln!(out, "from :{p}");
        }
        if let Some(m) = merge {
            let _ = writeln!(out, "merge :{m}");
        }
        let _ = write!(out, "M 644 inline {file}\ndata {}\n{body}\n", body.len());
        n
    };
    let mut main = None;
    for _ in 0..50 {
        main = Some(commit(&mut out, "main", "f.txt", main, None));
    }
    for _ in 1..blocks {
        for _ in 0..47 {
            main = Some(commit(&mut out, "main", "f.txt", main, None));
        }
        let s1 = commit(&mut out, "side", "side.txt", main, None);
        let s2 = commit(&mut out, "side", "side.txt", Some(s1), None);
        main = Some(commit(&mut out, "main", "f.txt", main, Some(s2)));
    }
    out.extend_from_slice(b"done\n");
    out
}

/// A repository at `dir` from [`fast_import_stream`]: `main` checked out (HEAD), `side`.
pub(super) fn generated(dir: &Path, blocks: usize) {
    std::fs::create_dir_all(dir).unwrap();
    sh(dir, &["init", "-q", "-b", "main"]);
    let mut child = isolated(Path::new("git"))
        .current_dir(dir)
        .args(["fast-import", "--quiet", "--done"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&fast_import_stream(blocks))
        .unwrap();
    assert!(child.wait().unwrap().success(), "fast-import failed");
}

/// The 100 000-commit fixture, built once per test run in a temp folder that is removed when
/// the test binary exits.
struct Big {
    dir: PathBuf,
}

static BIG: OnceLock<Big> = OnceLock::new();

extern "C" fn remove_big() {
    if let Some(big) = BIG.get() {
        let _ = std::fs::remove_dir_all(&big.dir);
    }
}

fn big() -> &'static Path {
    &BIG.get_or_init(|| {
        let dir = tempfile::Builder::new()
            .prefix("sopen-git-100k-")
            .tempdir()
            .unwrap()
            .keep();
        // SAFETY: registers a plain function to run at exit; it only removes the folder.
        unsafe { libc::atexit(remove_big) };
        let t0 = Instant::now();
        generated(&dir, 2000);
        eprintln!("100 000-commit fixture built in {:?}", t0.elapsed());
        Big { dir }
    })
    .dir
}

/// `rev-list` with `args` in `dir`, one id per line.
pub(super) fn rev_list(dir: &Path, args: &[&str]) -> Vec<String> {
    let mut all = vec!["rev-list"];
    all.extend_from_slice(args);
    sh(dir, &all).lines().map(str::to_string).collect()
}

/// The default roots of the repository at `top` (refs, HEAD, stashes), read as a load reads them.
fn load_roots(git: &TestGit, top: &str) -> Vec<Oid> {
    let refs = history::refs(git, top).unwrap();
    let head = history::head(git, top).unwrap();
    let stashes = history::stashes(git, top).unwrap();
    history::roots(&refs, Some(&head), &stashes)
}

/// The rows the stream gives until it ends, at most `cap` pages.
fn read_to_end(stream: &mut LogStream, cap: usize) -> Vec<crate::git::model::CommitRow> {
    let mut rows = Vec::new();
    for _ in 0..cap {
        let page = stream.page(PAGE);
        rows.extend(page.rows);
        match page.end {
            PageEnd::More => continue,
            PageEnd::AllLoaded => return rows,
            other => panic!("the stream ended with {other:?} after {} rows", rows.len()),
        }
    }
    panic!("no end after {cap} pages");
}

/// Every other read's words as 03 writes them (§2.1, §2.4, §4.1, §6.1, §6.2), one word per
/// element; the `--` after a revision is the one addition (`argv`'s header says why).
#[test]
fn every_read_runs_03s_words() {
    use crate::git::history::argv;
    let c = Oid::parse(&"c".repeat(40)).unwrap();
    let p = Oid::parse(&"b".repeat(40)).unwrap();
    let cases: [(Vec<String>, &[&str]); 11] = [
        (argv::version(), &["version"]),
        (
            argv::detection(),
            &[
                "rev-parse",
                "--is-bare-repository",
                "--is-inside-git-dir",
                "--is-shallow-repository",
                "--absolute-git-dir",
                "--git-common-dir",
                "--show-toplevel",
                "--show-superproject-working-tree",
            ],
        ),
        (
            argv::refs(),
            &[
                "for-each-ref",
                "--format=%(objectname)%00%(objecttype)%00%(*objectname)%00%(refname)%00%(upstream)%00%(upstream:track)",
                "refs/heads",
                "refs/remotes",
                "refs/tags",
            ],
        ),
        (argv::symbolic_head(), &["symbolic-ref", "-q", "HEAD"]),
        (argv::verify_head(), &["rev-parse", "-q", "--verify", "HEAD"]),
        (
            argv::stashes(),
            &["stash", "list", "-z", "--format=%H%x1f%P%x1f%gd%x1f%ct%x1f%s"],
        ),
        (
            argv::status(),
            &["status", "--porcelain=v2", "-z", "--branch", "--untracked-files=normal"],
        ),
        (
            argv::details(&c),
            &[
                "log",
                "-1",
                "-z",
                "--format=%H%x1f%P%x1f%aN%x1f%aE%x1f%at%x1f%cN%x1f%cE%x1f%ct%x1f%B",
                c.as_str(),
                "--",
            ],
        ),
        (
            argv::name_status(&p, &c),
            &["diff", "-z", "--find-renames", "--name-status", p.as_str(), c.as_str(), "--"],
        ),
        (
            argv::numstat(&p, &c),
            &["diff", "-z", "--find-renames", "--numstat", p.as_str(), c.as_str(), "--"],
        ),
        (argv::empty_tree(), &["hash-object", "-t", "tree", "--stdin"]),
    ];
    for (got, want) in cases {
        assert_eq!(got, want);
    }
}

/// 03 §4.1's log form, word for word; the roots go on stdin.
#[test]
fn the_log_runs_03s_form() {
    let words = |o: LogOptions| history::argv::log(&o).join(" ");
    let format = "--format=%H%x1f%P%x1f%aN%x1f%aE%x1f%at%x1f%ct%x1f%s";
    assert_eq!(
        words(LogOptions::default()),
        format!("log -z --date-order {format} --stdin --")
    );
    let all = LogOptions {
        order: history::Order::Topo,
        first_parent: true,
        reflog: true,
    };
    assert_eq!(
        words(all),
        format!("log -z --topo-order {format} --first-parent --reflog --stdin --")
    );
    let author = LogOptions {
        order: history::Order::AuthorDate,
        ..LogOptions::default()
    };
    assert_eq!(
        words(author),
        format!("log -z --author-date-order {format} --stdin --")
    );
}

/// G-P1 (03 §4.5, 03:1134): on the 100 000-commit fixture the first page is exactly 500 rows,
/// within 5 s of the load's start; git is still there, blocked on the full pipe, and sopen has
/// read little more than the page; the rows are git's own first 500 of the walk, with their
/// parents; dropping the stream ends git.
#[test]
fn g_p1_the_first_page_is_500_rows_and_git_waits() {
    let Some(git) = TestGit::local() else { return };
    let dir = big();
    let top = dir.to_str().unwrap();
    let generation = Generation::new();
    let load = generation.bump();

    let t0 = Instant::now();
    let roots = load_roots(&git, top);
    let mut stream =
        LogStream::start(&git, top, &LogOptions::default(), &roots, &generation, load).unwrap();
    let page = stream.page(PAGE);
    let took = t0.elapsed();

    assert_eq!(page.rows.len(), 500);
    assert_eq!(page.end, PageEnd::More);
    assert_eq!(page.generation, load);
    eprintln!("G-P1: first page of 500 rows in {took:?}");
    assert!(took < Duration::from_secs(5), "first page took {took:?}");
    std::thread::sleep(Duration::from_millis(200));
    assert!(stream.running(), "git should be parked on the full pipe");
    assert!(!stream.ended());
    assert_eq!(stream.rows_read(), 500);
    assert!(
        stream.bytes_read() < 1 << 20,
        "read {} bytes for one page",
        stream.bytes_read()
    );

    let ids: Vec<&str> = roots.iter().map(Oid::as_str).collect();
    let mut args = vec!["--date-order", "--parents", "--max-count=500"];
    args.extend(&ids);
    let walk = rev_list(dir, &args);
    assert_eq!(walk.len(), 500);
    for (row, line) in page.rows.iter().zip(&walk) {
        let mut words = line.split(' ');
        assert_eq!(row.oid.as_str(), words.next().unwrap());
        let parents: Vec<&str> = row.parents.iter().map(Oid::as_str).collect();
        assert_eq!(parents, words.collect::<Vec<_>>());
    }
    let top_row = &page.rows[0];
    assert_eq!(top_row.subject, "commit 100000");
    assert_eq!(top_row.author_name, "Fixture");
    assert_eq!(top_row.author_email, "fixture@example.invalid");
    assert_eq!(top_row.author_time, 1_700_000_000 + 100_000 * 60);
    assert_eq!(top_row.commit_time, top_row.author_time);
    assert_eq!(top_row.parents.len(), 2, "the newest commit is a merge");

    let pid = *git.pids.lock().unwrap().last().unwrap();
    drop(stream);
    assert!(
        gone(pid),
        "git {pid} is still there after the stream was dropped"
    );
}

/// G-P2 (03 §4.5, 03:1145): three more pages make 2 000 rows, no hash twice, the first 500
/// unchanged, in git's own order of one walk; reading on to the end gives all 100 000 distinct
/// hashes and `all loaded`, and git has exited.
#[test]
fn g_p2_pages_are_one_walk_to_the_end() {
    let Some(git) = TestGit::local() else { return };
    let dir = big();
    let top = dir.to_str().unwrap();
    let generation = Generation::new();
    let load = generation.bump();
    let roots = load_roots(&git, top);
    let mut stream =
        LogStream::start(&git, top, &LogOptions::default(), &roots, &generation, load).unwrap();

    let first = stream.page(PAGE);
    let mut rows = first.rows.clone();
    for _ in 0..3 {
        let page = stream.page(PAGE);
        assert_eq!(page.end, PageEnd::More);
        assert_eq!(page.rows.len(), 500);
        rows.extend(page.rows);
    }
    assert_eq!(rows.len(), 2000);
    assert_eq!(rows[..500], first.rows[..]);
    let distinct: HashSet<&str> = rows.iter().map(|r| r.oid.as_str()).collect();
    assert_eq!(distinct.len(), 2000);
    let ids: Vec<&str> = roots.iter().map(Oid::as_str).collect();
    let mut args = vec!["--date-order", "--max-count=2000"];
    args.extend(&ids);
    let walk = rev_list(dir, &args);
    let got: Vec<&str> = rows.iter().map(|r| r.oid.as_str()).collect();
    assert_eq!(got, walk);

    rows.extend(read_to_end(&mut stream, 250));
    assert_eq!(rows.len(), 100_000);
    let distinct: HashSet<&str> = rows.iter().map(|r| r.oid.as_str()).collect();
    assert_eq!(distinct.len(), 100_000);
    assert!(stream.ended());
    assert!(!stream.running());
    let after = stream.page(PAGE);
    assert!(after.rows.is_empty());
    assert_eq!(after.end, PageEnd::AllLoaded);
}

/// G-P4 (03 §4.1, 03:1142): a ref moved by another process between the refs read and the log's
/// start, and again mid-stream → the rows are exactly the history of the ids the labels show,
/// none of the moved ref's new commits, until the next load, which has them; a load whose
/// generation is bumped mid-stream drops its page unread and stops git; a stale load starts
/// nothing; no roots run no log.
#[test]
fn g_p4_a_moved_ref_waits_for_the_next_load_and_a_superseded_load_is_dropped() {
    let Some(git) = TestGit::local() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("r");
    generated(&dir, 25);
    let top = dir.to_str().unwrap();
    let generation = Generation::new();

    let first = generation.bump();
    let roots = load_roots(&git, top);
    let shown = rev_list(&dir, &roots.iter().map(Oid::as_str).collect::<Vec<_>>());
    assert_eq!(shown.len(), 1250);
    let move_main = |t: i64, msg: &str| {
        let new = sh_at(
            &dir,
            t,
            &["commit-tree", "main^{tree}", "-p", "main", "-m", msg],
        );
        sh(&dir, &["update-ref", "refs/heads/main", &new]);
        new
    };
    let moved = move_main(1_800_000_000, "moved before the log");
    let mut stream = LogStream::start(
        &git,
        top,
        &LogOptions::default(),
        &roots,
        &generation,
        first,
    )
    .unwrap();
    let page = stream.page(PAGE);
    assert_eq!(page.rows.len(), 500);
    let moved_again = move_main(1_800_000_060, "moved mid-stream");
    let mut rows = page.rows;
    rows.extend(read_to_end(&mut stream, 10));
    let got: Vec<&str> = rows.iter().map(|r| r.oid.as_str()).collect();
    assert!(!got.contains(&moved.as_str()) && !got.contains(&moved_again.as_str()));
    let got_set: HashSet<&str> = got.iter().copied().collect();
    let shown_set: HashSet<&str> = shown.iter().map(String::as_str).collect();
    assert_eq!(got.len(), 1250);
    assert_eq!(got_set, shown_set);

    // The next load reads the refs again: the moved ref's commits come first.
    let next = generation.bump();
    assert_eq!(
        generation.accept(first, ()),
        None,
        "a result of the old load is dropped"
    );
    let roots = load_roots(&git, top);
    let mut stream =
        LogStream::start(&git, top, &LogOptions::default(), &roots, &generation, next).unwrap();
    let page = stream.page(PAGE);
    assert_eq!(page.rows[0].oid.as_str(), moved_again);
    assert_eq!(page.rows[1].oid.as_str(), moved);
    drop(stream);

    // A load superseded while its first page is being read: the hook bumps the generation
    // after the stream's first read from the pipe.
    let bumper = generation.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = reads.clone();
    let hooked = TestGit::local().unwrap().on_read(move || {
        if counted.fetch_add(1, Ordering::SeqCst) == 0 {
            bumper.bump();
        }
    });
    let load = generation.bump();
    let mut stream = LogStream::start(
        &hooked,
        top,
        &LogOptions::default(),
        &roots,
        &generation,
        load,
    )
    .unwrap();
    let page = stream.page(PAGE);
    assert_eq!(page.end, PageEnd::Superseded);
    assert!(page.rows.is_empty(), "a superseded page is dropped unread");
    assert_eq!(page.generation, load);
    assert!(stream.ended());
    assert!(!stream.running());
    let pid = *hooked.pids.lock().unwrap().last().unwrap();
    assert!(
        gone(pid),
        "the superseded stream's git {pid} is still there"
    );
    assert_eq!(generation.accept(page.generation, page.rows), None);

    // A load that is already stale, or has no root, starts no git.
    let before = hooked.runs();
    let stale = LogStream::start(
        &hooked,
        top,
        &LogOptions::default(),
        &roots,
        &generation,
        load,
    );
    assert_eq!(stale.err(), Some(StartError::Superseded));
    let current = generation.current();
    let none = LogStream::start(
        &hooked,
        top,
        &LogOptions::default(),
        &[],
        &generation,
        current,
    );
    assert_eq!(none.err(), Some(StartError::NoRoots));
    assert_eq!(hooked.runs(), before);
}

/// The roots (03 §4.1): HEAD's commit, every ref (an annotated tag by what it points at, a
/// remote's `HEAD` left out) and each stash's base, each once; `None` for HEAD with a filter.
#[test]
fn roots_are_the_ids_the_labels_show() {
    use crate::git::model::{Head, ObjectType, RefKind, RefLabel, StashRow};
    let id = |c: char| Oid::parse(&c.to_string().repeat(40)).unwrap();
    let label = |refname: &str, kind, object: Oid, object_type, peeled: Option<Oid>| RefLabel {
        object,
        object_type,
        peeled,
        refname: refname.into(),
        kind,
        upstream: None,
        track: String::new(),
        lossy: false,
    };
    let refs = vec![
        label(
            "refs/heads/main",
            RefKind::LocalBranch,
            id('a'),
            ObjectType::Commit,
            None,
        ),
        label(
            "refs/remotes/origin/HEAD",
            RefKind::RemoteBranch,
            id('9'),
            ObjectType::Commit,
            None,
        ),
        label(
            "refs/remotes/origin/main",
            RefKind::RemoteBranch,
            id('b'),
            ObjectType::Commit,
            None,
        ),
        label(
            "refs/tags/v1",
            RefKind::Tag,
            id('c'),
            ObjectType::Tag,
            Some(id('d')),
        ),
        label(
            "refs/tags/light",
            RefKind::Tag,
            id('a'),
            ObjectType::Commit,
            None,
        ),
    ];
    let stash = StashRow {
        oid: id('e'),
        base: id('f'),
        index: Some(id('1')),
        untracked: None,
        selector: "stash@{0}".into(),
        commit_time: 0,
        subject: "WIP".into(),
    };
    let head = Head::Detached(id('2'));
    let roots = history::roots(&refs, Some(&head), &[stash]);
    assert_eq!(roots, vec![id('2'), id('a'), id('b'), id('d'), id('f')]);
    let filtered = history::roots(&refs[..1], None, &[]);
    assert_eq!(filtered, vec![id('a')]);
    let unborn = Head::Unborn {
        refname: "refs/heads/main".into(),
        lossy: false,
    };
    assert!(history::roots(&[], Some(&unborn), &[]).is_empty());
}
