//! Running git (03 §3): `GitRunner` and its transports — `LocalGit` (this machine: an argument
//! vector, a session of its own), `SshGit` (a remote, over sopen's ssh master with
//! [`crate::remote::exec`]'s arguments, every word quoted as `forward::quote` quotes it, after
//! the fixed `<locate>` script) and `ShellGit` (tests: `SshGit`'s words through a local `sh -c`)
//! — with 03 §3.3's environment, §3.4's classes and limits, stopping (§3.5), progress (§3.6) and
//! the command log (§3.7); at most 3 commands in flight per host (02 §6).
//!
//! # The API (S2-B, S2-F and S3 build on it)
//!
//! - **One [`GitRunner`] per host** (02 §6's `GitHost` holds it): [`GitRunner::local`] with a
//!   [`LocalGit`] ([`LocalGit::find`]: `SOPEN_GIT`, else `git` on `PATH`, 03 §2.4), or
//!   [`GitRunner::ssh`] with an [`SshGit`] ([`SshGit::new`]: the master's arguments, read once).
//!   [`GitRunner::with_revive`] gives it the browser's `Connect`, which brings back a missing
//!   master (03 §3.2). A `GitRunner` is cheap to clone and every clone shares the host's 3
//!   in-flight slots and its [`CommandLog`]; `with_revive` and `with_limits` configure it before
//!   the first clone (after one, they panic).
//! - **A command** is a [`GitCommand`]: its [`Class`] (read / network / write, 03 §3.4), the
//!   folder it runs in (`-C`), and the words after git's own options, the subcommand first:
//!   `GitCommand::new(Class::Read, top, ["for-each-ref", "--format=…", "refs/heads"])`.
//!   Builders: [`GitCommand::feed`] (bytes on stdin, said in the log as `message, 3 lines`;
//!   otherwise stdin is `/dev/null`), [`GitCommand::env`] (`GIT_EDITOR=true`,
//!   `GIT_COMMITTER_DATE=…`, shown in the log), [`GitCommand::patterns`] (History search:
//!   `GIT_LITERAL_PATHSPECS=0`), [`GitCommand::within`] (a bound on the whole run: detection's
//!   10 s), [`GitCommand::git_dir`] (where to look for `*.lock` files after a `SIGKILL`).
//!   [`GitCommand::snippet`] runs one of sopen's fixed POSIX sh [`Snippet`]s (the in-progress
//!   probe, 03 §8.5) the same way, in the folder, with its arguments as `"$@"`.
//! - **The runner adds**, so no caller has to: 03 §3.3's environment and options, and to every
//!   `fetch` `--progress --no-prune --no-prune-tags` right after the subcommand (an action that
//!   prunes passes `--prune` / `--prune-tags` later, which wins), to every `push` `--progress`
//!   (git reports progress to a pipe only when asked, and §3.4's 120 s rule counts its bytes).
//!   [`GitCommand::command_line`] is the line as it runs and as the log shows it.
//! - **One-shot**: [`GitRunner::run`]`(&cmd, &handle)` → [`Done`]: the [`Ending`], all of
//!   stdout, stderr (masked, `\r` updates collapsed, its last 64 KiB) and the [`LogEntry`]
//!   already added to the log. A read whose connection is lost is revived and run once more; a
//!   write or network command never is (`Ending::OutcomeUnknown`).
//! - **A stream**: [`GitRunner::start`]`(&cmd, &handle)` → [`Running`], or the boxed [`Done`]
//!   of a command not sent. [`Running::read_record`]`(b'\0')` (or `b'\n'` for lines) returns
//!   the next record without its delimiter, or `None` once stdout has ended; nothing is read
//!   ahead beyond what the pipe holds, so a caller that stops reading parks git on the full pipe
//!   (03 §4.5). [`Running::finish`] reads the rest and gives the [`Done`]; dropping a `Running`
//!   stops the command in the background and logs it (a panel dropped, or hidden for 30 s).
//!   A stream whose connection is lost ends with `Ending::ConnectionLost`; restarting it is the
//!   caller's.
//! - **The log and errors**: [`GitRunner::log`] is the host's [`CommandLog`], newest first;
//!   [`LogEntry::status_line`] is the status row's text, [`LogEntry::debug_line`] what
//!   `SOPEN_LOG` gets, [`Done::error_text`] what an error dialog holds (03 §3.8).
//!   `Ending::NotSent` is a command never sent: its handle stopped first, the master gone and not
//!   revived, more than [`MAX_ARGS`] bytes of arguments (batch paths below it), or no program.
//! - **Stop and progress**: a [`Handle`] (clone it to the UI thread) is made before the run and
//!   passed to it. [`Handle::stop`] (Esc) never blocks: the stop runs on a thread of its own and
//!   the worker's `run` / `finish` returns `Ending::Stopped` (or `Ending::StopSent`, when it may
//!   still run on the host). One handle may be given to several commands at once (a load's
//!   parallel reads, its stream): `stop` stops every one still running. A handle stopped before
//!   its command was sent sends nothing, and the next commands given the same handle are not
//!   sent either (an action's later steps; a superseded generation). [`Handle::progress`],
//!   [`Handle::elapsed`] and [`Handle::silent_for`] are the newest running command's, for the
//!   status row (03 §3.4: the newest progress line; after 10 s without output, what git may
//!   wait for).
//! - **Limits**: read 30 s and network 120 s without a byte on stdout or stderr while sopen
//!   waits (a parked stream is not waiting), then stopped; write none ([`Limits`]). Output is
//!   bounded too: one record at most [`MAX_RECORD`] ([`GitCommand::max_record`]), all that
//!   `finish` collects at most [`MAX_OUTPUT`] ([`GitCommand::max_output`]); past either the
//!   command is stopped and ends `Ending::Stopped` with `StopBy::Overflow(limit)`
//!   (`read_record` returns `None`).
//! - **Nothing is left behind** locally: once a finished command's output has had a second,
//!   whatever of its process group still holds stdout or stderr is killed; a pipe held from
//!   outside the group (a process in a session of its own) is let go of. Remotely the command's
//!   leader has ended by then, and a group is never signalled by its number alone (03 §3.5): the
//!   reader lets go of the pipe and a leftover may still run there. No reader thread outlives
//!   `finish`.
//! - **A remote stop checks whom it signals**: the `sopen-pid` line carries the leader's start
//!   time, and each stop session reads it again right before each signal (one `sh -c`, one
//!   answer per group); a group whose leader is gone or whose pid another process has is not
//!   signalled, and the result says it may still be running (03 §3.5).
//! - **No git**: `Ending::NoGit` — locally the git program is missing, remotely `<locate>`
//!   found none (`sopen: no git`, exit 127).
//! - [`mask_credentials`]: what every logged line and kept stderr went through; stdout is
//!   returned as git printed it, so show it through [`Done::stdout_text`] (masked, and as text
//!   cut short when `Done::stdout_cut`).
//!
//! Not here: the per-repository write lock (02 §6's `GitRepo`), the status row's and dialogs'
//! wording and the Stop question of a write (03 §3.4), refresh (03 §5).

use crate::remote::exec;
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant, SystemTime};

/// A read stops after this long without a byte while sopen waits for it (03 §3.4).
pub const READ_IDLE: Duration = Duration::from_secs(30);
/// A network command stops after this long without a byte (03 §3.4).
pub const NETWORK_IDLE: Duration = Duration::from_secs(120);
/// Between `SIGTERM` and `SIGKILL` (03 §3.5).
pub const STOP_GRACE: Duration = Duration::from_secs(5);
/// Commands in flight per host (02 §6; sshd's `MaxSessions` is shared with SFTP).
pub const IN_FLIGHT: usize = 3;
/// The most argument bytes one command carries, quoted as they travel (03 §3.2: a remote command
/// line is one argument to the login shell, and Linux takes at most 128 KiB in one argument);
/// callers batch paths below it.
pub const MAX_ARGS: usize = 64 * 1024;
/// The last bytes of stderr a log entry keeps (03 §3.7).
pub const STDERR_KEPT: usize = 64 * 1024;
/// Entries the command log holds, the oldest dropped first (memory only, 03 §3.7).
pub const LOG_ENTRIES: usize = 500;
/// The longest stdout record [`Running::read_record`] returns unless [`GitCommand::max_record`]
/// says otherwise; a longer one stops the command (`StopBy::Overflow`).
pub const MAX_RECORD: usize = 16 * 1024 * 1024;
/// The most stdout [`Running::finish`] (and so [`GitRunner::run`]) collects unless
/// [`GitCommand::max_output`] says otherwise; more stops the command (`StopBy::Overflow`).
pub const MAX_OUTPUT: usize = 256 * 1024 * 1024;

/// The master check and a remote `kill` (each its own short command) are given up after this.
const CONTROL_LIMIT: Duration = Duration::from_secs(10);
/// Once the process has exited, how long its output is still read (a hook's daemon can hold
/// the pipes open for good).
const AFTER_EXIT: Duration = Duration::from_secs(1);
/// How long a stop waits for the `sopen-pid` line before it gives up on the remote group.
const PID_WAIT: Duration = Duration::from_secs(2);
/// How often a remote stop asks whether the group is gone, within the grace.
const REMOTE_PROBE: Duration = Duration::from_millis(500);
/// Once a leftover of a finished command has been killed, how long its pipes may take to close.
const LEFTOVER_WAIT: Duration = Duration::from_secs(2);
/// The longest a blocked read waits before it looks at the limits and the stop again.
const TICK: Duration = Duration::from_millis(50);

/// 03 §3.3's environment, in this order; [`GitCommand::env`] replaces a value.
const ENV: [(&str, &str); 10] = [
    ("LC_ALL", "C"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ASKPASS", ""),
    ("SSH_ASKPASS", ""),
    ("SSH_ASKPASS_REQUIRE", "never"),
    ("GIT_PAGER", "cat"),
    ("PAGER", "cat"),
    ("GIT_EDITOR", "false"),
    ("GIT_SEQUENCE_EDITOR", "false"),
    ("GIT_MERGE_AUTOEDIT", "no"),
];

/// git's global pathspec settings besides `GIT_LITERAL_PATHSPECS`, set off for every command: a
/// session that exports one would change what a path names, and git refuses any of them next
/// to the literal setting (`global 'literal' pathspec setting is incompatible…`).
const PATHSPEC_OFF: [&str; 3] = [
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// 03 §3.3's options, after `-C <folder>` and before the subcommand.
const OPTIONS: [&str; 9] = [
    "-c",
    "core.quotePath=false",
    "-c",
    "color.ui=false",
    "-c",
    "log.showSignature=false",
    "-c",
    "i18n.commitEncoding=UTF-8",
    "--no-pager",
];

/// The variables that point git at a repository (`git rev-parse --local-env-vars`). A local
/// command and `ShellGit`'s stand-in session run without them: sopen started from a git hook
/// or alias inherits `GIT_DIR` and `GIT_INDEX_FILE`, and `-C <folder>` must be the repository.
pub const LOCAL_REPO_ENV: [&str; 15] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// `sopen_id <pid>`: the process's identity in `$sopen_i` — its start time, which a pid that
/// is used again does not share (S2-F review 1; 03 §3.5): field 22 of `/proc/<pid>/stat` (clock
/// ticks since boot) where there is a `/proc`, else `ps -o lstart=` (macOS); empty when there is
/// no such process, and when `ps` fails (what a failed `ps` printed is no start time). Builtins
/// only on Linux (no process started): `read`, the parameter cut and the function's own
/// positional parameters (the caller's `"$@"` is untouched). No `'` and no backslash in it. A
/// macro, so the pid line, `<locate>` and the stop share one text.
macro_rules! id_fn {
    () => {
        concat!(
            r#"sopen_id() { sopen_i=; if { read -r sopen_s <"/proc/$1/stat"; } 2>/dev/null; then "#,
            r#"sopen_s=${sopen_s##*) }; set -- $sopen_s; [ $# -ge 20 ] && shift 19 && sopen_i=$1; "#,
            r#"else sopen_i=$(ps -o lstart= -p "$1" 2>/dev/null) || sopen_i=; fi; }; "#,
        )
    };
}

/// The first line of every remote command and snippet: the pid the stop signals and its
/// identity (03 §3.5). sshd starts the command as the leader of a new session, and every `exec`
/// keeps the pid and its start time.
const PID_LINE: &str = concat!(
    id_fn!(),
    r#"sopen_id $$; echo "sopen-pid $$ $sopen_i" >&2;"#,
);

/// `<locate>` (03 §3.2, §2.4): one fixed POSIX sh script, sent as a word and run by `sh -c`. It
/// prints the pid line, finds git — on `PATH`, then in Homebrew's and the usual directories;
/// `/usr/bin/git` on a Darwin host only when `xcode-select -p` succeeds (otherwise it is the
/// developer-tools shim) — and `exec`s it with `"$@"`; with none it says `sopen: no git` on
/// stderr and exits 127. No `'` and no `\` in it, so its quoted word is the script between two
/// quotes.
pub const LOCATE: &str = concat!(
    id_fn!(),
    r#"sopen_id $$; echo "sopen-pid $$ $sopen_i" >&2; "#,
    r#"ok() { [ -f "$1" ] && [ -x "$1" ] || return 1; [ "$1" = /usr/bin/git ] || return 0; "#,
    r#"[ "$(uname -s)" = Darwin ] || return 0; xcode-select -p >/dev/null 2>&1; }; "#,
    r#"g=; c=$(command -v git 2>/dev/null) && ok "$c" && g=$c; "#,
    r#"if [ -z "$g" ]; then for c in /opt/homebrew/bin/git /usr/local/bin/git /usr/bin/git; do "#,
    r#"if ok "$c"; then g=$c; break; fi; done; fi; "#,
    r#"if [ -z "$g" ]; then echo "sopen: no git" >&2; exit 127; fi; "#,
    r#"exec "$g" "$@""#,
);

/// What `<locate>` says on stderr when the host has no git.
const NO_GIT: &str = "sopen: no git";

/// 03 §3.4's command classes: what limits a command and what Esc does to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Detection, refs, status, log pages, details, diffs, files at a revision, `ls-remote`,
    /// the probe: [`Limits::read_idle`] without a byte, then stopped; Esc stops it.
    Read,
    /// `fetch`, `push`, a remote delete, a backup's fetch: [`Limits::network_idle`] without a
    /// byte, then stopped; Esc stops it.
    Network,
    /// Everything that changes the repository or the work tree: no limit (a hook or a signer may
    /// take minutes); Esc only after 10 s and a question (the UI's).
    Write,
}

/// The automatic limits (03 §3.4) and the stop's grace (§3.5); tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub read_idle: Duration,
    pub network_idle: Duration,
    pub stop_grace: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            read_idle: READ_IDLE,
            network_idle: NETWORK_IDLE,
            stop_grace: STOP_GRACE,
        }
    }
}

/// One of sopen's fixed POSIX sh snippets (03 §3.2: the in-progress probe, the files snapshot
/// and restore): part of the binary, sent as a word, never written to the remote. It runs in the
/// command's folder with the command's environment, its arguments as `"$@"`.
#[derive(Debug)]
pub struct Snippet {
    /// What the log shows in place of `git`.
    pub name: &'static str,
    pub script: &'static str,
}

#[derive(Debug, Clone, Copy)]
enum Program {
    Git,
    Snippet(&'static Snippet),
}

#[derive(Debug, Clone)]
struct Feed {
    what: String,
    bytes: Vec<u8>,
}

/// One command for a [`GitRunner`] (see the module's API notes).
#[derive(Debug, Clone)]
pub struct GitCommand {
    class: Class,
    dir: OsString,
    program: Program,
    args: Vec<OsString>,
    feed: Option<Feed>,
    env: Vec<(String, String)>,
    patterns: bool,
    within: Option<Duration>,
    git_dir: Option<PathBuf>,
    max_record: usize,
    max_output: usize,
}

impl GitCommand {
    /// `git -C <dir> <03 §3.3's options> <args>`: `args` start at the subcommand.
    pub fn new<I, S>(class: Class, dir: impl Into<OsString>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            class,
            dir: dir.into(),
            program: Program::Git,
            args: args.into_iter().map(Into::into).collect(),
            feed: None,
            env: Vec::new(),
            patterns: false,
            within: None,
            git_dir: None,
            max_record: MAX_RECORD,
            max_output: MAX_OUTPUT,
        }
    }

    /// `sh -c <snippet> sopen-git <args>…` in `dir` (03 §3.2's fixed snippets).
    pub fn snippet<I, S>(
        class: Class,
        dir: impl Into<OsString>,
        snippet: &'static Snippet,
        args: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            program: Program::Snippet(snippet),
            ..Self::new(class, dir, args)
        }
    }

    /// `bytes` on stdin instead of `/dev/null`; the log says `<what>, <n> lines` (03 §3.3: a
    /// message, a todo, a ref transaction, a path list, the log's roots).
    pub fn feed(mut self, what: &str, bytes: impl Into<Vec<u8>>) -> Self {
        self.feed = Some(Feed {
            what: what.to_string(),
            bytes: bytes.into(),
        });
        self
    }

    /// Sets `key=value` for this command, replacing 03 §3.3's value of the same name
    /// (`GIT_EDITOR=true`); the log shows it in front of the command line.
    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.retain(|(k, _)| k != key);
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    /// With `GIT_LITERAL_PATHSPECS=0`: History search's path field, the one place a pathspec is
    /// a pattern (03 §3.3).
    pub fn patterns(mut self) -> Self {
        self.patterns = true;
        self
    }

    /// Stopped once it has run this long, output or not (detection: 10 s in all, 03 §2.1).
    pub fn within(mut self, limit: Duration) -> Self {
        self.within = Some(limit);
        self
    }

    /// The repository's git directory, where a stop that needed `SIGKILL` looks for `*.lock`
    /// files to list (03 §3.5); without it, `<dir>/.git` when that is a folder.
    pub fn git_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.git_dir = Some(dir.into());
        self
    }

    /// The longest record [`Running::read_record`] returns ([`MAX_RECORD`] by default); a
    /// longer one stops the command with `StopBy::Overflow`.
    pub fn max_record(mut self, bytes: usize) -> Self {
        self.max_record = bytes;
        self
    }

    /// The most stdout [`Running::finish`] collects ([`MAX_OUTPUT`] by default); more stops the
    /// command with `StopBy::Overflow`, and `Done::stdout` holds the first `bytes`.
    pub fn max_output(mut self, bytes: usize) -> Self {
        self.max_output = bytes;
        self
    }

    pub fn class(&self) -> Class {
        self.class
    }

    /// The folder it runs in (`-C`).
    pub fn dir(&self) -> &OsStr {
        &self.dir
    }

    /// The words after git's own options as they run: the caller's, with what the runner adds
    /// to a `fetch` and a `push`.
    pub fn args(&self) -> Vec<OsString> {
        let mut args = self.args.clone();
        if let Program::Git = self.program {
            let added: &[&str] = match args.first().and_then(|a| a.to_str()) {
                Some("fetch") => &["--progress", "--no-prune", "--no-prune-tags"],
                Some("push") => &["--progress"],
                _ => &[],
            };
            for (i, word) in added.iter().enumerate() {
                args.insert(1 + i, OsString::from(word));
            }
        }
        args
    }

    /// The command line as the log, the status row and `SOPEN_LOG` show it (03 §3.7): what
    /// differs from 03 §3.3's environment, `git` (or the snippet's name) and [`Self::args`],
    /// rendered as [`crate::render_command`] renders a line, credentials masked.
    pub fn command_line(&self) -> String {
        let mut words: Vec<OsString> = self
            .env
            .iter()
            .map(|(k, v)| OsString::from(format!("{k}={v}")))
            .collect();
        words.push(match self.program {
            Program::Git => "git".into(),
            Program::Snippet(s) => s.name.into(),
        });
        words.extend(self.args());
        let line = crate::render_command(&words[0], &words[1..]);
        mask_credentials(&line)
    }

    /// The environment it runs with: 03 §3.3's, `GIT_OPTIONAL_LOCKS=0` for a read,
    /// `GIT_LITERAL_PATHSPECS=1` (`0` with [`Self::patterns`] — set, not left out, so a value
    /// the session exports cannot stand), the other pathspec settings `0`, then [`Self::env`]'s
    /// own.
    fn env_pairs(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = ENV
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if self.class == Class::Read {
            pairs.push(("GIT_OPTIONAL_LOCKS".into(), "0".into()));
        }
        let literal = if self.patterns { "0" } else { "1" };
        pairs.push(("GIT_LITERAL_PATHSPECS".into(), literal.into()));
        pairs.extend(
            PATHSPEC_OFF
                .iter()
                .map(|k| (k.to_string(), "0".to_string())),
        );
        for (k, v) in &self.env {
            match pairs.iter_mut().find(|(key, _)| key == k) {
                Some(pair) => pair.1 = v.clone(),
                None => pairs.push((k.clone(), v.clone())),
            }
        }
        pairs
    }

    /// The script `sh -c` runs and the words after its `$0` (`sopen-git`): `<locate>` and
    /// `-C <dir> <options> <args>`, or a snippet behind the pid line and a `cd` into the folder,
    /// and `<dir> <args>`.
    fn script_and_args(&self) -> (OsString, Vec<OsString>) {
        match self.program {
            Program::Git => (LOCATE.into(), self.git_args()),
            Program::Snippet(s) => {
                let script = format!(r#"{PID_LINE} cd "$1" || exit 1; shift; {}"#, s.script);
                let mut args = vec![plain_dir(&self.dir)];
                args.extend(self.args());
                (script.into(), args)
            }
        }
    }

    /// `-C <dir> <options> <args>`: git's arguments.
    fn git_args(&self) -> Vec<OsString> {
        let mut args = vec![OsString::from("-C"), self.dir.clone()];
        args.extend(OPTIONS.iter().map(OsString::from));
        args.extend(self.args());
        args
    }

    /// The quoted size of the caller's arguments, as they travel.
    fn args_size(&self) -> usize {
        self.args()
            .iter()
            .map(|a| quote(a.as_bytes()).len() + 1)
            .sum()
    }

    /// `<what>, <n> lines` for the log.
    fn stdin_said(&self) -> Option<String> {
        self.feed.as_ref().map(|f| {
            let lines = f.bytes.iter().filter(|&&b| b == b'\n').count()
                + usize::from(f.bytes.last().is_some_and(|&b| b != b'\n'));
            let unit = if lines == 1 { "line" } else { "lines" };
            format!("{}, {lines} {unit}", f.what)
        })
    }
}

/// A folder a snippet `cd`s into: never read as an option or through `CDPATH` (an absolute
/// path, or `./` in front).
fn plain_dir(dir: &OsStr) -> OsString {
    if dir.as_bytes().starts_with(b"/") {
        dir.to_os_string()
    } else {
        let mut d = OsString::from("./");
        d.push(dir);
        d
    }
}

/// One shell word that reads back as `word`, byte for byte: `forward::quote`'s rule (single
/// quotes around everything except `'` and `\`, which go outside them as `\'` and `\\`, so sh,
/// bash, zsh, dash and fish read it the same) over bytes, since a path can be any bytes and
/// `forward::quote` takes UTF-8. On UTF-8 the two agree (both quote only ASCII bytes).
pub fn quote(word: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(word.len() + 2);
    out.push(b'\'');
    for &b in word {
        match b {
            b'\'' => out.extend_from_slice(br"'\''"),
            b'\\' => out.extend_from_slice(br"'\\'"),
            b => out.push(b),
        }
    }
    out.push(b'\'');
    out
}

/// Credentials in URLs masked (03 §3.7): `scheme://user:secret@host` → `scheme://user:***@host`,
/// and an `http(s)` URL's user name with no password — a token — → `***@host`; every other
/// user name (`ssh://git@host`) stays. The authority ends where URL syntax ends it — the first
/// `/`, `?` or `#`, whitespace, or the end — and the user part is everything before its last
/// `@`, so a password holding `'`, `"` or another sub-delim is masked whole (a quote around the
/// URL that follows it may be masked with it). Applied to every line the log, `SOPEN_LOG`, the
/// status row and a dialog show, and to each line of git's stderr before it is kept.
pub fn mask_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let scheme_start = rest[..at]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || "+-.".contains(c)))
            .map_or(0, |i| i + 1);
        let scheme = &rest[scheme_start..at];
        let authority_start = at + 3;
        let authority_len = rest[authority_start..]
            .find(|c: char| "/?#".contains(c) || c.is_whitespace())
            .unwrap_or(rest.len() - authority_start);
        let authority = &rest[authority_start..authority_start + authority_len];
        out.push_str(&rest[..authority_start]);
        match authority.rfind('@') {
            Some(user_end) if !scheme.is_empty() => {
                let user = &authority[..user_end];
                let web = ["http", "https"]
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(scheme));
                match user.find(':') {
                    Some(colon) => {
                        out.push_str(&user[..colon]);
                        out.push_str(":***");
                    }
                    None if web && !user.is_empty() => out.push_str("***"),
                    None => out.push_str(user),
                }
                out.push_str(&authority[user_end..]);
            }
            _ => out.push_str(authority),
        }
        rest = &rest[authority_start + authority_len..];
    }
    out.push_str(rest);
    out
}

/// [`mask_credentials`] for text cut short: when it ends inside a URL's authority with no `@` in
/// view, what is there of the authority may be the start of a user and password whose `@` was
/// cut off, so it is replaced by `***`.
fn mask_cut(text: &str) -> String {
    let mut masked = mask_credentials(text);
    if let Some(at) = masked.rfind("://") {
        let start = at + 3;
        let open = !masked[start..].contains(|c: char| "/?#@".contains(c) || c.is_whitespace());
        if open {
            masked.truncate(start);
            masked.push_str("***");
        }
    }
    masked
}

/// This machine's git (03 §3.1): an argument vector, no shell, in a session of its own.
#[derive(Debug, Clone)]
pub struct LocalGit {
    git: PathBuf,
    env: Vec<(OsString, OsString)>,
}

impl LocalGit {
    pub fn new(git: impl Into<PathBuf>) -> Self {
        Self {
            git: git.into(),
            env: Vec::new(),
        }
    }

    /// 03 §2.4: `SOPEN_GIT` when set (an empty value is ignored), else `git` on `PATH`; on macOS
    /// `/usr/bin/git` (the developer-tools shim) only when `xcode-select -p` succeeds, so sopen
    /// never makes macOS offer to install the tools. `None`: no git here. A `SOPEN_GIT` that
    /// names nothing runs as `Ending::NoGit`.
    pub fn find() -> Option<LocalGit> {
        Self::find_with(
            |key| std::env::var_os(key),
            cfg!(target_os = "macos"),
            || {
                Command::new("xcode-select")
                    .arg("-p")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            },
        )
    }

    /// [`Self::find`] with the environment, the platform and `xcode-select -p` given.
    pub fn find_with(
        var: impl Fn(&str) -> Option<OsString>,
        macos: bool,
        tools_installed: impl FnOnce() -> bool,
    ) -> Option<LocalGit> {
        if let Some(git) = var("SOPEN_GIT").filter(|v| !v.is_empty()) {
            return Some(Self::new(git));
        }
        let path = var("PATH")?;
        let git = std::env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join("git"))
            .find(|p| is_executable(p))?;
        if macos && git == Path::new("/usr/bin/git") && !tools_installed() {
            return None;
        }
        Some(Self::new(git))
    }

    /// The git program it runs.
    pub fn git(&self) -> &Path {
        &self.git
    }

    /// Adds `key=value` to every command's environment, under 03 §3.3's (tests: a fixture's
    /// `GIT_CONFIG_GLOBAL`, `HOME`).
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    fn command(&self, cmd: &GitCommand) -> Command {
        let mut c = match cmd.program {
            Program::Git => {
                let mut c = Command::new(&self.git);
                c.args(cmd.git_args());
                c
            }
            Program::Snippet(_) => {
                let (script, args) = cmd.script_and_args();
                let mut c = Command::new("/bin/sh");
                c.arg("-c").arg(script).arg("sopen-git").args(args);
                c
            }
        };
        for key in LOCAL_REPO_ENV {
            c.env_remove(key);
        }
        c.envs(self.env.iter().map(|(k, v)| (k, v)));
        c.envs(cmd.env_pairs());
        c
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The command's stdin, stdout and stderr; a session of its own when `own_session` (03 §3.1:
/// no controlling terminal, and its process group is what a stop signals), with the stop's
/// signals at their defaults (an ignored signal would be handed on across exec).
fn prepare(c: &mut Command, feed: bool, own_session: bool) {
    c.stdin(if feed { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if own_session {
        // SAFETY: setsid and signal are async-signal-safe, as `pre_exec` requires.
        unsafe {
            c.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
                    libc::signal(signal, libc::SIG_DFL);
                }
                Ok(())
            });
        }
    }
}

/// A remote's git over sopen's ssh master (03 §3.2): `ssh <exec args> -T -o RemoteCommand=none
/// -o LogLevel=ERROR <dest> -- <words>`.
#[derive(Debug, Clone)]
pub struct SshGit {
    dest: String,
    label: String,
    exec: Vec<OsString>,
}

impl SshGit {
    /// For `dest` (`RemoteSpec::destination`), shown as `label` (`RemoteSpec::label`): reads the
    /// master's arguments once ([`exec::exec_args`], `ssh -G`).
    pub fn new(dest: &str, label: &str, state_dir: &Path) -> anyhow::Result<Self> {
        Ok(Self::with_exec_args(
            dest,
            label,
            exec::exec_args(dest, state_dir)?,
        ))
    }

    /// With the master's arguments already read.
    pub fn with_exec_args(dest: &str, label: &str, exec: Vec<OsString>) -> Self {
        Self {
            dest: dest.to_string(),
            label: label.to_string(),
            exec,
        }
    }

    /// The words the remote's login shell reads (03 §3.2): a bare `exec`, then `env`, 03 §3.3's
    /// environment, `sh -c <locate> sopen-git -C <dir> <options> <args>` (or a snippet), every
    /// word but `exec` quoted by [`quote`].
    pub fn words(cmd: &GitCommand) -> Vec<OsString> {
        let (script, args) = cmd.script_and_args();
        let mut plain: Vec<OsString> = vec!["env".into()];
        plain.extend(
            cmd.env_pairs()
                .into_iter()
                .map(|(k, v)| OsString::from(format!("{k}={v}"))),
        );
        plain.extend(["sh", "-c"].map(OsString::from));
        plain.push(script);
        plain.push("sopen-git".into());
        plain.extend(args);
        std::iter::once(OsString::from("exec"))
            .chain(
                plain
                    .iter()
                    .map(|w| OsString::from_vec(quote(w.as_bytes()))),
            )
            .collect()
    }

    /// `ssh`'s whole argument list for `words`.
    pub fn argv(&self, words: Vec<OsString>) -> Vec<OsString> {
        exec::command_args(&self.exec, &self.dest, words)
    }
}

/// `exec 'kill' '<signal>' '--' '-<pid>'`: the stop's words (03 §3.5).
pub fn kill_words(pid: u32, signal: &str) -> Vec<OsString> {
    let pgid = format!("-{pid}");
    std::iter::once(OsString::from("exec"))
        .chain(
            ["kill", signal, "--", &pgid]
                .iter()
                .map(|w| OsString::from_vec(quote(w.as_bytes()))),
        )
        .collect()
}

/// One stop session for several groups of a host (S2-F review 1): for each `<pid> <identity>`
/// pair, the leader's identity is read again right before the signal ([`ID_FN`]), and the group
/// is signalled only when it is the same process — `kill <sig> -- -<pid>` through `env` (dash's
/// builtin refuses `--`, measured; the program is what `exec 'kill'` runs under every login
/// shell, E5). A line per group: `<pid> ok` (signalled; with `-0`, there and ours), `<pid> gone`
/// (no group), `<pid> leaderless` (the leader is gone, the group is not: not signalled — a number
/// alone proves nothing), `<pid> other` (another process has the pid now: not signalled). The
/// residual: the moment between the reading and the `kill` inside this one shell.
const STOP: &str = concat!(
    id_fn!(),
    r#"sig=$1; shift; while [ $# -ge 2 ]; do p=$1; w=$2; shift 2; sopen_id "$p"; "#,
    r#"if [ -z "$sopen_i" ]; then if env kill -0 -- "-$p" 2>/dev/null; "#,
    r#"then echo "$p leaderless"; else echo "$p gone"; fi; "#,
    r#"elif [ "$sopen_i" != "$w" ]; then echo "$p other"; "#,
    r#"elif env kill "$sig" -- "-$p" 2>/dev/null; then echo "$p ok"; else echo "$p gone"; fi; "#,
    r#"done; exit 0"#,
);

/// `exec 'sh' '-c' '<STOP>' 'sopen-stop' '<sig>' '<pid1>' '<identity1>' …`: the one session of a
/// stop step for a host's groups.
pub fn stop_words(signal: &str, groups: &[(u32, String)]) -> Vec<OsString> {
    let mut plain: Vec<String> = ["sh", "-c", STOP, "sopen-stop", signal]
        .map(String::from)
        .into();
    for (pid, id) in groups {
        plain.push(pid.to_string());
        plain.push(id.clone());
    }
    std::iter::once(OsString::from("exec"))
        .chain(
            plain
                .iter()
                .map(|w| OsString::from_vec(quote(w.as_bytes()))),
        )
        .collect()
}

/// What one stop session says of a group (see [`STOP`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupAnswer {
    /// Signalled (with `-0`: there, and the leader is the one the command started with).
    Ok,
    /// No such group.
    Gone,
    /// The leader is gone and the group is not: not signalled.
    Leaderless,
    /// Another process has the leader's pid now: not signalled.
    Other,
}

/// A stop session's lines, by pid; a pid it said nothing of is not in it.
fn group_answers(stdout: &[u8]) -> Vec<(u32, GroupAnswer)> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|l| {
            let (pid, word) = l.trim().split_once(' ')?;
            let answer = match word {
                "ok" => GroupAnswer::Ok,
                "gone" => GroupAnswer::Gone,
                "leaderless" => GroupAnswer::Leaderless,
                "other" => GroupAnswer::Other,
                _ => return None,
            };
            Some((pid.parse().ok()?, answer))
        })
        .collect()
}

/// `words` joined as ssh joins them: one line for the login shell.
#[cfg(test)]
fn line_of(words: &[OsString]) -> OsString {
    let mut line = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            line.push(b' ');
        }
        line.extend_from_slice(w.as_bytes());
    }
    OsString::from_vec(line)
}

/// Tests' remote (02 §9, 03 §3.2): [`SshGit::words`] joined into one line, as sshd hands it to
/// the login shell, run by a local login shell in a session of its own (sshd starts a session's
/// command as a session leader), so quoting, `<locate>`, the pid line and stdin are exercised as
/// a remote reads them. [`Faults`] play a lost connection and a missing master.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct ShellGit {
    shell: PathBuf,
    label: String,
    env: Vec<(OsString, OsString)>,
    faults: Arc<Faults>,
}

/// `ShellGit`'s fault switch and counter.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct Faults {
    /// The next this many commands run to their end, then lose their output and exit status:
    /// the "connection" exits 255 (03 §11 G-R4).
    pub lose: std::sync::atomic::AtomicU32,
    /// The master is gone: its check fails (the pre-flight check, the stop's).
    pub master_gone: std::sync::atomic::AtomicBool,
    /// Commands dispatched (a stop's `kill` is not one).
    pub dispatched: std::sync::atomic::AtomicU32,
    /// The signals sent to the "remote" group over the master, in order (`-TERM`, `-0`, …):
    /// one entry per stop session.
    pub signals: Mutex<Vec<String>>,
    /// Every stop session's words after `kill`: the signal and the groups (`-<pid>`), in order.
    pub sessions: Mutex<Vec<Vec<String>>>,
    /// Stop sessions open now, and the most ever open at once.
    pub open: std::sync::atomic::AtomicU32,
    pub most_open: std::sync::atomic::AtomicU32,
    /// Run once, right before the next stop session (a test's "another process takes the pid").
    pub before_stop: BeforeStop,
}

/// [`Faults::before_stop`].
#[cfg(test)]
#[derive(Default)]
pub struct BeforeStop(Mutex<Option<Box<dyn FnOnce() + Send>>>);

#[cfg(test)]
impl BeforeStop {
    pub fn set(&self, hook: impl FnOnce() + Send + 'static) {
        *lock(&self.0) = Some(Box::new(hook));
    }

    fn take(&self) -> Option<Box<dyn FnOnce() + Send>> {
        lock(&self.0).take()
    }
}

#[cfg(test)]
impl std::fmt::Debug for BeforeStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BeforeStop")
    }
}

#[cfg(test)]
impl ShellGit {
    pub fn new(shell: impl Into<PathBuf>, label: &str) -> Self {
        Self {
            shell: shell.into(),
            label: label.to_string(),
            env: Vec::new(),
            faults: Arc::default(),
        }
    }

    /// Adds `key=value` to the login shell's environment (the host's session: `PATH`, `HOME`,
    /// a fixture's `GIT_CONFIG_GLOBAL`).
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn faults(&self) -> Arc<Faults> {
        self.faults.clone()
    }

    /// The login shell running `line`, as sshd starts it.
    fn login(&self, line: &OsStr) -> Command {
        let mut c = Command::new(&self.shell);
        c.arg("-c").arg(line);
        self.session_env(&mut c);
        c
    }

    fn session_env(&self, c: &mut Command) {
        for key in LOCAL_REPO_ENV {
            c.env_remove(key);
        }
        c.envs(self.env.iter().map(|(k, v)| (k, v)));
    }

    fn command(&self, cmd: &GitCommand) -> Command {
        self.faults.dispatched.fetch_add(1, Ordering::SeqCst);
        let line = line_of(&SshGit::words(cmd));
        let lose = {
            let mut n = self.faults.lose.load(Ordering::SeqCst);
            loop {
                if n == 0 {
                    break false;
                }
                match self.faults.lose.compare_exchange(
                    n,
                    n - 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break true,
                    Err(now) => n = now,
                }
            }
        };
        if !lose {
            return self.login(&line);
        }
        // The command runs to its end; what it said and how it ended are lost on the way.
        let mut c = Command::new("/bin/sh");
        c.arg("-c")
            .arg(r#""$1" -c "$2" >/dev/null 2>&1; exit 255"#)
            .arg("lose")
            .arg(&self.shell)
            .arg(line);
        self.session_env(&mut c);
        c
    }
}

/// How a [`GitRunner`] reaches its host.
#[derive(Debug, Clone)]
enum Via {
    Local(LocalGit),
    Ssh(SshGit),
    #[cfg(test)]
    Shell(ShellGit),
}

impl Via {
    fn host(&self) -> &str {
        match self {
            Via::Local(_) => "local",
            Via::Ssh(g) => &g.label,
            #[cfg(test)]
            Via::Shell(g) => &g.label,
        }
    }

    fn remote(&self) -> bool {
        !matches!(self, Via::Local(_))
    }

    fn command(&self, cmd: &GitCommand) -> Command {
        match self {
            Via::Local(g) => g.command(cmd),
            Via::Ssh(g) => {
                let mut c = Command::new("ssh");
                c.args(g.argv(SshGit::words(cmd)));
                c
            }
            #[cfg(test)]
            Via::Shell(g) => g.command(cmd),
        }
    }

    /// The local process leads a session of its own: its group may be signalled.
    fn leads_session(&self) -> bool {
        !matches!(self, Via::Ssh(_))
    }

    /// Is the master there? (03 §3.2's pre-flight check; locally always.)
    fn check(&self) -> bool {
        match self {
            Via::Local(_) => true,
            Via::Ssh(g) => {
                let mut c = Command::new("ssh");
                c.args(exec::check_args(&g.exec, &g.dest));
                bounded(c).is_some_and(|code| code == 0)
            }
            #[cfg(test)]
            Via::Shell(g) => !g.faults.master_gone.load(Ordering::SeqCst),
        }
    }

    /// One stop session on the host ([`STOP`]), over the master: what it said of each group, or
    /// `None` when it did not run there (or not to its end). The host's stop lane is held by the
    /// caller: never two at once.
    fn stop_session(
        &self,
        signal: &str,
        groups: &[(u32, String)],
    ) -> Option<Vec<(u32, GroupAnswer)>> {
        let words = stop_words(signal, groups);
        let c = match self {
            Via::Local(_) => return None,
            Via::Ssh(g) => {
                let mut c = Command::new("ssh");
                c.args(g.argv(words));
                c
            }
            #[cfg(test)]
            Via::Shell(g) => {
                lock(&g.faults.signals).push(signal.to_string());
                let session = std::iter::once(signal.to_string())
                    .chain(groups.iter().map(|(p, _)| format!("-{p}")))
                    .collect();
                lock(&g.faults.sessions).push(session);
                let open = g.faults.open.fetch_add(1, Ordering::SeqCst) + 1;
                g.faults.most_open.fetch_max(open, Ordering::SeqCst);
                if let Some(hook) = g.faults.before_stop.take() {
                    hook();
                }
                let out = bounded_output(g.login(&line_of(&words)));
                g.faults.open.fetch_sub(1, Ordering::SeqCst);
                return out
                    .filter(|(code, _)| *code == 0)
                    .map(|(_, stdout)| group_answers(&stdout));
            }
        };
        bounded_output(c)
            .filter(|(code, _)| *code == 0)
            .map(|(_, stdout)| group_answers(&stdout))
    }
}

/// [`bounded`], with what the command printed on stdout (at most 64 KiB kept).
fn bounded_output(mut c: Command) -> Option<(i32, Vec<u8>)> {
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(64 * 1024).read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + CONTROL_LIMIT;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    code.map(|code| (code, out))
}

/// Runs a short control command (no input, output dropped) for at most [`CONTROL_LIMIT`]: its
/// exit code, `None` when it did not start, was killed or took too long.
fn bounded(mut c: Command) -> Option<i32> {
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = c.spawn().ok()?;
    let deadline = Instant::now() + CONTROL_LIMIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// The browser's `Connect` for the host: brings back a missing master (03 §3.2), the one place a
/// prompt may appear (ssh's own, on the terminal).
pub type Revive = Arc<dyn Fn() -> anyhow::Result<()> + Send + Sync>;

/// Runs git on one host (see the module's API notes). Clones share the host's in-flight slots
/// and its command log.
#[derive(Clone)]
pub struct GitRunner {
    inner: Arc<Inner>,
}

struct Inner {
    via: Via,
    limits: Limits,
    log: CommandLog,
    revive: Option<Revive>,
    busy: Mutex<usize>,
    freed: Condvar,
    /// The host's stop lane (S2-F, 02 §6): a remote stop — its `kill` sessions, all its waits —
    /// runs while it holds this, so the host never has two stop sessions open at once (sshd's
    /// `MaxSessions`: git's worst case is 3 commands and 1 stop); a second stop waits behind.
    stop_lane: Mutex<()>,
}

impl GitRunner {
    fn with_via(via: Via) -> Self {
        Self {
            inner: Arc::new(Inner {
                via,
                limits: Limits::default(),
                log: CommandLog::default(),
                revive: None,
                busy: Mutex::new(0),
                freed: Condvar::new(),
                stop_lane: Mutex::new(()),
            }),
        }
    }

    pub fn local(git: LocalGit) -> Self {
        Self::with_via(Via::Local(git))
    }

    pub fn ssh(git: SshGit) -> Self {
        Self::with_via(Via::Ssh(git))
    }

    #[cfg(test)]
    pub fn shell(git: ShellGit) -> Self {
        Self::with_via(Via::Shell(git))
    }

    fn configure(&mut self) -> &mut Inner {
        Arc::get_mut(&mut self.inner).expect("a GitRunner is configured before it is cloned")
    }

    /// Other limits than 03 §3.4's (tests).
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.configure().limits = limits;
        self
    }

    /// The browser's `Connect` for this host (03 §3.2).
    pub fn with_revive(mut self, revive: Revive) -> Self {
        self.configure().revive = Some(revive);
        self
    }

    /// The host as the log names it: `local`, or the remote's label.
    pub fn host(&self) -> &str {
        self.inner.via.host()
    }

    pub fn log(&self) -> &CommandLog {
        &self.inner.log
    }

    /// Commands running now (at most [`IN_FLIGHT`]).
    pub fn in_flight(&self) -> usize {
        *lock(&self.inner.busy)
    }

    /// Runs `cmd` to its end (see the module's API notes). A read whose connection was lost
    /// is run once more, after the master is checked (and revived when gone): 03 §3.2.
    pub fn run(&self, cmd: &GitCommand, handle: &Handle) -> Done {
        let first = match self.start(cmd, handle) {
            Ok(running) => running.finish(),
            Err(done) => return *done,
        };
        if !matches!(first.ending, Ending::ConnectionLost { .. }) || self.master().is_err() {
            return first;
        }
        match self.start(cmd, handle) {
            Ok(running) => running.finish(),
            Err(done) => *done,
        }
    }

    /// The master is there, or has been revived through the browser's `Connect` (03 §3.2).
    fn master(&self) -> Result<(), String> {
        let via = &self.inner.via;
        if via.check() {
            return Ok(());
        }
        let host = self.host();
        let Some(revive) = &self.inner.revive else {
            return Err(format!("the ssh connection to {host} is gone"));
        };
        if let Err(e) = revive() {
            return Err(format!(
                "the ssh connection to {host} is gone and could not be made again: {e:#}"
            ));
        }
        if via.check() {
            Ok(())
        } else {
            Err(format!(
                "the ssh connection to {host} is still gone after reconnecting"
            ))
        }
    }

    /// Starts `cmd`: a [`Running`] to read, or the [`Done`] of a command that was not sent
    /// (boxed: it is large next to a `Running`).
    pub fn start(&self, cmd: &GitCommand, handle: &Handle) -> Result<Running, Box<Done>> {
        let inner = &self.inner;
        let not_sent = |why: String| Err(Box::new(self.not_sent(cmd, Ending::NotSent(why))));
        if handle.is_stopped() {
            return not_sent("stopped before it was sent".into());
        }
        let size = cmd.args_size();
        if size > MAX_ARGS {
            return not_sent(format!(
                "its arguments are {size} bytes; one command carries at most {MAX_ARGS}"
            ));
        }
        let Some(slot) = self.slot(handle) else {
            return not_sent("stopped before it was sent".into());
        };
        // 03 §3.2: a command that may change something is sent only over a master that is
        // there, so a lost connection afterwards is the only way its outcome is unknown.
        if inner.via.remote() && cmd.class != Class::Read {
            if let Err(why) = self.master() {
                return not_sent(why);
            }
        }
        let mut c = inner.via.command(cmd);
        prepare(&mut c, cmd.feed.is_some(), inner.via.leads_session());
        let line = cmd.command_line();
        crate::debug!("git {}: {line} (in {})", self.host(), lossy(&cmd.dir));
        let started = Instant::now();
        let mut child = match c.spawn() {
            Ok(child) => child,
            Err(e) if e.kind() == io::ErrorKind::NotFound && !inner.via.remote() => {
                return Err(Box::new(self.not_sent(cmd, Ending::NoGit)));
            }
            Err(e) => return not_sent(format!("cannot start it: {e}")),
        };
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        if let (Some(feed), Some(mut stdin)) = (cmd.feed.clone(), child.stdin.take()) {
            // Written apart from the reading, so a large feed and a large output never wait on
            // each other; an early exit leaves the rest unwritten (a broken pipe).
            std::thread::spawn(move || {
                let _ = stdin.write_all(&feed.bytes);
            });
        }
        let shared = Arc::new(Shared {
            runner: inner.clone(),
            pid: child.id(),
            child: Mutex::new(ChildState {
                child,
                status: None,
            }),
            stderr: Mutex::new(Stderr::default()),
            stderr_changed: Condvar::new(),
            stderr_cancel: AtomicBool::new(false),
            last_byte: Mutex::new(started),
            started,
            stop: Mutex::new(StopState::default()),
            stop_changed: Condvar::new(),
            class: cmd.class,
            within: cmd.within,
            line,
            repo: lossy(&cmd.dir),
            stdin: cmd.stdin_said(),
            time: SystemTime::now(),
            git_dir: cmd
                .git_dir
                .clone()
                .or_else(|| Some(Path::new(&cmd.dir).join(".git")).filter(|d| d.is_dir())),
        });
        let reader = shared.clone();
        let stderr_thread = std::thread::spawn(move || read_stderr(&reader, stderr));
        handle.attach(&shared);
        Ok(Running {
            reading: Some(Reading {
                shared,
                stdout,
                buf: Vec::new(),
                pos: 0,
                eof: false,
                stdout_bytes: 0,
                max_record: cmd.max_record,
                max_output: cmd.max_output,
                overflowed: false,
                held: false,
                handle: handle.clone(),
                _slot: slot,
                stderr_thread: Some(stderr_thread),
            }),
        })
    }

    /// Waits for a free slot (02 §6); `None` when the handle was stopped meanwhile.
    fn slot(&self, handle: &Handle) -> Option<Slot> {
        let inner = &self.inner;
        let mut busy = lock(&inner.busy);
        while *busy >= IN_FLIGHT {
            if handle.is_stopped() {
                return None;
            }
            busy = inner
                .freed
                .wait_timeout(busy, Duration::from_millis(100))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *busy += 1;
        Some(Slot(inner.clone()))
    }

    /// The [`Done`] of a command that never ran, logged.
    fn not_sent(&self, cmd: &GitCommand, ending: Ending) -> Done {
        let entry = LogEntry {
            time: SystemTime::now(),
            host: self.host().to_string(),
            repo: lossy(&cmd.dir),
            line: cmd.command_line(),
            stdin: cmd.stdin_said(),
            ending: ending.clone(),
            duration: Duration::ZERO,
            stdout_bytes: 0,
            stderr: String::new(),
        };
        self.inner.log.push(entry.clone());
        crate::debug!("{}", entry.debug_line());
        Done {
            ending,
            stdout: Vec::new(),
            stdout_cut: false,
            stderr: String::new(),
            entry,
        }
    }
}

/// One of the host's in-flight slots, given back when dropped.
struct Slot(Arc<Inner>);

impl Drop for Slot {
    fn drop(&mut self) {
        *lock(&self.0.busy) -= 1;
        self.0.freed.notify_all();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn lossy(s: &OsStr) -> String {
    s.to_string_lossy().into_owned()
}

/// What stopped a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopBy {
    /// Esc, a dropped stream, a hidden panel's stream after 30 s.
    Asked,
    /// The automatic limit: no byte for this long (or, with [`GitCommand::within`], this long
    /// in all).
    Limit(Duration),
    /// More stdout than this many bytes: one record past [`GitCommand::max_record`], or all of
    /// it past [`GitCommand::max_output`].
    Overflow(usize),
}

/// A stop that ended the command (03 §3.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub by: StopBy,
    /// `SIGKILL` was needed.
    pub killed: bool,
    /// Locally after a `SIGKILL`: the `*.lock` files in the git directory, relative to it,
    /// listed and never removed.
    pub locks: Vec<String>,
}

/// How a command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// git's exit code.
    Exit(i32),
    /// Killed by a signal sopen did not send (locally).
    Signal(i32),
    /// Stopped (Esc or the automatic limit).
    Stopped(Stopped),
    /// A remote stop that could not be confirmed: the master was gone, or no `sopen-pid` line
    /// came back (03 §3.5).
    StopSent { host: String },
    /// A write or network command whose connection was lost after it was sent: it may or may
    /// not have run; never re-run (03 §3.2).
    OutcomeUnknown { host: String },
    /// A read whose connection was lost; [`GitRunner::run`] runs it once more first.
    ConnectionLost { host: String },
    /// Not sent: stopped before, the master gone and not revived, too many arguments, or the
    /// program could not start.
    NotSent(String),
    /// No git: locally the program is missing; remotely `<locate>` found none.
    NoGit,
}

impl fmt::Display for Ending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ending::Exit(code) => write!(f, "exit {code}"),
            Ending::Signal(signal) => write!(f, "signal {signal}"),
            Ending::Stopped(s) => {
                f.write_str("stopped")?;
                match s.by {
                    StopBy::Asked => {}
                    StopBy::Limit(limit) => {
                        write!(f, ": git did not answer in {} s", limit.as_secs())?
                    }
                    StopBy::Overflow(bytes) => write!(f, ": more than {bytes} bytes of output")?,
                }
                if s.killed {
                    f.write_str(" (SIGKILL)")?;
                }
                if !s.locks.is_empty() {
                    write!(f, "; lock files left: {}", s.locks.join(", "))?;
                }
                Ok(())
            }
            Ending::StopSent { host } => {
                write!(f, "stop sent; the command may still be running on {host}")
            }
            Ending::OutcomeUnknown { host } => write!(
                f,
                "outcome unknown: the connection to {host} was lost after the command was sent"
            ),
            Ending::ConnectionLost { host } => write!(f, "the connection to {host} was lost"),
            Ending::NotSent(why) => write!(f, "not sent: {why}"),
            Ending::NoGit => f.write_str("no git"),
        }
    }
}

/// A finished command.
#[derive(Debug, Clone)]
pub struct Done {
    pub ending: Ending,
    /// All of stdout, as git printed it (not masked); its first bytes only when `stdout_cut`.
    pub stdout: Vec<u8>,
    /// stdout was cut short (`StopBy::Overflow`): show it through [`Done::stdout_text`].
    pub stdout_cut: bool,
    /// stderr as the log keeps it: `\r` updates collapsed, credentials masked, the last
    /// [`STDERR_KEPT`] bytes.
    pub stderr: String,
    /// The log's entry for it.
    pub entry: LogEntry,
}

impl Done {
    /// Exit 0.
    pub fn ok(&self) -> bool {
        self.ending == Ending::Exit(0)
    }

    /// What an error dialog holds (03 §3.8): stderr, or stdout (masked) when stderr is empty,
    /// or the ending when the command said nothing (not sent, stopped).
    pub fn error_text(&self) -> String {
        if !self.stderr.trim().is_empty() {
            return self.stderr.clone();
        }
        let stdout = self.stdout_text();
        if stdout.trim().is_empty() {
            self.ending.to_string()
        } else {
            stdout
        }
    }

    /// stdout as text to show: masked — as text cut short when it was (a URL whose `@` was cut
    /// off loses its authority).
    pub fn stdout_text(&self) -> String {
        let text = String::from_utf8_lossy(&self.stdout);
        if self.stdout_cut {
            mask_cut(&text)
        } else {
            mask_credentials(&text)
        }
    }
}

/// One entry of the command log (03 §3.7).
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub time: SystemTime,
    pub host: String,
    /// The folder it ran in (`-C`).
    pub repo: String,
    /// [`GitCommand::command_line`].
    pub line: String,
    /// What went on stdin: `message, 3 lines`.
    pub stdin: Option<String>,
    pub ending: Ending,
    pub duration: Duration,
    pub stdout_bytes: u64,
    /// The last [`STDERR_KEPT`] bytes of stderr, collapsed and masked.
    pub stderr: String,
}

impl LogEntry {
    /// The status row (03 §3.7): `git push origin main · exit 0 · 1.2 s`; for a failure the
    /// first error line in place of the exit code.
    pub fn status_line(&self) -> String {
        let outcome = match &self.ending {
            Ending::Exit(code) if *code != 0 => self
                .error_line()
                .map(str::to_string)
                .unwrap_or_else(|| self.ending.to_string()),
            ending => ending.to_string(),
        };
        format!(
            "{} · {outcome} · {:.1} s",
            self.line,
            self.duration.as_secs_f64()
        )
    }

    /// The line `SOPEN_LOG` gets for it.
    pub fn debug_line(&self) -> String {
        format!(
            "git {}: {} (in {}) · {} · {:.1} s",
            self.host,
            self.line,
            self.repo,
            self.ending,
            self.duration.as_secs_f64()
        )
    }

    /// stderr's first `fatal:` / `error:` / `sopen:` line, else its last non-empty one.
    fn error_line(&self) -> Option<&str> {
        let mut lines = self.stderr.lines().map(str::trim).filter(|l| !l.is_empty());
        let first = lines.clone().find(|l| {
            ["fatal:", "error:", "sopen:"]
                .iter()
                .any(|p| l.starts_with(p))
        });
        first.or_else(|| lines.next_back())
    }
}

/// The command log (03 §3.7): in memory only, newest first, at most [`LOG_ENTRIES`].
#[derive(Debug, Clone, Default)]
pub struct CommandLog(Arc<Mutex<VecDeque<LogEntry>>>);

impl CommandLog {
    /// Newest first.
    pub fn entries(&self) -> Vec<LogEntry> {
        lock(&self.0).iter().cloned().collect()
    }

    pub fn last(&self) -> Option<LogEntry> {
        lock(&self.0).front().cloned()
    }

    fn push(&self, entry: LogEntry) {
        let mut entries = lock(&self.0);
        entries.push_front(entry);
        entries.truncate(LOG_ENTRIES);
    }
}

/// Stops and watches the commands it is given (see the module's API notes). Cheap to clone; the
/// UI thread keeps one.
#[derive(Clone, Default)]
pub struct Handle(Arc<Mutex<HandleState>>);

#[derive(Default)]
struct HandleState {
    stopped: bool,
    /// Every command given this handle that has not finished, oldest first.
    attached: Vec<Weak<Shared>>,
}

impl Handle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stops what runs now and keeps what comes next from being sent. Never blocks.
    pub fn stop(&self) {
        let attached: Vec<Arc<Shared>> = {
            let mut state = lock(&self.0);
            state.stopped = true;
            state.attached.iter().filter_map(Weak::upgrade).collect()
        };
        // Together: a host's commands are stopped in one stop session (S2-F).
        Shared::request_stops(&attached, StopBy::Asked);
    }

    pub fn is_stopped(&self) -> bool {
        lock(&self.0).stopped
    }

    fn attach(&self, shared: &Arc<Shared>) {
        let stopped = {
            let mut state = lock(&self.0);
            state.attached.retain(|w| w.strong_count() > 0);
            state.attached.push(Arc::downgrade(shared));
            state.stopped
        };
        if stopped {
            shared.request_stop(StopBy::Asked);
        }
    }

    fn detach(&self, shared: &Arc<Shared>) {
        lock(&self.0)
            .attached
            .retain(|w| w.strong_count() > 0 && !std::ptr::eq(w.as_ptr(), Arc::as_ptr(shared)));
    }

    /// The newest command given this handle that still runs.
    fn running(&self) -> Option<Arc<Shared>> {
        lock(&self.0).attached.iter().rev().find_map(Weak::upgrade)
    }

    /// The command running now: its line.
    pub fn command_line(&self) -> Option<String> {
        self.running().map(|s| s.line.clone())
    }

    /// The newest stderr line of what runs now (03 §3.6), masked.
    pub fn progress(&self) -> Option<String> {
        let shared = self.running()?;
        let newest = lock(&shared.stderr).newest.clone()?;
        Some(newest.trim_end().to_string())
    }

    /// How long what runs now has been running.
    pub fn elapsed(&self) -> Option<Duration> {
        self.running().map(|s| s.started.elapsed())
    }

    /// How long what runs now has been silent on stdout and stderr.
    pub fn silent_for(&self) -> Option<Duration> {
        self.running().map(|s| lock(&s.last_byte).elapsed())
    }
}

/// What a running command's threads share.
struct Shared {
    runner: Arc<Inner>,
    /// The local process: git, the login shell (`ShellGit`) or `ssh`.
    pid: u32,
    child: Mutex<ChildState>,
    stderr: Mutex<Stderr>,
    stderr_changed: Condvar,
    /// Tells the stderr reader to let go of the pipe (something outside the command's group
    /// holds it open for good).
    stderr_cancel: AtomicBool,
    /// The last byte on stdout or stderr.
    last_byte: Mutex<Instant>,
    started: Instant,
    stop: Mutex<StopState>,
    stop_changed: Condvar,
    class: Class,
    within: Option<Duration>,
    line: String,
    repo: String,
    stdin: Option<String>,
    time: SystemTime,
    git_dir: Option<PathBuf>,
}

struct ChildState {
    child: Child,
    status: Option<ExitStatus>,
}

#[derive(Default)]
struct StopState {
    by: Option<StopBy>,
    outcome: Option<StopOutcome>,
}

/// What a stop came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopOutcome {
    /// The command is gone; `killed`: `SIGKILL` was needed.
    Stopped { killed: bool },
    /// Sent, not confirmed: it may still run on the host.
    Sent,
    /// It had ended before anything was sent: its own ending stands.
    Ended,
}

impl Shared {
    /// The local process's exit status, once it has exited (reaped here).
    fn exit(&self) -> Option<ExitStatus> {
        let mut state = lock(&self.child);
        if state.status.is_none() {
            if let Ok(Some(status)) = state.child.try_wait() {
                state.status = Some(status);
            }
        }
        state.status
    }

    fn touch(&self) {
        *lock(&self.last_byte) = Instant::now();
    }

    /// Starts the stop, once, on a thread of its own.
    fn request_stop(self: &Arc<Self>, by: StopBy) {
        Self::request_stops(std::slice::from_ref(self), by);
    }

    /// Starts the stop of each of `all` not stopping yet, on threads of their own: locally each
    /// its own (signals are no sessions); remotely one stop for all of a host's, in the host's
    /// stop lane ([`Shared::stop_remote_all`]).
    fn request_stops(all: &[Arc<Self>], by: StopBy) {
        let fresh: Vec<Arc<Self>> = all
            .iter()
            .filter(|s| {
                let mut stop = lock(&s.stop);
                let first = stop.by.is_none();
                if first {
                    stop.by = Some(by);
                }
                first
            })
            .cloned()
            .collect();
        let mut hosts: Vec<Vec<Arc<Self>>> = Vec::new();
        for shared in fresh {
            if !shared.runner.via.remote() {
                std::thread::spawn(move || {
                    let outcome = shared.stop_local();
                    shared.stopped_with(outcome);
                });
                continue;
            }
            match hosts
                .iter_mut()
                .find(|h| Arc::ptr_eq(&h[0].runner, &shared.runner))
            {
                Some(host) => host.push(shared),
                None => hosts.push(vec![shared]),
            }
        }
        for group in hosts {
            std::thread::spawn(move || {
                let outcomes = {
                    let _lane = lock(&group[0].runner.stop_lane);
                    Shared::stop_remote_all(&group)
                };
                for (shared, outcome) in group.iter().zip(outcomes) {
                    shared.stopped_with(outcome);
                }
            });
        }
    }

    /// The stop came to `outcome`: said to whoever waits for it.
    fn stopped_with(&self, outcome: StopOutcome) {
        lock(&self.stop).outcome = Some(outcome);
        self.stop_changed.notify_all();
    }

    fn stop_requested(&self) -> Option<StopBy> {
        lock(&self.stop).by
    }

    fn stop_outcome(&self) -> Option<StopOutcome> {
        lock(&self.stop).outcome
    }

    fn wait_stop_outcome(&self) -> StopOutcome {
        let mut stop = lock(&self.stop);
        loop {
            if let Some(outcome) = stop.outcome {
                return outcome;
            }
            stop = self
                .stop_changed
                .wait(stop)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    /// 03 §3.5, local: `SIGTERM` to the command's process group (it leads a session of its
    /// own); after the grace, `SIGKILL` to the group.
    fn stop_local(&self) -> StopOutcome {
        if self.exit().is_some() && !self.group_alive() {
            return StopOutcome::Ended;
        }
        self.signal_group(libc::SIGTERM);
        if self.wait_group_gone(self.runner.limits.stop_grace) {
            return StopOutcome::Stopped { killed: false };
        }
        let killed = self.signal_group(libc::SIGKILL);
        self.wait_group_gone(Duration::from_secs(2));
        StopOutcome::Stopped { killed }
    }

    /// 03 §3.5, remote, over the same master, for one host's commands stopped together (S2-F:
    /// one stop session at a time per host, run in its stop lane), each by the pid and identity
    /// of its `sopen-pid` line. Nothing is ever signalled by its number alone (review 1): every
    /// session reads each leader's identity again right before its signal ([`STOP`]), and a group
    /// whose leader is gone, or whose pid another process has now, is not signalled. So: a
    /// command whose channel has already ended (its leader exited) is not signalled at all — it
    /// ended, or a leftover of it may still run; the others get one `-TERM` session; then the
    /// grace: the channels' end, then the groups', asked every half second (`-0`, the same
    /// check); one `-KILL` session for those still there and still theirs; then the channels are
    /// closed. A leader gone with its group still there (a child that ignores `SIGTERM`) is
    /// "may still be running". Without the line, or with the master gone, a command may still be
    /// running. Its output is read and dropped meanwhile by its own reader (`Running`'s finish or
    /// drop): a channel cannot end while the master holds output nobody reads (E5).
    fn stop_remote_all(group: &[Arc<Self>]) -> Vec<StopOutcome> {
        let via = &group[0].runner.via;
        let limits = group[0].runner.limits;
        let mut outcome: Vec<Option<StopOutcome>> = vec![None; group.len()];
        let ids: Vec<Option<(u32, String)>> = group.iter().map(|s| s.wait_remote_pid()).collect();
        for (i, s) in group.iter().enumerate() {
            if ids[i].is_none() {
                outcome[i] = Some(if s.exit().is_some() {
                    StopOutcome::Ended
                } else {
                    s.close_channel();
                    StopOutcome::Sent
                });
            }
        }
        let open: Vec<usize> = (0..group.len()).filter(|&i| outcome[i].is_none()).collect();
        if !open.is_empty() && !via.check() {
            for &i in &open {
                group[i].close_channel();
                outcome[i] = Some(StopOutcome::Sent);
            }
        }
        let id = |i: usize| ids[i].clone().unwrap_or((0, String::new()));
        // A channel already ended: its leader exited, so nothing is signalled. Asked only (`-0`,
        // no signal) whether something of its group is left, to say so.
        let open: Vec<usize> = (0..group.len()).filter(|&i| outcome[i].is_none()).collect();
        let (ended, todo): (Vec<usize>, Vec<usize>) =
            open.into_iter().partition(|&i| group[i].exit().is_some());
        for (i, answer) in Self::session(via, "-0", &ended, &id) {
            outcome[i] = Some(match answer {
                Some(GroupAnswer::Gone | GroupAnswer::Other) => StopOutcome::Ended,
                Some(GroupAnswer::Ok | GroupAnswer::Leaderless) | None => StopOutcome::Sent,
            });
        }
        // One `-TERM` for all, each checked.
        let mut alive = Vec::new();
        for (i, answer) in Self::session(via, "-TERM", &todo, &id) {
            match answer {
                Some(GroupAnswer::Ok) => alive.push(i),
                Some(GroupAnswer::Gone | GroupAnswer::Other) => {
                    outcome[i] = Some(StopOutcome::Stopped { killed: false })
                }
                Some(GroupAnswer::Leaderless) | None => {
                    group[i].close_channel();
                    outcome[i] = Some(StopOutcome::Sent);
                }
            }
        }
        let deadline = Instant::now() + limits.stop_grace;
        for &i in &alive {
            group[i].wait_exit(deadline.saturating_duration_since(Instant::now()));
        }
        // Asked until each is gone or the grace is over.
        loop {
            let mut still = Vec::new();
            for (i, answer) in Self::session(via, "-0", &alive, &id) {
                match answer {
                    Some(GroupAnswer::Ok) => still.push(i),
                    Some(GroupAnswer::Gone | GroupAnswer::Other) => {
                        outcome[i] = Some(StopOutcome::Stopped { killed: false })
                    }
                    Some(GroupAnswer::Leaderless) | None => outcome[i] = Some(StopOutcome::Sent),
                }
            }
            alive = still;
            let left = deadline.saturating_duration_since(Instant::now());
            if alive.is_empty() || left.is_zero() {
                break;
            }
            std::thread::sleep(REMOTE_PROBE.min(left));
        }
        // `-KILL` for those still there and still theirs.
        for (i, answer) in Self::session(via, "-KILL", &alive, &id) {
            outcome[i] = Some(match answer {
                Some(GroupAnswer::Ok) => StopOutcome::Stopped { killed: true },
                Some(GroupAnswer::Gone | GroupAnswer::Other) => {
                    StopOutcome::Stopped { killed: false }
                }
                Some(GroupAnswer::Leaderless) | None => StopOutcome::Sent,
            });
        }
        for &i in &todo {
            group[i].close_channel();
        }
        outcome
            .into_iter()
            .map(|o| o.unwrap_or(StopOutcome::Stopped { killed: false }))
            .collect()
    }

    /// One stop session ([`STOP`]) for `which` of a host's commands: each one's answer, `None`
    /// where the session did not run or said nothing of it. Nothing runs for none.
    fn session(
        via: &Via,
        signal: &str,
        which: &[usize],
        id: &dyn Fn(usize) -> (u32, String),
    ) -> Vec<(usize, Option<GroupAnswer>)> {
        if which.is_empty() {
            return Vec::new();
        }
        let groups: Vec<(u32, String)> = which.iter().map(|&i| id(i)).collect();
        let answers = via.stop_session(signal, &groups);
        which
            .iter()
            .zip(&groups)
            .map(|(&i, (pid, _))| {
                let answer = answers
                    .as_ref()
                    .and_then(|a| a.iter().find(|(p, _)| p == pid).map(|(_, a)| *a));
                (i, answer)
            })
            .collect()
    }

    /// The `sopen-pid` line's pid and identity, waited for up to [`PID_WAIT`]; `None` without it or once
    /// the channel has ended.
    fn wait_remote_pid(&self) -> Option<(u32, String)> {
        let deadline = Instant::now() + PID_WAIT;
        let mut err = lock(&self.stderr);
        loop {
            if err.remote_pid.is_some() || err.eof {
                return err.remote_pid.map(|pid| (pid, err.remote_id.clone()));
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            err = self
                .stderr_changed
                .wait_timeout(err, (deadline - now).min(TICK))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Signals the group the local process leads, when it is found alive just before (never
    /// anything else: only a process this runner made a session leader, and never the pid 0 or
    /// 1 groups).
    /// Whether it was signalled.
    fn signal_group(&self, signal: libc::c_int) -> bool {
        // SAFETY: plain kill(2) on the group of a process this runner started.
        self.group_alive() && unsafe { libc::kill(-(self.pid as libc::pid_t), signal) } == 0
    }

    fn group_alive(&self) -> bool {
        self.runner.via.leads_session()
            && self.pid >= 2
            // SAFETY: signal 0 only checks that the group has a process.
            && unsafe { libc::kill(-(self.pid as libc::pid_t), 0) } == 0
    }

    /// Waits up to `limit` for the leader to exit and its group to empty.
    fn wait_group_gone(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if self.exit().is_some() && !self.group_alive() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits up to `limit` for the local process (the channel) to end.
    fn wait_exit(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if self.exit().is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits up to `limit` for the stderr reader to reach its end: has it?
    fn stderr_ended(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut err = lock(&self.stderr);
        while !err.eof {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            err = self
                .stderr_changed
                .wait_timeout(err, (deadline - now).min(TICK))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }

    /// Kills what is left of a finished command's process group (03 §3.5's group): locally the
    /// group it led, remotely the `sopen-pid` group over the master.
    fn end_leftovers(&self) {
        let via = &self.runner.via;
        if !via.remote() {
            self.signal_group(libc::SIGKILL);
            return;
        }
        // Remotely the command's leader has ended: its group is never signalled by its number
        // alone (03 §3.5, review 1) — a leftover there may still run. The reader lets go of the
        // pipe instead (`finish`).
        crate::debug!(
            "git {}: {}: a leftover may still hold its output; not signalled (its leader ended)",
            via.host(),
            self.line
        );
    }

    /// Closes the command's channel: the local end (`ssh`, or `ShellGit`'s login shell) is
    /// killed and reaped; what runs on the host is not touched by it.
    fn close_channel(&self) {
        let mut state = lock(&self.child);
        if state.status.is_none() {
            let _ = state.child.kill();
            if let Ok(status) = state.child.wait() {
                state.status = Some(status);
            }
        }
    }

    /// The `*.lock` files under the git directory (not under `objects/`), relative to it.
    fn locks(&self) -> Vec<String> {
        let Some(root) = &self.git_dir else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut dirs = vec![root.clone()];
        let mut seen = 0;
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                seen += 1;
                if seen > 20_000 {
                    return found;
                }
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() && !(dir == *root && entry.file_name() == "objects") {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "lock") {
                    let rel = path.strip_prefix(root).unwrap_or(&path);
                    found.push(rel.to_string_lossy().into_owned());
                }
            }
        }
        found.sort();
        found
    }
}

/// stderr as it comes (03 §3.6): split at `\r` and `\n`, a `\r` update collapsed into its last
/// value, the first `sopen-pid` line taken out. Every line and update is masked as a whole,
/// before anything of it is kept, shown or cut (a line longer than [`STDERR_KEPT`] is kept to
/// its first bytes, and masked as text cut short).
#[derive(Default)]
struct Stderr {
    /// Committed lines, masked, each ending in `\n`: the last [`STDERR_KEPT`] bytes (with some
    /// slack).
    kept: Vec<u8>,
    /// The line being written, raw, and whether bytes of it were dropped.
    current: Vec<u8>,
    current_cut: bool,
    /// The last value a `\r` closed (raw, and whether it was cut), for the `\n` that ends its
    /// line.
    shown: Option<(Vec<u8>, bool)>,
    /// The newest non-empty line or update, masked: the status row's progress.
    newest: Option<String>,
    remote_pid: Option<u32>,
    /// The `sopen-pid` line's identity of that process (its start time; empty when the host
    /// could not say): what a stop compares before it signals (03 §3.5).
    remote_id: String,
    eof: bool,
}

impl Stderr {
    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match b {
                b'\r' => {
                    if !self.current.is_empty() {
                        let update = std::mem::take(&mut self.current);
                        let cut = std::mem::take(&mut self.current_cut);
                        self.newest = Some(masked_line(&update, cut));
                        self.shown = Some((update, cut));
                    }
                }
                b'\n' => {
                    let (line, cut) = if self.current.is_empty() {
                        self.shown.take().unwrap_or_default()
                    } else {
                        (
                            std::mem::take(&mut self.current),
                            std::mem::take(&mut self.current_cut),
                        )
                    };
                    self.shown = None;
                    self.commit(&line, cut);
                }
                // A line without an end is kept to its first bytes.
                _ if self.current.len() < STDERR_KEPT => self.current.push(b),
                _ => self.current_cut = true,
            }
        }
    }

    fn commit(&mut self, line: &[u8], cut: bool) {
        if self.remote_pid.is_none() && !cut {
            if let Some((pid, id)) = pid_line(line) {
                self.remote_pid = Some(pid);
                self.remote_id = id;
                return;
            }
        }
        let masked = masked_line(line, cut);
        if !masked.is_empty() {
            self.newest = Some(masked.clone());
        }
        self.kept.extend_from_slice(masked.as_bytes());
        self.kept.push(b'\n');
        if self.kept.len() > 2 * STDERR_KEPT {
            let cut = self.kept.len() - STDERR_KEPT;
            self.kept.drain(..cut);
        }
    }

    fn end(&mut self) {
        if !self.current.is_empty() || self.shown.is_some() {
            self.feed(b"\n");
        }
        self.eof = true;
    }

    /// The kept text (masked line by line as it came): its last [`STDERR_KEPT`] bytes from a
    /// line's start.
    fn text(&self) -> String {
        let mut kept = &self.kept[..];
        if kept.len() > STDERR_KEPT {
            kept = &kept[kept.len() - STDERR_KEPT..];
            if let Some(nl) = kept.iter().position(|&b| b == b'\n') {
                kept = &kept[nl + 1..];
            }
        }
        String::from_utf8_lossy(kept).into_owned()
    }
}

/// One line of stderr as it may be kept or shown: masked, as text cut short when it was.
fn masked_line(line: &[u8], cut: bool) -> String {
    let text = String::from_utf8_lossy(line);
    if cut {
        mask_cut(&text)
    } else {
        mask_credentials(&text)
    }
}

/// `sopen-pid <pid> <identity>`: the pid, when it can name a process group of its own (2 or
/// more; `-0` and `-1` would signal far more than the command), and the rest of the line as the
/// process's identity (03 §3.5).
fn pid_line(line: &[u8]) -> Option<(u32, String)> {
    let rest = line.strip_prefix(b"sopen-pid ")?;
    let (digits, id) = match rest.iter().position(|&b| b == b' ') {
        Some(at) => (&rest[..at], &rest[at + 1..]),
        None => (rest, &b""[..]),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let pid = std::str::from_utf8(digits).ok()?.parse::<u32>().ok()?;
    if pid < 2 || pid > i32::MAX as u32 {
        return None;
    }
    Some((pid, String::from_utf8_lossy(id).into_owned()))
}

/// Reads stderr to its end, or until told to let go: the pipe is non-blocking and waited on
/// with `poll` a tick at a time, so the reader is never stuck in a `read` a process outside the
/// command's group keeps open.
fn read_stderr(shared: &Shared, mut stderr: std::process::ChildStderr) {
    let fd = stderr.as_raw_fd();
    // SAFETY: fcntl on the read end this thread owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let mut buf = [0u8; 8192];
    while !shared.stderr_cancel.load(Ordering::SeqCst) {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut pfd, 1, TICK.as_millis() as i32) };
        if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            break;
        }
        if ready <= 0 {
            continue;
        }
        match stderr.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                shared.touch();
                lock(&shared.stderr).feed(&buf[..n]);
                shared.stderr_changed.notify_all();
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => break,
        }
    }
    lock(&shared.stderr).end();
    shared.stderr_changed.notify_all();
}

/// A started command (see the module's API notes).
pub struct Running {
    reading: Option<Reading>,
}

struct Reading {
    shared: Arc<Shared>,
    stdout: ChildStdout,
    /// Bytes read and not yet returned start at `pos`.
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    stdout_bytes: u64,
    /// [`GitCommand::max_record`] and [`GitCommand::max_output`].
    max_record: usize,
    max_output: usize,
    /// Stopped for too much output: nothing more is returned or collected, and what was
    /// collected is cut short.
    overflowed: bool,
    /// stdout was given up a moment after the process exited, still open: something the
    /// command left behind holds it.
    held: bool,
    handle: Handle,
    _slot: Slot,
    stderr_thread: Option<std::thread::JoinHandle<()>>,
}

impl Running {
    /// The next record of stdout up to `delim` (not included), the last one even without it;
    /// `None` once stdout has ended (or the command was stopped). Waiting here is what the
    /// read and network limits count.
    pub fn read_record(&mut self, delim: u8) -> Option<Vec<u8>> {
        self.reading.as_mut()?.record(delim)
    }

    /// stdout's next bytes into `buf`, as `std::io::Read::read` gives them (S2-F: the log
    /// stream's pipe, `history::Pipe`): `Ok(0)` at its end. Waiting here is what the read and
    /// network limits count, as in [`Running::read_record`]: once a limit, a stop (Esc, a
    /// superseded load) or too much output has ended the command, the end is an error that says
    /// so, never a quiet end of the output.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.reading.as_mut() {
            Some(reading) => reading.read(buf),
            None => Ok(0),
        }
    }

    /// The process has not exited yet (a parked stream: git blocked on the full pipe).
    pub fn running(&self) -> bool {
        self.reading
            .as_ref()
            .is_some_and(|r| r.shared.exit().is_none())
    }

    /// Reads the rest of stdout, waits for the end and logs it.
    pub fn finish(mut self) -> Done {
        let reading = self.reading.take().expect("not finished yet");
        reading.finish()
    }

    /// The local process's id: git's (it leads its process group), the login shell's
    /// (`ShellGit`) or `ssh`'s.
    pub fn pid(&self) -> u32 {
        self.reading.as_ref().map_or(0, |r| r.shared.pid)
    }

    /// Tests: has the stderr reader seen its end (and so let go of the pipe)?
    #[cfg(test)]
    pub fn stderr_probe(&self) -> impl Fn() -> bool + Send + 'static {
        let shared = self.reading.as_ref().map(|r| r.shared.clone());
        move || shared.as_ref().is_some_and(|s| lock(&s.stderr).eof)
    }

    /// The pid the `sopen-pid` line gave (remote commands), once it came.
    pub fn remote_pid(&self) -> Option<u32> {
        let r = self.reading.as_ref()?;
        let pid = lock(&r.shared.stderr).remote_pid;
        pid
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(reading) = self.reading.take() {
            // A stream dropped (a panel gone, or hidden for 30 s): stopped, and logged when the
            // stop has ended it.
            reading.shared.request_stop(StopBy::Asked);
            std::thread::spawn(move || {
                reading.finish();
            });
        }
    }
}

impl Reading {
    fn record(&mut self, delim: u8) -> Option<Vec<u8>> {
        let wait_start = Instant::now();
        loop {
            if self.overflowed {
                return None;
            }
            if let Some(i) = self.buf[self.pos..].iter().position(|&b| b == delim) {
                if i > self.max_record {
                    self.overflow(self.max_record);
                    return None;
                }
                let record = self.buf[self.pos..self.pos + i].to_vec();
                self.pos += i + 1;
                if self.pos == self.buf.len() {
                    self.buf.clear();
                    self.pos = 0;
                }
                return Some(record);
            }
            if self.eof {
                if self.buf.len() - self.pos > self.max_record {
                    self.overflow(self.max_record);
                    return None;
                }
                if self.pos < self.buf.len() {
                    let record = self.buf[self.pos..].to_vec();
                    self.buf.clear();
                    self.pos = 0;
                    return Some(record);
                }
                return None;
            }
            if self.pos > 0 {
                self.buf.drain(..self.pos);
                self.pos = 0;
            }
            if self.buf.len() > self.max_record {
                self.overflow(self.max_record);
                return None;
            }
            self.fill(wait_start);
        }
    }

    /// [`Running::read`].
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let wait_start = Instant::now();
        loop {
            if self.pos < self.buf.len() && !self.overflowed {
                let n = out.len().min(self.buf.len() - self.pos);
                out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
                self.pos += n;
                if self.pos == self.buf.len() {
                    self.buf.clear();
                    self.pos = 0;
                }
                return Ok(n);
            }
            if self.eof || self.overflowed {
                return match self.shared.stop_requested() {
                    None => Ok(0),
                    Some(StopBy::Limit(limit)) => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("git did not answer in {} s", limit.as_secs()),
                    )),
                    Some(StopBy::Asked) => Err(io::Error::other("stopped")),
                    Some(StopBy::Overflow(bytes)) => Err(io::Error::other(format!(
                        "stopped: more than {bytes} bytes of output"
                    ))),
                };
            }
            if out.is_empty() {
                return Ok(0);
            }
            self.fill(wait_start);
        }
    }

    /// Moves what was read and not returned into `stdout`, up to [`GitCommand::max_output`];
    /// past it, the command is stopped and the rest dropped.
    fn collect(&mut self, stdout: &mut Vec<u8>) {
        let more = &self.buf[self.pos.min(self.buf.len())..];
        let room = self.max_output.saturating_sub(stdout.len());
        if !self.overflowed {
            stdout.extend_from_slice(&more[..more.len().min(room)]);
            if more.len() > room {
                self.overflow(self.max_output);
            }
        }
        self.buf.clear();
        self.pos = 0;
    }

    /// Too much output: the command is stopped, and what was read of it is dropped.
    fn overflow(&mut self, limit: usize) {
        self.overflowed = true;
        self.buf = Vec::new();
        self.pos = 0;
        self.shared.request_stop(StopBy::Overflow(limit));
    }

    /// The limit that has passed while sopen waited since `wait_start`, if one has.
    fn limit_passed(&self, wait_start: Instant) -> Option<StopBy> {
        let limits = self.shared.runner.limits;
        if let Some(within) = self.shared.within {
            if self.shared.started.elapsed() >= within {
                return Some(StopBy::Limit(within));
            }
        }
        let idle = match self.shared.class {
            Class::Read => limits.read_idle,
            Class::Network => limits.network_idle,
            Class::Write => return None,
        };
        let last = (*lock(&self.shared.last_byte)).max(wait_start);
        (last.elapsed() >= idle).then_some(StopBy::Limit(idle))
    }

    /// Reads what stdout has next into `buf`, waiting for it; sets `eof` at its end, once a stop
    /// has ended the command (or given up on it), or a moment after the process has exited
    /// (`held`: something left behind keeps it open).
    fn fill(&mut self, wait_start: Instant) {
        let fd = self.stdout.as_raw_fd();
        let mut chunk = vec![0u8; 64 * 1024];
        let mut exited_at: Option<Instant> = None;
        loop {
            if let Some(by) = self.limit_passed(wait_start) {
                self.shared.request_stop(by);
            }
            // Stopped, or a stop that may not have arrived: the output after it is not wanted,
            // and whatever still writes may never stop.
            if matches!(
                self.shared.stop_outcome(),
                Some(StopOutcome::Stopped { .. } | StopOutcome::Sent)
            ) {
                self.eof = true;
                return;
            }
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd for the duration of the call.
            let ready = unsafe { libc::poll(&mut pfd, 1, TICK.as_millis() as i32) };
            if ready > 0 {
                match self.stdout.read(&mut chunk) {
                    Ok(0) => {
                        self.eof = true;
                        return;
                    }
                    Ok(n) => {
                        self.shared.touch();
                        self.stdout_bytes += n as u64;
                        self.buf.extend_from_slice(&chunk[..n]);
                        return;
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        self.eof = true;
                        return;
                    }
                }
            }
            if self.shared.exit().is_some() {
                let at = *exited_at.get_or_insert_with(Instant::now);
                if at.elapsed() >= AFTER_EXIT {
                    self.eof = true;
                    self.held = true;
                    return;
                }
            }
        }
    }

    fn finish(mut self) -> Done {
        let wait_start = Instant::now();
        // What a stream left unread counts against the bound like the rest.
        let mut stdout = Vec::new();
        self.collect(&mut stdout);
        while !self.eof {
            self.fill(wait_start);
            self.collect(&mut stdout);
        }
        let mut pause = Duration::from_millis(1);
        let status = loop {
            if let Some(status) = self.shared.exit() {
                break status;
            }
            if let Some(by) = self.limit_passed(wait_start) {
                self.shared.request_stop(by);
            }
            std::thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_millis(20));
        };
        let stop = self
            .shared
            .stop_requested()
            .map(|by| (by, self.shared.wait_stop_outcome()));
        // What stderr still has, for a moment: a process left behind can hold it open.
        {
            let deadline = Instant::now() + AFTER_EXIT;
            let mut err = lock(&self.shared.stderr);
            while !err.eof && Instant::now() < deadline {
                err = self
                    .shared
                    .stderr_changed
                    .wait_timeout(err, TICK)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
        // Something the command left behind still holds its stdout or stderr (a hook's
        // background job): it goes with the command. What its group does not hold (a process in
        // a session of its own) the reader lets go of. No reader outlives `finish`.
        if self.held || !self.shared.stderr_ended(Duration::ZERO) {
            self.shared.end_leftovers();
            if !self.shared.stderr_ended(LEFTOVER_WAIT) {
                crate::debug!(
                    "git {}: {}: stderr still held after the command ended; let go",
                    self.shared.runner.via.host(),
                    self.shared.line
                );
                self.shared.stderr_cancel.store(true, Ordering::SeqCst);
                self.shared.stderr_ended(LEFTOVER_WAIT);
            }
        }
        if let Some(t) = self.stderr_thread.take() {
            // It has ended (a cancelled reader stops within a tick): joined, its pipe closed.
            if self.shared.stderr_ended(Duration::ZERO) {
                let _ = t.join();
            }
        }
        self.handle.detach(&self.shared);
        let shared = &self.shared;
        let stderr = lock(&shared.stderr).text();
        let ending = self.ending(status, stop, &stderr);
        let entry = LogEntry {
            time: shared.time,
            host: shared.runner.via.host().to_string(),
            repo: shared.repo.clone(),
            line: shared.line.clone(),
            stdin: shared.stdin.clone(),
            ending: ending.clone(),
            duration: shared.started.elapsed(),
            stdout_bytes: self.stdout_bytes,
            stderr: stderr.clone(),
        };
        shared.runner.log.push(entry.clone());
        crate::debug!("{}", entry.debug_line());
        Done {
            ending,
            stdout,
            stdout_cut: self.overflowed,
            stderr,
            entry,
        }
    }

    fn ending(
        &self,
        status: ExitStatus,
        stop: Option<(StopBy, StopOutcome)>,
        stderr: &str,
    ) -> Ending {
        let shared = &self.shared;
        let via = &shared.runner.via;
        let host = via.host().to_string();
        match stop {
            Some((by, StopOutcome::Stopped { killed })) => {
                let locks = if killed && !via.remote() {
                    shared.locks()
                } else {
                    Vec::new()
                };
                return Ending::Stopped(Stopped { by, killed, locks });
            }
            Some((_, StopOutcome::Sent)) => return Ending::StopSent { host },
            // Too much output from a command that ended on its own meanwhile: still the
            // overflow, since stdout was not all read.
            Some((by @ StopBy::Overflow(_), StopOutcome::Ended)) => {
                return Ending::Stopped(Stopped {
                    by,
                    killed: false,
                    locks: Vec::new(),
                })
            }
            Some((_, StopOutcome::Ended)) | None => {}
        }
        if via.remote() && status.code() == Some(127) && stderr.lines().any(|l| l == NO_GIT) {
            return Ending::NoGit;
        }
        // ssh's own failure (255) or no exit status: the connection, not git, ended it (03
        // §3.2). `ShellGit` reports a login shell killed under it the same way.
        if via.remote() && matches!(status.code(), Some(255) | None) {
            return match shared.class {
                Class::Read => Ending::ConnectionLost { host },
                Class::Network | Class::Write => Ending::OutcomeUnknown { host },
            };
        }
        match (status.code(), status.signal()) {
            (Some(code), _) => Ending::Exit(code),
            (None, Some(signal)) => Ending::Signal(signal),
            (None, None) => Ending::Exit(-1),
        }
    }
}

/// The host's stop lane (S2-F; 02 §6: git's worst case on a host is 3 commands and 1 stop
/// session): a handle's commands stopped together share one stop session at a time; two
/// stops never have two sessions open at once; a group that ignores `SIGTERM` still gets the
/// `SIGKILL`, and only it.
#[cfg(test)]
mod stop_lane_tests {
    use super::*;
    use crate::git::history::fixture::{self, Repo};

    /// A `ShellGit` runner on the fixture with a 1 s grace, and its fault switch (which records
    /// every stop session).
    fn shell(repo: &Repo) -> (GitRunner, Arc<Faults>) {
        let sh = fixture::env(&repo.home())
            .into_iter()
            .fold(ShellGit::new("/bin/sh", "box"), |g, (k, v)| {
                g.with_env(k, v)
            })
            .with_env("PATH", std::env::var_os("PATH").unwrap_or_default());
        let faults = sh.faults();
        let limits = Limits {
            stop_grace: Duration::from_secs(1),
            ..Limits::default()
        };
        (GitRunner::shell(sh).with_limits(limits), faults)
    }

    /// A git alias that runs `script` (no file written for it).
    fn alias(repo: &Repo, script: &str) -> GitCommand {
        let set = format!("alias.hang=!{script}");
        GitCommand::new(Class::Read, repo.top(), ["-c", set.as_str(), "hang"])
    }

    /// A leader that ignores `SIGTERM` (a snippet: the leader is its own `sh`) keeps its group
    /// past the grace; it says `ready` on stderr once it ignores it.
    static STUBBORN: Snippet = Snippet {
        name: "stubborn",
        script: "trap '' TERM; echo ready >&2; sleep 30",
    };

    fn stubborn(repo: &Repo) -> GitCommand {
        GitCommand::snippet(Class::Read, repo.top(), &STUBBORN, Vec::<String>::new())
    }

    fn group_alive(pid: u32) -> bool {
        // SAFETY: kill(2) with signal 0 only asks.
        unsafe { libc::kill(-(pid as libc::pid_t), 0) == 0 }
    }

    fn wait_for(what: &str, limit: Duration, done: impl Fn() -> bool) {
        let t = Instant::now();
        while !done() {
            assert!(t.elapsed() < limit, "{what}: not within {limit:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits until the stubborn leader of `handle`'s newest command ignores `SIGTERM`.
    fn stubborn_ready(handle: &Handle) {
        wait_for("the leader ignoring TERM", Duration::from_secs(5), || {
            handle.progress().as_deref() == Some("ready")
        });
    }

    /// Starts `cmds` on `handle` and waits for their `sopen-pid` lines.
    fn started(runner: &GitRunner, cmds: &[GitCommand], handle: &Handle) -> Vec<Running> {
        let runs: Vec<Running> = cmds
            .iter()
            .map(|c| {
                runner
                    .start(c, handle)
                    .unwrap_or_else(|d| panic!("{}", d.ending))
            })
            .collect();
        wait_for("the sopen-pid lines", Duration::from_secs(5), || {
            runs.iter().all(|r| r.remote_pid().is_some())
        });
        runs
    }

    /// Three commands of one handle stopped together: one `-TERM` session names all three
    /// groups, and never more than one stop session is open.
    #[test]
    fn three_attached_commands_are_stopped_in_one_session() {
        let Some(git) = fixture::git() else {
            return;
        };
        let repo = Repo::new(&git);
        let (runner, faults) = shell(&repo);
        let handle = Handle::new();
        let cmd = alias(&repo, "sleep 30");
        let runs = started(&runner, &[cmd.clone(), cmd.clone(), cmd], &handle);
        let pids: Vec<u32> = runs.iter().filter_map(Running::remote_pid).collect();
        handle.stop();
        for run in runs {
            let done = run.finish();
            assert!(
                matches!(&done.ending, Ending::Stopped(s) if !s.killed),
                "{}",
                done.ending
            );
        }
        let sessions = lock(&faults.sessions).clone();
        let terms: Vec<&Vec<String>> = sessions.iter().filter(|s| s[0] == "-TERM").collect();
        assert_eq!(terms.len(), 1, "{sessions:?}");
        let mut named: Vec<String> = terms[0][1..].to_vec();
        named.sort();
        let mut want: Vec<String> = pids.iter().map(|p| format!("-{p}")).collect();
        want.sort();
        assert_eq!(named, want, "{sessions:?}");
        assert!(!sessions.iter().any(|s| s[0] == "-KILL"), "{sessions:?}");
        assert_eq!(faults.most_open.load(Ordering::SeqCst), 1, "{sessions:?}");
        for pid in pids {
            wait_for("the group gone", Duration::from_secs(2), || {
                !group_alive(pid)
            });
        }
    }

    /// Two stops at once (two handles): the second waits behind the first — at no moment are
    /// two stop sessions open — and both end their commands.
    #[test]
    fn two_overlapping_stops_never_open_two_sessions() {
        let Some(git) = fixture::git() else {
            return;
        };
        let repo = Repo::new(&git);
        let (runner, faults) = shell(&repo);
        let (first, second) = (Handle::new(), Handle::new());
        let a = started(&runner, &[stubborn(&repo)], &first);
        stubborn_ready(&first);
        let b = started(&runner, &[alias(&repo, "sleep 30")], &second);
        let pids: Vec<u32> = a.iter().chain(&b).filter_map(Running::remote_pid).collect();
        first.stop();
        second.stop();
        for run in a.into_iter().chain(b) {
            let done = run.finish();
            assert!(matches!(done.ending, Ending::Stopped(_)), "{}", done.ending);
        }
        let sessions = lock(&faults.sessions).clone();
        assert_eq!(faults.most_open.load(Ordering::SeqCst), 1, "{sessions:?}");
        assert_eq!(
            sessions.iter().filter(|s| s[0] == "-TERM").count(),
            2,
            "one stop each: {sessions:?}"
        );
        for pid in pids {
            wait_for("the group gone", Duration::from_secs(3), || {
                !group_alive(pid)
            });
        }
    }

    /// A group whose leader ignores `SIGTERM`, stopped with one that does not: both get the one
    /// `-TERM`; the asking finds only the stubborn one there (and still its own), and the
    /// `-KILL` goes to it alone.
    #[test]
    fn a_group_that_ignores_term_still_gets_the_kill() {
        let Some(git) = fixture::git() else {
            return;
        };
        let repo = Repo::new(&git);
        let (runner, faults) = shell(&repo);
        let handle = Handle::new();
        let runs = started(
            &runner,
            &[alias(&repo, "sleep 30"), stubborn(&repo)],
            &handle,
        );
        stubborn_ready(&handle);
        let pids: Vec<u32> = runs.iter().filter_map(Running::remote_pid).collect();
        handle.stop();
        let endings: Vec<Ending> = runs.into_iter().map(|r| r.finish().ending).collect();
        assert!(
            matches!(&endings[0], Ending::Stopped(s) if !s.killed),
            "{endings:?}"
        );
        let sessions = lock(&faults.sessions).clone();
        assert!(
            matches!(&endings[1], Ending::Stopped(s) if s.killed),
            "{endings:?} {sessions:?} {pids:?}"
        );
        let kills: Vec<&Vec<String>> = sessions.iter().filter(|s| s[0] == "-KILL").collect();
        assert_eq!(kills.len(), 1, "{sessions:?}");
        assert_eq!(kills[0][1..], [format!("-{}", pids[1])], "{sessions:?}");
        assert_eq!(faults.most_open.load(Ordering::SeqCst), 1, "{sessions:?}");
        for pid in pids {
            wait_for("the group gone", Duration::from_secs(3), || {
                !group_alive(pid)
            });
        }
    }

    /// A stop session's words: one `sh -c` (the [`STOP`] script), the signal, then each group's
    /// pid and identity as words of their own (an identity with spaces stays one word).
    #[test]
    fn the_stop_sessions_words() {
        let words = stop_words(
            "-TERM",
            &[
                (12, "98765".into()),
                (34, "Mon Nov 23 18:58:09 2026".into()),
            ],
        );
        let tail: Vec<String> = words[4..]
            .iter()
            .map(|w| w.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            tail,
            [
                "'sopen-stop'",
                "'-TERM'",
                "'12'",
                "'98765'",
                "'34'",
                "'Mon Nov 23 18:58:09 2026'"
            ]
        );
        assert_eq!(words[..3], ["exec", "'sh'", "'-c'"].map(OsString::from));
        assert_eq!(
            kill_words(12, "-KILL"),
            ["exec", "'kill'", "'-KILL'", "'--'", "'-12'"].map(OsString::from)
        );
    }

    /// `pid_line`: the pid and the rest of the line as the identity; an old form without one.
    #[test]
    fn the_pid_line_carries_the_identity() {
        assert_eq!(
            pid_line(b"sopen-pid 4242 98765"),
            Some((4242, "98765".to_string()))
        );
        assert_eq!(
            pid_line(b"sopen-pid 4242 Mon Nov 23 18:58:09 2026    "),
            Some((4242, "Mon Nov 23 18:58:09 2026    ".to_string()))
        );
        assert_eq!(pid_line(b"sopen-pid 4242"), Some((4242, String::new())));
        assert_eq!(pid_line(b"sopen-pid 1 x"), None);
        assert_eq!(pid_line(b"sopen-pid x 1"), None);
    }

    /// A process of a group of its own, started here ("the host" is this machine): its pid.
    fn own_group(script: &str) -> (std::process::Child, u32) {
        let mut c = Command::new("/bin/sh");
        c.arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid is async-signal-safe, as `pre_exec` requires.
        unsafe {
            c.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = c.spawn().unwrap();
        let pid = child.id();
        (child, pid)
    }

    /// The stop script's answers, run here as a remote's login shell would run its words.
    fn stop_here(signal: &str, groups: &[(u32, String)]) -> Vec<(u32, GroupAnswer)> {
        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg(line_of(&stop_words(signal, groups)))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        group_answers(&out.stdout)
    }

    /// The identity `id_fn!` reads for a pid with `ps` a stand-in, first on `PATH`, that prints
    /// a start time and exits `code` (a pid no `/proc` has, so Linux takes the `ps` branch too).
    fn identity_with_ps(code: i32) -> String {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ps = dir.path().join("ps");
        let stand_in = format!("#!/bin/sh\necho 'Mon Nov 23 10:00:00 2026'\nexit {code}\n");
        std::fs::write(&ps, stand_in).unwrap();
        std::fs::set_permissions(&ps, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Run once now: a just-written stand-in can be "busy" for a moment on Linux (testutil),
        // and the shell below would fail to start it.
        crate::testutil::run_fresh(&mut Command::new(&ps));
        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let script = concat!(id_fn!(), r#"sopen_id "$1"; printf %s "$sopen_i""#);
        let out = Command::new("/bin/sh")
            .env("PATH", path)
            .args(["-c", script, "id", "999999999"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// S2-F fix-check (defensive): the identity's `ps` fallback keeps nothing from a `ps` that
    /// failed — what it printed is no start time, so a stop never matches a group by it; a `ps`
    /// that succeeds is kept as before.
    #[test]
    fn a_ps_that_fails_gives_no_identity() {
        assert_eq!(identity_with_ps(1), "", "a failed ps is no identity");
        assert_eq!(identity_with_ps(0), "Mon Nov 23 10:00:00 2026");
    }

    /// This machine's identity of `pid`, as the pid line reads it.
    fn identity_here(pid: u32) -> String {
        let script = concat!(id_fn!(), r#"sopen_id "$1"; printf %s "$sopen_i""#);
        let out = Command::new("/bin/sh")
            .args(["-c", script, "id", &pid.to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Review 1.4 (pid reuse), on a stand-in host — this machine: a group whose leader's pid is
    /// another process's now (its identity is not the one recorded) is not signalled, answered
    /// `other`; a group whose leader is gone while the group is not is not signalled either
    /// (`leaderless`) — its member lives on; only the group whose leader is still the one recorded
    /// is signalled (`ok`).
    #[test]
    fn a_number_that_is_not_ours_is_never_signalled() {
        // Another process at the number: the identity recorded is the old leader's.
        let (mut other, pid) = own_group("exec sleep 30");
        let theirs = identity_here(pid);
        assert!(!theirs.is_empty(), "this machine names a process's start");
        let recorded = format!("{theirs}-before");
        assert_eq!(
            stop_here("-TERM", &[(pid, recorded.clone())]),
            [(pid, GroupAnswer::Other)]
        );
        assert_eq!(
            stop_here("-0", &[(pid, recorded)]),
            [(pid, GroupAnswer::Other)]
        );
        std::thread::sleep(Duration::from_millis(100));
        assert!(other.try_wait().unwrap().is_none(), "not signalled");
        // A group without its leader: its member is not signalled.
        let (mut leader, pid) = own_group("sleep 30 & echo $! > \"$0\".child; exit 0");
        let _ = leader.wait();
        assert!(group_alive(pid), "the member keeps the group");
        assert_eq!(
            stop_here("-KILL", &[(pid, "whatever".into())]),
            [(pid, GroupAnswer::Leaderless)]
        );
        assert!(group_alive(pid), "not signalled");
        // The leader recorded: signalled.
        assert_eq!(
            stop_here("-TERM", &[(other.id(), identity_here(other.id()))]),
            [(other.id(), GroupAnswer::Ok)]
        );
        let _ = other.wait();
        // SAFETY: the test's own group.
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        wait_for("the member gone", Duration::from_secs(2), || {
            !group_alive(pid)
        });
    }

    /// Review 1.4 through the runner: a command whose leader dies just before the stop's
    /// `-TERM` session, leaving a child that holds its group: the child is not signalled by the
    /// number, and the stop says it may still be running.
    #[test]
    fn a_stop_never_signals_a_group_whose_leader_is_gone() {
        let Some(git) = fixture::git() else {
            return;
        };
        let repo = Repo::new(&git);
        let (runner, faults) = shell(&repo);
        let handle = Handle::new();
        let runs = started(
            &runner,
            &[alias(&repo, "echo ready >&2; sleep 30")],
            &handle,
        );
        // The child is there (the pid line comes before git has even run).
        wait_for("the child", Duration::from_secs(5), || {
            handle.progress().as_deref() == Some("ready")
        });
        let leader = runs[0].remote_pid().unwrap();
        // Right before the stop's first session the leader goes (its child, `sleep 30`, stays).
        faults.before_stop.set(move || {
            // SAFETY: the test's own process (the leader only, not its group).
            unsafe { libc::kill(leader as libc::pid_t, libc::SIGKILL) };
            // Until it is reaped (the command's reader does it): a zombie still has its start
            // time, a host's sshd reaps at once.
            // SAFETY: signal 0 only asks.
            wait_for("the leader reaped", Duration::from_secs(3), || unsafe {
                libc::kill(leader as libc::pid_t, 0) != 0
            });
        });
        handle.stop();
        let done = runs.into_iter().next().unwrap().finish();
        assert_eq!(done.ending, Ending::StopSent { host: "box".into() });
        assert!(group_alive(leader), "the child was not signalled");
        let sessions = lock(&faults.sessions).clone();
        assert!(!sessions.iter().any(|s| s[0] == "-KILL"), "{sessions:?}");
        // SAFETY: the test's own group.
        unsafe { libc::kill(-(leader as libc::pid_t), libc::SIGKILL) };
        wait_for("the group gone", Duration::from_secs(2), || {
            !group_alive(leader)
        });
    }
}
