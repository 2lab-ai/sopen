//! The parsers (03 §4.1, §6.1, §8.5): what each read prints, into [`super::model`]'s types —
//! a record that does not parse is an error, never a panic. S2-B writes it.
//!
//! Every parser takes git's bytes as they came: records end in NUL or a newline as the read's
//! form says, names and paths are bytes (a name that is not UTF-8 shows with U+FFFD and its
//! `lossy` flag is set), and text is cut at a character boundary. §8.5's probe has no parser
//! yet: 03 gives its outputs, not its printed form.

use super::model::*;

/// A record that does not parse: what was wrong, for the paging line or the read's error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

fn error<T>(what: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError(what.into()))
}

/// `%aN` and `%aE` are cut at 256 bytes (03 §4.5).
pub const NAME_CAP: usize = 256;
/// `%s` is cut at 1 KiB (03 §4.5).
pub const SUBJECT_CAP: usize = 1024;

/// `text` cut to at most `max` bytes at a character boundary.
pub fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut at = max;
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        text.truncate(at);
    }
    text
}

/// Bytes as text: U+FFFD for what is not UTF-8, and whether there was any.
fn text(bytes: &[u8]) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), false),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), true),
    }
}

/// Bytes as text that is only shown.
fn shown(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn oid(bytes: &[u8], what: &str) -> Result<Oid, ParseError> {
    match std::str::from_utf8(bytes).ok().and_then(Oid::parse) {
        Some(oid) => Ok(oid),
        None => error(format!("{what} is not an object id: {:?}", shown(bytes))),
    }
}

/// `%P`'s space-separated ids; empty for none.
fn oids(bytes: &[u8], what: &str) -> Result<Vec<Oid>, ParseError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    bytes.split(|&b| b == b' ').map(|p| oid(p, what)).collect()
}

fn number<T: std::str::FromStr>(bytes: &[u8], what: &str) -> Result<T, ParseError> {
    match std::str::from_utf8(bytes).ok().and_then(|s| s.parse().ok()) {
        Some(n) => Ok(n),
        None => error(format!("{what} is not a number: {:?}", shown(bytes))),
    }
}

/// `record` split at `sep` into exactly `n` fields, the last taking the rest.
fn fields<'a>(
    record: &'a [u8],
    sep: u8,
    n: usize,
    what: &str,
) -> Result<Vec<&'a [u8]>, ParseError> {
    let f: Vec<&[u8]> = record.splitn(n, |&b| b == sep).collect();
    if f.len() != n {
        return error(format!("{what} has {} fields, not {n}", f.len()));
    }
    Ok(f)
}

/// Records ending in `end`: the last may lack it; an empty tail is no record.
fn records(out: &[u8], end: u8) -> impl Iterator<Item = &[u8]> {
    let out = out.strip_suffix(&[end]).unwrap_or(out);
    out.split(move |&b| b == end)
        .filter(move |_| !out.is_empty())
}

/// stderr's first line, as git printed it.
fn first_line(stderr: &[u8]) -> String {
    shown(stderr)
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim_end()
        .to_string()
}

/// `path` with `.` and `..` taken lexically, `/` joined.
fn normalized(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Detection's `rev-parse` (03 §2.1) as it ended — `code` (`None`: no exit status), stdout,
/// stderr — run with `-C folder`.
pub fn detection(code: Option<i32>, stdout: &[u8], stderr: &[u8], folder: &str) -> Detection {
    let failed = |why: String| Detection::Unavailable(Unavailable::Failed(why));
    let err = shown(stderr);
    match code {
        Some(0) => work_tree(stdout, folder).unwrap_or_else(|e| failed(e.0)),
        Some(128) if err.contains("safe.directory") || err.contains("dubious ownership") => {
            Detection::Refused(first_line(stderr))
        }
        Some(128) if err.contains("must be run in a work tree") => {
            match stdout.split(|&b| b == b'\n').next() {
                Some(b"true") => Detection::NoWorkTree(NoWorkTree::Bare),
                _ => Detection::NoWorkTree(NoWorkTree::GitDir),
            }
        }
        Some(128)
            if err.contains("Stopping at filesystem boundary")
                || err.contains("up to mount point") =>
        {
            Detection::NotARepo(Some(first_line(stderr)))
        }
        Some(128) if err.contains("not a git repository") => Detection::NotARepo(None),
        None => failed("git ended without an exit status".into()),
        Some(c) => match first_line(stderr) {
            line if line.is_empty() => failed(format!("git exited with {c}")),
            line => failed(line),
        },
    }
}

/// Exit 0: `false`, `false`, shallow, git dir, common dir, top level [, superproject].
fn work_tree(stdout: &[u8], folder: &str) -> Result<Detection, ParseError> {
    let lines: Vec<&[u8]> = records(stdout, b'\n').collect();
    if !matches!(lines.len(), 6 | 7) {
        return error(format!("git's answer has {} lines", lines.len()));
    }
    let mut flags = [false; 3];
    for (flag, line) in flags.iter_mut().zip(&lines) {
        *flag = match *line {
            b"true" => true,
            b"false" => false,
            other => {
                return error(format!(
                    "git's answer is not true or false: {}",
                    shown(other)
                ))
            }
        };
    }
    let (top, top_lossy) = text(lines[5]);
    if top_lossy {
        return Ok(Detection::Unavailable(Unavailable::NotUtf8(top)));
    }
    if !top.starts_with('/') {
        return error(format!("the top level is not absolute: {top}"));
    }
    let git_dir = shown(lines[3]);
    let common = shown(lines[4]);
    // git prints the common dir relative to the folder's real path only when it is the git dir
    // it found there; a linked worktree's and a submodule's are printed absolute.
    let (common_dir, linked) = if common.starts_with('/') {
        let linked = lines[4] != lines[3];
        (common, linked)
    } else {
        (normalized(&format!("{folder}/{common}")), false)
    };
    Ok(Detection::WorkTree {
        top,
        git_dir,
        common_dir,
        linked,
        submodule_of: lines.get(6).map(|l| shown(l)),
        shallow: flags[2],
    })
}

/// `git version`'s stdout; `None` when it is not `git version <n>.<n>…`.
pub fn git_version(stdout: &[u8]) -> Option<GitVersion> {
    let out = std::str::from_utf8(stdout).ok()?;
    let rest = out.lines().next()?.strip_prefix("git version ")?.trim();
    let numbers = rest.split([' ', '-']).next()?;
    let mut parts = numbers.split('.').map(|p| p.parse::<u32>());
    let major = parts.next()?.ok()?;
    let minor = parts.next()?.ok()?;
    let patch = parts.next().and_then(Result::ok).unwrap_or(0);
    Some(GitVersion {
        major,
        minor,
        patch,
        text: rest.to_string(),
    })
}

/// The refs read (03 §4.1): one ref per line, six fields split at NUL.
pub fn refs(stdout: &[u8]) -> Result<Vec<RefLabel>, ParseError> {
    records(stdout, b'\n').map(ref_line).collect()
}

fn ref_line(line: &[u8]) -> Result<RefLabel, ParseError> {
    let f: Vec<&[u8]> = line.split(|&b| b == 0).collect();
    if f.len() != 6 {
        return error(format!("a ref line has {} fields, not 6", f.len()));
    }
    let object = oid(f[0], "a ref's object")?;
    let object_type = match f[1] {
        b"commit" => ObjectType::Commit,
        b"tag" => ObjectType::Tag,
        b"tree" => ObjectType::Tree,
        b"blob" => ObjectType::Blob,
        other => return error(format!("not an object type: {:?}", shown(other))),
    };
    let peeled = match f[2] {
        b"" => None,
        p => Some(oid(p, "a tag's target")?),
    };
    let (refname, name_lossy) = text(f[3]);
    let kind = if refname.starts_with("refs/heads/") {
        RefKind::LocalBranch
    } else if refname.starts_with("refs/remotes/") {
        RefKind::RemoteBranch
    } else if refname.starts_with("refs/tags/") {
        RefKind::Tag
    } else {
        return error(format!("not a branch or a tag: {refname:?}"));
    };
    let (upstream, upstream_lossy) = match f[4] {
        b"" => (None, false),
        u => {
            let (u, lossy) = text(u);
            (Some(u), lossy)
        }
    };
    Ok(RefLabel {
        object,
        object_type,
        peeled,
        refname,
        kind,
        upstream,
        track: shown(f[5]),
        lossy: name_lossy || upstream_lossy,
    })
}

/// The HEAD read (03 §4.1): `symbolic-ref -q HEAD`'s exit and stdout, then `rev-parse -q
/// --verify HEAD`'s.
pub fn head(
    symbolic: (Option<i32>, &[u8]),
    verify: (Option<i32>, &[u8]),
) -> Result<Head, ParseError> {
    let line = |out: &[u8]| -> Vec<u8> { out.strip_suffix(b"\n").unwrap_or(out).to_vec() };
    let commit = match verify {
        (Some(0), out) => Some(oid(&line(out), "HEAD")?),
        (Some(1), _) => None,
        (code, _) => return error(format!("rev-parse --verify HEAD ended with {code:?}")),
    };
    match (symbolic, commit) {
        ((Some(0), out), commit) => {
            let name = line(out);
            if name.is_empty() {
                return error("symbolic-ref printed no branch");
            }
            let (refname, lossy) = text(&name);
            Ok(match commit {
                Some(oid) => Head::Branch {
                    refname,
                    lossy,
                    oid,
                },
                None => Head::Unborn { refname, lossy },
            })
        }
        ((Some(1), _), Some(oid)) => Ok(Head::Detached(oid)),
        ((Some(1), _), None) => error("HEAD is neither a branch nor a commit"),
        ((code, _), _) => error(format!("symbolic-ref HEAD ended with {code:?}")),
    }
}

/// The stashes read (03 §4.1): NUL-terminated records, five fields split at `\x1f`.
pub fn stashes(stdout: &[u8]) -> Result<Vec<StashRow>, ParseError> {
    records(stdout, 0).map(stash_record).collect()
}

fn stash_record(record: &[u8]) -> Result<StashRow, ParseError> {
    let f = fields(record, 0x1f, 5, "a stash record")?;
    let parents = oids(f[1], "a stash's parent")?;
    if parents.len() > 3 {
        return error(format!("a stash has {} parents", parents.len()));
    }
    let mut parents = parents.into_iter();
    let Some(base) = parents.next() else {
        return error("a stash without its base");
    };
    Ok(StashRow {
        oid: oid(f[0], "a stash")?,
        base,
        index: parents.next(),
        untracked: parents.next(),
        selector: shown(f[2]),
        commit_time: number(f[3], "a stash's date")?,
        subject: shown(f[4]),
    })
}

/// The status read (03 §4.1): `status --porcelain=v2 -z --branch`'s headers and entries.
pub fn status(stdout: &[u8]) -> Result<Status, ParseError> {
    let mut branch = BranchStatus::default();
    let mut entries = Vec::new();
    let mut tokens = records(stdout, 0);
    while let Some(token) = tokens.next() {
        let (kind, rest) = match token.split_first() {
            Some((k, rest)) if rest.first() == Some(&b' ') => (*k, &rest[1..]),
            _ => return error(format!("not a status record: {:?}", shown(token))),
        };
        match kind {
            b'#' => header(&mut branch, rest)?,
            b'1' => {
                let f = fields(rest, b' ', 8, "a status record")?;
                let state = changed(f[0])?;
                entries.push(entry(f[7], None, state));
            }
            b'2' => {
                let f = fields(rest, b' ', 9, "a rename record")?;
                let state = changed(f[0])?;
                let Some(old) = tokens.next() else {
                    return error("a rename record without its old path");
                };
                entries.push(entry(f[8], Some(old), state));
            }
            b'u' => {
                let f = fields(rest, b' ', 10, "an unmerged record")?;
                if f[0].len() != 2 {
                    return error(format!("not a conflict's XY: {:?}", shown(f[0])));
                }
                entries.push(entry(f[9], None, StatusState::Conflict(shown(f[0]))));
            }
            b'?' => entries.push(entry(rest, None, StatusState::Untracked)),
            b'!' => {}
            other => return error(format!("not a status record kind: {:?}", other as char)),
        }
    }
    Ok(Status { branch, entries })
}

fn header(branch: &mut BranchStatus, line: &[u8]) -> Result<(), ParseError> {
    let (key, value) = match line.iter().position(|&b| b == b' ') {
        Some(at) => (&line[..at], &line[at + 1..]),
        None => (line, &b""[..]),
    };
    match key {
        b"branch.oid" => {
            branch.oid = match value {
                b"(initial)" => None,
                v => Some(oid(v, "branch.oid")?),
            };
        }
        b"branch.head" => {
            branch.head = match value {
                b"(detached)" => None,
                v => {
                    let (head, lossy) = text(v);
                    branch.lossy |= lossy;
                    Some(head)
                }
            };
        }
        b"branch.upstream" => {
            let (upstream, lossy) = text(value);
            branch.lossy |= lossy;
            branch.upstream = Some(upstream);
        }
        b"branch.ab" => {
            let f = fields(value, b' ', 2, "branch.ab")?;
            match (f[0].strip_prefix(b"+"), f[1].strip_prefix(b"-")) {
                (Some(a), Some(b)) => {
                    branch.ahead_behind = Some(AheadBehind {
                        ahead: number(a, "ahead")?,
                        behind: number(b, "behind")?,
                    });
                }
                _ => return error(format!("not branch.ab: {:?}", shown(value))),
            }
        }
        _ => {}
    }
    Ok(())
}

fn entry(path: &[u8], old: Option<&[u8]>, state: StatusState) -> StatusEntry {
    let (path, path_lossy) = text(path);
    let (old_path, old_lossy) = match old {
        Some(o) => {
            let (o, lossy) = text(o);
            (Some(o), lossy)
        }
        None => (None, false),
    };
    StatusEntry {
        path,
        old_path,
        state,
        lossy: path_lossy || old_lossy,
    }
}

/// Porcelain v2's `XY` of a tracked entry: index, then work tree.
fn changed(xy: &[u8]) -> Result<StatusState, ParseError> {
    let letter = |b: u8| -> Result<Option<Change>, ParseError> {
        Ok(Some(match b {
            b'.' => return Ok(None),
            b'A' => Change::Added,
            b'M' => Change::Modified,
            b'D' => Change::Deleted,
            b'R' => Change::Renamed,
            b'T' => Change::TypeChanged,
            b'C' => Change::Copied,
            other => return error(format!("not a change letter: {:?}", other as char)),
        }))
    };
    match xy {
        [x, y] => Ok(StatusState::Changed {
            staged: letter(*x)?,
            unstaged: letter(*y)?,
        }),
        _ => error(format!("not an XY: {:?}", shown(xy))),
    }
}

/// One record of the log stream (03 §4.1), its NUL terminator already taken off: seven fields
/// split at `\x1f`, the subject last so a stray `\x1f` in it stays in it.
pub fn commit_record(record: &[u8]) -> Result<CommitRow, ParseError> {
    let f = fields(record, 0x1f, 7, "a commit record")?;
    Ok(CommitRow {
        oid: oid(f[0], "a commit")?,
        parents: oids(f[1], "a parent")?,
        author_name: cut(shown(f[2]), NAME_CAP),
        author_email: cut(shown(f[3]), NAME_CAP),
        author_time: number(f[4], "the author date")?,
        commit_time: number(f[5], "the commit date")?,
        subject: cut(shown(f[6]), SUBJECT_CAP),
    })
}

/// The details read (03 §6.1): one NUL-terminated record, nine fields, `%B` last.
pub fn details(stdout: &[u8]) -> Result<CommitDetails, ParseError> {
    let record = stdout.strip_suffix(&[0]).unwrap_or(stdout);
    let f = fields(record, 0x1f, 9, "a details record")?;
    let (message, message_lossy) = text(f[8]);
    Ok(CommitDetails {
        oid: oid(f[0], "a commit")?,
        parents: oids(f[1], "a parent")?,
        author_name: shown(f[2]),
        author_email: shown(f[3]),
        author_time: number(f[4], "the author date")?,
        committer_name: shown(f[5]),
        committer_email: shown(f[6]),
        commit_time: number(f[7], "the commit date")?,
        message,
        message_lossy,
    })
}

/// `hash-object -t tree --stdin`'s stdout: the empty tree's id (03 §6.2), whichever hash the
/// repository uses.
pub fn object_id(stdout: &[u8]) -> Result<Oid, ParseError> {
    oid(
        stdout.strip_suffix(b"\n").unwrap_or(stdout),
        "hash-object's answer",
    )
}

/// One `--name-status -z` row: the letter, the path and a rename's or copy's old path.
type Named<'a> = (u8, &'a [u8], Option<&'a [u8]>);

/// One `--numstat -z` row: the line counts (`None`: binary), the path and an old path.
type Counted<'a> = (Option<(u64, u64)>, &'a [u8], Option<&'a [u8]>);

/// The details pane's Files (03 §6.1): `--name-status`'s and `--numstat`'s `-z` output of the
/// same two endpoints, joined row by row (git lists both in the same order).
pub fn changed_files(name_status: &[u8], numstat: &[u8]) -> Result<Vec<ChangedFile>, ParseError> {
    let mut named: Vec<Named> = Vec::new();
    let mut tokens = records(name_status, 0);
    while let Some(status) = tokens.next() {
        let Some(&letter) = status.first() else {
            return error("an empty name-status letter");
        };
        let Some(first) = tokens.next() else {
            return error("a name-status row without its path");
        };
        if matches!(letter, b'R' | b'C') {
            let Some(new) = tokens.next() else {
                return error("a rename without its new path");
            };
            named.push((letter, new, Some(first)));
        } else {
            named.push((letter, first, None));
        }
    }

    let mut counted: Vec<Counted> = Vec::new();
    let mut tokens = records(numstat, 0);
    while let Some(row) = tokens.next() {
        let f = fields(row, b'\t', 3, "a numstat row")?;
        let lines = match (f[0], f[1]) {
            (b"-", b"-") => None,
            (a, d) => Some((number(a, "added lines")?, number(d, "deleted lines")?)),
        };
        if f[2].is_empty() {
            let (Some(old), Some(new)) = (tokens.next(), tokens.next()) else {
                return error("a numstat rename without its paths");
            };
            counted.push((lines, new, Some(old)));
        } else {
            counted.push((lines, f[2], None));
        }
    }

    if named.len() != counted.len() {
        return error(format!(
            "name-status lists {} paths, numstat {}",
            named.len(),
            counted.len()
        ));
    }
    named
        .into_iter()
        .zip(counted)
        .map(
            |((letter, path, old), (lines, counted_path, counted_old))| {
                if path != counted_path || old != counted_old {
                    return error(format!(
                        "name-status and numstat disagree at {:?}",
                        shown(path)
                    ));
                }
                let letter = match letter {
                    b'A' => FileLetter::Change(Change::Added),
                    b'M' => FileLetter::Change(Change::Modified),
                    b'D' => FileLetter::Change(Change::Deleted),
                    b'R' => FileLetter::Change(Change::Renamed),
                    b'T' => FileLetter::Change(Change::TypeChanged),
                    b'C' => FileLetter::Change(Change::Copied),
                    b'U' => FileLetter::Unmerged,
                    other => return error(format!("not a change letter: {:?}", other as char)),
                };
                let (path, path_lossy) = text(path);
                let (old_path, old_lossy) = match old {
                    Some(o) => {
                        let (o, lossy) = text(o);
                        (Some(o), lossy)
                    }
                    None => (None, false),
                };
                Ok(ChangedFile {
                    letter,
                    path,
                    old_path,
                    lines,
                    lossy: path_lossy || old_lossy,
                })
            },
        )
        .collect()
}
