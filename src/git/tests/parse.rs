//! The parsers' tests (03 §4.1, §6.1, §8.5). S2-B writes them.
//!
//! Live fixtures for G-G1, G-T1 and G-T4 through [`TestGit`], and every parser's bytes table:
//! what git prints, names that are not UTF-8, the caps, and records cut short or garbled — each
//! an answer or an error, never a panic.

use super::history::{framed, oid, real, rev_list, sh, sh_at, sh_with, TestGit};
use crate::git::history::{self, Generation, LogOptions, LogStream, PageEnd, PAGE};
use crate::git::model::*;
use crate::git::parse::{self, NAME_CAP, SUBJECT_CAP};
use std::collections::HashMap;
use std::path::Path;

/// Commits `files` (name, content) in `dir` at `t` with `msg`; the new commit's id.
fn commit(dir: &Path, t: i64, files: &[(&str, &str)], msg: &str) -> String {
    for (name, content) in files {
        std::fs::write(dir.join(name), content).unwrap();
        sh(dir, &["add", name]);
    }
    sh_at(dir, t, &["commit", "-q", "--allow-empty", "-m", msg]);
    sh(dir, &["rev-parse", "HEAD"])
}

/// G-G1's fixture (03:1128): two branches and a merge, an octopus merge, an annotated and a
/// lightweight tag, a remote whose `main` is the local `main` and whose `feature` is behind it,
/// a stash and two uncommitted changes.
struct Gg1 {
    root: String,
    c2: String,
    f1: String,
    f2: String,
    m1: String,
    oct: String,
    o1: String,
    o2: String,
}

fn gg1(tmp: &Path) -> Gg1 {
    let dir = tmp.join("repo");
    std::fs::create_dir(&dir).unwrap();
    sh(&dir, &["init", "-q", "-b", "main"]);
    let t = |n: i64| 1_700_000_000 + n * 60;
    let root = commit(&dir, t(1), &[("a.txt", "1\n"), ("b.txt", "b\n")], "root");
    let c2 = commit(&dir, t(2), &[("a.txt", "2\n")], "second");
    sh_at(&dir, t(3), &["tag", "-a", "v1.0", "-m", "release", &c2]);
    sh(&dir, &["checkout", "-q", "-b", "feature"]);
    let f1 = commit(&dir, t(4), &[("f.txt", "f\n")], "feature work");
    sh(&dir, &["checkout", "-q", "main"]);
    commit(&dir, t(5), &[("c.txt", "c\n")], "main work");
    sh_at(
        &dir,
        t(6),
        &["merge", "-q", "--no-ff", "feature", "-m", "merge feature"],
    );
    let m1 = sh(&dir, &["rev-parse", "HEAD"]);
    sh(&dir, &["tag", "light"]);
    sh(&dir, &["checkout", "-q", "-b", "o1"]);
    let o1 = commit(&dir, t(7), &[("o1.txt", "1\n")], "octo one");
    sh(&dir, &["checkout", "-q", "-b", "o2", "main"]);
    let o2 = commit(&dir, t(8), &[("o2.txt", "2\n")], "octo two");
    sh(&dir, &["checkout", "-q", "main"]);
    sh_at(
        &dir,
        t(9),
        &["merge", "-q", "--no-ff", "o1", "o2", "-m", "octopus"],
    );
    let oct = sh(&dir, &["rev-parse", "HEAD"]);
    sh(tmp, &["init", "-q", "--bare", "origin.git"]);
    sh(&dir, &["remote", "add", "origin", "../origin.git"]);
    sh(&dir, &["push", "-q", "-u", "origin", "main", "feature"]);
    sh(&dir, &["remote", "set-head", "origin", "main"]);
    sh(&dir, &["checkout", "-q", "feature"]);
    let f2 = commit(&dir, t(10), &[("f.txt", "f2\n")], "feature more");
    sh(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("a.txt"), "stashed\n").unwrap();
    sh_at(&dir, t(11), &["stash", "push", "-q", "-m", "wip"]);
    std::fs::write(dir.join("b.txt"), "b changed\n").unwrap();
    std::fs::write(dir.join("n.txt"), "new\n").unwrap();
    Gg1 {
        root,
        c2,
        f1,
        f2,
        m1,
        oct,
        o1,
        o2,
    }
}

/// G-G1 (03 §4.1, §4.4, 03:1128): the fixture's refs and labels — which kind, where each is
/// drawn, upstream and tracking — HEAD, the stash with its parents, the two uncommitted changes
/// with the branch's ahead/behind, and the log's parents (a merge's two, the octopus' three, a
/// root's none) for every commit the labels show.
#[test]
fn g_g1_refs_labels_and_parents() {
    let Some(git) = TestGit::local() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let ids = gg1(tmp.path());
    let top = real(&tmp.path().join("repo"));
    let top = top.as_str();

    let refs = history::refs(&git, top).unwrap();
    let by_name: HashMap<&str, &RefLabel> = refs.iter().map(|r| (r.refname.as_str(), r)).collect();
    let label = |n: &str| {
        *by_name
            .get(n)
            .unwrap_or_else(|| panic!("no {n} in {refs:?}"))
    };
    let main = label("refs/heads/main");
    assert_eq!(
        (main.kind, main.object_type),
        (RefKind::LocalBranch, ObjectType::Commit)
    );
    assert_eq!(main.target().as_str(), ids.oct);
    assert_eq!(main.peeled, None);
    assert_eq!(main.upstream.as_deref(), Some("refs/remotes/origin/main"));
    assert_eq!(main.track, "");
    assert_eq!(main.name(), "main");
    let feature = label("refs/heads/feature");
    assert_eq!(feature.target().as_str(), ids.f2);
    assert_eq!(
        feature.upstream.as_deref(),
        Some("refs/remotes/origin/feature")
    );
    assert_eq!(feature.track, "[ahead 1]");
    let origin_main = label("refs/remotes/origin/main");
    assert_eq!(origin_main.kind, RefKind::RemoteBranch);
    assert_eq!(
        origin_main.target(),
        main.target(),
        "main↔origin: one commit"
    );
    assert_eq!(origin_main.name(), "origin/main");
    let origin_feature = label("refs/remotes/origin/feature");
    assert_eq!(origin_feature.target().as_str(), ids.f1, "behind feature");
    assert_eq!(origin_feature.upstream, None);
    assert_eq!(label("refs/remotes/origin/HEAD").target().as_str(), ids.oct);
    let annotated = label("refs/tags/v1.0");
    assert_eq!(
        (annotated.kind, annotated.object_type),
        (RefKind::Tag, ObjectType::Tag)
    );
    assert_ne!(annotated.object.as_str(), ids.c2, "the tag object's own id");
    assert_eq!(
        annotated.peeled.as_ref().map(Oid::as_str),
        Some(ids.c2.as_str())
    );
    assert_eq!(
        annotated.target().as_str(),
        ids.c2,
        "drawn at what it points at"
    );
    assert_eq!(annotated.name(), "v1.0");
    let light = label("refs/tags/light");
    assert_eq!(
        (light.kind, light.object_type),
        (RefKind::Tag, ObjectType::Commit)
    );
    assert_eq!(
        (light.target().as_str(), light.peeled.as_ref()),
        (ids.m1.as_str(), None)
    );
    assert!(refs.iter().all(|r| !r.lossy));
    assert_eq!(
        refs.len(),
        9,
        "main feature o1 o2, origin/HEAD main feature, v1.0 light"
    );

    let head = history::head(&git, top).unwrap();
    assert_eq!(
        head,
        Head::Branch {
            refname: "refs/heads/main".into(),
            lossy: false,
            oid: oid(&ids.oct),
        }
    );

    let stashes = history::stashes(&git, top).unwrap();
    assert_eq!(stashes.len(), 1);
    let s = &stashes[0];
    assert_eq!(s.selector, "stash@{0}");
    assert_eq!(s.base.as_str(), ids.oct);
    assert_eq!(s.subject, "On main: wip");
    assert_eq!(s.commit_time, 1_700_000_000 + 11 * 60);
    assert_eq!(s.untracked, None);
    let stash_parents = sh(
        &tmp.path().join("repo"),
        &["rev-parse", "stash@{0}^2", "stash@{0}"],
    );
    let mut lines = stash_parents.lines();
    assert_eq!(s.index.as_ref().map(Oid::as_str), lines.next());
    assert_eq!(Some(s.oid.as_str()), lines.next());

    let status = history::status(&git, top).unwrap();
    assert_eq!(
        status.branch,
        BranchStatus {
            oid: Some(oid(&ids.oct)),
            head: Some("main".into()),
            upstream: Some("origin/main".into()),
            ahead_behind: Some(AheadBehind {
                ahead: 0,
                behind: 0
            }),
            lossy: false,
        }
    );
    assert_eq!(
        status.entries,
        vec![
            StatusEntry {
                path: "b.txt".into(),
                old_path: None,
                state: StatusState::Changed {
                    staged: None,
                    unstaged: Some(Change::Modified),
                },
                lossy: false,
            },
            StatusEntry {
                path: "n.txt".into(),
                old_path: None,
                state: StatusState::Untracked,
                lossy: false,
            },
        ],
        "Uncommitted changes (2)"
    );

    let roots = history::roots(&refs, Some(&head), &stashes);
    let generation = Generation::new();
    let load = generation.bump();
    let mut stream =
        LogStream::start(&git, top, &LogOptions::default(), &roots, &generation, load).unwrap();
    let page = stream.page(PAGE);
    assert_eq!(page.end, PageEnd::AllLoaded);
    let parents: HashMap<&str, Vec<&str>> = page
        .rows
        .iter()
        .map(|r| (r.oid.as_str(), r.parents.iter().map(Oid::as_str).collect()))
        .collect();
    assert_eq!(page.rows.len(), 9, "every commit, no stash internals");
    assert_eq!(page.rows[0].oid.as_str(), ids.f2, "the newest first");
    assert_eq!(parents[ids.oct.as_str()].len(), 3);
    assert_eq!(
        &parents[ids.oct.as_str()][1..],
        [ids.o1.as_str(), ids.o2.as_str()]
    );
    assert_eq!(parents[ids.m1.as_str()][1], ids.f1);
    assert!(parents[ids.root.as_str()].is_empty());
    let ids_shown: Vec<&str> = roots.iter().map(Oid::as_str).collect();
    let mut args = vec!["--date-order", "--parents"];
    args.extend(&ids_shown);
    for line in rev_list(&tmp.path().join("repo"), &args) {
        let mut w = line.split(' ');
        let c = w.next().unwrap();
        assert_eq!(parents[c], w.collect::<Vec<_>>(), "{c}");
    }
    let merge = page.rows.iter().find(|r| r.oid.as_str() == ids.m1).unwrap();
    assert_eq!(merge.subject, "merge feature");
    assert_eq!(merge.author_name, "Ann Author");
    assert_eq!(merge.author_email, "ann@example.invalid");
    assert_eq!(merge.author_time, 1_700_000_000 + 6 * 60);
}

/// G-T1 (03 §6.1, 03:1159): a commit's summary — author and committer apart, the whole message —
/// and its files with `A`/`M`/`D`/`R` and `+adds -dels` (blank for a binary file); a root commit
/// against the empty tree; a merge against its first parent; a work-tree file named like the
/// hash changes nothing (the `--` after the revision).
#[test]
fn g_t1_details_and_files() {
    let Some(git) = TestGit::local() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir(&dir).unwrap();
    sh(&dir, &["init", "-q", "-b", "main"]);
    let moved: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    let root = commit(
        &dir,
        1_700_000_000,
        &[
            ("a.txt", "one\ntwo\n"),
            ("b.txt", "b1\nb2\nb3\n"),
            ("bin.dat", "\0\x01\x02"),
            ("r.txt", &moved),
        ],
        "root",
    );
    std::fs::write(dir.join("new.txt"), "n1\nn2\nn3\n").unwrap();
    std::fs::write(dir.join("a.txt"), "one\nTWO\n").unwrap();
    std::fs::write(dir.join("bin.dat"), "\0\x01\x03").unwrap();
    sh(&dir, &["rm", "-q", "b.txt"]);
    sh(&dir, &["mv", "r.txt", "moved.txt"]);
    sh(&dir, &["add", "-A"]);
    let message = "the subject\n\nthe body\u{1f}with a unit separator\n";
    sh_with(
        &dir,
        1_700_000_100,
        &[
            ("GIT_COMMITTER_NAME", "Carl Committer"),
            ("GIT_COMMITTER_EMAIL", "carl@example.invalid"),
            ("GIT_COMMITTER_DATE", "1700000200 +0000"),
        ],
        &["commit", "-q", "--cleanup=verbatim", "-F", "-"],
        message.as_bytes(),
    );
    let c = oid(&sh(&dir, &["rev-parse", "HEAD"]));
    std::fs::write(dir.join(c.as_str()), "a file named like the hash\n").unwrap();
    let top = real(&dir);
    let top = top.as_str();

    let d = history::details(&git, top, &c).unwrap();
    assert_eq!(
        d,
        CommitDetails {
            oid: c.clone(),
            parents: vec![oid(&root)],
            author_name: "Ann Author".into(),
            author_email: "ann@example.invalid".into(),
            author_time: 1_700_000_100,
            committer_name: "Carl Committer".into(),
            committer_email: "carl@example.invalid".into(),
            commit_time: 1_700_000_200,
            message: message.into(),
            message_lossy: false,
        }
    );
    let files = history::commit_files(&git, top, &d).unwrap();
    let row = |letter, path: &str, old: Option<&str>, lines| ChangedFile {
        letter: FileLetter::Change(letter),
        path: path.into(),
        old_path: old.map(str::to_string),
        lines,
        lossy: false,
    };
    assert_eq!(
        files,
        vec![
            row(Change::Modified, "a.txt", None, Some((1, 1))),
            row(Change::Deleted, "b.txt", None, Some((0, 3))),
            row(Change::Modified, "bin.dat", None, None),
            row(Change::Renamed, "moved.txt", Some("r.txt"), Some((0, 0))),
            row(Change::Added, "new.txt", None, Some((3, 0))),
        ]
    );

    let root_details = history::details(&git, top, &oid(&root)).unwrap();
    assert!(root_details.parents.is_empty());
    let root_files = history::commit_files(&git, top, &root_details).unwrap();
    assert_eq!(root_files.len(), 4);
    assert!(root_files
        .iter()
        .all(|f| f.letter == FileLetter::Change(Change::Added)));
    assert_eq!(
        history::empty_tree(&git, top).unwrap().as_str(),
        sh(&dir, &["hash-object", "-t", "tree", "/dev/null"])
    );

    // A merge: its files against the first parent.
    sh(&dir, &["checkout", "-q", "-b", "side", &root]);
    commit(&dir, 1_700_000_300, &[("side.txt", "s\n")], "side");
    sh(&dir, &["checkout", "-q", "main"]);
    sh_at(
        &dir,
        1_700_000_400,
        &["merge", "-q", "--no-ff", "side", "-m", "merge side"],
    );
    let merge = oid(&sh(&dir, &["rev-parse", "HEAD"]));
    let md = history::details(&git, top, &merge).unwrap();
    assert_eq!(md.parents.len(), 2);
    assert_eq!(md.parents[0], c);
    assert_eq!(
        history::commit_files(&git, top, &md).unwrap(),
        vec![row(Change::Added, "side.txt", None, Some((1, 0)))]
    );

    // A message and a path that are not UTF-8: written straight into the object store (APFS
    // refuses such file names), shown with U+FFFD and flagged.
    let blob = sh_with(&dir, 0, &[], &["hash-object", "-w", "--stdin"], b"x\n");
    let mut entry = format!("100644 blob {blob}\t").into_bytes();
    entry.extend_from_slice(b"na\xefve.txt\0");
    let tree = sh_with(&dir, 0, &[], &["mktree", "-z"], &entry);
    // `commit-tree` would turn the Latin-1 byte into UTF-8; `hash-object` keeps it.
    let ident = "Ann Author <ann@example.invalid> 1700000500 +0000";
    let mut object = format!(
        "tree {tree}\nparent {}\nauthor {ident}\ncommitter {ident}\n\n",
        merge.as_str()
    )
    .into_bytes();
    object.extend_from_slice(b"caf\xe9 latin-1\n");
    let raw = sh_with(
        &dir,
        0,
        &[],
        &["hash-object", "-t", "commit", "-w", "--stdin"],
        &object,
    );
    let raw = oid(&raw);
    let rd = history::details(&git, top, &raw).unwrap();
    assert!(rd.message_lossy);
    assert_eq!(rd.message, "caf\u{fffd} latin-1\n");
    let files = history::files(&git, top, &merge, &raw).unwrap();
    let lossy: Vec<&ChangedFile> = files.iter().filter(|f| f.lossy).collect();
    assert_eq!(lossy.len(), 1, "{files:?}");
    assert_eq!(lossy[0].path, "na\u{fffd}ve.txt");
    assert_eq!(lossy[0].letter, FileLetter::Change(Change::Added));
}

/// G-T4 (03 §6.1–§6.2, 03:1165): a stash holding an untracked `u.txt` while a working `u.txt`
/// with other bytes exists → the stash read gives all three parents; the stash's files are its
/// tracked change and `u.txt` as `?`; the third parent the parser gave holds the stash's bytes,
/// not the working file's.
#[test]
fn g_t4_a_stashs_parents_and_its_untracked_file() {
    let Some(git) = TestGit::local() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir(&dir).unwrap();
    sh(&dir, &["init", "-q", "-b", "main"]);
    let base = commit(&dir, 1_700_000_000, &[("t.txt", "t\n")], "base");
    std::fs::write(dir.join("t.txt"), "t changed\n").unwrap();
    std::fs::write(dir.join("u.txt"), "the stash's bytes\n").unwrap();
    sh_at(
        &dir,
        1_700_000_060,
        &["stash", "push", "-q", "-u", "-m", "with untracked"],
    );
    std::fs::write(dir.join("u.txt"), "the working file\n").unwrap();
    let top = real(&dir);
    let top = top.as_str();

    let stashes = history::stashes(&git, top).unwrap();
    assert_eq!(stashes.len(), 1);
    let s = &stashes[0];
    let want = sh(
        &dir,
        &[
            "rev-parse",
            "stash@{0}",
            "stash@{0}^1",
            "stash@{0}^2",
            "stash@{0}^3",
        ],
    );
    let want: Vec<&str> = want.lines().collect();
    assert_eq!(s.oid.as_str(), want[0]);
    assert_eq!(s.base.as_str(), want[1]);
    assert_eq!(s.base.as_str(), base);
    assert_eq!(s.index.as_ref().map(Oid::as_str), Some(want[2]));
    assert_eq!(s.untracked.as_ref().map(Oid::as_str), Some(want[3]));
    assert_eq!(s.subject, "On main: with untracked");

    let files = history::stash_files(&git, top, s).unwrap();
    assert_eq!(
        files,
        vec![
            ChangedFile {
                letter: FileLetter::Change(Change::Modified),
                path: "t.txt".into(),
                old_path: None,
                lines: Some((1, 1)),
                lossy: false,
            },
            ChangedFile {
                letter: FileLetter::Untracked,
                path: "u.txt".into(),
                old_path: None,
                lines: Some((1, 0)),
                lossy: false,
            },
        ]
    );
    let untracked = s.untracked.as_ref().unwrap();
    let spec = format!("{}:u.txt", untracked.as_str());
    assert_eq!(sh(&dir, &["cat-file", "blob", &spec]), "the stash's bytes");
    assert_eq!(
        std::fs::read_to_string(dir.join("u.txt")).unwrap(),
        "the working file\n"
    );
}

/// An empty repository: HEAD is unborn, no refs, no stashes, `(initial)` — no roots, so no log.
#[test]
fn an_empty_repository_has_no_head_commit_and_no_roots() {
    let Some(git) = TestGit::local() else { return };
    let tmp = tempfile::tempdir().unwrap();
    sh(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("x.txt"), "x\n").unwrap();
    let top = real(tmp.path());
    let head = history::head(&git, &top).unwrap();
    assert_eq!(
        head,
        Head::Unborn {
            refname: "refs/heads/main".into(),
            lossy: false,
        }
    );
    assert_eq!(head.oid(), None);
    let refs = history::refs(&git, &top).unwrap();
    let stashes = history::stashes(&git, &top).unwrap();
    assert!(refs.is_empty() && stashes.is_empty());
    assert!(history::roots(&refs, Some(&head), &stashes).is_empty());
    let status = history::status(&git, &top).unwrap();
    assert_eq!(status.branch.oid, None);
    assert_eq!(status.branch.head.as_deref(), Some("main"));
    assert_eq!(status.entries.len(), 1);

    sh_at(
        tmp.path(),
        1_700_000_000,
        &["commit", "-q", "--allow-empty", "-m", "one"],
    );
    sh(tmp.path(), &["checkout", "-q", "--detach"]);
    let at = sh(tmp.path(), &["rev-parse", "HEAD"]);
    assert_eq!(history::head(&git, &top).unwrap(), Head::Detached(oid(&at)));
    assert_eq!(history::status(&git, &top).unwrap().branch.head, None);
}

fn id(c: char) -> String {
    c.to_string().repeat(40)
}

/// `cut` keeps whole characters: a multibyte character across the cap goes whole.
#[test]
fn cut_keeps_characters_whole() {
    assert_eq!(parse::cut("abc".into(), 3), "abc");
    assert_eq!(parse::cut("abcd".into(), 3), "abc");
    assert_eq!(parse::cut("ab한".into(), 4), "ab");
    assert_eq!(parse::cut("ab한".into(), 5), "ab한");
    assert_eq!(parse::cut(String::new(), 0), "");
    assert_eq!(parse::cut("한".into(), 0), "");
}

/// The log record (03 §4.1): the fields, a unit separator in the name, email and subject, text
/// that is not UTF-8, the 256-byte and 1 KiB cuts at a character boundary; and the records that
/// do not parse — a field too few or too many, a hash, a parent or a date that is not one.
#[test]
fn commit_records() {
    // `commit_record` takes a record without the NUL `-z` ends it with.
    let rec = |fields: &[&[u8]]| {
        let mut r = framed(fields);
        r.pop();
        r
    };
    let (a, b, c) = (id('a'), id('b'), id('c'));
    let parents = format!("{b} {c}");
    let fields: [&[u8]; 7] = [
        a.as_bytes(),
        parents.as_bytes(),
        b"Ann\x1fAlias",
        b"ann\x1f@x",
        b"1700000000",
        b"1700000060",
        b"a\x1fb",
    ];
    let row = parse::commit_record(&rec(&fields)).unwrap();
    assert_eq!(
        row,
        CommitRow {
            oid: oid(&a),
            parents: vec![oid(&b), oid(&c)],
            author_name: "Ann\u{1f}Alias".into(),
            author_email: "ann\u{1f}@x".into(),
            author_time: 1_700_000_000,
            commit_time: 1_700_000_060,
            subject: "a\u{1f}b".into(),
        }
    );
    let root = rec(&[a.as_bytes(), b"", b"Ann", b"", b"0", b"0", b""]);
    let row = parse::commit_record(&root).unwrap();
    assert!(row.parents.is_empty() && row.subject.is_empty() && row.author_email.is_empty());

    let long_name = format!("{}한한", "n".repeat(NAME_CAP - 1));
    let long_subject = format!("{}é{}", "s".repeat(SUBJECT_CAP - 1), "t".repeat(5000));
    let row = parse::commit_record(&rec(&[
        a.as_bytes(),
        b"",
        long_name.as_bytes(),
        b"e\xffmail",
        b"1",
        b"2",
        long_subject.as_bytes(),
    ]))
    .unwrap();
    // A cut field is marked: cut to 3 bytes under its cap, then `…`.
    assert_eq!(row.author_name, format!("{}…", "n".repeat(NAME_CAP - 3)));
    assert_eq!(row.author_email, "e\u{fffd}mail");
    assert_eq!(row.subject, format!("{}…", "s".repeat(SUBJECT_CAP - 3)));
    let exact_cap = "s".repeat(SUBJECT_CAP);
    let at_cap = parse::commit_record(&rec(&[
        a.as_bytes(),
        b"",
        b"A",
        b"a",
        b"1",
        b"2",
        exact_cap.as_bytes(),
    ]))
    .unwrap();
    assert_eq!(at_cap.subject, exact_cap, "a field at its cap is not cut");

    let (h256, p256) = ("4f".repeat(32), "5e".repeat(32));
    let sha256 = rec(&[
        h256.as_bytes(),
        p256.as_bytes(),
        b"A",
        b"a",
        b"1",
        b"1",
        b"s",
    ]);
    assert_eq!(parse::commit_record(&sha256).unwrap().parents.len(), 1);

    let two = format!("{b}  {c}");
    let upper = a.to_uppercase();
    // Past the parents' cap (64 KiB: an octopus of about 1 600 parents).
    let many_parents = vec![b.as_str(); 1700].join(" ");
    let trailing = format!("{b} ");
    let bad: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        a.clone().into_bytes(),
        rec(&[a.as_bytes(), b"", b"A", b"a", b"1", b"1"]),
        rec(&[a.as_bytes(), b"", b"A", b"a", b"1", b"1", b"s", b"extra"]),
        rec(&[b"XYZ", b"", b"A", b"a", b"1", b"1", b"s"]),
        rec(&[upper.as_bytes(), b"", b"A", b"a", b"1", b"1", b"s"]),
        rec(&[a.as_bytes(), b"nope", b"A", b"a", b"1", b"1", b"s"]),
        rec(&[a.as_bytes(), two.as_bytes(), b"A", b"a", b"1", b"1", b"s"]),
        rec(&[
            a.as_bytes(),
            trailing.as_bytes(),
            b"A",
            b"a",
            b"1",
            b"1",
            b"s",
        ]),
        rec(&[a.as_bytes(), b"", b"A", b"a", b"soon", b"1", b"s"]),
        rec(&[a.as_bytes(), b"", b"A", b"a", b"1", b"", b"s"]),
        rec(&[a.as_bytes(), b"", b"A", b"a", b"+1", b"1", b"s"]),
        rec(&[a.as_bytes(), b"", b"A", b"a", b"1", b" 1", b"s"]),
        rec(&[a.as_bytes(), b"", b"A", b"a", b"-1", b"1", b"s"]),
        rec(&[
            a.as_bytes(),
            b"",
            b"A",
            b"a",
            b"99999999999999999999",
            b"1",
            b"s",
        ]),
        rec(&[b"\xff\xfe", b"", b"", b"", b"", b"", b""]),
        rec(&[
            a.as_bytes(),
            b"",
            b"A",
            b"a",
            b"1",
            b"1".repeat(21).as_slice(),
            b"s",
        ]),
        rec(&[
            a.as_bytes(),
            many_parents.as_bytes(),
            b"A",
            b"a",
            b"1",
            b"1",
            b"s",
        ]),
    ];
    for r in bad {
        assert!(
            parse::commit_record(&r).is_err(),
            "{:?}",
            String::from_utf8_lossy(&r)
        );
    }
}

/// Review round 1, item 5: a capped field is decoded only up to its cap, with the same result as
/// decoding it whole and cutting — a multibyte character, a byte that is not UTF-8 and a
/// character cut by the cap included; a cut field ends in `…` and stays within its cap.
#[test]
fn capped_text_decodes_only_up_to_the_cap() {
    let long = "x".repeat(10_000);
    let cases: Vec<(Vec<u8>, usize)> = vec![
        (b"".to_vec(), 4),
        (b"abc".to_vec(), 4),
        (long.clone().into_bytes(), 256),
        (format!("{}한한", "n".repeat(255)).into_bytes(), 256),
        (
            format!("{}\u{1f600}tail", "n".repeat(254)).into_bytes(),
            256,
        ),
        (
            [
                b"n".repeat(255),
                b"\xff\xfe\xfd".to_vec(),
                long.clone().into_bytes(),
            ]
            .concat(),
            256,
        ),
        (
            [b"n".repeat(250), b"\xe2\x82".to_vec(), long.into_bytes()].concat(),
            256,
        ),
        (b"\xf0\x9f\x98".to_vec(), 4),
        (b"abcd".to_vec(), 3),
    ];
    for (bytes, cap) in cases {
        let decoded = String::from_utf8_lossy(&bytes).into_owned();
        let whole = if decoded.len() <= cap {
            decoded
        } else {
            format!("{}…", parse::cut(decoded, cap.saturating_sub(3)))
        };
        let got = parse::capped(&bytes, cap);
        assert!(got.len() <= cap, "{got:?}");
        assert_eq!(got, whole, "{:?}", &bytes[..bytes.len().min(16)]);
    }
}

/// Review round 1, item 1: a unit separator in an author's or committer's name and email, and in
/// a subject, shifts no field — through the live log stream and details read; a message with a
/// NUL inside is printed by git up to the NUL, and the records after it still parse.
#[test]
fn identity_fields_never_shift() {
    let Some(git) = TestGit::local() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir(&dir).unwrap();
    sh(&dir, &["init", "-q", "-b", "main"]);
    let base = commit(&dir, 1_700_000_000, &[("a.txt", "a\n")], "base");
    let tree = sh(&dir, &["rev-parse", "HEAD^{tree}"]);
    let odd = format!(
        "tree {tree}\nparent {base}\n\
         author Alice\u{1f}alias <alice\u{1f}x@example.invalid> 1700000100 +0000\n\
         committer Bob\u{1f}by <bob@example.invalid> 1700000200 +0000\n\n\
         subject\u{1f}with a unit\n\nbody\n"
    );
    let hash = ["hash-object", "-t", "commit", "-w", "--stdin"];
    let odd = sh_with(&dir, 0, &[], &hash, odd.as_bytes());
    let mut nul = format!(
        "tree {tree}\nparent {odd}\n\
         author Ann <ann@example.invalid> 1700000300 +0000\n\
         committer Ann <ann@example.invalid> 1700000300 +0000\n\nbefore"
    )
    .into_bytes();
    nul.extend_from_slice(b"\0after\x1fmore\n\nbody\0tail\n");
    // fsck refuses a NUL in a commit; a remote repository may still hold one.
    let literally = [
        "hash-object",
        "-t",
        "commit",
        "-w",
        "--stdin",
        "--literally",
    ];
    let nul = sh_with(&dir, 0, &[], &literally, &nul);
    sh(&dir, &["update-ref", "refs/heads/main", &nul]);
    let top = real(&dir);

    let generation = Generation::new();
    let load = generation.bump();
    let roots = [oid(&nul)];
    let mut stream = LogStream::start(
        &git,
        &top,
        &LogOptions::default(),
        &roots,
        &generation,
        load,
    )
    .unwrap();
    let page = stream.page(PAGE);
    assert_eq!(page.end, PageEnd::AllLoaded);
    assert_eq!(page.rows.len(), 3);
    assert_eq!(
        (page.rows[0].oid.as_str(), page.rows[0].subject.as_str()),
        (nul.as_str(), "before"),
        "git prints a message up to its NUL"
    );
    assert_eq!(page.rows[0].parents, vec![oid(&odd)]);
    assert_eq!(
        page.rows[1],
        CommitRow {
            oid: oid(&odd),
            parents: vec![oid(&base)],
            author_name: "Alice\u{1f}alias".into(),
            author_email: "alice\u{1f}x@example.invalid".into(),
            author_time: 1_700_000_100,
            commit_time: 1_700_000_200,
            subject: "subject\u{1f}with a unit".into(),
        }
    );
    assert_eq!(page.rows[2].oid.as_str(), base);

    let d = history::details(&git, &top, &oid(&odd)).unwrap();
    assert_eq!(
        (
            d.author_name.as_str(),
            d.author_email.as_str(),
            d.author_time
        ),
        (
            "Alice\u{1f}alias",
            "alice\u{1f}x@example.invalid",
            1_700_000_100
        )
    );
    assert_eq!(
        (
            d.committer_name.as_str(),
            d.committer_email.as_str(),
            d.commit_time
        ),
        ("Bob\u{1f}by", "bob@example.invalid", 1_700_000_200)
    );
    assert_eq!(d.message, "subject\u{1f}with a unit\n\nbody\n");
    let d = history::details(&git, &top, &oid(&nul)).unwrap();
    assert_eq!((d.message.as_str(), d.message_lossy), ("before", false));
}

/// Review round 1, item 2: every read framed by a terminator refuses output whose last record
/// lacks it (cut short), where a lenient reader would take the cut text as a whole name.
#[test]
fn a_read_cut_short_is_an_error() {
    assert!(parse::status(b"? file").is_err());
    assert!(parse::status(b"# branch.oid (initial)\0? file").is_err());
    assert!(parse::status(b"# branch.oid (initial)\0? file\0").is_ok());
    let rename = format!(
        "2 R. N... 100644 100644 100644 {a} {a} R100 new\0old",
        a = id('a')
    );
    assert!(parse::status(rename.as_bytes()).is_err());
    assert!(parse::changed_files(b"M\0file", b"1\t1\tfile\0").is_err());
    assert!(parse::changed_files(b"M\0file\0", b"1\t1\tfile").is_err());
    assert!(parse::changed_files(b"R100\0old\0new", b"0\t0\t\0old\0new\0").is_err());
    assert!(parse::changed_files(b"R100\0old\0new\0", b"0\t0\t\0old\0new").is_err());
    assert!(parse::changed_files(b"M\0file\0", b"1\t1\tfile\0").is_ok());
    let stash = format!(
        "{}\x1f{} {}\x1fstash@{{0}}\x1f1\x1fWIP",
        id('a'),
        id('b'),
        id('c')
    );
    assert!(parse::stashes(stash.as_bytes()).is_err());
    assert!(parse::stashes(format!("{stash}\0").as_bytes()).is_ok());
    let line = format!("{}\0commit\0\0refs/heads/main\0\0", id('a'));
    assert!(parse::refs(line.as_bytes()).is_err());
    assert!(parse::refs(format!("{line}\n").as_bytes()).is_ok());
    let cut = parse::detection(
        Some(0),
        b"false\nfalse\nfalse\n/r/.git\n.git\n/r",
        b"",
        "/r",
    );
    assert!(
        matches!(&cut, Detection::Unavailable(Unavailable::Failed(why)) if why.contains("cut short")),
        "{cut:?}"
    );
    let a_line = format!("{}\n", id('a'));
    assert!(parse::head((Some(0), b"refs/heads/ma"), (Some(0), a_line.as_bytes())).is_err());
    assert!(parse::head((Some(1), b""), (Some(0), id('a').as_bytes())).is_err());
    assert_eq!(parse::git_version(b"git version 2.5"), None);
    let details = framed(&[
        id('a').as_bytes(),
        b"",
        b"A",
        b"a",
        b"1",
        b"C",
        b"c",
        b"2",
        b"the message",
    ]);
    assert!(parse::details(&details).is_ok());
    assert!(parse::details(&details[..details.len() - 1]).is_err());
}

/// The refs read's lines: each field, a ref name that is not UTF-8, and lines that do not parse.
#[test]
fn ref_lines() {
    let mut out = Vec::new();
    out.extend_from_slice(
        format!(
            "{}\0commit\0\0refs/heads/main\0refs/remotes/origin/main\0[behind 2]\n",
            id('a')
        )
        .as_bytes(),
    );
    out.extend_from_slice(format!("{}\0tag\0{}\0refs/tags/v1\0\0\n", id('b'), id('c')).as_bytes());
    out.extend_from_slice(format!("{}\0commit\0\0refs/heads/x", id('d')).as_bytes());
    out.extend_from_slice(b"\xffy\0\0\n");
    out.extend_from_slice(format!("{}\0tree\0\0refs/tags/t\0\0\n", id('e')).as_bytes());
    let refs = parse::refs(&out).unwrap();
    assert_eq!(refs.len(), 4);
    assert_eq!(
        refs[0],
        RefLabel {
            object: oid(&id('a')),
            object_type: ObjectType::Commit,
            peeled: None,
            refname: "refs/heads/main".into(),
            kind: RefKind::LocalBranch,
            upstream: Some("refs/remotes/origin/main".into()),
            track: "[behind 2]".into(),
            lossy: false,
        }
    );
    assert_eq!(refs[1].object_type, ObjectType::Tag);
    assert_eq!(refs[1].target(), &oid(&id('c')));
    assert_eq!(refs[1].kind, RefKind::Tag);
    assert_eq!(refs[2].refname, "refs/heads/x\u{fffd}y");
    assert!(refs[2].lossy);
    assert_eq!(refs[3].object_type, ObjectType::Tree);
    assert_eq!(parse::refs(b"").unwrap(), vec![]);

    for bad in [
        format!("{}\0commit\0\0refs/heads/main\0\n", id('a')),
        format!("{}\0commit\0\0refs/heads/main\0\0\0\n", id('a')),
        "abc\0commit\0\0refs/heads/main\0\0\n".to_string(),
        format!("{}\0thing\0\0refs/heads/main\0\0\n", id('a')),
        format!("{}\0tag\0zz\0refs/tags/v\0\0\n", id('a')),
        format!("{}\0commit\0\0refs/notes/x\0\0\n", id('a')),
        "\0\0\0\0\0\n".to_string(),
    ] {
        assert!(parse::refs(bad.as_bytes()).is_err(), "{bad:?}");
    }
}

/// The HEAD read's two exits: a branch, an unborn branch, detached; and what is neither.
#[test]
fn head_reads() {
    let a = id('a');
    let a_line = format!("{a}\n");
    let branch = parse::head(
        (Some(0), b"refs/heads/main\n"),
        (Some(0), a_line.as_bytes()),
    );
    assert_eq!(
        branch.unwrap(),
        Head::Branch {
            refname: "refs/heads/main".into(),
            lossy: false,
            oid: oid(&a),
        }
    );
    let unborn = parse::head((Some(0), b"refs/heads/m\xffn\n"), (Some(1), b""));
    assert_eq!(
        unborn.unwrap(),
        Head::Unborn {
            refname: "refs/heads/m\u{fffd}n".into(),
            lossy: true,
        }
    );
    let detached = parse::head((Some(1), b""), (Some(0), a_line.as_bytes()));
    assert_eq!(detached.unwrap(), Head::Detached(oid(&a)));
    for (s, v) in [
        ((Some(1), &b""[..]), (Some(1), &b""[..])),
        ((Some(128), &b""[..]), (Some(0), a_line.as_bytes())),
        ((Some(0), &b""[..]), (Some(0), a_line.as_bytes())),
        (
            (Some(0), &b"refs/heads/main\n"[..]),
            (Some(0), &b"nope\n"[..]),
        ),
        ((None, &b""[..]), (Some(0), a_line.as_bytes())),
    ] {
        assert!(parse::head(s, v).is_err(), "{s:?} {v:?}");
    }
}

/// The stashes read: two and three parents, the subject's stray `\x1f`; and records that do
/// not parse.
#[test]
fn stash_records() {
    let out = format!(
        "{}\x1f{} {}\x1fstash@{{0}}\x1f1700000000\x1fOn main: a\x1fb\0{}\x1f{} {} {}\x1fstash@{{1}}\x1f1\x1fWIP\0",
        id('a'),
        id('b'),
        id('c'),
        id('d'),
        id('e'),
        id('f'),
        id('1')
    );
    let rows = parse::stashes(out.as_bytes()).unwrap();
    assert_eq!(
        rows[0],
        StashRow {
            oid: oid(&id('a')),
            base: oid(&id('b')),
            index: Some(oid(&id('c'))),
            untracked: None,
            selector: "stash@{0}".into(),
            commit_time: 1_700_000_000,
            subject: "On main: a\u{1f}b".into(),
        }
    );
    assert_eq!(rows[1].untracked, Some(oid(&id('1'))));
    assert_eq!(parse::stashes(b"").unwrap(), vec![]);
    for bad in [
        format!("{}\x1f\x1fstash@{{0}}\x1f1\x1fs\0", id('a')),
        format!(
            "{}\x1f{} {} {} {}\x1fstash@{{0}}\x1f1\x1fs\0",
            id('a'),
            id('b'),
            id('c'),
            id('d'),
            id('e')
        ),
        format!("{}\x1f{}\x1fstash@{{0}}\x1fx\x1fs\0", id('a'), id('b')),
        format!("{}\x1f{}\x1fstash@{{0}}\0", id('a'), id('b')),
        "\x1f\x1f\x1f\x1f\0".to_string(),
    ] {
        assert!(parse::stashes(bad.as_bytes()).is_err(), "{bad:?}");
    }
}

/// The status read: every header, every record kind (a copy `C` and a rename with their old
/// paths, a conflict, an untracked folder), a path that is not UTF-8; and records that do not
/// parse.
#[test]
fn status_records() {
    let h = |c: char| c.to_string().repeat(40);
    let mut out = Vec::new();
    let mut put = |b: &[u8]| {
        out.extend_from_slice(b);
        out.push(0);
    };
    put(format!("# branch.oid {}", h('a')).as_bytes());
    put(b"# branch.head feature");
    put(b"# branch.upstream origin/feature");
    put(b"# branch.ab +3 -1");
    put(b"# stash 2");
    put(format!(
        "1 .M N... 100644 100644 100644 {} {} a b.txt",
        h('1'),
        h('1')
    )
    .as_bytes());
    put(format!(
        "1 AD N... 000000 100644 000000 {} {} gone.txt",
        h('0'),
        h('2')
    )
    .as_bytes());
    put(format!("1 T. N... 100644 120000 120000 {} {} link", h('3'), h('4')).as_bytes());
    put(format!(
        "2 R. N... 100644 100644 100644 {} {} R100 new name.txt",
        h('5'),
        h('5')
    )
    .as_bytes());
    put(b"old name.txt");
    put(format!(
        "2 C. N... 100644 100644 100644 {} {} C75 copy.txt",
        h('6'),
        h('6')
    )
    .as_bytes());
    put(b"orig.txt");
    put(format!(
        "u UU N... 100644 100644 100644 100644 {} {} {} both.txt",
        h('7'),
        h('8'),
        h('9')
    )
    .as_bytes());
    let mut lossy = format!("1 M. N... 100644 100644 100644 {} {} na", h('b'), h('c')).into_bytes();
    lossy.extend_from_slice(b"\xefve");
    put(&lossy);
    put(b"? dir/");
    put(b"? untracked file");
    put(b"! ignored.o");
    let status = parse::status(&out).unwrap();
    assert_eq!(
        status.branch,
        BranchStatus {
            oid: Some(oid(&h('a'))),
            head: Some("feature".into()),
            upstream: Some("origin/feature".into()),
            ahead_behind: Some(AheadBehind {
                ahead: 3,
                behind: 1
            }),
            lossy: false,
        }
    );
    let changed = |staged, unstaged| StatusState::Changed { staged, unstaged };
    let entry = |path: &str, old: Option<&str>, state, lossy| StatusEntry {
        path: path.into(),
        old_path: old.map(str::to_string),
        state,
        lossy,
    };
    assert_eq!(
        status.entries,
        vec![
            entry(
                "a b.txt",
                None,
                changed(None, Some(Change::Modified)),
                false
            ),
            entry(
                "gone.txt",
                None,
                changed(Some(Change::Added), Some(Change::Deleted)),
                false
            ),
            entry(
                "link",
                None,
                changed(Some(Change::TypeChanged), None),
                false
            ),
            entry(
                "new name.txt",
                Some("old name.txt"),
                changed(Some(Change::Renamed), None),
                false
            ),
            entry(
                "copy.txt",
                Some("orig.txt"),
                changed(Some(Change::Copied), None),
                false
            ),
            entry("both.txt", None, StatusState::Conflict("UU".into()), false),
            entry(
                "na\u{fffd}ve",
                None,
                changed(Some(Change::Modified), None),
                true
            ),
            entry("dir/", None, StatusState::Untracked, false),
            entry("untracked file", None, StatusState::Untracked, false),
        ]
    );

    let fresh = parse::status(b"# branch.oid (initial)\0# branch.head (detached)\0").unwrap();
    assert_eq!(fresh.branch, BranchStatus::default());
    assert!(fresh.entries.is_empty());
    assert_eq!(parse::status(b"").unwrap().entries, vec![]);

    for bad in [
        &b"x what\0"[..],
        b"1 .M N... 100644 100644 100644\0",
        b"1 .Q N... 100644 100644 100644 a b c\0",
        b"2 R. N... 100644 100644 100644 a b R100 new.txt\0",
        b"u U N... 1 2 3 4 a b c\0",
        b"# branch.ab +x -1\0",
        b"# branch.oid zz\0",
        b"?\0",
    ] {
        assert!(
            parse::status(bad).is_err(),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
}

/// The details record: every field, a unit separator in the names and `%B` kept whole; the
/// message cap; and records that do not parse.
#[test]
fn details_records() {
    let (a, b) = (id('a'), id('b'));
    let out = framed(&[
        a.as_bytes(),
        b.as_bytes(),
        b"Ann\x1fAlias",
        b"ann@x",
        b"10",
        b"Carl\x1fC",
        b"carl@x",
        b"20",
        b"subject\n\nbody\x1fmore\n",
    ]);
    let d = parse::details(&out).unwrap();
    assert_eq!(d.parents, vec![oid(&b)]);
    assert_eq!((d.author_time, d.commit_time), (10, 20));
    assert_eq!(d.author_name, "Ann\u{1f}Alias");
    assert_eq!(
        (d.committer_name.as_str(), d.committer_email.as_str()),
        ("Carl\u{1f}C", "carl@x")
    );
    assert_eq!(d.message, "subject\n\nbody\u{1f}more\n");
    assert!(!d.message_lossy);

    // Item 5: past the cap, an error, never a message cut without saying so.
    let big = "m".repeat(parse::DETAILS_CAP);
    let fields = |message: &str| {
        framed(&[
            a.as_bytes(),
            b"",
            b"A",
            b"a",
            b"1",
            b"C",
            b"c",
            b"2",
            message.as_bytes(),
        ])
    };
    assert!(parse::details(&fields(&big[..parse::DETAILS_CAP - 100])).is_ok());
    let past = parse::details(&fields(&big)).unwrap_err();
    assert!(past.0.contains("1 MiB"), "{past}");

    for bad in [
        Vec::new(),
        framed(&[a.as_bytes(), b"", b"A", b"a", b"1", b"C", b"c", b"2"]),
        framed(&[a.as_bytes(), b"", b"A", b"a", b"x", b"C", b"c", b"2", b"m"]),
        framed(&[b"nope", b"", b"A", b"a", b"1", b"C", b"c", b"2", b"m"]),
        framed(&[
            a.as_bytes(),
            b"",
            b"A",
            b"a",
            b"1",
            b"C",
            b"c",
            b"2",
            b"m",
            b"extra",
        ]),
    ] {
        assert!(
            parse::details(&bad).is_err(),
            "{:?}",
            String::from_utf8_lossy(&bad)
        );
    }
    assert_eq!(
        parse::object_id(format!("{}\n", id('4')).as_bytes()).unwrap(),
        oid(&id('4'))
    );
    assert!(parse::object_id(b"\n").is_err());
}

/// The details pane's Files from `--name-status -z` and `--numstat -z`: renames and copies with
/// both paths, a binary file, an unmerged path, a path that is not UTF-8; and pairs that do not
/// parse or do not agree.
#[test]
fn changed_files_join_both_reads() {
    let names = b"M\0a.txt\0R087\0old.txt\0new.txt\0C100\0src.txt\0dst.txt\0A\0bin.dat\0U\0c.txt\0D\0na\xefve\0";
    let counts: &[u8] = &[
        &b"1\t2\ta.txt\0"[..],
        b"3\t1\t\0old.txt\0new.txt\0",
        b"0\t0\t\0src.txt\0dst.txt\0",
        b"-\t-\tbin.dat\0",
        b"0\t0\tc.txt\0",
        b"0\t4\tna\xefve\0",
    ]
    .concat();
    let files = parse::changed_files(names, counts).unwrap();
    let f = |letter, path: &str, old: Option<&str>, lines, lossy| ChangedFile {
        letter,
        path: path.into(),
        old_path: old.map(str::to_string),
        lines,
        lossy,
    };
    let c = FileLetter::Change;
    assert_eq!(
        files,
        vec![
            f(c(Change::Modified), "a.txt", None, Some((1, 2)), false),
            f(
                c(Change::Renamed),
                "new.txt",
                Some("old.txt"),
                Some((3, 1)),
                false
            ),
            f(
                c(Change::Copied),
                "dst.txt",
                Some("src.txt"),
                Some((0, 0)),
                false
            ),
            f(c(Change::Added), "bin.dat", None, None, false),
            f(FileLetter::Unmerged, "c.txt", None, Some((0, 0)), false),
            f(c(Change::Deleted), "na\u{fffd}ve", None, Some((0, 4)), true),
        ]
    );
    assert_eq!(parse::changed_files(b"", b"").unwrap(), vec![]);

    let refused = |names: &[u8], counts: &[u8]| {
        assert!(
            parse::changed_files(names, counts).is_err(),
            "{:?} {:?}",
            String::from_utf8_lossy(names),
            String::from_utf8_lossy(counts)
        );
    };
    refused(b"M\0a.txt\0", b"");
    refused(b"M\0a.txt\0", b"1\t1\tb.txt\0");
    refused(b"X\0a.txt\0", b"1\t1\ta.txt\0");
    refused(b"R100\0old.txt\0", b"1\t1\t\0old.txt\0");
    refused(b"M\0", b"1\t1\t\0");
    refused(b"M\0a.txt\0", b"one\t1\ta.txt\0");
    refused(b"M\0a.txt\0", b"1\ta.txt\0");
    refused(b"R100\0o\0n\0", b"1\t1\t\0o\0m\0");
}
