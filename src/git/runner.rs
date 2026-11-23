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
//! - **Nothing is left behind**: once a finished command's output has had a second, whatever
//!   of its process group still holds stdout or stderr is killed (remotely over the master).
//! - **No git**: `Ending::NoGit` — locally the git program is missing, remotely `<locate>`
//!   found none (`sopen: no git`, exit 127).
//! - [`mask_credentials`]: what every logged line and kept stderr went through; stdout is
//!   returned as git printed it, so mask it before showing it.
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
#[cfg(test)]
use std::sync::atomic::Ordering;
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

/// The first line of every remote command and snippet: the pid the stop signals (03 §3.5).
/// sshd starts the command as the leader of a new session, and every `exec` keeps the pid.
const PID_LINE: &str = r#"echo "sopen-pid $$" >&2;"#;

/// `<locate>` (03 §3.2, §2.4): one fixed POSIX sh script, sent as a word and run by `sh -c`. It
/// prints the pid line, finds git — on `PATH`, then in Homebrew's and the usual directories;
/// `/usr/bin/git` on a Darwin host only when `xcode-select -p` succeeds (otherwise it is the
/// developer-tools shim) — and `exec`s it with `"$@"`; with none it says `sopen: no git` on
/// stderr and exits 127. No `'` and no `\` in it, so its quoted word is the script between two
/// quotes.
pub const LOCATE: &str = concat!(
    r#"echo "sopen-pid $$" >&2; "#,
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

    /// `kill <signal> -- -<pid>` on the host, over the master: `kill`'s exit code (0: signalled,
    /// 1: no such group), or `None` when it did not run there.
    fn kill(&self, pid: u32, signal: &str) -> Option<i32> {
        let words = kill_words(pid, signal);
        let c = match self {
            Via::Local(_) => return None,
            Via::Ssh(g) => {
                let mut c = Command::new("ssh");
                c.args(g.argv(words));
                c
            }
            #[cfg(test)]
            Via::Shell(g) => g.login(&line_of(&words)),
        };
        bounded(c).filter(|code| matches!(code, 0 | 1))
    }
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
    /// All of stdout, as git printed it (not masked).
    pub stdout: Vec<u8>,
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
        let stdout = String::from_utf8_lossy(&self.stdout);
        if stdout.trim().is_empty() {
            self.ending.to_string()
        } else {
            mask_credentials(&stdout)
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
        for shared in attached {
            shared.request_stop(StopBy::Asked);
        }
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
        {
            let mut stop = lock(&self.stop);
            if stop.by.is_some() {
                return;
            }
            stop.by = Some(by);
        }
        let shared = self.clone();
        std::thread::spawn(move || {
            let outcome = shared.stop_sequence();
            lock(&shared.stop).outcome = Some(outcome);
            shared.stop_changed.notify_all();
        });
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

    fn stop_sequence(&self) -> StopOutcome {
        if self.runner.via.remote() {
            self.stop_remote()
        } else {
            self.stop_local()
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
        self.signal_group(libc::SIGKILL);
        self.wait_group_gone(Duration::from_secs(2));
        StopOutcome::Stopped { killed: true }
    }

    /// 03 §3.5, remote: `kill -TERM -- -<pid>` over the same master, the pid from the
    /// `sopen-pid` line; then the grace — the channel's end, and then the group's, asked with
    /// `kill -0` over the master (a child that ignores `SIGTERM` and closed its descriptors
    /// outlives the channel) — and then always `kill -KILL` the same way ("no such process"
    /// counts as done); then the command's channel is closed. Without the line, or with the
    /// master gone, it may still be running.
    fn stop_remote(&self) -> StopOutcome {
        let via = &self.runner.via;
        let Some(pid) = self.wait_remote_pid() else {
            if self.exit().is_some() {
                return StopOutcome::Ended;
            }
            self.close_channel();
            return StopOutcome::Sent;
        };
        if self.exit().is_some() {
            return StopOutcome::Ended;
        }
        if !via.check() || via.kill(pid, "-TERM").is_none() {
            self.close_channel();
            return StopOutcome::Sent;
        }
        let deadline = Instant::now() + self.runner.limits.stop_grace;
        if self.wait_exit(self.runner.limits.stop_grace) {
            // One `kill -0` per half second until the group is gone or the grace is over; any
            // answer but "still there" ends the asking.
            while Instant::now() < deadline && via.kill(pid, "-0") == Some(0) {
                std::thread::sleep(
                    REMOTE_PROBE.min(deadline.saturating_duration_since(Instant::now())),
                );
            }
        }
        let killed = via.kill(pid, "-KILL");
        self.close_channel();
        match killed {
            Some(0) => StopOutcome::Stopped { killed: true },
            Some(_) => StopOutcome::Stopped { killed: false },
            None => StopOutcome::Sent,
        }
    }

    /// The `sopen-pid` line's pid, waited for up to [`PID_WAIT`]; `None` without it or once
    /// the channel has ended.
    fn wait_remote_pid(&self) -> Option<u32> {
        let deadline = Instant::now() + PID_WAIT;
        let mut err = lock(&self.stderr);
        loop {
            if err.remote_pid.is_some() || err.eof {
                return err.remote_pid;
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

    /// Signals the group the local process leads (never anything else: only a process this
    /// runner made a session leader, and never the pid 0 or 1 groups).
    fn signal_group(&self, signal: libc::c_int) {
        if self.runner.via.leads_session() && self.pid >= 2 {
            // SAFETY: plain kill(2) on the group of a process this runner started.
            unsafe { libc::kill(-(self.pid as libc::pid_t), signal) };
        }
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

    /// Kills what is left of a finished command's process group (03 §3.5's group): locally the
    /// group it led, remotely the `sopen-pid` group over the master.
    fn end_leftovers(&self) {
        let via = &self.runner.via;
        if !via.remote() {
            self.signal_group(libc::SIGKILL);
            return;
        }
        let pid = lock(&self.stderr).remote_pid;
        // Over the same master only, as a stop is: with it gone, what is left may still run.
        if let Some(pid) = pid.filter(|_| via.check()) {
            let _ = via.kill(pid, "-KILL");
        }
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
            if let Some(pid) = pid_line(line) {
                self.remote_pid = Some(pid);
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

/// `sopen-pid <pid>`: the pid, when it can name a process group of its own (2 or more; `-0` and
/// `-1` would signal far more than the command).
fn pid_line(line: &[u8]) -> Option<u32> {
    let digits = line.strip_prefix(b"sopen-pid ")?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits)
        .ok()?
        .parse::<u32>()
        .ok()
        .filter(|&pid| pid >= 2 && pid <= i32::MAX as u32)
}

fn read_stderr(shared: &Shared, mut stderr: std::process::ChildStderr) {
    let mut buf = [0u8; 8192];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                shared.touch();
                lock(&shared.stderr).feed(&buf[..n]);
                shared.stderr_changed.notify_all();
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
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
    /// Stopped for too much output: nothing more is returned or collected.
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
        let mut stdout = if self.overflowed {
            Vec::new()
        } else {
            self.buf.split_off(self.pos)
        };
        self.buf.clear();
        self.pos = 0;
        while !self.eof {
            self.fill(wait_start);
            if self.overflowed {
                self.buf.clear();
            } else if stdout.len() + self.buf.len() > self.max_output {
                let room = self.max_output - stdout.len();
                stdout.extend_from_slice(&self.buf[..room]);
                self.overflow(self.max_output);
            } else {
                stdout.append(&mut self.buf);
            }
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
        // background job): it goes with the command, so no reader waits on it for good.
        if self.held || !lock(&self.shared.stderr).eof {
            self.shared.end_leftovers();
            let deadline = Instant::now() + LEFTOVER_WAIT;
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
        if lock(&self.shared.stderr).eof {
            if let Some(t) = self.stderr_thread.take() {
                let _ = t.join();
            }
        } else {
            crate::debug!(
                "git {}: {}: stderr still held after the command ended",
                self.shared.runner.via.host(),
                self.shared.line
            );
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
