//! The runner's tests (03 §3; G-R1–G-R4, G-R6), through `LocalGit` and `ShellGit`, and
//! `SshGit`'s argv word by word.
//!
//! Every git here runs isolated from the account's own: a fixture `GIT_CONFIG_GLOBAL`, no system
//! config, a temporary `HOME`, and none of the variables that point git at a repository — so the
//! owner's signing, hooks or credential helper never run (a helper could raise a window). git
//! missing is a skip with a note, a failure under `SOPEN_REQUIRE_GIT=1` (CI). `ShellGit` runs
//! under `sh` everywhere, and under dash, bash, zsh and fish where they are installed for the
//! name tests (missing ones fail under `SOPEN_REQUIRE_SHELLS=1`, as `forward.rs`'s lanes do).

use crate::git::runner::{
    kill_words, mask_credentials, quote, Class, Done, Ending, Faults, GitCommand, GitRunner,
    Handle, Limits, LocalGit, Running, ShellGit, Snippet, SshGit, StopBy, Stopped, LOCAL_REPO_ENV,
    LOCATE, MAX_ARGS,
};
use crate::remote::exec;
use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// Fixtures

/// git for the tests, or `None` with a note (a failure under `SOPEN_REQUIRE_GIT=1`).
fn git_or_skip() -> Option<PathBuf> {
    match LocalGit::find() {
        Some(git) if Command::new(git.git()).arg("--version").output().is_ok() => {
            Some(git.git().to_path_buf())
        }
        _ if std::env::var_os("SOPEN_REQUIRE_GIT").is_some_and(|v| v == "1") => {
            panic!("SOPEN_REQUIRE_GIT=1 but git is not on PATH")
        }
        _ => {
            eprintln!("note: git is not on PATH; the runner's tests are skipped");
            None
        }
    }
}

/// A folder of repositories, with its own git config and home.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    config: PathBuf,
    git: PathBuf,
}

const CONFIG: &str = "[user]\n\tname = sopen test\n\temail = test@example.invalid\n\
                      [init]\n\tdefaultBranch = main\n[commit]\n\tgpgSign = false\n\
                      [tag]\n\tgpgSign = false\n";

fn fixture() -> Option<Fixture> {
    let git = git_or_skip()?;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let config = root.join("gitconfig");
    std::fs::write(&config, CONFIG).unwrap();
    Some(Fixture {
        _tmp: tmp,
        root,
        home,
        config,
        git,
    })
}

/// The isolation every git of a test gets: config, home, no repository variables.
fn isolate(c: &mut Command, fx: &Fixture) {
    for key in LOCAL_REPO_ENV {
        c.env_remove(key);
    }
    c.env("HOME", &fx.home)
        .env("GIT_CONFIG_GLOBAL", &fx.config)
        .env("GIT_CONFIG_NOSYSTEM", "1");
}

impl Fixture {
    /// Plain git in `dir`, for setting up and checking (not through the runner).
    fn git(&self, dir: &Path, args: &[&str]) -> Output {
        let mut c = Command::new(&self.git);
        c.arg("-C").arg(dir).args(args);
        isolate(&mut c, self);
        let out = c.output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    /// A new repository `name` with one commit of `README`.
    fn repo(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        self.git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("README"), "fixture\n").unwrap();
        self.git(&dir, &["add", "README"]);
        self.git(&dir, &["commit", "-q", "-m", "first"]);
        dir
    }

    fn local_git(&self) -> LocalGit {
        LocalGit::new(&self.git)
            .with_env("HOME", &self.home)
            .with_env("GIT_CONFIG_GLOBAL", &self.config)
            .with_env("GIT_CONFIG_NOSYSTEM", "1")
    }

    fn local(&self) -> GitRunner {
        GitRunner::local(self.local_git())
    }

    fn shell_git(&self, shell: &Path) -> ShellGit {
        ShellGit::new(shell, "box")
            .with_env("HOME", &self.home)
            .with_env("GIT_CONFIG_GLOBAL", &self.config)
            .with_env("GIT_CONFIG_NOSYSTEM", "1")
    }

    /// A `ShellGit` runner under `sh`, and its fault switch.
    fn shell(&self) -> (GitRunner, Arc<Faults>) {
        let git = self.shell_git(Path::new("/bin/sh"));
        let faults = git.faults();
        (GitRunner::shell(git), faults)
    }

    /// `local` and `sh`.
    fn lanes(&self) -> Vec<(String, GitRunner)> {
        vec![
            ("local".into(), self.local()),
            ("sh".into(), self.shell().0),
        ]
    }

    /// `local`, then `ShellGit` under every login shell there is.
    fn all_lanes(&self) -> Vec<(String, GitRunner)> {
        let mut lanes = vec![("local".to_string(), self.local())];
        for shell in login_shells() {
            let name = shell.file_name().unwrap().to_string_lossy().into_owned();
            lanes.push((name, GitRunner::shell(self.shell_git(&shell))));
        }
        lanes
    }
}

/// `name` on PATH: the first absolute, executable file, canonicalized.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .filter_map(|dir| dir.join(name).canonicalize().ok())
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0))
}

/// `/bin/sh`, dash where it is not `/bin/sh`, and bash, zsh and fish when installed.
fn login_shells() -> Vec<PathBuf> {
    let require = std::env::var_os("SOPEN_REQUIRE_SHELLS").is_some_and(|v| v == "1");
    let sh = PathBuf::from("/bin/sh");
    let mut shells = vec![sh.clone()];
    let dash = Path::new("/bin/dash");
    if dash.exists() && dash.canonicalize().ok() != sh.canonicalize().ok() {
        shells.push(dash.to_path_buf());
    }
    for name in ["bash", "zsh", "fish"] {
        match which(name) {
            Some(path) => shells.push(path),
            None if require => panic!("SOPEN_REQUIRE_SHELLS=1 but {name} is not on PATH"),
            None => eprintln!("note: {name} is not on PATH; its ShellGit lane is skipped"),
        }
    }
    shells
}

fn read(dir: impl Into<OsString>, args: &[&str]) -> GitCommand {
    GitCommand::new(Class::Read, dir, args.iter().copied())
}

fn write(dir: impl Into<OsString>, args: &[&str]) -> GitCommand {
    GitCommand::new(Class::Write, dir, args.iter().copied())
}

#[track_caller]
fn ok(lane: &str, done: &Done) {
    assert!(
        done.ok(),
        "{lane}: {} — {}\n{}",
        done.entry.line,
        done.ending,
        done.stderr
    );
}

fn wait_for(what: &str, limit: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !cond() {
        assert!(Instant::now() < deadline, "{what}: not within {limit:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A process group the test started (through the runner): alive while any process is in it;
/// killed when the guard goes, so a failed assertion leaves nothing running.
struct Group(u32);

impl Group {
    fn alive(&self) -> bool {
        assert!(self.0 >= 2);
        // SAFETY: signal 0 only checks; the group is one this test made.
        unsafe { libc::kill(-(self.0 as i32), 0) == 0 }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if self.0 >= 2 && self.alive() {
            // SAFETY: the group this test made, and only it.
            unsafe { libc::kill(-(self.0 as i32), libc::SIGKILL) };
        }
    }
}

/// The `*.lock` files under `dir`.
fn lock_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "lock") {
                found.push(path);
            }
        }
    }
    found
}

/// A shell word for a fixture path in a test's own scripts.
fn sh_word(path: &Path) -> String {
    String::from_utf8(quote(path.as_os_str().as_encoded_bytes())).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Words and arguments (G-R1's argv half)

const NAMES: [&str; 4] = ["it's", "a$b", "ü-한", "@"];
const FILES: [&str; 5] = ["a b'c", "x\\y", "a*", "[x].txt", ":(top)z"];
/// What a pattern would also match: `a*` → `ab`, `a b`; `[x].txt` → `x.txt`; `:(top)z` → `z`.
const DECOYS: [&str; 4] = ["ab", "a b", "x.txt", "z"];

/// The runner quotes as `forward::quote` does, over bytes: equal on every name, and a word
/// that is not UTF-8 keeps its bytes.
#[test]
fn quoting_is_forward_quote_over_bytes() {
    for word in
        NAMES
            .iter()
            .chain(&FILES)
            .chain(&["", "plain", "''", "\\\\", "a\nb", "$(x) `y` \"z\""])
    {
        assert_eq!(
            quote(word.as_bytes()),
            crate::forward::quote(word).into_bytes(),
            "{word:?}"
        );
    }
    assert_eq!(quote(b"\xff'\\"), b"'\xff'\\'''\\\\''".to_vec());
}

/// The master's arguments for a command: `-S` first, then `ControlMaster=no` and `BatchMode`,
/// then the keepalives and the connect bound — the master's own `ControlMaster=auto` and
/// `ControlPersist` gone (ssh keeps the first value given for an option).
fn connection() -> Vec<OsString> {
    [
        "-S",
        "/state/cm-abc",
        "-o",
        "ControlMaster=auto",
        "-o",
        "ControlPersist=60s",
        "-o",
        "ServerAliveInterval=10",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "ConnectTimeout=15",
    ]
    .map(OsString::from)
    .to_vec()
}

const EXEC_ARGS: [&str; 12] = [
    "-S",
    "/state/cm-abc",
    "-o",
    "ControlMaster=no",
    "-o",
    "BatchMode=yes",
    "-o",
    "ServerAliveInterval=10",
    "-o",
    "ServerAliveCountMax=3",
    "-o",
    "ConnectTimeout=15",
];

#[test]
fn exec_args_attach_to_the_master_and_never_prompt() {
    assert_eq!(
        exec::exec_args_from(connection()),
        EXEC_ARGS.map(OsString::from)
    );
    assert_eq!(
        exec::check_args(&exec::exec_args_from(connection()), "box"),
        ["-S", "/state/cm-abc", "-O", "check", "box"].map(OsString::from)
    );
}

/// `SshGit`'s argv, word by word (03 §3.2): `ssh <exec args> -T -o RemoteCommand=none
/// -o LogLevel=ERROR <dest> -- exec 'env' <03 §3.3> 'sh' '-c' '<locate>' 'sopen-git' -C …`.
#[test]
fn ssh_argv_word_by_word() {
    let ssh = SshGit::with_exec_args("z@box", "box", exec::exec_args_from(connection()));
    let env = [
        "'LC_ALL=C'",
        "'GIT_TERMINAL_PROMPT=0'",
        "'GIT_ASKPASS='",
        "'SSH_ASKPASS='",
        "'SSH_ASKPASS_REQUIRE=never'",
        "'GIT_PAGER=cat'",
        "'PAGER=cat'",
        "'GIT_EDITOR=false'",
        "'GIT_SEQUENCE_EDITOR=false'",
        "'GIT_MERGE_AUTOEDIT=no'",
    ];
    let options = [
        "'-c'",
        "'core.quotePath=false'",
        "'-c'",
        "'color.ui=false'",
        "'-c'",
        "'log.showSignature=false'",
        "'-c'",
        "'i18n.commitEncoding=UTF-8'",
        "'--no-pager'",
    ];
    // Every other global pathspec setting off (git refuses one next to the literal setting).
    let pathspecs_off = [
        "'GIT_GLOB_PATHSPECS=0'",
        "'GIT_NOGLOB_PATHSPECS=0'",
        "'GIT_ICASE_PATHSPECS=0'",
    ];
    let locate = format!("'{LOCATE}'");
    let expect = |tail_env: &[&str], dir: &str, args: &[&str]| -> Vec<OsString> {
        let mut argv: Vec<String> = EXEC_ARGS.iter().map(|s| s.to_string()).collect();
        argv.extend(
            [
                "-T",
                "-o",
                "RemoteCommand=none",
                "-o",
                "LogLevel=ERROR",
                "z@box",
                "--",
                "exec",
                "'env'",
            ]
            .map(String::from),
        );
        argv.extend(
            env.iter()
                .chain(tail_env)
                .chain(&pathspecs_off)
                .map(|s| s.to_string()),
        );
        argv.extend(["'sh'", "'-c'"].map(String::from));
        argv.push(locate.clone());
        argv.extend(["'sopen-git'", "'-C'", dir].map(String::from));
        argv.extend(options.iter().chain(args).map(|s| s.to_string()));
        argv.into_iter().map(OsString::from).collect()
    };

    // A write: literal pathspecs, no optional-locks override; every name quoted.
    let checkout = write("/srv/it's a $dir", &["checkout", "-q", "it's", "--"]);
    assert_eq!(
        ssh.argv(SshGit::words(&checkout)),
        expect(
            &["'GIT_LITERAL_PATHSPECS=1'"],
            r"'/srv/it'\''s a $dir'",
            &["'checkout'", "'-q'", r"'it'\''s'", "'--'"]
        )
    );
    let add = write("/r", &["add", "--", "x\\y", "a b'c"]);
    assert_eq!(
        ssh.argv(SshGit::words(&add)),
        expect(
            &["'GIT_LITERAL_PATHSPECS=1'"],
            "'/r'",
            &["'add'", "'--'", r"'x'\\'y'", r"'a b'\''c'"]
        )
    );
    // A read: GIT_OPTIONAL_LOCKS=0 as well.
    let refs = read(
        "/r",
        &["for-each-ref", "--format=%(refname)", "refs/heads/ü-한"],
    );
    assert_eq!(
        ssh.argv(SshGit::words(&refs)),
        expect(
            &["'GIT_OPTIONAL_LOCKS=0'", "'GIT_LITERAL_PATHSPECS=1'"],
            "'/r'",
            &[
                "'for-each-ref'",
                "'--format=%(refname)'",
                "'refs/heads/ü-한'"
            ]
        )
    );
    // History search: GIT_LITERAL_PATHSPECS=0 (set, so an exported 1 cannot stand); a changed
    // editor replaces 03 §3.3's value.
    let search = read("/r", &["log", "--", ":(glob)a*"]).patterns();
    assert_eq!(
        ssh.argv(SshGit::words(&search)),
        expect(
            &["'GIT_OPTIONAL_LOCKS=0'", "'GIT_LITERAL_PATHSPECS=0'"],
            "'/r'",
            &["'log'", "'--'", "':(glob)a*'"]
        )
    );
    let words = SshGit::words(&write("/r", &["rebase", "--continue"]).env("GIT_EDITOR", "true"));
    assert!(words.contains(&OsString::from("'GIT_EDITOR=true'")));
    assert!(!words.contains(&OsString::from("'GIT_EDITOR=false'")));
    // The stop's words (03 §3.5).
    assert_eq!(
        kill_words(4242, "-TERM"),
        ["exec", "'kill'", "'-TERM'", "'--'", "'-4242'"].map(OsString::from)
    );
}

/// 03 §3.3 and §3.6: every fetch gets `--no-prune --no-prune-tags` (and `--progress`), every
/// push `--progress`, right after the subcommand; the log shows the line as it runs, and only
/// the environment that differs from 03 §3.3.
#[test]
fn the_runner_adds_prune_and_progress_and_the_log_shows_it() {
    let fetch = GitCommand::new(Class::Network, "/r", ["fetch", "origin"]);
    assert_eq!(
        fetch.command_line(),
        "git fetch --progress --no-prune --no-prune-tags origin"
    );
    let pruning = GitCommand::new(Class::Network, "/r", ["fetch", "--prune", "origin"]);
    assert_eq!(
        pruning.command_line(),
        "git fetch --progress --no-prune --no-prune-tags --prune origin"
    );
    let push = GitCommand::new(Class::Network, "/r", ["push", "origin", "main"]);
    assert_eq!(push.command_line(), "git push --progress origin main");
    let cont = write("/r", &["rebase", "--continue"]).env("GIT_EDITOR", "true");
    assert_eq!(cont.command_line(), "GIT_EDITOR=true git rebase --continue");
    let quoted = write("/r", &["checkout", "it's"]);
    assert_eq!(quoted.command_line(), r"git checkout 'it'\''s'");
}

// ---------------------------------------------------------------------------------------------
// G-R1: names byte for byte

/// G-R1 (03:1098), the runner's half: branches and tags named `it's`, `a$b`, `ü-한` and `@`,
/// files named `a b'c`, `x\y`, `a*`, `[x].txt` and `:(top)z`, in a folder whose own name needs
/// quoting; through `LocalGit` and `ShellGit` under every login shell: created, listed, checked
/// out, staged and diffed → every name comes back byte for byte and each file stages only
/// itself. (git reads `checkout @` as HEAD, so `@` is checked by its full name instead.)
#[test]
fn names_come_back_byte_for_byte() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.all_lanes() {
        let repo = fx.repo(&format!("{lane} it's a $dir"));
        for f in FILES.iter().chain(&DECOYS) {
            std::fs::write(repo.join(f), "one\n").unwrap();
        }
        fx.git(&repo, &["add", "-A"]);
        fx.git(&repo, &["commit", "-q", "-m", "files"]);
        for f in FILES.iter().chain(&DECOYS) {
            std::fs::write(repo.join(f), "one\ntwo\n").unwrap();
        }
        let h = Handle::new();
        let dir = repo.as_os_str();
        for name in NAMES {
            ok(&lane, &runner.run(&write(dir, &["branch", "--", name]), &h));
            ok(&lane, &runner.run(&write(dir, &["tag", "--", name]), &h));
        }

        let refs = runner.run(
            &read(
                dir,
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/heads",
                    "refs/tags",
                ],
            ),
            &h,
        );
        ok(&lane, &refs);
        let mut got: Vec<&[u8]> = refs.stdout.split(|&b| b == b'\n').collect();
        got.retain(|l| !l.is_empty());
        got.sort();
        let mut want: Vec<Vec<u8>> = NAMES
            .iter()
            .flat_map(|n| [format!("refs/heads/{n}"), format!("refs/tags/{n}")])
            .chain(["refs/heads/main".to_string()])
            .map(String::into_bytes)
            .collect();
        want.sort();
        assert_eq!(
            got,
            want.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            "{lane}"
        );

        for name in ["it's", "a$b", "ü-한"] {
            ok(
                &lane,
                &runner.run(&write(dir, &["checkout", "-q", name, "--"]), &h),
            );
            let head = runner.run(&read(dir, &["symbolic-ref", "HEAD"]), &h);
            assert_eq!(
                head.stdout,
                format!("refs/heads/{name}\n").into_bytes(),
                "{lane}"
            );
        }
        let at = runner.run(&read(dir, &["show-ref", "--verify", "refs/heads/@"]), &h);
        ok(&lane, &at);
        assert!(at.stdout.ends_with(b" refs/heads/@\n"), "{lane}");
        ok(
            &lane,
            &runner.run(&write(dir, &["checkout", "-q", "main", "--"]), &h),
        );

        for f in FILES {
            ok(&lane, &runner.run(&write(dir, &["add", "--", f]), &h));
            let staged = runner.run(&read(dir, &["diff", "--cached", "--name-only", "-z"]), &h);
            assert_eq!(staged.stdout, format!("{f}\0").into_bytes(), "{lane}: {f}");
            let diff = runner.run(
                &read(dir, &["diff", "--cached", "-z", "--numstat", "--", f]),
                &h,
            );
            assert_eq!(
                diff.stdout,
                format!("1\t0\t{f}\0").into_bytes(),
                "{lane}: {f}"
            );
            ok(
                &lane,
                &runner.run(&write(dir, &["reset", "-q", "--", f]), &h),
            );
            let unstaged = runner.run(&read(dir, &["diff", "-z", "--numstat", "--", f]), &h);
            assert_eq!(
                unstaged.stdout,
                format!("1\t0\t{f}\0").into_bytes(),
                "{lane}: {f}"
            );
        }
    }
}

/// 03 §3.3: a message travels on stdin and is committed byte for byte (`--cleanup=verbatim`):
/// leading blank lines, trailing spaces, tabs, CR, `#` lines and trailing blank lines stay —
/// through every login shell's reading of the words. The log says `message, 8 lines`.
#[test]
fn a_message_on_stdin_is_committed_byte_for_byte() {
    let Some(fx) = fixture() else { return };
    let message = "\n\nsubject  \t\r\n\n# not a comment\nbody 'it's' $HOME \\n\n\n\n";
    for (lane, runner) in fx.all_lanes() {
        let repo = fx.repo(&format!("msg-{lane}"));
        let h = Handle::new();
        let commit = write(
            repo.as_os_str(),
            &[
                "commit",
                "-q",
                "--allow-empty",
                "--cleanup=verbatim",
                "-F",
                "-",
            ],
        )
        .feed("message", message);
        let done = runner.run(&commit, &h);
        ok(&lane, &done);
        assert_eq!(
            done.entry.stdin.as_deref(),
            Some("message, 8 lines"),
            "{lane}"
        );
        let raw = fx.git(&repo, &["cat-file", "commit", "HEAD"]).stdout;
        let at = raw.windows(2).position(|w| w == b"\n\n").unwrap();
        assert_eq!(&raw[at + 2..], message.as_bytes(), "{lane}");
    }
}

/// A snippet runs in the folder with its arguments as `"$@"`, by a local `sh` and by the
/// remote's login shell alike (03 §3.2).
#[test]
fn a_snippet_runs_in_the_folder_with_its_arguments() {
    static SAY: Snippet = Snippet {
        name: "say",
        script: r#"pwd; for a in "$@"; do printf '[%s]\n' "$a"; done"#,
    };
    let Some(fx) = fixture() else { return };
    let dir = fx.root.join("it's a $dir");
    std::fs::create_dir_all(&dir).unwrap();
    for (lane, runner) in fx.lanes() {
        let cmd = GitCommand::snippet(Class::Read, dir.as_os_str(), &SAY, ["-x", "a b'c"]);
        let done = runner.run(&cmd, &Handle::new());
        ok(&lane, &done);
        let want = format!("{}\n[-x]\n[a b'c]\n", dir.display());
        assert_eq!(String::from_utf8_lossy(&done.stdout), want, "{lane}");
        assert!(done.entry.line.starts_with("say -x"), "{lane}");
    }
}

// ---------------------------------------------------------------------------------------------
// G-R2: never a prompt

/// A test HTTP server that answers every request `401` with a Basic challenge.
struct Challenge {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Challenge {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        let _ = s.set_nonblocking(false);
                        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut req = Vec::new();
                        let mut buf = [0u8; 4096];
                        while !req.windows(4).any(|w| w == b"\r\n\r\n") && req.len() < 65536 {
                            match s.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => req.extend_from_slice(&buf[..n]),
                            }
                        }
                        let _ = s.write_all(
                            b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"x\"\r\n\
                              Content-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            port,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Challenge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// G-R2 (03:1104), the runner's half: a fetch from a server that asks for a password fails
/// within 5 s with git's `terminal prompts disabled` in the log entry; nothing prompts (a
/// session of its own, no terminal, every askpass blanked). The served URL is `127.0.0.1`, not
/// `localhost`, so a resolver that tries `::1` first cannot slow it. Fetch as an action is S3's.
#[test]
fn a_fetch_that_needs_a_password_fails_at_once() {
    let Some(fx) = fixture() else { return };
    let server = Challenge::start();
    for (lane, runner) in fx.lanes() {
        let repo = fx.repo(&format!("auth-{lane}"));
        let url = format!("http://127.0.0.1:{}/x.git", server.port);
        let fetch = GitCommand::new(Class::Network, repo.as_os_str(), ["fetch", url.as_str()]);
        let t = Instant::now();
        let done = runner.run(&fetch, &Handle::new());
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "{lane}: {:?}",
            t.elapsed()
        );
        assert_eq!(done.ending, Ending::Exit(128), "{lane}: {}", done.stderr);
        assert!(
            done.entry.stderr.contains("terminal prompts disabled"),
            "{lane}: {}",
            done.entry.stderr
        );
    }
}

// ---------------------------------------------------------------------------------------------
// G-R3: stopping

/// `fetch --upload-pack='touch <marker>; sleep 30; git-upload-pack' <origin>`: a fetch that
/// waits 30 s on the "server", the marker made once it waits.
fn slow_fetch(fx: &Fixture, repo: &Path, marker: &Path) -> GitCommand {
    let origin = fx.repo(&format!(
        "{}-origin",
        repo.file_name().unwrap().to_string_lossy()
    ));
    let upload = format!(
        "--upload-pack=touch {}; sleep 30; git-upload-pack",
        sh_word(marker)
    );
    GitCommand::new(
        Class::Network,
        repo.as_os_str(),
        [OsString::from("fetch"), upload.into(), origin.into()],
    )
}

/// G-R3 (03:1107), the runner's half: Esc on a fetch whose upload-pack sleeps 30 s → within 6 s
/// the log says `stopped`, no process of its group is left and no `*.lock` exists. Locally the
/// group is git's own (a session of its own); through `ShellGit` it is the pid of the
/// `sopen-pid` line — the login shell's, which sshd would make a session leader — and the stop
/// travels as `exec 'kill' '-TERM' '--' '-<pid>'` through the login shell.
#[test]
fn esc_stops_a_slow_fetch_and_its_whole_group() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let repo = fx.repo(&format!("slow-{lane}"));
        let marker = fx.root.join(format!("waiting-{lane}"));
        let handle = Handle::new();
        let running = runner
            .start(&slow_fetch(&fx, &repo, &marker), &handle)
            .unwrap();
        let group = Group(running.pid());
        wait_for("the upload-pack's sleep", Duration::from_secs(10), || {
            marker.exists()
        });
        if lane != "local" {
            wait_for("the sopen-pid line", Duration::from_secs(5), || {
                running.remote_pid().is_some()
            });
            assert_eq!(running.remote_pid(), Some(running.pid()), "{lane}");
        }
        assert!(group.alive());
        let t = Instant::now();
        handle.stop();
        let done = running.finish();
        assert!(
            t.elapsed() < Duration::from_secs(6),
            "{lane}: {:?}",
            t.elapsed()
        );
        assert!(
            matches!(
                done.ending,
                Ending::Stopped(Stopped {
                    by: StopBy::Asked,
                    ..
                })
            ),
            "{lane}: {}",
            done.ending
        );
        assert!(
            done.entry.ending.to_string().starts_with("stopped"),
            "{lane}"
        );
        wait_for("the group gone", Duration::from_secs(1), || !group.alive());
        assert_eq!(
            lock_files(&repo.join(".git")),
            Vec::<PathBuf>::new(),
            "{lane}"
        );
    }
}

/// 03 §3.5: with the master gone, a remote stop cannot be sent, so the result says the command
/// may still be running — never `stopped` (G-R3's live receipt, the master killed before Esc).
/// It is: its group is still there.
#[test]
fn with_the_master_gone_a_stop_says_it_may_still_be_running() {
    let Some(fx) = fixture() else { return };
    let (runner, faults) = fx.shell();
    let repo = fx.repo("orphan");
    let marker = fx.root.join("waiting-orphan");
    let handle = Handle::new();
    let running = runner
        .start(&slow_fetch(&fx, &repo, &marker), &handle)
        .unwrap();
    let group = Group(running.pid());
    wait_for("the upload-pack's sleep", Duration::from_secs(10), || {
        marker.exists()
    });
    wait_for("the sopen-pid line", Duration::from_secs(5), || {
        running.remote_pid().is_some()
    });
    faults.master_gone.store(true, Ordering::SeqCst);
    handle.stop();
    let done = running.finish();
    assert_eq!(done.ending, Ending::StopSent { host: "box".into() });
    assert_eq!(
        done.entry.ending.to_string(),
        "stop sent; the command may still be running on box"
    );
    assert!(
        group.alive(),
        "the remote group was left running, as it would be"
    );
}

/// 03 §3.5, local: a command that ignores `SIGTERM` gets `SIGKILL` after the grace, and the
/// result lists the `*.lock` files it finds in the git directory, removing none.
#[test]
fn a_stop_that_needs_sigkill_lists_the_locks() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("stubborn");
    let lock = repo.join(".git/index.lock");
    std::fs::write(&lock, "").unwrap();
    let marker = fx.root.join("trapped");
    let runner = fx.local().with_limits(Limits {
        stop_grace: Duration::from_secs(1),
        ..Limits::default()
    });
    let alias = format!(
        "alias.stuck=!trap '' TERM; touch {}; sleep 30",
        sh_word(&marker)
    );
    let cmd = write(repo.as_os_str(), &["-c", &alias, "stuck"]);
    let handle = Handle::new();
    let running = runner.start(&cmd, &handle).unwrap();
    let group = Group(running.pid());
    wait_for("the trap", Duration::from_secs(10), || marker.exists());
    let t = Instant::now();
    handle.stop();
    let done = running.finish();
    assert!(t.elapsed() >= Duration::from_secs(1) && t.elapsed() < Duration::from_secs(5));
    assert_eq!(
        done.ending,
        Ending::Stopped(Stopped {
            by: StopBy::Asked,
            killed: true,
            locks: vec!["index.lock".into()],
        })
    );
    assert!(!group.alive());
    assert!(lock.exists(), "a lock file is never removed");
}

/// 03 §3.4: a read silent for its limit while sopen waits is stopped — `git did not answer in
/// N s` — through both transports; `within` bounds a whole run (detection's 10 s).
#[test]
fn a_silent_read_is_stopped_at_its_limit() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("silent");
    let short = Limits {
        read_idle: Duration::from_secs(1),
        ..Limits::default()
    };
    let hang = read(repo.as_os_str(), &["-c", "alias.hang=!sleep 30", "hang"]);
    let (shell, _) = fx.shell();
    for (lane, runner) in [("local", fx.local()), ("sh", shell)] {
        let runner = runner.with_limits(short);
        let handle = Handle::new();
        let t = Instant::now();
        let running = runner.start(&hang, &handle).unwrap();
        let group = Group(running.pid());
        let done = running.finish();
        assert!(
            t.elapsed() < Duration::from_secs(4),
            "{lane}: {:?}",
            t.elapsed()
        );
        assert_eq!(
            done.entry.ending.to_string(),
            "stopped: git did not answer in 1 s",
            "{lane}"
        );
        wait_for("the group gone", Duration::from_secs(1), || !group.alive());
    }
    let t = Instant::now();
    let done = fx
        .local()
        .run(&hang.clone().within(Duration::from_secs(1)), &Handle::new());
    assert!(t.elapsed() < Duration::from_secs(4));
    assert!(matches!(
        done.ending,
        Ending::Stopped(Stopped {
            by: StopBy::Limit(_),
            ..
        })
    ));
}

/// 03 §3.4: a parked stream is not waiting — the read limit counts only while sopen waits for
/// a byte, so a stream read again after a pause longer than the limit goes on.
#[test]
fn a_parked_stream_is_not_waiting() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("parked");
    let runner = fx.local().with_limits(Limits {
        read_idle: Duration::from_secs(1),
        ..Limits::default()
    });
    let cmd = read(
        repo.as_os_str(),
        &["-c", "alias.two=!echo one; sleep 2; echo two", "two"],
    );
    let mut running: Running = runner.start(&cmd, &Handle::new()).unwrap();
    assert_eq!(running.read_record(b'\n').as_deref(), Some(&b"one"[..]));
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(running.read_record(b'\n').as_deref(), Some(&b"two"[..]));
    assert_eq!(running.read_record(b'\n'), None);
    let done = running.finish();
    assert_eq!(done.ending, Ending::Exit(0));
    assert_eq!(done.entry.stdout_bytes, 8);
}

/// 03 §3.6: stderr split at `\r` and `\n`; the newest update is the progress, and the log keeps
/// each line at its last value (a CRLF line too).
#[test]
fn progress_is_the_newest_update_and_the_log_keeps_the_last() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("progress");
    let marker = fx.root.join("progressed");
    let alias = format!(
        "alias.p=!printf 'R 1\\rR 50\\rR 100, done.\\nhook says hi\\r\\nRecv 7\\r' >&2; touch {}; sleep 1",
        sh_word(&marker)
    );
    let cmd = GitCommand::new(
        Class::Network,
        repo.as_os_str(),
        ["-c", alias.as_str(), "p"],
    );
    let handle = Handle::new();
    let running = fx.local().start(&cmd, &handle).unwrap();
    wait_for("the updates", Duration::from_secs(10), || marker.exists());
    wait_for("the progress", Duration::from_secs(2), || {
        handle.progress().as_deref() == Some("Recv 7")
    });
    let done = running.finish();
    assert_eq!(done.entry.stderr, "R 100, done.\nhook says hi\nRecv 7\n");
}

// ---------------------------------------------------------------------------------------------
// G-R4: a lost connection

/// A repository whose `post-commit` hook counts its runs in `counter`.
fn counted_repo(fx: &Fixture, name: &str) -> (PathBuf, PathBuf) {
    let repo = fx.repo(name);
    let counter = fx.root.join(format!("{name}-counter"));
    let hook = repo.join(".git/hooks/post-commit");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\necho ran >> {}\n", sh_word(&counter)),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Run once now: a just-written hook can be "busy" for a moment on Linux (testutil).
    crate::testutil::run_fresh(&mut Command::new(&hook));
    std::fs::remove_file(&counter).unwrap();
    (repo, counter)
}

fn runs(counter: &Path) -> usize {
    std::fs::read_to_string(counter).map_or(0, |s| s.lines().count())
}

fn commit(repo: &Path, message: &str) -> GitCommand {
    write(
        repo.as_os_str(),
        &["commit", "-q", "--allow-empty", "-F", "-"],
    )
    .feed("message", message)
}

/// G-R4 (03:1113): a commit that runs and then loses its stderr and exit status (the connection
/// exits 255) is `outcome unknown`; it ran exactly once (the hook's counter), nothing is re-run,
/// and a read afterwards shows the commit.
#[test]
fn a_lost_commit_is_outcome_unknown_and_never_run_again() {
    let Some(fx) = fixture() else { return };
    let (repo, counter) = counted_repo(&fx, "lost-commit");
    let (runner, faults) = fx.shell();
    faults.lose.store(1, Ordering::SeqCst);
    let done = runner.run(&commit(&repo, "lost on the way\n"), &Handle::new());
    assert_eq!(done.ending, Ending::OutcomeUnknown { host: "box".into() });
    assert_eq!(runs(&counter), 1);
    assert_eq!(faults.dispatched.load(Ordering::SeqCst), 1);
    assert_eq!(runner.log().entries().len(), 1);
    let subject = runner.run(
        &read(repo.as_os_str(), &["log", "-1", "--format=%s"]),
        &Handle::new(),
    );
    assert_eq!(subject.stdout, b"lost on the way\n");
}

/// G-R4: a network command lost on the way is `outcome unknown` too, never re-run.
#[test]
fn a_lost_fetch_is_outcome_unknown() {
    let Some(fx) = fixture() else { return };
    let origin = fx.repo("lost-origin");
    let repo = fx.repo("lost-fetch");
    let (runner, faults) = fx.shell();
    faults.lose.store(1, Ordering::SeqCst);
    let fetch = GitCommand::new(
        Class::Network,
        repo.as_os_str(),
        [OsString::from("fetch"), origin.into()],
    );
    let done = runner.run(&fetch, &Handle::new());
    assert_eq!(done.ending, Ending::OutcomeUnknown { host: "box".into() });
    assert_eq!(faults.dispatched.load(Ordering::SeqCst), 1);
}

/// G-R4: a read whose connection is lost is run once more, and its answer is the second run's.
#[test]
fn a_lost_read_is_run_again() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("lost-read");
    let head = fx.git(&repo, &["rev-parse", "HEAD"]).stdout;
    let (runner, faults) = fx.shell();
    faults.lose.store(1, Ordering::SeqCst);
    let done = runner.run(
        &read(repo.as_os_str(), &["rev-parse", "HEAD"]),
        &Handle::new(),
    );
    ok("sh", &done);
    assert_eq!(done.stdout, head);
    assert_eq!(faults.dispatched.load(Ordering::SeqCst), 2);
    let endings: Vec<Ending> = runner
        .log()
        .entries()
        .into_iter()
        .map(|e| e.ending)
        .collect();
    assert_eq!(
        endings,
        [
            Ending::Exit(0),
            Ending::ConnectionLost { host: "box".into() }
        ]
    );
}

/// G-R4: with the master missing before a write, the pre-flight check fails, `Connect` revives
/// it, and the command runs once.
#[test]
fn a_missing_master_is_revived_before_a_write() {
    let Some(fx) = fixture() else { return };
    let (repo, counter) = counted_repo(&fx, "revived");
    let (runner, faults) = fx.shell();
    faults.master_gone.store(true, Ordering::SeqCst);
    let revived = Arc::new(AtomicU32::new(0));
    let (f, r) = (faults.clone(), revived.clone());
    let runner = runner.with_revive(Arc::new(move || {
        r.fetch_add(1, Ordering::SeqCst);
        f.master_gone.store(false, Ordering::SeqCst);
        Ok(())
    }));
    let done = runner.run(&commit(&repo, "after the revival\n"), &Handle::new());
    ok("sh", &done);
    assert_eq!(revived.load(Ordering::SeqCst), 1);
    assert_eq!(runs(&counter), 1);
    assert_eq!(faults.dispatched.load(Ordering::SeqCst), 1);
}

/// 03 §3.2: a master that cannot be revived means the command was never sent.
#[test]
fn a_master_that_cannot_be_revived_sends_nothing() {
    let Some(fx) = fixture() else { return };
    let (repo, counter) = counted_repo(&fx, "unrevived");
    let (runner, faults) = fx.shell();
    faults.master_gone.store(true, Ordering::SeqCst);
    let runner = runner.with_revive(Arc::new(|| anyhow::bail!("the passphrase was not given")));
    let done = runner.run(&commit(&repo, "never\n"), &Handle::new());
    assert!(matches!(done.ending, Ending::NotSent(_)), "{}", done.ending);
    assert_eq!(faults.dispatched.load(Ordering::SeqCst), 0);
    assert_eq!(runs(&counter), 0);
}

// ---------------------------------------------------------------------------------------------
// G-R6: credentials

#[test]
fn credentials_in_urls_are_masked() {
    for (text, masked) in [
        ("https://user:secret@host/x", "https://user:***@host/x"),
        (
            "fetch 'https://u:p@h:8/x.git/' failed",
            "fetch 'https://u:***@h:8/x.git/' failed",
        ),
        ("https://ghp_token@host/x", "https://***@host/x"),
        ("http://token@host/x", "http://***@host/x"),
        (
            "ssh://git@host/x and ssh://git:pw@host/y",
            "ssh://git@host/x and ssh://git:***@host/y",
        ),
        ("https://host/x", "https://host/x"),
        ("git@host:x.git", "git@host:x.git"),
        (
            "https://a:b@h/1 https://c:d@h/2",
            "https://a:***@h/1 https://c:***@h/2",
        ),
    ] {
        assert_eq!(mask_credentials(text), masked, "{text}");
    }
}

/// G-R6 (03:1123), the runner's half: a URL `https://user:secret@127.0.0.1:<port>/x.git` as a
/// remote, then stderr that repeats it → the log's lines and stderr, `SOPEN_LOG`'s line, the
/// status row and an error dialog's text show `user:***@`, never the secret.
#[test]
fn secrets_never_reach_the_log() {
    let Some(fx) = fixture() else { return };
    let url = "https://user:secret@127.0.0.1:9/x.git";
    for (lane, runner) in fx.lanes() {
        let repo = fx.repo(&format!("masked-{lane}"));
        let dir = repo.as_os_str();
        let h = Handle::new();
        let added = runner.run(&write(dir, &["remote", "add", "origin", url]), &h);
        ok(&lane, &added);
        assert!(
            added.entry.line.contains("user:***@"),
            "{lane}: {}",
            added.entry.line
        );
        let alias = format!("alias.say=!echo {url} >&2; exit 1");
        let said = runner.run(&read(dir, &["-c", &alias, "say"]), &h);
        assert_eq!(said.ending, Ending::Exit(1), "{lane}");
        assert!(said.stderr.contains("user:***@"), "{lane}: {}", said.stderr);
        assert!(said.entry.status_line().contains("user:***@"), "{lane}");
        let fetched = runner.run(
            &GitCommand::new(Class::Network, dir, ["fetch", "origin"]),
            &h,
        );
        assert!(!fetched.ok(), "{lane}");
        for e in runner.log().entries() {
            for shown in [&e.line, &e.stderr, &e.status_line(), &e.debug_line()] {
                assert!(!shown.contains("secret"), "{lane}: {shown}");
            }
        }
        for done in [&added, &said, &fetched] {
            assert!(!done.error_text().contains("secret"), "{lane}");
            assert!(!done.stderr.contains("secret"), "{lane}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The rest of 03 §3

/// 02 §6: at most 3 commands in flight per host; a fourth waits for a slot, and a handle
/// stopped while it waits sends nothing.
#[test]
fn a_fourth_command_waits_for_a_slot() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("slots");
    let runner = fx.local();
    let hang = read(repo.as_os_str(), &["-c", "alias.hang=!sleep 30", "hang"]);
    let handles: Vec<Handle> = (0..3).map(|_| Handle::new()).collect();
    let mut running: Vec<Running> = handles
        .iter()
        .map(|h| runner.start(&hang, h).unwrap())
        .collect();
    let groups: Vec<Group> = running.iter().map(|r| Group(r.pid())).collect();
    assert_eq!(runner.in_flight(), 3);

    let queued = Handle::new();
    let (r, q) = (runner.clone(), queued.clone());
    let cmd = read(repo.as_os_str(), &["rev-parse", "HEAD"]);
    let waiting = std::thread::spawn(move || r.run(&cmd, &q));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!waiting.is_finished(), "the fourth waits");
    drop(running.pop());
    let done = waiting.join().unwrap();
    ok("local", &done);

    let stopped = Handle::new();
    stopped.stop();
    let refused = runner.run(&read(repo.as_os_str(), &["rev-parse", "HEAD"]), &stopped);
    assert_eq!(
        refused.ending,
        Ending::NotSent("stopped before it was sent".into())
    );
    for h in &handles {
        h.stop();
    }
    for r in running {
        r.finish();
    }
    for g in &groups {
        wait_for("the group gone", Duration::from_secs(6), || !g.alive());
    }
}

/// One handle given to several commands at once (a load's parallel reads): one `stop` ends
/// every one of them, and what is given it afterwards is not sent.
#[test]
fn one_stop_ends_every_command_of_a_handle() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("shared-handle");
    let hang = read(repo.as_os_str(), &["-c", "alias.hang=!sleep 30", "hang"]);
    let (shell, _) = fx.shell();
    for (lane, runner) in [("local", fx.local()), ("sh", shell)] {
        let handle = Handle::new();
        let first = runner.start(&hang, &handle).unwrap();
        let second = runner.start(&hang, &handle).unwrap();
        let groups = [Group(first.pid()), Group(second.pid())];
        if lane != "local" {
            wait_for("the sopen-pid lines", Duration::from_secs(5), || {
                first.remote_pid().is_some() && second.remote_pid().is_some()
            });
        }
        let t = Instant::now();
        handle.stop();
        for done in [first.finish(), second.finish()] {
            assert!(
                matches!(done.ending, Ending::Stopped(_)),
                "{lane}: {}",
                done.ending
            );
        }
        assert!(
            t.elapsed() < Duration::from_secs(6),
            "{lane}: {:?}",
            t.elapsed()
        );
        for g in &groups {
            wait_for("the group gone", Duration::from_secs(1), || !g.alive());
        }
        let later = runner.run(&hang, &handle);
        assert_eq!(
            later.ending,
            Ending::NotSent("stopped before it was sent".into()),
            "{lane}"
        );
    }
}

/// 03 §3.2: one command carries at most 64 KiB of arguments; more is never sent.
#[test]
fn too_many_argument_bytes_are_not_sent() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("many");
    let runner = fx.local();
    let path = "p".repeat(MAX_ARGS);
    let done = runner.run(
        &write(repo.as_os_str(), &["add", "--", &path]),
        &Handle::new(),
    );
    assert!(matches!(done.ending, Ending::NotSent(_)), "{}", done.ending);
    assert_eq!(runner.in_flight(), 0);
}

/// 03 §2.4: no git — locally the program is missing; on a remote `<locate>` finds none and says
/// `sopen: no git` (exit 127). The remote half runs only where none of `<locate>`'s other
/// places has a git (a Linux runner: `/usr/bin/git` is refused there as a Darwin host's shim).
#[test]
fn no_git_is_said_as_such() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("nogit");
    let missing = GitRunner::local(LocalGit::new(fx.root.join("no-such-git")));
    let done = missing.run(&read(repo.as_os_str(), &["version"]), &Handle::new());
    assert_eq!(done.ending, Ending::NoGit);

    if ["/opt/homebrew/bin/git", "/usr/local/bin/git"]
        .iter()
        .any(|p| Path::new(p).exists())
    {
        eprintln!("note: a git outside PATH exists here; the remote no-git case is skipped");
        return;
    }
    let bin = fx.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (name, body) in [("uname", "echo Darwin"), ("xcode-select", "exit 2")] {
        let p = bin.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::testutil::run_fresh(&mut Command::new(&p));
    }
    for tool in ["env", "sh"] {
        std::os::unix::fs::symlink(which(tool).unwrap(), bin.join(tool)).unwrap();
    }
    let git = fx.shell_git(Path::new("/bin/sh")).with_env("PATH", &bin);
    let done = GitRunner::shell(git).run(&read(repo.as_os_str(), &["version"]), &Handle::new());
    assert_eq!(done.ending, Ending::NoGit, "{}", done.stderr);
}

/// 03 §2.4: `SOPEN_GIT` (not empty) first, else `git` on `PATH`; the macOS shim only with the
/// developer tools.
#[test]
fn which_local_git() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let git = bin.join("git");
    std::fs::write(&git, "").unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = |sopen_git: Option<&str>, path: &Path| {
        let path = path.as_os_str().to_os_string();
        let sopen_git = sopen_git.map(OsString::from);
        move |key: &str| match key {
            "SOPEN_GIT" => sopen_git.clone(),
            "PATH" => Some(path.clone()),
            _ => None,
        }
    };
    let found = |sopen_git, path: &Path, macos, tools| {
        LocalGit::find_with(env(sopen_git, path), macos, || tools).map(|g| g.git().to_path_buf())
    };
    assert_eq!(
        found(Some("/x/git"), &bin, false, true),
        Some("/x/git".into())
    );
    assert_eq!(found(Some(""), &bin, false, true), Some(git.clone()));
    assert_eq!(found(None, &bin, true, false), Some(git));
    assert_eq!(found(None, tmp.path(), false, true), None);
    if Path::new("/usr/bin/git").exists() {
        let usr = Path::new("/usr/bin");
        assert_eq!(found(None, usr, true, false), None);
        assert_eq!(found(None, usr, true, true), Some("/usr/bin/git".into()));
        assert_eq!(found(None, usr, false, false), Some("/usr/bin/git".into()));
    }
}

/// `ShellGit` stands in for a remote with exactly `SshGit`'s words: the line it hands its login
/// shell is the words joined by spaces (sshd's join), so what it proves holds for the argv.
#[test]
fn shell_git_runs_ssh_git_words() {
    let cmd = write("/r", &["commit", "-F", "-"]);
    let words = SshGit::words(&cmd);
    let line: Vec<u8> = words
        .iter()
        .map(|w| w.clone().into_vec())
        .collect::<Vec<_>>()
        .join(&b' ');
    assert!(line.starts_with(b"exec 'env' 'LC_ALL=C' "));
    assert!(line.ends_with(b" '--no-pager' 'commit' '-F' '-'"));
}

// ---------------------------------------------------------------------------------------------
// Review round 1

/// Review 1.1: the authority ends where URL syntax ends it (the first `/`, `?` or `#`, or
/// whitespace), so a password holding `'` or another sub-delim is masked whole — as written and
/// as the command line quotes it.
#[test]
fn a_password_with_any_sub_delim_is_masked() {
    for (text, masked) in [
        ("https://user:pa'ss@host/x", "https://user:***@host/x"),
        ("https://user:p\"a<s>s`@host/x", "https://user:***@host/x"),
        (
            "https://user:p!$&()*+,;=s@host?q",
            "https://user:***@host?q",
        ),
        (
            "'https://user:pa'\\''ss@host/x'",
            "'https://user:***@host/x'",
        ),
    ] {
        assert_eq!(mask_credentials(text), masked, "{text}");
    }
    let add = write("/r", &["remote", "add", "o", "https://user:pa'ss@host/x"]);
    assert_eq!(
        add.command_line(),
        "git remote add o 'https://user:***@host/x'"
    );
}

/// Review 1.2: stderr is masked before it is cut. Two cuts: the kept tail starting inside
/// `https://user:SECRET@host` once older lines are dropped (the `https://` left behind), and the
/// progress of an update longer than the kept 64 KiB whose kept part ends inside the URL (its
/// `@host` past the cut). Neither shows any of the secret.
#[test]
fn a_cut_in_stderr_never_shows_a_secret() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("cut");
    let kept = crate::git::runner::STDERR_KEPT;
    // The log keeps the last 64 KiB from a line's start: a first line of 64 KiB, the URL line,
    // then one line long enough that what is dropped ends 8 bytes into the URL.
    let url_line = "https://user:SECRET@host/x.git";
    let mut tail = "a".repeat(kept - 1);
    tail.push('\n');
    tail.push_str(url_line);
    tail.push('\n');
    tail.push_str(&"b".repeat(kept - url_line.len() - 2 + 8 - 1));
    tail.push('\n');
    let tail_file = fx.root.join("tail");
    std::fs::write(&tail_file, tail).unwrap();
    // An update (ended by `\r`) longer than 64 KiB, cut inside the URL.
    let cut_url = "https://user:SECRET";
    let mut update = "x".repeat(kept - cut_url.len());
    update.push_str(cut_url);
    update.push_str("SECRET@host/x.git\r");
    let update_file = fx.root.join("update");
    std::fs::write(&update_file, update).unwrap();
    for (lane, runner) in fx.lanes() {
        let alias = format!("alias.tail=!cat {} >&2; exit 1", sh_word(&tail_file));
        let done = runner.run(
            &read(repo.as_os_str(), &["-c", &alias, "tail"]),
            &Handle::new(),
        );
        assert_eq!(done.ending, Ending::Exit(1), "{lane}");
        for shown in [
            &done.stderr,
            &done.entry.stderr,
            &done.entry.status_line(),
            &done.error_text(),
        ] {
            let head = &shown[..shown.len().min(40)];
            assert!(!shown.contains("SECRET"), "{lane}: {head}…");
        }

        let marker = fx.root.join(format!("updated-{lane}"));
        let alias = format!(
            "alias.update=!cat {} >&2; touch {}; sleep 1",
            sh_word(&update_file),
            sh_word(&marker)
        );
        let handle = Handle::new();
        let running = runner
            .start(&read(repo.as_os_str(), &["-c", &alias, "update"]), &handle)
            .unwrap();
        wait_for("the update", Duration::from_secs(10), || marker.exists());
        wait_for("the progress", Duration::from_secs(2), || {
            handle.progress().is_some_and(|p| p.len() > 1000)
        });
        let progress = handle.progress().unwrap();
        let end = &progress[progress.len().saturating_sub(40)..];
        assert!(!progress.contains("SECRET"), "{lane}: …{end}");
        running.finish();
    }
}

/// Review 1.3: a record that never meets its delimiter, and output past what `finish` collects,
/// stop the command with `StopBy::Overflow` instead of growing without end.
#[test]
fn a_record_or_an_output_past_its_bound_is_stopped() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("overflow");
    // 8 MiB with no newline.
    let alias = "alias.flood=!head -c 8388608 /dev/zero | tr '\\0' x";
    let flood = read(repo.as_os_str(), &["-c", alias, "flood"]);
    let mib = 1024 * 1024;
    for (lane, runner) in fx.lanes() {
        let mut running = runner
            .start(&flood.clone().max_record(mib), &Handle::new())
            .unwrap();
        assert_eq!(running.read_record(b'\n'), None, "{lane}");
        let done = running.finish();
        assert!(
            matches!(&done.ending, Ending::Stopped(s) if s.by == StopBy::Overflow(mib)),
            "{lane}: {}",
            done.ending
        );
        let done = runner.run(&flood.clone().max_output(mib), &Handle::new());
        assert!(
            matches!(&done.ending, Ending::Stopped(s) if s.by == StopBy::Overflow(mib)),
            "{lane}: {}",
            done.ending
        );
        assert!(done.stdout.len() <= mib, "{lane}: {}", done.stdout.len());
        assert_eq!(
            done.entry.ending.to_string(),
            format!("stopped: more than {mib} bytes of output"),
            "{lane}"
        );
    }
}

/// Review 1.4: a helper the command left behind holding stderr (a hook's background job) is
/// ended with the command once its output has had its moment: `finish` returns, nothing of the
/// group is left, and the stderr reader has let go of the pipe.
#[test]
fn a_helper_left_holding_stderr_is_ended_with_the_command() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("left-behind");
    let alias = "alias.bg=!sleep 30 >/dev/null & echo done";
    for (lane, runner) in fx.lanes() {
        let running = runner
            .start(
                &read(repo.as_os_str(), &["-c", alias, "bg"]),
                &Handle::new(),
            )
            .unwrap();
        let group = Group(running.pid());
        let reader_done = running.stderr_probe();
        let t = Instant::now();
        let done = running.finish();
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "{lane}: {:?}",
            t.elapsed()
        );
        assert_eq!(done.ending, Ending::Exit(0), "{lane}: {}", done.stderr);
        assert_eq!(done.stdout, b"done\n", "{lane}");
        wait_for("the group gone", Duration::from_secs(2), || !group.alive());
        assert!(
            reader_done(),
            "{lane}: the stderr reader still holds the pipe"
        );
    }
}

/// Review 1.5: a remote stop does not take the channel's end for the group's: a child that
/// ignores `SIGTERM` and has closed its descriptors outlives the channel, and only the
/// `kill -KILL -- -<pid>` sent after the grace ends it. Locally the group itself is watched.
#[test]
fn a_stop_kills_a_child_that_ignores_term_and_left_the_channel() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("term-proof");
    let short = Limits {
        stop_grace: Duration::from_secs(1),
        ..Limits::default()
    };
    let (shell, _) = fx.shell();
    for (lane, runner) in [("local", fx.local()), ("sh", shell)] {
        let runner = runner.with_limits(short);
        let marker = fx.root.join(format!("immune-{lane}"));
        let alias = format!(
            "alias.immune=!(trap '' TERM; exec >/dev/null 2>&1 </dev/null; touch {}; sleep 30) & sleep 30",
            sh_word(&marker)
        );
        let handle = Handle::new();
        let running = runner
            .start(&read(repo.as_os_str(), &["-c", &alias, "immune"]), &handle)
            .unwrap();
        let group = Group(running.pid());
        wait_for("the immune child", Duration::from_secs(10), || {
            marker.exists()
        });
        if lane != "local" {
            wait_for("the sopen-pid line", Duration::from_secs(5), || {
                running.remote_pid().is_some()
            });
        }
        handle.stop();
        let done = running.finish();
        assert!(
            matches!(&done.ending, Ending::Stopped(s) if s.killed),
            "{lane}: {}",
            done.ending
        );
        wait_for("the group gone", Duration::from_secs(2), || !group.alive());
    }
}

/// Review 1.6: pathspec settings the session exports never reach git: History search's pattern
/// matches under an inherited `GIT_LITERAL_PATHSPECS=1`, and a literal command works under an
/// inherited `GIT_GLOB_PATHSPECS=1` (which git refuses next to the literal setting).
#[test]
fn inherited_pathspec_settings_do_not_reach_git() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("pathspec-env");
    for f in ["ab", "ac"] {
        std::fs::write(repo.join(f), "x\n").unwrap();
    }
    fx.git(&repo, &["add", "ab", "ac"]);
    fx.git(&repo, &["commit", "-q", "-m", "a files"]);
    let search = read(repo.as_os_str(), &["ls-files", "-z", "--", ":(glob)a*"]).patterns();
    let literal = read(repo.as_os_str(), &["ls-files", "-z", "--", "ab"]);
    for (key, cmd, want) in [
        ("GIT_LITERAL_PATHSPECS", &search, &b"ab\0ac\0"[..]),
        ("GIT_GLOB_PATHSPECS", &literal, &b"ab\0"[..]),
    ] {
        let local = GitRunner::local(fx.local_git().with_env(key, "1"));
        let shell = GitRunner::shell(fx.shell_git(Path::new("/bin/sh")).with_env(key, "1"));
        for (lane, runner) in [("local", local), ("sh", shell)] {
            let done = runner.run(cmd, &Handle::new());
            assert_eq!(done.stdout, want, "{lane} {key}=1: {}", done.stderr);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Review round 2

/// A process the test started in a session of its own (a helper's `setsid` child), killed when
/// the guard goes.
struct Process(i32);

impl Drop for Process {
    fn drop(&mut self) {
        if self.0 >= 2 {
            // SAFETY: the one process this test made, by its pid.
            unsafe { libc::kill(self.0, libc::SIGKILL) };
        }
    }
}

/// `-c alias.away=!perl … ; echo done`: a helper whose child leaves for a session of its own
/// (`setsid`), keeping the command's stderr, its pid written to `pid_file`. `None` without
/// perl (a note).
fn session_leaver(pid_file: &Path) -> Option<String> {
    let Some(perl) = which("perl") else {
        eprintln!("note: perl is not on PATH; the setsid-helper case is skipped");
        return None;
    };
    let code = r#"use POSIX qw(setsid); exit 0 if fork; setsid(); open(my $f, ">", $ARGV[0]) or die; print $f $$; close $f; sleep 30;"#;
    Some(format!(
        "alias.away=!{} -e '{code}' {} >/dev/null; echo done",
        sh_word(&perl),
        sh_word(pid_file)
    ))
}

/// The helper's child, once its pid is written.
fn left_process(pid_file: &Path) -> Process {
    wait_for("the helper's pid", Duration::from_secs(5), || {
        std::fs::read_to_string(pid_file).is_ok_and(|s| !s.is_empty())
    });
    Process(
        std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap(),
    )
}

/// Review 2.1: stdout cut by `max_output` is masked as text cut short: `https://user:secret`
/// with its `@host` past the cut shows no password, in the error dialog or anywhere else.
#[test]
fn cut_stdout_never_shows_a_password() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("cut-stdout");
    let alias = "alias.url=!printf 'https://user:secret@host/path'";
    for (lane, runner) in fx.lanes() {
        let cmd = read(repo.as_os_str(), &["-c", alias, "url"]).max_output(19);
        let done = runner.run(&cmd, &Handle::new());
        assert_eq!(done.stdout, b"https://user:secret", "{lane}");
        assert!(
            !done.error_text().contains("secret"),
            "{lane}: {}",
            done.error_text()
        );
        assert!(!done.stdout_text().contains("secret"), "{lane}");
        assert!(done.stdout_cut, "{lane}");
    }
}

/// Review 2.2: what a stream left in its buffer counts against `max_output` too: `finish`
/// after `read_record` neither underflows nor returns more than the bound.
#[test]
fn finish_bounds_what_a_stream_left_over() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("left-over");
    let alias = "alias.rest=!printf 'a\\n0123456789'";
    for (lane, runner) in fx.lanes() {
        let cmd = read(repo.as_os_str(), &["-c", alias, "rest"]).max_output(4);
        let mut running = runner.start(&cmd, &Handle::new()).unwrap();
        assert_eq!(
            running.read_record(b'\n').as_deref(),
            Some(&b"a"[..]),
            "{lane}"
        );
        let done = running.finish();
        assert!(done.stdout.len() <= 4, "{lane}: {}", done.stdout.len());
        assert!(
            matches!(&done.ending, Ending::Stopped(s) if s.by == StopBy::Overflow(4)),
            "{lane}: {}",
            done.ending
        );
    }
}

/// Review 2.3: a helper's child in a session of its own keeps stderr open; killing the group
/// cannot reach it, so the reader gives up on the pipe instead: `finish` returns and no reader
/// thread outlives it.
#[test]
fn no_reader_outlives_a_helper_in_a_session_of_its_own() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("setsid");
    for (lane, runner) in fx.lanes() {
        let pid_file = fx.root.join(format!("away-{lane}"));
        let Some(alias) = session_leaver(&pid_file) else {
            return;
        };
        let running = runner
            .start(
                &read(repo.as_os_str(), &["-c", &alias, "away"]),
                &Handle::new(),
            )
            .unwrap();
        let reader_done = running.stderr_probe();
        let t = Instant::now();
        let done = running.finish();
        let _helper = left_process(&pid_file);
        assert!(
            t.elapsed() < Duration::from_secs(6),
            "{lane}: {:?}",
            t.elapsed()
        );
        assert_eq!(done.ending, Ending::Exit(0), "{lane}: {}", done.stderr);
        assert_eq!(done.stdout, b"done\n", "{lane}");
        assert!(reader_done(), "{lane}: the stderr reader is still in read");
    }
}

/// Review 2.4: a stop that comes once the channel has ended still asks the group, and ends what
/// is left of it: a child that ignores `SIGTERM` and closed its descriptors.
#[test]
fn a_stop_after_the_channel_ended_still_ends_what_is_left() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("after-channel");
    let short = Limits {
        stop_grace: Duration::from_secs(1),
        ..Limits::default()
    };
    let (shell, _) = fx.shell();
    for (lane, runner) in [("local", fx.local()), ("sh", shell)] {
        let runner = runner.with_limits(short);
        let marker = fx.root.join(format!("outlived-{lane}"));
        let alias = format!(
            "alias.outlive=!(trap '' TERM; exec >/dev/null 2>&1 </dev/null; touch {}; sleep 30) & echo done",
            sh_word(&marker)
        );
        let handle = Handle::new();
        let running = runner
            .start(&read(repo.as_os_str(), &["-c", &alias, "outlive"]), &handle)
            .unwrap();
        let group = Group(running.pid());
        wait_for("the child", Duration::from_secs(10), || marker.exists());
        if lane != "local" {
            wait_for("the sopen-pid line", Duration::from_secs(5), || {
                running.remote_pid().is_some()
            });
        }
        // git and its alias have ended by now; only the child is left.
        std::thread::sleep(Duration::from_millis(500));
        handle.stop();
        let done = running.finish();
        assert!(
            matches!(&done.ending, Ending::Stopped(s) if s.killed),
            "{lane}: {}",
            done.ending
        );
        wait_for("the group gone", Duration::from_secs(2), || !group.alive());
    }
}

/// Review 2.5: `SIGKILL` goes only to a group the probe just before it found alive — after a
/// stop whose `SIGTERM` ended everything, and in the cleanup after a command whose leftover is
/// not in its group, only `kill -0` is sent.
#[test]
fn kill_goes_only_to_a_group_found_alive() {
    let Some(fx) = fixture() else { return };
    let repo = fx.repo("no-blind-kill");
    let (runner, faults) = fx.shell();
    let hang = read(repo.as_os_str(), &["-c", "alias.hang=!sleep 30", "hang"]);
    let handle = Handle::new();
    let running = runner.start(&hang, &handle).unwrap();
    let group = Group(running.pid());
    wait_for("the sopen-pid line", Duration::from_secs(5), || {
        running.remote_pid().is_some()
    });
    handle.stop();
    let done = running.finish();
    assert!(
        matches!(&done.ending, Ending::Stopped(s) if !s.killed),
        "{}",
        done.ending
    );
    wait_for("the group gone", Duration::from_secs(1), || !group.alive());
    let sent = std::mem::take(&mut *faults.signals.lock().unwrap());
    assert!(sent.contains(&"-TERM".to_string()), "{sent:?}");
    assert!(!sent.contains(&"-KILL".to_string()), "{sent:?}");

    let pid_file = fx.root.join("away-kill");
    let Some(alias) = session_leaver(&pid_file) else {
        return;
    };
    let done = runner.run(
        &read(repo.as_os_str(), &["-c", &alias, "away"]),
        &Handle::new(),
    );
    let _helper = left_process(&pid_file);
    assert_eq!(done.ending, Ending::Exit(0));
    let sent = faults.signals.lock().unwrap().clone();
    assert_eq!(
        sent,
        ["-0"],
        "the cleanup asks first, and the group is gone"
    );
}
