//! The log stream (03 §4.5): one `git log` per load, its roots as object ids on stdin (§4.1),
//! read 500 records at a time from the same stream, parked while hidden and ended after 30 s
//! hidden or when its panel is dropped. S2-B writes it.
//!
//! Also here, until S2-F plugs S2-A's runner in: [`Git`], the little a read needs from a
//! runner ("run these words in this folder and give me what it printed, or a stream of its
//! stdout"); [`argv`], every read's words exactly as 03 writes them, in one place; and the
//! reads of §4.1 and §6.1 themselves ([`refs`], [`head`], [`stashes`], [`status`],
//! [`details`], [`files`]).

use super::model::*;
use super::parse::{self, ParseError};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Records per page (03 §4.5).
pub const PAGE: usize = 500;

/// How long a read may take (03 §3.4): the runner's limit for the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// Detection and `git version`: 10 s in all.
    Detection,
    /// Every other read: 30 s without a byte on stdout or stderr.
    Read,
}

/// How a command ended: its exit code (`None`: no exit status, a signal), what it printed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Why a command gave no output to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// No git to run: a local spawn's `ENOENT`, a remote's `sopen: no git` (exit 127; 03 §2.4).
    NoGit,
    /// The bound passed and the runner stopped the command.
    TimedOut,
    /// Anything else the runner could not do (a spawn error, a lost channel), as text.
    Failed(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::NoGit => f.write_str("no git"),
            RunError::TimedOut => f.write_str("git did not answer in time"),
            RunError::Failed(e) => f.write_str(e),
        }
    }
}

/// What S2-B's reads need from a runner (S2-A's `GitRunner` behind it once S2-F plugs it in).
/// `dir` goes to `-C` (detection: the folder; every other read: the top level, 03 §3.1); `args`
/// start at the subcommand, the runner adding 03 §3.3's environment and options in front
/// (`GIT_OPTIONAL_LOCKS=0` included: every call here is a read); `stdin` is fed and closed
/// (empty: `/dev/null`).
pub trait Git: Send + Sync {
    /// Runs the command to its end, bounded by `bound`.
    fn run(&self, dir: &str, args: &[String], stdin: &[u8], bound: Bound) -> Result<Ran, RunError>;

    /// Starts the command and hands back its stdout to read as it comes: the log stream. Nothing
    /// bounds it while nobody reads (a parked stream, 03 §3.4).
    fn spawn(&self, dir: &str, args: &[String], stdin: &[u8]) -> Result<Box<dyn Pipe>, RunError>;
}

/// A started command's stdout (the log stream's pipe).
///
/// A page blocks in [`Pipe::read`], and [`LogStream`] checks its load's generation only between
/// reads; so the runner owns the two ways out of a wait: `read` fails once 30 s pass without a
/// byte while it waits (03 §3.4: the page then ends with that error and git is stopped), and Esc
/// or a superseded load during a slow page stops the command through the runner's own handle,
/// which makes `read` return.
pub trait Pipe: Send {
    /// Reads stdout as `std::io::Read::read` does: 0 is its end; an error after 30 s without a
    /// byte while waiting.
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    /// The command has not ended (a parked stream: git blocked on the full pipe, 03 §4.5).
    fn running(&mut self) -> bool;
    /// Ends the command (03 §3.5) and reaps it; again is a no-op.
    fn stop(&mut self);
    /// After stdout's end: waits for the exit and gives it with stderr (`stdout` empty).
    fn finish(&mut self) -> Ran;
}

/// Every read's words, starting at the subcommand, exactly as 03 writes them — the one place
/// S2-F checks against the runner. The only additions to 03's forms are the `--` after a
/// revision (`details`, `name_status`, `numstat`): without it a work-tree file named like the
/// hash makes git stop with `ambiguous argument … both revision and filename` (verified, git
/// 2.56.1), as 03 §4.1's log form already ends in `--`.
pub mod argv {
    use super::{LogOptions, Oid};

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    /// `git version` (03 §2.4), once per host.
    pub fn version() -> Vec<String> {
        words(&["version"])
    }

    /// Detection (03 §2.1), run with `-C <folder>`.
    pub fn detection() -> Vec<String> {
        words(&[
            "rev-parse",
            "--is-bare-repository",
            "--is-inside-git-dir",
            "--is-shallow-repository",
            "--absolute-git-dir",
            "--git-common-dir",
            "--show-toplevel",
            "--show-superproject-working-tree",
        ])
    }

    /// The refs read (03 §4.1).
    pub fn refs() -> Vec<String> {
        words(&[
            "for-each-ref",
            "--format=%(objectname)%00%(objecttype)%00%(*objectname)%00%(refname)%00%(upstream)%00%(upstream:track)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ])
    }

    /// The HEAD read's first command (03 §4.1).
    pub fn symbolic_head() -> Vec<String> {
        words(&["symbolic-ref", "-q", "HEAD"])
    }

    /// The HEAD read's second command (03 §4.1).
    pub fn verify_head() -> Vec<String> {
        words(&["rev-parse", "-q", "--verify", "HEAD"])
    }

    /// The stashes read (03 §4.1).
    pub fn stashes() -> Vec<String> {
        words(&[
            "stash",
            "list",
            "-z",
            "--format=%H%x1f%P%x1f%gd%x1f%ct%x1f%s",
        ])
    }

    /// The status read (03 §4.1; `GIT_OPTIONAL_LOCKS=0`, as every read).
    pub fn status() -> Vec<String> {
        words(&[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=normal",
        ])
    }

    /// The log stream (03 §4.1): `log -z --<order> --format=… [--first-parent] [--reflog]
    /// --stdin --`, the roots' ids on stdin.
    pub fn log(options: &LogOptions) -> Vec<String> {
        let mut w = words(&["log", "-z", options.order.flag()]);
        w.push("--format=%H%x1f%P%x1f%aN%x1f%aE%x1f%at%x1f%ct%x1f%s".into());
        if options.first_parent {
            w.push("--first-parent".into());
        }
        if options.reflog {
            w.push("--reflog".into());
        }
        w.extend(words(&["--stdin", "--"]));
        w
    }

    /// The details read (03 §6.1), `--` added.
    pub fn details(c: &Oid) -> Vec<String> {
        let mut w = words(&[
            "log",
            "-1",
            "-z",
            "--format=%H%x1f%P%x1f%aN%x1f%aE%x1f%at%x1f%cN%x1f%cE%x1f%ct%x1f%B",
        ]);
        w.push(c.as_str().into());
        w.push("--".into());
        w
    }

    /// The details pane's Files, letters (03 §6.1), `--` added.
    pub fn name_status(old: &Oid, new: &Oid) -> Vec<String> {
        diff("--name-status", old, new)
    }

    /// The details pane's Files, line counts (03 §6.1), `--` added.
    pub fn numstat(old: &Oid, new: &Oid) -> Vec<String> {
        diff("--numstat", old, new)
    }

    fn diff(what: &str, old: &Oid, new: &Oid) -> Vec<String> {
        let mut w = words(&["diff", "-z", "--find-renames", what]);
        w.extend([
            old.as_str().to_string(),
            new.as_str().to_string(),
            "--".into(),
        ]);
        w
    }

    /// The empty tree's id (03 §6.2: a root commit's and a stash's untracked files' old side),
    /// with stdin `/dev/null`.
    pub fn empty_tree() -> Vec<String> {
        words(&["hash-object", "-t", "tree", "--stdin"])
    }
}

/// Why a read gave no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The runner could not run it.
    Run(RunError),
    /// git ran and failed: its exit (`None`: no exit status) and stderr's first line.
    Exit { code: Option<i32>, message: String },
    /// git's output did not parse.
    Parse(ParseError),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Run(e) => write!(f, "{e}"),
            ReadError::Exit { code, message } => match code {
                Some(c) => write!(f, "git exited with {c}: {message}"),
                None => write!(f, "git ended without an exit status: {message}"),
            },
            ReadError::Parse(e) => write!(f, "cannot read git's output: {e}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<ParseError> for ReadError {
    fn from(e: ParseError) -> Self {
        ReadError::Parse(e)
    }
}

/// stderr's first line, for an error.
pub(crate) fn first_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim_end()
        .to_string()
}

/// Runs a read that must exit 0 (or one of `ok`), in the top level.
fn read(git: &dyn Git, top: &str, args: Vec<String>, ok: &[i32]) -> Result<Ran, ReadError> {
    let ran = git
        .run(top, &args, &[], Bound::Read)
        .map_err(ReadError::Run)?;
    match ran.code {
        Some(c) if c == 0 || ok.contains(&c) => Ok(ran),
        code => Err(ReadError::Exit {
            code,
            message: first_line(&ran.stderr),
        }),
    }
}

/// The refs read (03 §4.1).
pub fn refs(git: &dyn Git, top: &str) -> Result<Vec<RefLabel>, ReadError> {
    let ran = read(git, top, argv::refs(), &[])?;
    Ok(parse::refs(&ran.stdout)?)
}

/// The HEAD read (03 §4.1): both commands, the second after the first.
pub fn head(git: &dyn Git, top: &str) -> Result<Head, ReadError> {
    let symbolic = read(git, top, argv::symbolic_head(), &[1])?;
    let verify = read(git, top, argv::verify_head(), &[1])?;
    Ok(parse::head(
        (symbolic.code, &symbolic.stdout),
        (verify.code, &verify.stdout),
    )?)
}

/// The stashes read (03 §4.1).
pub fn stashes(git: &dyn Git, top: &str) -> Result<Vec<StashRow>, ReadError> {
    let ran = read(git, top, argv::stashes(), &[])?;
    Ok(parse::stashes(&ran.stdout)?)
}

/// The status read (03 §4.1).
pub fn status(git: &dyn Git, top: &str) -> Result<Status, ReadError> {
    let ran = read(git, top, argv::status(), &[])?;
    Ok(parse::status(&ran.stdout)?)
}

/// The empty tree's id in this repository (03 §6.2).
pub fn empty_tree(git: &dyn Git, top: &str) -> Result<Oid, ReadError> {
    let ran = read(git, top, argv::empty_tree(), &[])?;
    Ok(parse::object_id(&ran.stdout)?)
}

/// One commit's summary (03 §6.1).
pub fn details(git: &dyn Git, top: &str, c: &Oid) -> Result<CommitDetails, ReadError> {
    let ran = read(git, top, argv::details(c), &[])?;
    Ok(parse::details(&ran.stdout)?)
}

/// The changed files between two commits or trees (03 §6.1): `--name-status` and `--numstat`
/// of the same endpoints, joined.
pub fn files(
    git: &dyn Git,
    top: &str,
    old: &Oid,
    new: &Oid,
) -> Result<Vec<ChangedFile>, ReadError> {
    let letters = read(git, top, argv::name_status(old, new), &[])?;
    let counts = read(git, top, argv::numstat(old, new), &[])?;
    Ok(parse::changed_files(&letters.stdout, &counts.stdout)?)
}

/// A commit's files (03 §6.1–§6.2): against its first parent (a merge too), a root commit
/// against the empty tree.
pub fn commit_files(
    git: &dyn Git,
    top: &str,
    commit: &CommitDetails,
) -> Result<Vec<ChangedFile>, ReadError> {
    let old = match commit.parents.first() {
        Some(p) => p.clone(),
        None => empty_tree(git, top)?,
    };
    files(git, top, &old, &commit.oid)
}

/// A stash's files (03 §6.1): its tracked changes against its first parent (the base), then its
/// untracked files — the third parent's tree, against the empty tree — each as `?`.
pub fn stash_files(
    git: &dyn Git,
    top: &str,
    stash: &StashRow,
) -> Result<Vec<ChangedFile>, ReadError> {
    let mut rows = files(git, top, &stash.base, &stash.oid)?;
    if let Some(untracked) = &stash.untracked {
        let empty = empty_tree(git, top)?;
        rows.extend(
            files(git, top, &empty, untracked)?
                .into_iter()
                .map(|f| ChangedFile {
                    letter: FileLetter::Untracked,
                    ..f
                }),
        );
    }
    Ok(rows)
}

/// The log's order (03 §4.1): one is always passed, so children come before their parents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Order {
    /// `--date-order`, the default.
    #[default]
    Date,
    /// `--author-date-order`.
    AuthorDate,
    /// `--topo-order`.
    Topo,
}

impl Order {
    pub fn flag(self) -> &'static str {
        match self {
            Order::Date => "--date-order",
            Order::AuthorDate => "--author-date-order",
            Order::Topo => "--topo-order",
        }
    }
}

/// The log's options from the filters (03 §4.6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogOptions {
    pub order: Order,
    /// "First parent only".
    pub first_parent: bool,
    /// "Reflog commits".
    pub reflog: bool,
}

/// The log's roots (03 §4.1), the ids of what the labels show: HEAD's commit (pass `None` with a
/// branch filter), every ref given — an annotated tag by what it points at — except a remote's
/// `HEAD` (not drawn, 03 §4.4), and each stash's base; each id once, in that order. Filters
/// choose what is given.
pub fn roots(refs: &[RefLabel], head: Option<&Head>, stashes: &[StashRow]) -> Vec<Oid> {
    let drawn = refs
        .iter()
        .filter(|r| !(r.kind == RefKind::RemoteBranch && r.refname.ends_with("/HEAD")))
        .map(RefLabel::target);
    let ids = head
        .and_then(Head::oid)
        .into_iter()
        .chain(drawn)
        .chain(stashes.iter().map(|s| &s.base));
    let mut seen = std::collections::HashSet::new();
    ids.filter(|id| seen.insert(*id)).cloned().collect()
}

/// A panel's load generation (03 §4.1, 02 §6): a load takes a new number, and a result that
/// carries an older one is dropped unread. Clones share the number.
#[derive(Debug, Clone, Default)]
pub struct Generation(Arc<AtomicU64>);

impl Generation {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new load: its number (the first is 1).
    pub fn bump(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn current(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.current() == generation
    }

    /// `value` when it belongs to the current load; `None` (dropped unread) otherwise.
    pub fn accept<T>(&self, generation: u64, value: T) -> Option<T> {
        self.is_current(generation).then_some(value)
    }
}

/// Why no stream was started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// No root: git would fall back to HEAD (03 §4.1), so no log runs — `no refs chosen` with a
    /// filter, `no commits yet` in an empty repository.
    NoRoots,
    /// The load's generation is no longer current.
    Superseded,
    Run(RunError),
}

/// How a page ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageEnd {
    /// The page is full; the stream holds more or has not said it ended.
    More,
    /// The stream ended (`all loaded`).
    AllLoaded,
    /// A record did not parse, or git failed: the paging line's error. The stream is stopped.
    Error(String),
    /// The load's generation moved on: the page's rows were dropped unread and the stream
    /// stopped.
    Superseded,
}

/// One page of the log stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The load it belongs to.
    pub generation: u64,
    pub rows: Vec<CommitRow>,
    pub end: PageEnd,
}

/// The log stream of one load (03 §4.5): `git log` without `--max-count`, read a page at a time;
/// between pages git blocks on the full pipe. Dropping it stops git.
pub struct LogStream {
    generation: u64,
    current: Generation,
    pipe: Option<Box<dyn Pipe>>,
    /// Bytes read from the pipe and not parsed yet (`buf[at..]`).
    buf: Vec<u8>,
    at: usize,
    rows: usize,
    bytes: u64,
    /// How the stream ended, once it has: every later page ends the same way, with no rows.
    finished: Option<PageEnd>,
}

/// How much is asked of the pipe at a time.
const CHUNK: usize = 64 << 10;

impl LogStream {
    /// Starts the log for the load `generation` of `current`, `roots` on stdin, one id a line.
    pub fn start(
        git: &dyn Git,
        top: &str,
        options: &LogOptions,
        roots: &[Oid],
        current: &Generation,
        generation: u64,
    ) -> Result<LogStream, StartError> {
        if !current.is_current(generation) {
            return Err(StartError::Superseded);
        }
        if roots.is_empty() {
            return Err(StartError::NoRoots);
        }
        let mut stdin = Vec::with_capacity(roots.len() * 41);
        for r in roots {
            stdin.extend_from_slice(r.as_str().as_bytes());
            stdin.push(b'\n');
        }
        let pipe = git
            .spawn(top, &argv::log(options), &stdin)
            .map_err(StartError::Run)?;
        Ok(LogStream {
            generation,
            current: current.clone(),
            pipe: Some(pipe),
            buf: Vec::new(),
            at: 0,
            rows: 0,
            bytes: 0,
            finished: None,
        })
    }

    /// The next `n` records (03 §4.5: [`PAGE`]), blocking until they came or the stream ended.
    /// Only whole records are parsed; what the pipe gave past the n-th waits for the next page.
    /// Between records it checks the load's generation: once superseded, the rows read so far
    /// are dropped and git is stopped.
    pub fn page(&mut self, n: usize) -> Page {
        let mut rows = Vec::new();
        let end = match &self.finished {
            Some(end) => end.clone(),
            None => self.fill(&mut rows, n),
        };
        if !matches!(end, PageEnd::More) {
            if !matches!(end, PageEnd::AllLoaded) {
                self.stop();
            }
            self.finished = Some(end.clone());
        }
        if end == PageEnd::Superseded {
            rows.clear();
        }
        Page {
            generation: self.generation,
            rows,
            end,
        }
    }

    fn fill(&mut self, rows: &mut Vec<CommitRow>, n: usize) -> PageEnd {
        loop {
            if !self.current.is_current(self.generation) {
                return PageEnd::Superseded;
            }
            if rows.len() >= n {
                return PageEnd::More;
            }
            if let Some(len) = self.buf[self.at..].iter().position(|&b| b == 0) {
                let record = &self.buf[self.at..self.at + len];
                match parse::commit_record(record) {
                    Ok(row) => rows.push(row),
                    Err(e) => return PageEnd::Error(format!("record {}: {e}", self.rows + 1)),
                }
                self.at += len + 1;
                self.rows += 1;
                continue;
            }
            let Some(pipe) = self.pipe.as_mut() else {
                return PageEnd::Error("the stream was stopped".into());
            };
            self.buf.drain(..self.at);
            self.at = 0;
            let had = self.buf.len();
            self.buf.resize(had + CHUNK, 0);
            let read = pipe.read(&mut self.buf[had..]);
            self.buf.truncate(had + *read.as_ref().unwrap_or(&0));
            match read {
                Ok(0) => return self.end_of_stdout(rows),
                Ok(k) => self.bytes += k as u64,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return PageEnd::Error(format!("reading git log: {e}")),
            }
        }
    }

    /// stdout ended: a last record without its NUL, then git's exit.
    fn end_of_stdout(&mut self, rows: &mut Vec<CommitRow>) -> PageEnd {
        let ran = match self.pipe.take() {
            Some(mut pipe) => pipe.finish(),
            None => Ran::default(),
        };
        let tail = &self.buf[self.at..];
        if !tail.is_empty() {
            match parse::commit_record(tail) {
                Ok(row) => rows.push(row),
                Err(e) => return PageEnd::Error(format!("record {}: {e}", self.rows + 1)),
            }
            self.rows += 1;
        }
        self.buf.clear();
        self.at = 0;
        match ran.code {
            Some(0) => PageEnd::AllLoaded,
            Some(c) => PageEnd::Error(format!(
                "git log exited with {c}: {}",
                first_line(&ran.stderr)
            )),
            None => PageEnd::Error("git log ended without an exit status".into()),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Records handed out so far: a restarted stream reads as many (03 §4.5).
    pub fn rows_read(&self) -> usize {
        self.rows
    }

    /// Bytes taken from the pipe so far, parsed or not.
    pub fn bytes_read(&self) -> u64 {
        self.bytes
    }

    /// The stream has ended (all loaded, an error, superseded or stopped).
    pub fn ended(&self) -> bool {
        self.finished.is_some()
    }

    /// git is still there (parked between pages, or walking).
    pub fn running(&mut self) -> bool {
        self.pipe.as_mut().is_some_and(|p| p.running())
    }

    /// Ends the stream: git is stopped (a hidden panel after 30 s, a dropped panel, a refresh).
    pub fn stop(&mut self) {
        if let Some(mut pipe) = self.pipe.take() {
            pipe.stop();
        }
        if self.finished.is_none() {
            self.finished = Some(PageEnd::Error("the stream was stopped".into()));
        }
    }
}

impl Drop for LogStream {
    fn drop(&mut self) {
        self.stop();
    }
}
