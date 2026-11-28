//! The log stream (03 §4.5): one `git log` per load, its roots as object ids on stdin (§4.1),
//! read 500 records at a time from the same stream, parked while hidden and ended after 30 s
//! hidden or when its panel is dropped. S2-B writes it.
//!
//! Also here: [`Git`], the little a read needs from a runner ("run these words in this folder
//! and give me what it printed, or a stream of its stdout"), and [`RunnerGit`], S2-A's
//! `GitRunner` behind it (S2-F); [`argv`], every read's words exactly as 03 writes them, in one
//! place; and the reads of §4.1 and §6.1 themselves ([`refs`], [`head`], [`stashes`],
//! [`status`], [`details`], [`files`]).

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
/// S2-F checks against the runner. `details`, `name_status` and `numstat` end in `--`: without
/// it a work-tree file named like the hash makes git stop with `ambiguous argument … both
/// revision and filename` (verified, git 2.56.1). The log and details forms end every field in
/// NUL (`%x00`, `-z` ending the last), which git never prints inside one.
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
        w.push("--format=%H%x00%P%x00%aN%x00%aE%x00%at%x00%ct%x00%s".into());
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
            "--format=%H%x00%P%x00%aN%x00%aE%x00%at%x00%cN%x00%cE%x00%ct%x00%B",
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

    // S2-F: the reads the panel's integration adds, here so the one list stays one.

    /// The repository's remotes, one a line (03 §4.6: a remote branch's remote is the longest
    /// of them its name starts with).
    pub fn remotes() -> Vec<String> {
        words(&["remote"])
    }

    /// The details pane's `In` (03 §6.1): the branches and tags that contain `c`, one a line.
    pub fn contains(c: &Oid) -> Vec<String> {
        let mut w = words(&["for-each-ref", "--format=%(refname)", "--contains"]);
        w.push(c.as_str().into());
        w.extend(words(&["refs/heads", "refs/remotes", "refs/tags"]));
        w
    }

    /// A comparison with the uncommitted row (03 §6.4): `old`'s tree against the work tree,
    /// letters (`--name-status`) or line counts (`--numstat`).
    pub fn worktree_diff(what: &str, old: &Oid) -> Vec<String> {
        let mut w = words(&["diff", "-z", "--find-renames", what]);
        w.extend([old.as_str().to_string(), "--".into()]);
        w
    }

    /// The uncommitted row's line counts (03 §6.1): staged (`--cached`) or not staged.
    pub fn uncommitted_numstat(staged: bool) -> Vec<String> {
        let mut w = words(&["diff", "-z", "--find-renames", "--numstat"]);
        if staged {
            w.push("--cached".into());
        }
        w.push("--".into());
        w
    }

    /// An untracked folder's files (03 §6.1), read when its row is opened.
    pub fn others(dir: &str) -> Vec<String> {
        let mut w = words(&["ls-files", "-z", "--others", "--exclude-standard", "--"]);
        w.push(dir.into());
        w
    }

    /// One side of a diff (03 §6.2's endpoints, the working file aside).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Side {
        /// A commit, a stash or a tree by id (the empty tree for none).
        Tree(super::Oid),
        /// The index.
        Index,
        /// The work tree.
        WorkTree,
    }

    /// A file row's diff (03 §6.2): `diff --no-color --find-renames` over the two endpoints,
    /// limited to the row's path(s) — `<old> <new>` trees, `--cached <old>` for the index, `<old>`
    /// alone for the work tree, nothing for the index against the work tree.
    pub fn diff_text(old: &Side, new: &Side, paths: &[&str]) -> Vec<String> {
        let mut w = words(&["diff", "--no-color", "--find-renames"]);
        match (old, new) {
            (Side::Tree(a), Side::Tree(b)) => w.extend([a.as_str().into(), b.as_str().into()]),
            (Side::Tree(a), Side::Index) => w.extend(["--cached".into(), a.as_str().into()]),
            (Side::Tree(a), Side::WorkTree) => w.push(a.as_str().into()),
            (Side::Index, Side::WorkTree) => {}
            // No other pair is a row of 03 §6.2's table: the index against itself, the work
            // tree first. The plain form (the index against the work tree) stands in.
            _ => {}
        }
        w.push("--".into());
        let mut seen: Vec<&str> = Vec::new();
        for p in paths {
            if !seen.contains(p) {
                seen.push(p);
                w.push((*p).into());
            }
        }
        w
    }

    /// A blob's bytes (03 §6.3): `cat-file blob <rev>:<path>`, `:<path>` for the index.
    pub fn blob(rev: Option<&super::Oid>, path: &str) -> Vec<String> {
        let spec = match rev {
            Some(rev) => format!("{}:{path}", rev.as_str()),
            None => format!(":{path}"),
        };
        let mut w = words(&["cat-file", "blob"]);
        w.push(spec);
        w
    }

    /// The files renamed since `rev` (03 §6.3 "Diff against the working file"): its tree against
    /// the work tree, renames only.
    pub fn renames_since(rev: &super::Oid) -> Vec<String> {
        let mut w = words(&[
            "diff",
            "-z",
            "--name-status",
            "--find-renames",
            "--diff-filter=R",
        ]);
        w.extend([rev.as_str().to_string(), "--".into()]);
        w
    }

    /// History search (03 §4.7): the log's form with the query's options, the panel's revisions
    /// on stdin. `--grep` is git's regular expression, case-insensitive unless asked; a changed
    /// path goes after `--`.
    pub fn search(options: &LogOptions, query: &Query) -> Vec<String> {
        let mut w = log(options);
        // `--stdin --` close the log's form: the options go before them.
        let tail = w.split_off(w.len() - 2);
        if !query.message.is_empty() {
            w.push(format!("--grep={}", query.message));
        }
        if !query.author.is_empty() {
            w.push(format!("--author={}", query.author));
        }
        if !query.message.is_empty() || !query.author.is_empty() {
            if !query.case {
                w.push("-i".into());
            }
            if !query.message.is_empty() && !query.author.is_empty() {
                w.push("--all-match".into());
            }
        }
        if !query.text.is_empty() {
            w.push(format!("-S{}", query.text));
        }
        if !query.since.is_empty() {
            w.push(format!("--since={}", query.since));
        }
        if !query.until.is_empty() {
            w.push(format!("--until={}", query.until));
        }
        w.extend(tail);
        if !query.path.is_empty() {
            w.push(query.path.clone());
        }
        w
    }

    /// A history search's fields (03 §4.7), as the dialog gives them.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Query {
        pub message: String,
        pub author: String,
        /// Text added or removed (`-S`).
        pub text: String,
        pub path: String,
        pub since: String,
        pub until: String,
        /// Case-sensitive.
        pub case: bool,
    }
}

/// The repository's remotes (`git remote`), in git's order.
pub fn remotes(git: &dyn Git, top: &str) -> Result<Vec<String>, ReadError> {
    let ran = read(git, top, argv::remotes(), &[])?;
    Ok(String::from_utf8_lossy(&ran.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// The details pane's `In` (03 §6.1): the short names of the branches and tags containing `c`.
pub fn contains(git: &dyn Git, top: &str, c: &Oid) -> Result<Vec<String>, ReadError> {
    let ran = read(git, top, argv::contains(c), &[])?;
    Ok(String::from_utf8_lossy(&ran.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            ["refs/heads/", "refs/remotes/", "refs/tags/"]
                .iter()
                .find_map(|p| l.strip_prefix(p))
                .unwrap_or(l)
                .to_string()
        })
        .collect())
}

/// `old`'s tree against the work tree (03 §6.4, the uncommitted row as a comparison's end).
pub fn worktree_files(git: &dyn Git, top: &str, old: &Oid) -> Result<Vec<ChangedFile>, ReadError> {
    let letters = read(git, top, argv::worktree_diff("--name-status", old), &[])?;
    let counts = read(git, top, argv::worktree_diff("--numstat", old), &[])?;
    Ok(parse::changed_files(&letters.stdout, &counts.stdout)?)
}

/// The uncommitted row's line counts by path (03 §6.1): staged, or not staged. A binary file
/// has none.
pub fn uncommitted_counts(
    git: &dyn Git,
    top: &str,
    staged: bool,
) -> Result<std::collections::HashMap<String, (u64, u64)>, ReadError> {
    let ran = read(git, top, argv::uncommitted_numstat(staged), &[])?;
    let mut counts = std::collections::HashMap::new();
    // `<adds>\t<dels>\t<path>\0`, a rename `<adds>\t<dels>\t\0<old>\0<new>\0`.
    let mut fields = ran.stdout.split(|&b| b == 0);
    while let Some(field) = fields.next() {
        if field.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(field);
        let mut parts = text.splitn(3, '\t');
        let (adds, dels, path) = (parts.next(), parts.next(), parts.next());
        let path = match path {
            Some("") => {
                let _old = fields.next();
                fields
                    .next()
                    .map(|p| String::from_utf8_lossy(p).into_owned())
            }
            Some(path) => Some(path.to_string()),
            None => None,
        };
        if let (Some(Ok(a)), Some(Ok(d)), Some(path)) = (
            adds.map(str::parse::<u64>),
            dels.map(str::parse::<u64>),
            path,
        ) {
            counts.insert(path, (a, d));
        }
    }
    Ok(counts)
}

/// An untracked folder's files (03 §6.1), from the top level.
pub fn others(git: &dyn Git, top: &str, dir: &str) -> Result<Vec<String>, ReadError> {
    let ran = read(git, top, argv::others(dir), &[])?;
    Ok(ran
        .stdout
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect())
}

/// Where `path then` (at `rev`) is now (03 §6.3 "Diff against the working file"): followed
/// through the renames between `rev`'s tree and the work tree; itself when it was not renamed.
pub fn path_now(git: &dyn Git, top: &str, rev: &Oid, then: &str) -> Result<String, ReadError> {
    let ran = read(git, top, argv::renames_since(rev), &[])?;
    // `R<score>\0<old>\0<new>\0` records.
    let fields: Vec<&[u8]> = ran.stdout.split(|&b| b == 0).collect();
    let mut at = 0;
    while at + 2 < fields.len() {
        if fields[at].starts_with(b"R") && fields[at + 1] == then.as_bytes() {
            return Ok(String::from_utf8_lossy(fields[at + 2]).into_owned());
        }
        at += 3;
    }
    Ok(then.to_string())
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
    /// The last pipe read, taken up to `at`.
    buf: Vec<u8>,
    at: usize,
    /// The record being read.
    record: Partial,
    rows: usize,
    bytes: u64,
    /// How the stream ended, once it has: every later page ends the same way, with no rows.
    finished: Option<PageEnd>,
}

/// How much is asked of the pipe at a time.
const CHUNK: usize = 64 << 10;

/// The most bytes one log record may run past its fields' caps (review round 1): a field past
/// its cap is read to its end and dropped ([`parse::FIELD_KEEP`]), so memory stays at the caps;
/// a record that drops more than this — output that never ends a field — ends the stream with
/// an error.
pub const DROPPED_CAP: usize = 16 << 20;

/// The record being read: its fields so far and the one being read, each kept up to its cap.
#[derive(Default)]
struct Partial {
    fields: Vec<Vec<u8>>,
    field: Vec<u8>,
    /// Bytes dropped past the caps.
    dropped: usize,
    /// A byte of it has been read.
    started: bool,
}

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
        Self::start_with(git, top, argv::log(options), roots, current, generation)
    }

    /// [`LogStream::start`] with the log's words given (`argv::log`'s form and more: a history
    /// search, 03 §4.7).
    pub fn start_with(
        git: &dyn Git,
        top: &str,
        args: Vec<String>,
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
        let pipe = git.spawn(top, &args, &stdin).map_err(StartError::Run)?;
        Ok(LogStream {
            generation,
            current: current.clone(),
            pipe: Some(pipe),
            buf: Vec::new(),
            at: 0,
            record: Partial::default(),
            rows: 0,
            bytes: 0,
            finished: None,
        })
    }

    /// The next `n` records (03 §4.5: [`PAGE`]), blocking until they came or the stream ended.
    /// Only whole records are parsed — [`parse::RECORD_FIELDS`] fields, each ending in NUL —
    /// and what the pipe gave past the n-th waits for the next page. A name, email or subject
    /// past its cap is read to its end and cut (`…`); a record that drops more than
    /// [`DROPPED_CAP`], whose other fields run past theirs, or that the stream ends inside, ends
    /// the stream with an error and is no row. Between records it checks the load's generation: once superseded, the rows read so
    /// far are dropped and git is stopped.
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
            match self.take() {
                Some(Ok(row)) => {
                    rows.push(row);
                    self.rows += 1;
                    continue;
                }
                Some(Err(e)) => return PageEnd::Error(format!("record {}: {e}", self.rows + 1)),
                None => {}
            }
            let Some(pipe) = self.pipe.as_mut() else {
                return PageEnd::Error("the stream was stopped".into());
            };
            self.buf.clear();
            self.at = 0;
            self.buf.resize(CHUNK, 0);
            let read = pipe.read(&mut self.buf);
            self.buf.truncate(*read.as_ref().unwrap_or(&0));
            match read {
                Ok(0) => return self.end_of_stdout(),
                Ok(k) => self.bytes += k as u64,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return PageEnd::Error(format!("reading git log: {e}")),
            }
        }
    }

    /// Takes the last read's bytes into the record being read, each byte looked at once: a
    /// whole record, its error, or `None` once the read is used up.
    fn take(&mut self) -> Option<Result<CommitRow, String>> {
        while self.at < self.buf.len() {
            let index = self.record.fields.len();
            let rest = &self.buf[self.at..];
            let (piece, ends) = match rest.iter().position(|&b| b == 0) {
                Some(i) => (&rest[..i], true),
                None => (rest, false),
            };
            self.at += piece.len() + usize::from(ends);
            self.record.started = true;
            let keep = parse::FIELD_KEEP[index];
            let taken = piece.len().min(keep - self.record.field.len());
            self.record.field.extend_from_slice(&piece[..taken]);
            if taken < piece.len() {
                if !parse::FIELD_CUT[index] {
                    let what = parse::FIELD_NAMES[index];
                    return Some(Err(format!("{what} is longer than {keep} bytes")));
                }
                self.record.dropped += piece.len() - taken;
                if self.record.dropped > DROPPED_CAP {
                    let cap = DROPPED_CAP >> 20;
                    return Some(Err(format!("more than {cap} MiB past its fields' caps")));
                }
            }
            if ends {
                let field = std::mem::take(&mut self.record.field);
                self.record.fields.push(field);
                if self.record.fields.len() == parse::RECORD_FIELDS {
                    let record = std::mem::take(&mut self.record);
                    let f = std::array::from_fn(|i| record.fields[i].as_slice());
                    return Some(parse::commit_fields(f).map_err(|e| e.to_string()));
                }
            }
        }
        None
    }

    /// stdout ended: git's exit. A record the stream ended inside is no row, whatever the exit.
    fn end_of_stdout(&mut self) -> PageEnd {
        let ran = match self.pipe.take() {
            Some(mut pipe) => pipe.finish(),
            None => Ran::default(),
        };
        let cut = std::mem::take(&mut self.record).started;
        self.buf.clear();
        self.at = 0;
        match ran.code {
            Some(0) if cut => PageEnd::Error(format!(
                "record {}: cut short, the stream ended inside it",
                self.rows + 1
            )),
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

/// S2-A's runner as S2-B's reads ask for it (S2-F): every command a read ([`Class::Read`]: 30 s
/// without a byte while sopen waits, 03 §3.4; detection 10 s in all), each given `handle`, so
/// one stop — Esc, a superseded load — ends every read and stream of it and keeps the next ones
/// from being sent.
///
/// [`Class::Read`]: super::runner::Class::Read
#[derive(Clone)]
pub struct RunnerGit {
    pub runner: super::runner::GitRunner,
    pub handle: super::runner::Handle,
}

/// How long detection and `git version` may take in all (03 §2.1).
pub const DETECTION_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

impl RunnerGit {
    pub fn new(runner: super::runner::GitRunner, handle: super::runner::Handle) -> Self {
        Self { runner, handle }
    }

    fn command(&self, dir: &str, args: &[String], stdin: &[u8]) -> super::runner::GitCommand {
        let cmd = super::runner::GitCommand::new(super::runner::Class::Read, dir, args);
        if stdin.is_empty() {
            cmd
        } else {
            cmd.feed("stdin", stdin.to_vec())
        }
    }
}

/// A command that gave no output to read, in [`RunError`]'s words.
fn run_error(ending: &super::runner::Ending) -> RunError {
    use super::runner::{Ending, StopBy};
    match ending {
        Ending::NoGit => RunError::NoGit,
        Ending::Stopped(s) if matches!(s.by, StopBy::Limit(_)) => RunError::TimedOut,
        other => RunError::Failed(other.to_string()),
    }
}

/// A finished command as a [`Ran`]: its exit code (`None`: a signal), or why there is none.
fn ran(done: super::runner::Done) -> Result<Ran, RunError> {
    use super::runner::Ending;
    let code = match &done.ending {
        Ending::Exit(code) => Some(*code),
        Ending::Signal(_) => None,
        other => return Err(run_error(other)),
    };
    Ok(Ran {
        code,
        stdout: done.stdout,
        stderr: done.stderr.into_bytes(),
    })
}

impl Git for RunnerGit {
    fn run(&self, dir: &str, args: &[String], stdin: &[u8], bound: Bound) -> Result<Ran, RunError> {
        let mut cmd = self.command(dir, args, stdin);
        if bound == Bound::Detection {
            cmd = cmd.within(DETECTION_LIMIT);
        }
        ran(self.runner.run(&cmd, &self.handle))
    }

    fn spawn(&self, dir: &str, args: &[String], stdin: &[u8]) -> Result<Box<dyn Pipe>, RunError> {
        let cmd = self.command(dir, args, stdin);
        match self.runner.start(&cmd, &self.handle) {
            Ok(running) => Ok(Box::new(RunnerPipe(Some(running)))),
            Err(done) => Err(match ran(*done) {
                Ok(r) => RunError::Failed(first_line(&r.stderr)),
                Err(e) => e,
            }),
        }
    }
}

/// A started command's stdout as [`Pipe`] reads it: `read` waits as the runner waits (30 s
/// without a byte, then an error; a stop through the handle makes it return, an error too).
/// `stop` drops the command: it is stopped and finished on a thread of its own, which reads and
/// drops what it still writes meanwhile — a remote channel cannot end while the master holds
/// output nobody reads (E5: a parked stream stopped in 0.59 s read, 5.17 s not) — so it never
/// blocks the caller, a pass of the window included.
pub struct RunnerPipe(Option<super::runner::Running>);

impl Pipe for RunnerPipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.0.as_mut() {
            Some(running) => running.read(buf),
            None => Ok(0),
        }
    }

    fn running(&mut self) -> bool {
        self.0.as_ref().is_some_and(|r| r.running())
    }

    fn stop(&mut self) {
        drop(self.0.take());
    }

    fn finish(&mut self) -> Ran {
        let Some(running) = self.0.take() else {
            return Ran::default();
        };
        match ran(running.finish()) {
            Ok(ran) => ran,
            Err(e) => Ran {
                code: None,
                stdout: Vec::new(),
                stderr: e.to_string().into_bytes(),
            },
        }
    }
}

/// Repositories for the tests of this file and the browser's (S2-F): git run with no config of
/// the machine's, a fixed identity and fixed dates.
#[cfg(test)]
pub(crate) mod fixture {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// This machine's git for a test, or `None` with a note when there is none (a failure
    /// under `SOPEN_REQUIRE_GIT=1`, as CI sets it).
    pub fn git() -> Option<PathBuf> {
        match crate::git::detect::which_git() {
            Ok(git) if Command::new(&git).arg("version").output().is_ok() => Some(git),
            _ => {
                if std::env::var("SOPEN_REQUIRE_GIT").is_ok_and(|v| v == "1") {
                    panic!("git is required (SOPEN_REQUIRE_GIT=1) and there is none");
                }
                eprintln!("skipping: no git on this machine");
                None
            }
        }
    }

    /// The environment every fixture command and every test runner gets: no system or global
    /// config, a fixed identity, the C locale, UTC.
    pub fn env(home: &Path) -> Vec<(String, String)> {
        let home = home.to_string_lossy().into_owned();
        [
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("HOME", home.as_str()),
            ("GIT_AUTHOR_NAME", "Ann"),
            ("GIT_AUTHOR_EMAIL", "ann@example.com"),
            ("GIT_COMMITTER_NAME", "Ann"),
            ("GIT_COMMITTER_EMAIL", "ann@example.com"),
            ("LC_ALL", "C"),
            ("TZ", "UTC"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .into()
    }

    /// A repository in a temporary folder of its own.
    pub struct Repo {
        pub tmp: tempfile::TempDir,
        /// Its top level, canonical.
        pub top: PathBuf,
        pub git: PathBuf,
        /// The next commit's time (seconds since the epoch), one minute apart.
        clock: AtomicU64,
    }

    impl Repo {
        /// `git init -b main` in `<tmp>/repo`.
        pub fn new(git: &Path) -> Repo {
            let tmp = tempfile::tempdir().unwrap();
            let top = std::fs::canonicalize(tmp.path()).unwrap().join("repo");
            std::fs::create_dir(&top).unwrap();
            let repo = Repo {
                tmp,
                top,
                git: git.to_path_buf(),
                clock: AtomicU64::new(1_767_225_600),
            };
            repo.run(&["init", "-q", "-b", "main"]);
            repo
        }

        pub fn top(&self) -> &str {
            self.top.to_str().unwrap()
        }

        /// The fixture's home (where its global config would be, none is read).
        pub fn home(&self) -> PathBuf {
            self.tmp.path().join("home")
        }

        /// `git <args>` in the top level; its stdout. Panics when it fails.
        pub fn run(&self, args: &[&str]) -> String {
            self.run_in(&self.top, args)
        }

        /// `git <args>` in `dir`; its stdout. Started through [`crate::testutil::run_fresh`]: a
        /// stand-in a test has just written in `git`'s place can be busy for a moment on Linux.
        pub fn run_in(&self, dir: &Path, args: &[&str]) -> String {
            let at = self.clock.fetch_add(60, Ordering::SeqCst);
            let date = format!("{at} +0000");
            let mut c = Command::new(&self.git);
            c.current_dir(dir).args(args);
            for key in crate::git::runner::LOCAL_REPO_ENV {
                c.env_remove(key);
            }
            c.envs(env(&self.home()))
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date);
            let out = crate::testutil::run_fresh(&mut c);
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        }

        /// Writes `path` (relative to the top level) with `bytes`, its folders made.
        pub fn write(&self, path: &str, bytes: impl AsRef<[u8]>) {
            let file = self.top.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, bytes).unwrap();
        }

        /// `git add -A` and a commit with `message`; its id.
        pub fn commit(&self, message: &str) -> String {
            self.run(&["add", "-A"]);
            self.run(&["commit", "-q", "--allow-empty", "-m", message]);
            self.rev("HEAD")
        }

        /// The id `rev` names.
        pub fn rev(&self, rev: &str) -> String {
            self.run(&["rev-parse", "--verify", rev]).trim().to_string()
        }

        /// A runner on this machine's git with the fixture's environment.
        pub fn runner(&self) -> crate::git::runner::GitRunner {
            crate::git::runner::GitRunner::local(self.local())
        }

        /// This machine's git with the fixture's environment.
        pub fn local(&self) -> crate::git::runner::LocalGit {
            env(&self.home())
                .into_iter()
                .fold(crate::git::runner::LocalGit::new(&self.git), |g, (k, v)| {
                    g.with_env(k, v)
                })
        }
    }

    /// A history of `total` commits made by `git fast-import` (03 §4.5's generator: a
    /// two-commit side branch merged into `main` every 50 commits), no files, one second
    /// apart; `main` checked out at its tip.
    pub fn big(git: &Path, total: usize) -> Repo {
        let repo = Repo::new(git);
        let mut stream = String::with_capacity(total * 120);
        fn mark(
            stream: &mut String,
            made: &mut usize,
            branch: &str,
            from: Option<usize>,
            merge: Option<usize>,
        ) -> usize {
            *made += 1;
            let (n, t) = (*made, 1_700_000_000 + *made);
            stream.push_str(&format!(
                "commit refs/heads/{branch}\nmark :{n}\nauthor Ann <ann@example.com> {t} +0000\n\
                 committer Ann <ann@example.com> {t} +0000\ndata <<END\ncommit {n}\nEND\n"
            ));
            if let Some(from) = from {
                stream.push_str(&format!("from :{from}\n"));
            }
            if let Some(merge) = merge {
                stream.push_str(&format!("merge :{merge}\n"));
            }
            stream.push('\n');
            n
        }
        let mut made = 0usize;
        let mut main: Option<usize> = None;
        let mut since_merge = 0;
        while made < total {
            if since_merge >= 50 && total - made >= 3 {
                let base = main.unwrap();
                let one = mark(&mut stream, &mut made, "side", Some(base), None);
                let two = mark(&mut stream, &mut made, "side", Some(one), None);
                main = Some(mark(&mut stream, &mut made, "main", Some(base), Some(two)));
                since_merge = 0;
            } else {
                main = Some(mark(&mut stream, &mut made, "main", main, None));
                since_merge += 1;
            }
        }
        stream.push_str("done\n");
        let mut c = Command::new(git);
        c.current_dir(&repo.top)
            .args(["fast-import", "--done", "--quiet"])
            .envs(env(&repo.home()))
            .stdin(std::process::Stdio::piped());
        for key in crate::git::runner::LOCAL_REPO_ENV {
            c.env_remove(key);
        }
        let mut child = c.spawn().unwrap();
        {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(stream.as_bytes())
                .unwrap();
        }
        assert!(child.wait().unwrap().success(), "fast-import");
        repo.run(&["reset", "-q", "--hard", "main"]);
        repo
    }

    /// 03 §4.3's wide shape: `lanes` branches committed in turn, `per` commits each a round,
    /// each merged back into `main` after its round — about `lanes` lanes open at once. No
    /// files; `main` checked out.
    pub fn wide(git: &Path, lanes: usize, rounds: usize, per: usize) -> Repo {
        let repo = Repo::new(git);
        let mut stream = String::new();
        let mut made = 0usize;
        let mut commit = |stream: &mut String, branch: &str, parents: &[usize]| {
            made += 1;
            let t = 1_700_000_000 + made;
            stream.push_str(&format!(
                "commit refs/heads/{branch}\nmark :{made}\nauthor Ann <ann@example.com> {t} +0000\n\
                 committer Ann <ann@example.com> {t} +0000\ndata <<END\n{branch} {made}\nEND\n"
            ));
            if let Some(first) = parents.first() {
                stream.push_str(&format!("from :{first}\n"));
            }
            for other in parents.iter().skip(1) {
                stream.push_str(&format!("merge :{other}\n"));
            }
            stream.push('\n');
            made
        };
        let mut main = commit(&mut stream, "main", &[]);
        let mut tips: Vec<usize> = (0..lanes).map(|_| main).collect();
        for _ in 0..rounds {
            for (b, tip) in tips.iter_mut().enumerate() {
                for _ in 0..per {
                    *tip = commit(&mut stream, &format!("b{b}"), &[*tip]);
                }
            }
            for tip in &tips {
                main = commit(&mut stream, "main", &[main, *tip]);
            }
        }
        stream.push_str("done\n");
        let mut c = Command::new(git);
        c.current_dir(&repo.top)
            .args(["fast-import", "--done", "--quiet"])
            .envs(env(&repo.home()))
            .stdin(std::process::Stdio::piped());
        for key in crate::git::runner::LOCAL_REPO_ENV {
            c.env_remove(key);
        }
        let mut child = c.spawn().unwrap();
        {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(stream.as_bytes())
                .unwrap();
        }
        assert!(child.wait().unwrap().success(), "fast-import");
        repo.run(&["reset", "-q", "--hard", "main"]);
        repo
    }

    /// The real thing (Linux only: macOS starts a script that is still open for writing): a
    /// stand-in in `git`'s place whose write descriptor is still open — as a child another test
    /// thread forked holds it until its exec — is busy; the fixture's command runs once the
    /// descriptor is closed. The descriptor is closed only after the fixture's start was refused
    /// ([`crate::testutil::busy_refusals`]; 5 s at most, so a fixture that never retries still
    /// ends), never on a timer: a stall of this thread cannot let a start that does not retry
    /// pass.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_stand_in_git_still_open_for_writing_runs_once_it_is_closed() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};
        let Some(git) = git() else {
            return;
        };
        let mut repo = Repo::new(&git);
        let wrapper = repo.tmp.path().join("git-wrapper");
        let mut file = std::fs::File::create(&wrapper).unwrap();
        write!(file, "#!/bin/sh\nexec '{}' \"$@\"\n", git.display()).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = Command::new(&wrapper).output().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::ExecutableFileBusy, "{err}");
        repo.git = wrapper.clone();
        let refused = || crate::testutil::busy_refusals(&wrapper);
        let top = std::thread::scope(|s| {
            s.spawn(move || {
                let until = Instant::now() + Duration::from_secs(5);
                while refused() == 0 && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(5));
                }
                drop(file);
            });
            repo.run(&["rev-parse", "--show-toplevel"])
        });
        assert!(
            refused() >= 1,
            "the fixture's start was refused, then retried"
        );
        assert_eq!(top.trim(), repo.top());
    }
}

/// [`RunnerGit`] and [`RunnerPipe`] (S2-F): a parked stream's stop (E5), the read limit and the
/// outside stop a waiting page needs (S2-B's hand-over), locally and through `ShellGit`.
#[cfg(test)]
mod runner_tests {
    use super::fixture::{self, Repo};
    use super::*;
    use crate::git::runner::{Ending, GitRunner, Handle, Limits, ShellGit, StopBy};
    use std::time::{Duration, Instant};

    /// The two lanes: this machine's git, and a remote's words through a local `sh` (03 §3.2).
    fn lanes(repo: &Repo, limits: Limits) -> Vec<(&'static str, GitRunner)> {
        let shell = fixture::env(&repo.home())
            .into_iter()
            .fold(ShellGit::new("/bin/sh", "box"), |g, (k, v)| {
                g.with_env(k, v)
            })
            .with_env("PATH", std::env::var_os("PATH").unwrap_or_default());
        vec![
            ("local", GitRunner::local(repo.local()).with_limits(limits)),
            ("sh", GitRunner::shell(shell).with_limits(limits)),
        ]
    }

    /// Waits up to `limit` for `done`; how long it took.
    fn within(limit: Duration, done: impl Fn() -> bool) -> Option<Duration> {
        let t = Instant::now();
        while t.elapsed() < limit {
            if done() {
                return Some(t.elapsed());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    /// E5 / 03 §3.5: a 100 000-commit stream parked after its first page and then stopped is
    /// gone within 1.5 s — its stop reads and drops what git still writes, and finishes on a
    /// thread of its own (the log has its `stopped` entry, the host's slot is free), without the
    /// caller waiting.
    #[test]
    fn e5_a_parked_streams_stop_is_over_within_1_5_s() {
        let Some(git) = fixture::git() else {
            return;
        };
        let repo = fixture::big(&git, 100_000);
        let tip = Oid::parse(&repo.rev("main")).unwrap();
        for (lane, runner) in lanes(&repo, Limits::default()) {
            let reads = RunnerGit::new(runner.clone(), Handle::new());
            let current = Generation::new();
            let g = current.bump();
            let options = LogOptions::default();
            let mut stream = LogStream::start(
                &reads,
                repo.top(),
                &options,
                std::slice::from_ref(&tip),
                &current,
                g,
            )
            .unwrap_or_else(|e| panic!("{lane}: {e:?}"));
            let page = stream.page(PAGE);
            assert_eq!(
                (page.rows.len(), &page.end),
                (PAGE, &PageEnd::More),
                "{lane}"
            );
            std::thread::sleep(Duration::from_millis(200));
            assert!(stream.running(), "{lane}: parked, not ended");
            let t = Instant::now();
            stream.stop();
            assert!(
                t.elapsed() < Duration::from_millis(200),
                "{lane}: stop waited {:?}",
                t.elapsed()
            );
            let over = within(Duration::from_millis(1500), || {
                runner.in_flight() == 0
                    && runner.log().last().is_some_and(|e| {
                        e.line.contains(" log ")
                            && matches!(&e.ending, Ending::Stopped(s) if s.by == StopBy::Asked)
                    })
            });
            assert!(
                over.is_some(),
                "{lane}: not over in 1.5 s: {:?}, {} in flight",
                runner.log().last().map(|e| e.status_line()),
                runner.in_flight()
            );
            eprintln!("E5 {lane}: a parked stream's stop was over in {over:?}");
        }
    }

    /// A command that prints a little and then nothing for 30 s (a git alias, so no file is
    /// written for it): the stall a waiting page meets.
    fn stall() -> Vec<String> {
        ["-c", "alias.stall=!printf ab && sleep 30", "stall"]
            .map(String::from)
            .into()
    }

    /// S2-B's hand-over (a): a page waiting on a silent git ends with an error once the read
    /// limit (03 §3.4: 30 s, 1 s here) has passed without a byte, and git is stopped.
    #[test]
    fn a_silent_pipe_fails_at_the_read_limit() {
        let Some(git) = fixture::git() else {
            return;
        };
        let repo = Repo::new(&git);
        let limits = Limits {
            read_idle: Duration::from_secs(1),
            ..Limits::default()
        };
        for (lane, runner) in lanes(&repo, limits) {
            let reads = RunnerGit::new(runner.clone(), Handle::new());
            let mut pipe = reads.spawn(repo.top(), &stall(), b"").unwrap();
            let mut buf = [0u8; 64];
            let mut got = Vec::new();
            let t = Instant::now();
            let err = loop {
                match pipe.read(&mut buf) {
                    Ok(0) => panic!("{lane}: a quiet end, not the limit"),
                    Ok(n) => got.extend_from_slice(&buf[..n]),
                    Err(e) => break e,
                }
            };
            let took = t.elapsed();
            assert_eq!(got, b"ab", "{lane}");
            assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{lane}: {err}");
            assert!(err.to_string().contains("did not answer"), "{lane}: {err}");
            assert!(
                took >= Duration::from_millis(900) && took < Duration::from_secs(8),
                "{lane}: {took:?}"
            );
            // The page's end lets the command go: it was stopped by the limit, not by the drop.
            pipe.stop();
            let ended = within(Duration::from_secs(8), || {
                runner.in_flight() == 0
                    && runner.log().last().is_some_and(|e| {
                        matches!(&e.ending, Ending::Stopped(s) if matches!(s.by, StopBy::Limit(_)))
                    })
            });
            assert!(
                ended.is_some(),
                "{lane}: {:?}",
                runner.log().last().map(|e| e.status_line())
            );
        }
    }

    /// S2-B's hand-over (b): Esc, or a load superseded while a page waits, stops the command
    /// through the handle from another thread, and the waiting read returns with an error at
    /// once; the next command given the same handle is not sent.
    #[test]
    fn a_stop_through_the_handle_ends_a_waiting_read() {
        let Some(git) = fixture::git() else {
            return;
        };
        let repo = Repo::new(&git);
        for (lane, runner) in lanes(&repo, Limits::default()) {
            let handle = Handle::new();
            let reads = RunnerGit::new(runner.clone(), handle.clone());
            let mut pipe = reads.spawn(repo.top(), &stall(), b"").unwrap();
            let mut buf = [0u8; 2];
            assert_eq!(pipe.read(&mut buf).unwrap(), 2, "{lane}");
            let stopper = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                handle.stop();
            });
            let t = Instant::now();
            let err = pipe.read(&mut buf).expect_err(lane);
            assert!(
                t.elapsed() < Duration::from_secs(7),
                "{lane}: {:?}",
                t.elapsed()
            );
            assert_eq!(err.to_string(), "stopped", "{lane}");
            stopper.join().unwrap();
            let again = reads.run(repo.top(), &["version".into()], b"", Bound::Read);
            assert!(
                matches!(&again, Err(RunError::Failed(why)) if why.contains("not sent")),
                "{lane}: {again:?}"
            );
            pipe.stop();
            assert!(
                within(Duration::from_secs(8), || runner.in_flight() == 0).is_some(),
                "{lane}"
            );
        }
    }

    /// Notes 7 and 10 (S2-B's words against S2-A's runner): every read's `argv` reaches a remote
    /// unchanged — `SshGit::words` carries each word, quoted, after git's own options, and
    /// adds nothing between them.
    #[test]
    fn every_argv_reaches_the_remote_as_written() {
        use crate::git::runner::{Class, GitCommand, SshGit};
        let oid = Oid::parse(&"a".repeat(40)).unwrap();
        let query = argv::Query {
            message: "fix it".into(),
            path: "src/x y.rs".into(),
            ..Default::default()
        };
        let all: Vec<Vec<String>> = vec![
            argv::version(),
            argv::detection(),
            argv::refs(),
            argv::symbolic_head(),
            argv::verify_head(),
            argv::stashes(),
            argv::status(),
            argv::log(&LogOptions::default()),
            argv::details(&oid),
            argv::name_status(&oid, &oid),
            argv::numstat(&oid, &oid),
            argv::empty_tree(),
            argv::remotes(),
            argv::contains(&oid),
            argv::worktree_diff("--numstat", &oid),
            argv::uncommitted_numstat(true),
            argv::others("dir/"),
            argv::diff_text(&argv::Side::Tree(oid.clone()), &argv::Side::Index, &["a b"]),
            argv::blob(Some(&oid), "a'b"),
            argv::renames_since(&oid),
            argv::search(&LogOptions::default(), &query),
        ];
        for words in all {
            let cmd = GitCommand::new(Class::Read, "/r", &words);
            let sent: Vec<String> = SshGit::words(&cmd)
                .into_iter()
                .map(|w| w.to_string_lossy().into_owned())
                .collect();
            let quoted: Vec<String> = words
                .iter()
                .map(|w| String::from_utf8(crate::git::runner::quote(w.as_bytes())).unwrap())
                .collect();
            assert!(
                sent.ends_with(&quoted),
                "{words:?} not at the end of {sent:?}"
            );
            let at = sent.len() - quoted.len();
            assert_eq!(
                sent[at - 1],
                "'--no-pager'",
                "{words:?}: git's options end before them"
            );
        }
    }

    /// 03 G-P1's wide half (S2-B's hand-over): a history with many lanes open at once — 60
    /// branches committed in turn and merged back — gives its first page within the same 5 s
    /// bound, and the layout stores about 2 segments per row however wide it is (no per-lane
    /// storage).
    #[test]
    fn g_p1_a_wide_history_pages_in_time_and_stores_few_segments() {
        let Some(git) = fixture::git() else {
            return;
        };
        let repo = fixture::wide(&git, 60, 20, 2);
        let tip = Oid::parse(&repo.rev("main")).unwrap();
        let reads = RunnerGit::new(repo.runner(), Handle::new());
        let current = Generation::new();
        let g = current.bump();
        let t = std::time::Instant::now();
        let mut stream = LogStream::start(
            &reads,
            repo.top(),
            &LogOptions::default(),
            std::slice::from_ref(&tip),
            &current,
            g,
        )
        .unwrap();
        let page = stream.page(PAGE);
        let took = t.elapsed();
        eprintln!(
            "G-P1 wide: first page of {} rows in {took:?}",
            page.rows.len()
        );
        assert_eq!(page.rows.len(), PAGE);
        assert!(took < Duration::from_secs(5), "{took:?}");
        let mut rows = crate::git::graph::RowStream::new(Some(tip), false, Vec::new(), false);
        let mut all = rows.page(page.rows);
        loop {
            let next = stream.page(PAGE);
            let end = next.end.clone();
            all.extend(rows.page(next.rows));
            if end != PageEnd::More {
                break;
            }
        }
        let graph = crate::git::graph::layout(&all, false);
        let per_row = graph.segment_count() as f64 / graph.len() as f64;
        eprintln!(
            "G-P1 wide: {} rows, {} lanes, {per_row:.2} stored segments per row",
            graph.len(),
            graph.width()
        );
        assert!(graph.width() >= 50, "wide: {} lanes", graph.width());
        assert!(per_row < 2.5, "{per_row:.2} segments per row");
    }

    /// The adapter's one-shot reads: an exit is the exit, a missing program is `NoGit`, the
    /// words run as `argv` writes them.
    #[test]
    fn a_read_through_the_runner_is_the_reads_own() {
        let Some(git) = fixture::git() else {
            return;
        };
        let repo = Repo::new(&git);
        repo.write("a.txt", "a\n");
        let first = repo.commit("first");
        for (lane, runner) in lanes(&repo, Limits::default()) {
            let reads = RunnerGit::new(runner, Handle::new());
            let head = head(&reads, repo.top()).unwrap();
            assert_eq!(head.oid().map(Oid::as_str), Some(first.as_str()), "{lane}");
            let refs = refs(&reads, repo.top()).unwrap();
            assert_eq!(refs.len(), 1, "{lane}: {refs:?}");
            let bad = reads.run(
                repo.top(),
                &["rev-parse".into(), "nope".into()],
                b"",
                Bound::Read,
            );
            assert_eq!(bad.as_ref().map(|r| r.code), Ok(Some(128)), "{lane}");
        }
        let missing = GitRunner::local(crate::git::runner::LocalGit::new("/nonexistent/git"));
        let reads = RunnerGit::new(missing, Handle::new());
        assert_eq!(
            reads.run(repo.top(), &argv::version(), b"", Bound::Detection),
            Err(RunError::NoGit)
        );
    }
}
