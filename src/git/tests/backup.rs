//! The backups' tests (03 §8.2–§8.3; G-S1, G-S6, and the backup blobs of G-A6 and G-A8): the
//! group's one transaction, the manifest, raw `files` bytes, restore, and undo, redo and undo
//! again. The fixture is `tests/actions.rs`'.

use super::actions::{conflicted, done, fixture, repo_of, run, Fx};
use crate::git::actions::{inspect, Action, Outcome, Plan, Repo, ResetMode, StepOutcome};
use crate::git::backup::{
    self, kind_name, kind_of, stamp, Manifest, ManifestEntry, RestoreEntry, Subject, Undo, Value,
};
use crate::git::model::{BackupKind, Oid};
use crate::git::rebase::{self as history, Base, Edit, Model, Op, Verb};
use crate::git::runner::{GitRunner, Handle};
use std::path::Path;
use std::time::{Duration, SystemTime};

// ---------------------------------------------------------------------------------------------
// The pieces

/// 03 §8.2: `<stamp>` is the UTC time of Run as `YYYYMMDDTHHMMSSZ`, `-2`, `-3`… when taken.
#[test]
fn the_stamp_is_utc_and_never_taken() {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_795_000_000);
    let first = stamp(at, &[]);
    assert_eq!(first, "20261118T110640Z");
    assert_eq!(
        stamp(at, std::slice::from_ref(&first)),
        format!("{first}-2")
    );
    assert_eq!(
        stamp(at, &[first.clone(), format!("{first}-2")]),
        format!("{first}-3")
    );
}

/// 03 §8.2: every kind's name under the stamp, and back.
#[test]
fn kinds_round_trip() {
    for kind in [
        BackupKind::Head("feature/x".into()),
        BackupKind::Detached,
        BackupKind::Tag("v1".into()),
        BackupKind::Stash,
        BackupKind::Worktree,
        BackupKind::Files,
        BackupKind::Pushed("origin/heads/main".into()),
        BackupKind::Remote("origin/main".into()),
        BackupKind::Manifest,
    ] {
        assert_eq!(kind_of(&kind_name(&kind)), Some(kind.clone()), "{kind:?}");
    }
    assert_eq!(kind_of("after"), None);
}

/// 03 §8.2: the manifest's message records every line and reads back the same — a path with a
/// newline, a backslash and spaces included; anything else is not a manifest.
#[test]
fn the_manifest_reads_back() {
    let oid = |c: char| Oid::parse(&c.to_string().repeat(40)).unwrap();
    let manifest = Manifest {
        entries: vec![
            ManifestEntry {
                subject: Subject::Ref("refs/heads/main".into()),
                before: Value::Object(oid('a')),
            },
            ManifestEntry {
                subject: Subject::Ref("refs/tags/gone".into()),
                before: Value::Absent,
            },
            ManifestEntry {
                subject: Subject::Head,
                before: Value::Object(oid('b')),
            },
            ManifestEntry {
                subject: Subject::Stash { commit: oid('c') },
                before: Value::Absent,
            },
            ManifestEntry {
                subject: Subject::WorkTree,
                before: Value::Clean,
            },
            ManifestEntry {
                subject: Subject::Pushed {
                    remote: "origin".into(),
                    refname: "refs/heads/main".into(),
                },
                before: Value::Object(oid('d')),
            },
            ManifestEntry {
                subject: Subject::Path("a dir/new\nline \\ x".into()),
                before: Value::Object(oid('e')),
            },
        ],
        moved_by: Some(ResetMode::Mixed),
    };
    let message = manifest.message("sopen: reset main\nto x");
    assert!(message.contains("\nmoved-by mixed\n"), "{message}");
    assert!(
        message.starts_with("sopen: reset main to x\n\n"),
        "{message}"
    );
    assert_eq!(Manifest::parse(&message), Some(manifest));
    assert_eq!(Manifest::parse("sopen: x\n\nnot a manifest\n"), None);
}

// ---------------------------------------------------------------------------------------------
// Undo and restore through the runner

/// The restore of the group `stamp` with the entries as its dialog first shows them.
fn restore(runner: &GitRunner, dir: &Path, stamp: &str) -> (Vec<RestoreEntry>, Plan, Outcome) {
    let top = dir.to_str().unwrap();
    let record = backup::group(runner, &Handle::new(), top, stamp)
        .unwrap()
        .expect("the group");
    let repo = repo_of(runner, dir);
    let entries = backup::entries(runner, &Handle::new(), top, &record, &repo.head).unwrap();
    let (plan, outcome) = run(
        runner,
        dir,
        &Action::Restore {
            stamp: stamp.into(),
            entries: entries.clone(),
        },
    );
    (entries, plan, outcome)
}

/// Cmd/Ctrl+Z: restores the newest group of the window, whose own group becomes the newest.
fn undo(lane: &str, runner: &GitRunner, dir: &Path, window: &mut Undo) -> Vec<RestoreEntry> {
    let stamp = window.newest().expect("a group to undo").to_string();
    let (entries, _, outcome) = restore(runner, dir, &stamp);
    done(lane, &outcome);
    window.push(outcome.backup.clone().expect("the restore's own group"));
    entries
}

/// An action run in the window: its group pushed.
fn act(lane: &str, runner: &GitRunner, dir: &Path, window: &mut Undo, action: Action) -> String {
    let (_, outcome) = run(runner, dir, &action);
    done(lane, &outcome);
    let stamp = outcome.backup.clone().expect("a group");
    window.push(stamp.clone());
    stamp
}

/// The work tree's tracked state: the index's diff and the work tree's.
fn tracked(fx: &Fx, dir: &Path) -> (String, String) {
    (fx.git(dir, &["diff", "--cached"]), fx.git(dir, &["diff"]))
}

/// 03 §8.2: the group's kinds under one stamp, its reflog message, its manifest.
fn group_names(fx: &Fx, dir: &Path, stamp: &str) -> Vec<String> {
    let out = fx.git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/sopen-backup/{stamp}/"),
        ],
    );
    let mut names: Vec<String> = out
        .lines()
        .map(|l| l.rsplit_once(&format!("{stamp}/")).unwrap().1.to_string())
        .filter(|n| n != "after")
        .collect();
    names.sort();
    names
}

fn reflog_message(fx: &Fx, dir: &Path, stamp: &str) -> String {
    fx.git(
        dir,
        &[
            "reflog",
            "show",
            "--format=%gs",
            &format!("refs/sopen-backup/{stamp}/manifest"),
        ],
    )
    .trim()
    .to_string()
}

/// G-S1: each destructive action of §8.2's table → its kinds under one stamp with the reflog
/// message and a manifest; Restore puts each ref, stash, work-tree change (staged and unstaged
/// as they were) and file back.
#[test]
fn g_s1_every_kind_is_written_and_restored() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("kinds-{lane}"));
        let bare = super::actions::with_origin(&fx, &dir, &format!("kinds-{lane}"));
        let c1 = fx.oid(&dir, "HEAD");
        let c2 = fx.commit(&dir, "a", "a\n", "second");
        fx.git(&dir, &["checkout", "-q", "-b", "feature"]);
        let f1 = fx.commit(&dir, "f", "f\n", "feature one");
        fx.git(&dir, &["push", "-q", "origin", "feature"]);
        fx.git(&dir, &["checkout", "-q", "main"]);
        fx.git(&dir, &["tag", "-a", "-m", "tagged", "t", c1.as_str()]);
        let t_obj = fx.rev(&dir, "refs/tags/t").unwrap();
        // Reset hard over staged and unstaged changes: heads/main and worktree.
        std::fs::write(dir.join("a"), "staged\n").unwrap();
        fx.git(&dir, &["add", "a"]);
        std::fs::write(dir.join("a"), "unstaged\n").unwrap();
        let state = tracked(&fx, &dir);
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Reset {
                to: c1.clone(),
                mode: ResetMode::Hard,
            },
        );
        done(&lane, &outcome);
        let stamp = outcome.backup.clone().unwrap();
        assert_eq!(
            group_names(&fx, &dir, &stamp),
            vec!["heads/main", "manifest", "worktree"]
        );
        assert!(
            reflog_message(&fx, &dir, &stamp).starts_with("sopen: "),
            "{lane}"
        );
        let record = backup::group(&runner, &Handle::new(), dir.to_str().unwrap(), &stamp)
            .unwrap()
            .unwrap();
        assert_eq!(record.group.message, reflog_message(&fx, &dir, &stamp));
        assert_eq!(
            record.before(&Subject::Ref("refs/heads/main".into())),
            Some(&Value::Object(c2.clone()))
        );
        let (_, _, outcome) = restore(&runner, &dir, &stamp);
        done(&lane, &outcome);
        assert_eq!(fx.rev(&dir, "main").unwrap(), c2.as_str(), "{lane}");
        assert_eq!(
            tracked(&fx, &dir),
            state,
            "{lane}: staged and unstaged as they were"
        );
        fx.git(&dir, &["reset", "-q", "--hard"]);
        // Delete branch, delete tag (and on the remote), drop a stash, remove untracked files.
        std::fs::write(dir.join("README"), "stashed\n").unwrap();
        fx.git(&dir, &["stash", "push", "-q", "-m", "keep me"]);
        let stash = fx.oid(&dir, "refs/stash");
        std::fs::write(dir.join("loose.txt"), "loose\n").unwrap();
        let repo = repo_of(&runner, &dir);
        let actions = vec![
            Action::DeleteBranch {
                branch: "feature".into(),
                force: true,
                remotes: vec!["origin".into()],
            },
            Action::DeleteTag {
                tag: "t".into(),
                remote: None,
            },
            Action::StashDrop {
                stash: repo.stashes[0].clone(),
            },
            Action::Clean { folders: true },
        ];
        let want = [
            vec!["heads/feature", "manifest", "pushed/origin/heads/feature"],
            vec!["manifest", "tags/t"],
            vec!["manifest", "stash"],
            vec!["files", "manifest"],
        ];
        for (action, want) in actions.into_iter().zip(want) {
            let (_, outcome) = run(&runner, &dir, &action);
            done(&lane, &outcome);
            let stamp = outcome.backup.clone().unwrap();
            assert_eq!(
                group_names(&fx, &dir, &stamp),
                want,
                "{lane}: {:?}",
                action.id()
            );
            let (_, _, outcome) = restore(&runner, &dir, &stamp);
            done(&lane, &outcome);
        }
        assert_eq!(fx.rev(&dir, "refs/heads/feature").unwrap(), f1.as_str());
        assert_eq!(
            fx.rev(&bare, "refs/heads/feature").unwrap(),
            f1.as_str(),
            "{lane}: pushed back"
        );
        assert_eq!(
            fx.rev(&dir, "refs/tags/t").unwrap(),
            t_obj,
            "{lane}: the tag object"
        );
        assert_eq!(fx.rev(&dir, "stash@{0}").unwrap(), stash.as_str());
        assert_eq!(
            fx.git(&dir, &["stash", "list", "--format=%s"]),
            "On main: keep me\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("loose.txt")).unwrap(),
            "loose\n"
        );
        // A detached HEAD: `detached`, by the mixed reset that `checkout --detach` (and `reset
        // --keep`) refused to undo; undone in its own mode, `reset --mixed <backup>`.
        fx.git(&dir, &["checkout", "-q", "--detach", c2.as_str()]);
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Reset {
                to: c1.clone(),
                mode: ResetMode::Mixed,
            },
        );
        done(&lane, &outcome);
        let stamp = outcome.backup.clone().unwrap();
        assert_eq!(group_names(&fx, &dir, &stamp), vec!["detached", "manifest"]);
        let (_, plan, outcome) = restore(&runner, &dir, &stamp);
        done(&lane, &outcome);
        assert!(
            super::actions::commands(&plan)
                .iter()
                .any(|l| l == &format!("git reset --mixed {}", c2.as_str())),
            "{lane}: {:?}",
            super::actions::commands(&plan)
        );
        assert_eq!(fx.rev(&dir, "HEAD").unwrap(), c2.as_str(), "{lane}");
        assert_eq!(
            std::fs::read_to_string(dir.join("a")).unwrap(),
            "a\n",
            "{lane}"
        );
        // The list: newest first, each with its manifest.
        let list = backup::list(&runner, &Handle::new(), dir.to_str().unwrap()).unwrap();
        assert!(list.len() >= 12, "{lane}: {}", list.len());
        assert!(list
            .windows(2)
            .all(|w| super::actions::stamp_order(&w[0].group.stamp)
                > super::actions::stamp_order(&w[1].group.stamp)));
        assert!(list.iter().all(|r| !r.manifest.entries.is_empty()));
    }
}

/// G-S1: Drop a stash → undo (the entry back at `stash@{0}`) → redo (that entry dropped, the
/// others unchanged) → undo (one entry, not two).
#[test]
fn g_s1_stash_drop_round_trips() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("stash-trip-{lane}"));
        for n in ["one", "two"] {
            std::fs::write(dir.join("README"), format!("{n}\n")).unwrap();
            fx.git(&dir, &["stash", "push", "-q", "-m", n]);
        }
        let list = || fx.git(&dir, &["stash", "list", "--format=%H %s"]);
        let both = list();
        let repo = repo_of(&runner, &dir);
        let mut window = Undo::default();
        act(
            &lane,
            &runner,
            &dir,
            &mut window,
            Action::StashDrop {
                stash: repo.stashes[1].clone(),
            },
        );
        let dropped = list();
        assert_eq!(dropped.lines().count(), 1);
        undo(&lane, &runner, &dir, &mut window);
        assert_eq!(
            fx.git(&dir, &["stash", "list", "--format=%s"]),
            "On main: one\nOn main: two\n",
            "{lane}: back at stash@{{0}}"
        );
        undo(&lane, &runner, &dir, &mut window);
        assert_eq!(list(), dropped, "{lane}: redo drops that entry only");
        undo(&lane, &runner, &dir, &mut window);
        assert_eq!(list().lines().count(), 2, "{lane}: one entry back, not two");
        let mut now: Vec<&str> = both.lines().collect();
        now.sort();
        let again = list();
        let mut again: Vec<&str> = again.lines().collect();
        again.sort();
        assert_eq!(again, now);
    }
}

/// G-S1: Discard from a clean work tree → undo (staged and unstaged as before) → redo (clean
/// again) → undo; the same Discard undone after HEAD moved → the dialog's warning, the changes
/// applied onto the new HEAD.
#[test]
fn g_s1_discard_round_trips() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("discard-trip-{lane}"));
        fx.commit(&dir, "a", "a\n", "a");
        std::fs::write(dir.join("a"), "staged\n").unwrap();
        fx.git(&dir, &["add", "a"]);
        std::fs::write(dir.join("a"), "unstaged\n").unwrap();
        std::fs::write(dir.join("README"), "also\n").unwrap();
        let dirty = tracked(&fx, &dir);
        let clean = (String::new(), String::new());
        let mut window = Undo::default();
        act(
            &lane,
            &runner,
            &dir,
            &mut window,
            Action::Discard {
                paths: vec!["a".into(), "README".into()],
            },
        );
        assert_eq!(tracked(&fx, &dir), clean);
        undo(&lane, &runner, &dir, &mut window);
        assert_eq!(tracked(&fx, &dir), dirty, "{lane}: undo");
        undo(&lane, &runner, &dir, &mut window);
        assert_eq!(tracked(&fx, &dir), clean, "{lane}: redo");
        undo(&lane, &runner, &dir, &mut window);
        assert_eq!(tracked(&fx, &dir), dirty, "{lane}: undo again");
        // After HEAD moved: the warning, applied onto the new HEAD.
        let stamp = act(
            &lane,
            &runner,
            &dir,
            &mut window,
            Action::Discard {
                paths: vec!["a".into(), "README".into()],
            },
        );
        fx.commit(&dir, "other", "o\n", "HEAD moves");
        let top = dir.to_str().unwrap();
        let record = backup::group(&runner, &Handle::new(), top, &stamp)
            .unwrap()
            .unwrap();
        let repo = repo_of(&runner, &dir);
        let entries = backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
        let action = Action::Restore {
            stamp: stamp.clone(),
            entries,
        };
        let plan = super::actions::planned(&runner, &dir, &action).unwrap();
        let notes = plan.confirm.as_ref().unwrap().notes.join("\n");
        assert!(notes.contains("onto the current HEAD"), "{lane}: {notes}");
        let outcome = crate::git::actions::execute(&plan, top, &runner, &Handle::new());
        done(&lane, &outcome);
        assert_eq!(
            tracked(&fx, &dir),
            dirty,
            "{lane}: applied onto the new HEAD"
        );
    }
}

/// G-S1: undo after later edits — a ref that moved since is listed as moved and left alone
/// unless chosen; a deleted branch whose name is taken again; a moved tag restored by
/// compare-and-swap; a restored untracked file re-created with other bytes is kept and listed.
#[test]
fn g_s1_undo_after_later_edits() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("later-{lane}"));
        let c1 = fx.oid(&dir, "HEAD");
        let c2 = fx.commit(&dir, "a", "a\n", "second");
        let top = dir.to_str().unwrap();
        // A moved ref.
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Reset {
                to: c1.clone(),
                mode: ResetMode::Mixed,
            },
        );
        done(&lane, &outcome);
        let reset_stamp = outcome.backup.clone().unwrap();
        fx.git(&dir, &["checkout", "-q", "--", "."]);
        let c3 = fx.commit(&dir, "b", "b\n", "later");
        let record = backup::group(&runner, &Handle::new(), top, &reset_stamp)
            .unwrap()
            .unwrap();
        let repo = repo_of(&runner, &dir);
        let entries = backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
        let main = entries
            .iter()
            .find(|e| e.subject == Subject::Ref("refs/heads/main".into()))
            .unwrap();
        assert!(main.moved_since && !main.chosen, "{lane}: {main:?}");
        assert_eq!(main.now, Value::Object(c3.clone()));
        let refused = super::actions::planned(
            &runner,
            &dir,
            &Action::Restore {
                stamp: reset_stamp.clone(),
                entries,
            },
        )
        .unwrap_err();
        assert!(
            refused.0.contains("nothing chosen"),
            "{lane}: {}",
            refused.0
        );
        assert_eq!(
            fx.rev(&dir, "main").unwrap(),
            c3.as_str(),
            "{lane}: left alone"
        );
        // A deleted branch whose name is taken again: moved, restored only when chosen, by CAS.
        fx.git(&dir, &["branch", "gone", c2.as_str()]);
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::DeleteBranch {
                branch: "gone".into(),
                force: true,
                remotes: vec![],
            },
        );
        done(&lane, &outcome);
        let delete_stamp = outcome.backup.clone().unwrap();
        fx.git(&dir, &["branch", "gone", c1.as_str()]);
        let record = backup::group(&runner, &Handle::new(), top, &delete_stamp)
            .unwrap()
            .unwrap();
        let repo = repo_of(&runner, &dir);
        let mut entries =
            backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
        let gone = entries
            .iter_mut()
            .find(|e| matches!(&e.subject, Subject::Ref(r) if r == "refs/heads/gone"))
            .unwrap();
        assert!(gone.moved_since && !gone.chosen, "{lane}: {gone:?}");
        assert_eq!(gone.now, Value::Object(c1.clone()));
        gone.chosen = true;
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Restore {
                stamp: delete_stamp.clone(),
                entries,
            },
        );
        done(&lane, &outcome);
        assert_eq!(fx.rev(&dir, "refs/heads/gone").unwrap(), c2.as_str());
        // A moved tag: restored by compare-and-swap.
        fx.git(&dir, &["tag", "-a", "-m", "one", "t", c1.as_str()]);
        let t1 = fx.rev(&dir, "refs/tags/t").unwrap();
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::MoveTag {
                tag: "t".into(),
                to: c2.clone(),
                message: Some("one\n".into()),
            },
        );
        done(&lane, &outcome);
        let (entries, plan, outcome) = restore(&runner, &dir, &outcome.backup.clone().unwrap());
        done(&lane, &outcome);
        assert!(entries.iter().all(|e| e.chosen && !e.moved_since));
        let lines = super::actions::commands(&plan).join("\n");
        assert!(lines.contains("update-ref"), "{lane}: {lines}");
        assert_eq!(
            fx.rev(&dir, "refs/tags/t").unwrap(),
            t1,
            "{lane}: the old tag object"
        );
        // A restored untracked file re-created with other bytes: kept, listed.
        std::fs::write(dir.join("u.txt"), "first\n").unwrap();
        let (_, outcome) = run(&runner, &dir, &Action::Clean { folders: true });
        done(&lane, &outcome);
        let clean_stamp = outcome.backup.clone().unwrap();
        std::fs::write(dir.join("u.txt"), "second\n").unwrap();
        let (entries, _, outcome) = restore(&runner, &dir, &clean_stamp);
        done(&lane, &outcome);
        let u = entries
            .iter()
            .find(|e| e.subject == Subject::Path("u.txt".into()))
            .unwrap();
        assert!(backup::blocked(u) && !u.chosen, "{lane}: {u:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join("u.txt")).unwrap(),
            "second\n"
        );
    }
}

/// G-S6: Take theirs over a hand-edited conflicted file, then Restore with "replace" → the
/// hand-made bytes are back and the file is still unmerged; the same with the hand-made file in
/// CRLF under `* text eol=lf` and a clean filter → the bytes come back exactly.
#[test]
fn g_s6_take_theirs_then_restore() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        for awkward in [false, true] {
            let dir = conflicted(&fx, &format!("theirs-{lane}-{awkward}"));
            let bytes: &[u8] = if awkward {
                std::fs::write(
                    dir.join(".gitattributes"),
                    "* text eol=lf\nc.txt filter=del\n",
                )
                .unwrap();
                fx.git(&dir, &["config", "filter.del.clean", "tr -d x"]);
                b"hand x made\r\nx\r\n"
            } else {
                b"hand-made\n"
            };
            std::fs::write(dir.join("c.txt"), bytes).unwrap();
            let (_, outcome) = run(
                &runner,
                &dir,
                &Action::Theirs {
                    paths: vec!["c.txt".into()],
                },
            );
            done(&lane, &outcome);
            assert_eq!(std::fs::read(dir.join("c.txt")).unwrap(), b"side\n");
            let stamp = outcome.backup.clone().unwrap();
            let top = dir.to_str().unwrap();
            let record = backup::group(&runner, &Handle::new(), top, &stamp)
                .unwrap()
                .unwrap();
            let repo = repo_of(&runner, &dir);
            let mut entries =
                backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
            assert_eq!(entries.len(), 1);
            assert!(backup::blocked(&entries[0]), "{lane}: occupied by theirs");
            entries[0].chosen = true;
            entries[0].replace = true;
            let (_, outcome) = run(&runner, &dir, &Action::Restore { stamp, entries });
            done(&lane, &outcome);
            assert_eq!(
                std::fs::read(dir.join("c.txt")).unwrap(),
                bytes,
                "{lane}: byte for byte"
            );
            assert_eq!(
                fx.git(&dir, &["diff", "--name-only", "--diff-filter=U"]),
                "c.txt\n",
                "{lane}: still unmerged"
            );
        }
    }
}

/// The bytes at `path`, or `None` when nothing is there.
fn bytes(dir: &Path, path: &str) -> Option<Vec<u8>> {
    std::fs::read(dir.join(path)).ok()
}

/// Restores only the path entries of group `stamp`, as the dialog first chooses them.
fn undo_paths(lane: &str, runner: &GitRunner, dir: &Path, stamp: &str) -> String {
    let top = dir.to_str().unwrap();
    let record = backup::group(runner, &Handle::new(), top, stamp)
        .unwrap()
        .unwrap();
    let repo = repo_of(runner, dir);
    let mut entries = backup::entries(runner, &Handle::new(), top, &record, &repo.head).unwrap();
    for e in &mut entries {
        e.chosen &= matches!(e.subject, Subject::Path(_));
    }
    let (_, outcome) = run(
        runner,
        dir,
        &Action::Restore {
            stamp: stamp.into(),
            entries,
        },
    );
    done(lane, &outcome);
    outcome.backup.clone().expect("the restore's own group")
}

/// G-S1: Clean → undo (the untracked file back) → redo (removed again by the restore snippet's
/// `d`, `clean -f -x -- ./<p>`) → undo, bytes compared each time; the same for an ignored file a
/// Reset hard backed up, which a plain `clean -f` would keep.
#[test]
fn g_s1_files_round_trip() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("files-trip-{lane}"));
        std::fs::write(dir.join("u.txt"), "untracked\r\n").unwrap();
        let mut window = Undo::default();
        act(
            &lane,
            &runner,
            &dir,
            &mut window,
            Action::Clean { folders: true },
        );
        assert_eq!(bytes(&dir, "u.txt"), None);
        undo(&lane, &runner, &dir, &mut window);
        assert_eq!(
            bytes(&dir, "u.txt").as_deref(),
            Some(&b"untracked\r\n"[..]),
            "{lane}: undo"
        );
        undo(&lane, &runner, &dir, &mut window);
        assert_eq!(bytes(&dir, "u.txt"), None, "{lane}: redo");
        undo(&lane, &runner, &dir, &mut window);
        assert_eq!(
            bytes(&dir, "u.txt").as_deref(),
            Some(&b"untracked\r\n"[..]),
            "{lane}: undo again"
        );
        // An ignored file, backed up by a Reset hard.
        let dir = fx.repo(&format!("ignored-trip-{lane}"));
        std::fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
        fx.git(&dir, &["add", ".gitignore"]);
        fx.git(&dir, &["commit", "-q", "-m", "ignore"]);
        let start = fx.oid(&dir, "HEAD");
        let target = fx.commit(&dir, "x.log", "from git\n", "x.log");
        fx.git(&dir, &["reset", "-q", "--hard", start.as_str()]);
        std::fs::write(dir.join("x.log"), "mine\n").unwrap();
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Reset {
                to: target,
                mode: ResetMode::Hard,
            },
        );
        done(&lane, &outcome);
        let reset = outcome.backup.clone().unwrap();
        fx.git(&dir, &["reset", "-q", "--hard", start.as_str()]);
        assert_eq!(bytes(&dir, "x.log"), None);
        let restored = undo_paths(&lane, &runner, &dir, &reset);
        assert_eq!(
            bytes(&dir, "x.log").as_deref(),
            Some(&b"mine\n"[..]),
            "{lane}: restored"
        );
        let redone = undo_paths(&lane, &runner, &dir, &restored);
        assert_eq!(
            bytes(&dir, "x.log"),
            None,
            "{lane}: redo removes the ignored file"
        );
        undo_paths(&lane, &runner, &dir, &redone);
        assert_eq!(
            bytes(&dir, "x.log").as_deref(),
            Some(&b"mine\n"[..]),
            "{lane}: undo again"
        );
    }
}

/// 03 §8.3's detached row, `reset --keep <backup>` (coordinator, 2026-11-23): after a hard reset
/// of a detached HEAD the restore returns to the backup with the working files as the backup
/// has them; with a local change `--keep` would lose, it refuses with git's message and changes
/// nothing.
#[test]
fn a_detached_head_is_restored_by_reset_keep() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("detached-{lane}"));
        let c1 = fx.oid(&dir, "HEAD");
        let c2 = fx.commit(&dir, "a", "a\n", "second");
        fx.git(&dir, &["checkout", "-q", "--detach", c2.as_str()]);
        std::fs::write(dir.join("loose.txt"), "untracked, kept\n").unwrap();
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Reset {
                to: c1.clone(),
                mode: ResetMode::Hard,
            },
        );
        done(&lane, &outcome);
        let stamp = outcome.backup.clone().unwrap();
        assert!(!dir.join("a").exists());
        let (_, _, outcome) = restore(&runner, &dir, &stamp);
        done(&lane, &outcome);
        assert_eq!(
            fx.rev(&dir, "HEAD").unwrap(),
            c2.as_str(),
            "{lane}: back at the backup"
        );
        assert!(
            !fx.output(&dir, &["symbolic-ref", "-q", "HEAD"])
                .status
                .success(),
            "{lane}: still detached"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("a")).unwrap(),
            "a\n",
            "{lane}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("loose.txt")).unwrap(),
            "untracked, kept\n",
            "{lane}"
        );
        assert_eq!(
            fx.git(&dir, &["status", "--porcelain", "--untracked-files=no"]),
            ""
        );
        // A local change reset --keep would lose: refused, nothing changed.
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Reset {
                to: c1.clone(),
                mode: ResetMode::Hard,
            },
        );
        done(&lane, &outcome);
        let stamp = outcome.backup.clone().unwrap();
        std::fs::write(dir.join("README"), "a local change\n").unwrap();
        fx.commit(&dir, "a", "another a\n", "another a");
        std::fs::write(dir.join("a"), "edited, not committed\n").unwrap();
        let moved = fx.rev(&dir, "HEAD").unwrap();
        let top = dir.to_str().unwrap();
        let record = backup::group(&runner, &Handle::new(), top, &stamp)
            .unwrap()
            .unwrap();
        let repo = repo_of(&runner, &dir);
        let mut entries =
            backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
        for e in &mut entries {
            e.chosen = e.subject == Subject::Head;
        }
        let (_, outcome) = run(&runner, &dir, &Action::Restore { stamp, entries });
        let at = outcome
            .steps
            .iter()
            .position(|s| s.outcome == StepOutcome::Failed)
            .unwrap_or_else(|| panic!("{lane}: reset --keep did not refuse"));
        let text = super::actions::failed_at(&lane, &outcome, at);
        assert!(
            text.contains("not uptodate") || text.contains("would be overwritten"),
            "{lane}: {text}"
        );
        assert_eq!(
            fx.rev(&dir, "HEAD").unwrap(),
            moved,
            "{lane}: HEAD unchanged"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("a")).unwrap(),
            "edited, not committed\n",
            "{lane}: the local change kept"
        );
    }
}

/// G-S1 for a hard reset (03 §8.3, a HEAD item with a `worktree` item): Reset hard over staged
/// and unstaged changes to a file the two commits differ in → undo → redo → undo, on the
/// checked-out branch and on a detached HEAD. Each step is exact — HEAD, the index's tree, every
/// working file's bytes, the status — and nothing is refused: the work tree is reset to HEAD
/// before HEAD moves (reset after it, the redo's `reset --keep` refused: `Entry 'a.txt' would be
/// overwritten by merge`).
#[test]
fn g_s1_reset_hard_round_trips() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        for detached in [false, true] {
            let name = format!("hard-trip-{lane}-{detached}");
            let dir = fx.repo(&name);
            let c1 = fx.commit(&dir, "a.txt", "one\n", "first a.txt");
            // c2 changes a.txt and adds b.txt; both get changes c1 cannot carry.
            std::fs::write(dir.join("b.txt"), "b\n").unwrap();
            fx.git(&dir, &["add", "b.txt"]);
            let c2 = fx.commit(&dir, "a.txt", "two\n", "second");
            if detached {
                fx.git(&dir, &["checkout", "-q", "--detach", c2.as_str()]);
            }
            std::fs::write(dir.join("a.txt"), "staged\n").unwrap();
            fx.git(&dir, &["add", "a.txt"]);
            std::fs::write(dir.join("a.txt"), "unstaged\r\n").unwrap();
            std::fs::write(dir.join("b.txt"), "b, edited\n").unwrap();
            std::fs::write(dir.join("README"), "edited\n").unwrap();
            let state = || {
                (
                    fx.rev(&dir, "HEAD").unwrap(),
                    fx.git(&dir, &["write-tree"]),
                    bytes(&dir, "a.txt"),
                    bytes(&dir, "b.txt"),
                    bytes(&dir, "README"),
                    fx.git(&dir, &["status", "--porcelain", "--untracked-files=all"]),
                    fx.output(&dir, &["symbolic-ref", "-q", "HEAD"])
                        .status
                        .success(),
                )
            };
            let dirty = state();
            let mut window = Undo::default();
            act(
                &name,
                &runner,
                &dir,
                &mut window,
                Action::Reset {
                    to: c1.clone(),
                    mode: ResetMode::Hard,
                },
            );
            let reset = state();
            assert_eq!(reset.0, c1.as_str(), "{name}");
            assert_eq!(
                reset.2.as_deref(),
                Some(&b"one\n"[..]),
                "{name}: c1's a.txt"
            );
            assert_eq!(reset.3, None, "{name}: b.txt gone with c2");
            // Each restore's plan: HEAD goes back to the work tree backup's base first, so the
            // dialog never says the changes land on a moved HEAD.
            let step = |window: &mut Undo, what: &str| {
                let stamp = window.newest().unwrap().to_string();
                let (_, plan, outcome) = restore(&runner, &dir, &stamp);
                done(&name, &outcome);
                window.push(outcome.backup.clone().unwrap());
                let notes = plan.confirm.as_ref().unwrap().notes.join("\n");
                assert!(!notes.contains("HEAD has moved"), "{name}: {what}: {notes}");
                plan
            };
            step(&mut window, "undo");
            assert_eq!(state(), dirty, "{name}: undo");
            step(&mut window, "redo");
            assert_eq!(state(), reset, "{name}: redo");
            let plan = step(&mut window, "undo again");
            assert_eq!(state(), dirty, "{name}: undo again");
            // The order: the work tree reset to HEAD after HEAD's check, then HEAD moved, then
            // the changes applied.
            let lines = super::actions::commands(&plan);
            let at = |want: &dyn Fn(&str) -> bool| {
                lines
                    .iter()
                    .position(|l| want(l))
                    .unwrap_or_else(|| panic!("{name}: {lines:#?}"))
            };
            let check = at(&|l| l.starts_with("git update-ref --stdin"));
            let wipe = at(&|l| l == "git reset --hard HEAD");
            let back = at(&|l| l == format!("git reset --keep {}", c2.as_str()));
            let apply = at(&|l| l.starts_with("git stash apply --index "));
            assert!(
                check < wipe && wipe < back && back < apply,
                "{name}: {lines:#?}"
            );
        }
    }
}

/// The sentence a restore's result carries when it stopped after the work tree's reset and
/// before the apply (03 §8.3): `<the step> was refused; your uncommitted changes were set aside
/// in backup <stamp> — restore it to bring them back`.
fn set_aside(line: &str, stamp: &str) -> String {
    format!(
        "{line} was refused; your uncommitted changes were set aside in backup {stamp} — restore \
         it to bring them back"
    )
}

/// G-S1 for a Reset hard over an untracked file in the way (03 §8.3's order, `files` after the
/// move): undo puts the file back byte for byte without "replace" — the target's tracked file
/// there is HEAD's own and the move removes it before the files are written. The redo meets
/// that untracked file where its HEAD move writes: refused at plan time, naming it, nothing run
/// (§8.6). Put there only after the plan, the move refuses it at Run after the work tree's
/// reset: the result says where the uncommitted changes went (the restore's own group, by its
/// stamp), and restoring that group puts them back.
#[test]
fn g_s1_reset_hard_over_a_file_in_the_way_undo_and_redo() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("in-the-way-trip-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        let target = fx.commit(&dir, "u.txt", "tracked\n", "u.txt");
        fx.git(&dir, &["reset", "-q", "--hard", base.as_str()]);
        std::fs::write(dir.join("u.txt"), "mine, untracked\r\n").unwrap();
        std::fs::write(dir.join("README"), "edited\n").unwrap();
        let dirty = tracked(&fx, &dir);
        let mut window = Undo::default();
        act(
            &lane,
            &runner,
            &dir,
            &mut window,
            Action::Reset {
                to: target.clone(),
                mode: ResetMode::Hard,
            },
        );
        assert_eq!(bytes(&dir, "u.txt").as_deref(), Some(&b"tracked\n"[..]));
        // Undo: the file chosen as a free one, never "replace", written after the move.
        let entries = undo(&lane, &runner, &dir, &mut window);
        let path = entries
            .iter()
            .find(|e| e.subject == Subject::Path("u.txt".into()))
            .expect("the files entry");
        assert!(path.chosen && !path.replace, "{lane}: {path:?}");
        assert_eq!(fx.oid(&dir, "HEAD"), base, "{lane}: undo's HEAD");
        assert_eq!(tracked(&fx, &dir), dirty, "{lane}: undo's changes");
        assert_eq!(
            bytes(&dir, "u.txt").as_deref(),
            Some(&b"mine, untracked\r\n"[..]),
            "{lane}: undo's file, byte for byte"
        );
        // Redo: refused before anything runs, naming the file; the work tree untouched.
        let stamp = window.newest().unwrap().to_string();
        let top = dir.to_str().unwrap();
        let record = backup::group(&runner, &Handle::new(), top, &stamp)
            .unwrap()
            .unwrap();
        let repo = repo_of(&runner, &dir);
        let entries = backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
        let redo = Action::Restore {
            stamp: stamp.clone(),
            entries,
        };
        let why = super::actions::planned(&runner, &dir, &redo).unwrap_err().0;
        assert!(
            why.contains("u.txt") && why.contains("Move them away first"),
            "{lane}: {why}"
        );
        assert_eq!(fx.oid(&dir, "HEAD"), base, "{lane}: HEAD untouched");
        assert_eq!(tracked(&fx, &dir), dirty, "{lane}: the changes untouched");
        assert_eq!(
            bytes(&dir, "u.txt").as_deref(),
            Some(&b"mine, untracked\r\n"[..]),
            "{lane}: the file untouched"
        );
        // The file put there after the plan: the move refused at Run, and the result says so.
        let aside = fx.root.join(format!("u-{lane}.txt"));
        std::fs::rename(dir.join("u.txt"), &aside).unwrap();
        let plan = super::actions::planned(&runner, &dir, &redo).unwrap();
        std::fs::rename(&aside, dir.join("u.txt")).unwrap();
        let outcome = crate::git::actions::execute(&plan, top, &runner, &Handle::new());
        let at = outcome
            .steps
            .iter()
            .position(|s| s.outcome == StepOutcome::Failed)
            .unwrap_or_else(|| panic!("{lane}: the redo's move was not refused"));
        let line = outcome.steps[at].line.clone();
        assert_eq!(line, format!("git reset --keep {}", target.as_str()));
        super::actions::failed_at(&lane, &outcome, at);
        let own = outcome.backup.clone().expect("the restore's own group");
        // What the result dialog shows: the failed step's last unfinished command's text.
        let shown = outcome.steps[at]
            .ran
            .iter()
            .rev()
            .find(|d| !d.ok())
            .map(|d| d.error_text())
            .unwrap();
        assert!(
            shown.contains("would be overwritten"),
            "{lane}: git's text: {shown}"
        );
        assert!(shown.contains(&set_aside(&line, &own)), "{lane}: {shown}");
        assert_eq!(outcome.warning(), Some(set_aside(&line, &own)), "{lane}");
        assert_eq!(fx.oid(&dir, "HEAD"), base, "{lane}: HEAD unchanged");
        assert_eq!(tracked(&fx, &dir), (String::new(), String::new()));
        // Restoring that group puts the changes back; the untracked file is left alone.
        let (_, _, outcome) = restore(&runner, &dir, &own);
        done(&lane, &outcome);
        assert_eq!(tracked(&fx, &dir), dirty, "{lane}: the changes back");
        assert_eq!(
            bytes(&dir, "u.txt").as_deref(),
            Some(&b"mine, untracked\r\n"[..]),
            "{lane}: the untracked file left alone"
        );
    }
}

/// The same refusal over a clean work tree sets nothing aside, and the result says nothing of
/// the kind: the restore's own group says `clean`.
#[test]
fn a_refused_move_over_a_clean_tree_sets_nothing_aside() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("refused-clean-{lane}"));
        let start = fx.oid(&dir, "HEAD");
        let with_p = fx.commit(&dir, "p.txt", "tracked\n", "p.txt");
        std::fs::write(dir.join("README"), "edited\n").unwrap();
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::Reset {
                to: start.clone(),
                mode: ResetMode::Hard,
            },
        );
        done(&lane, &outcome);
        let stamp = outcome.backup.clone().unwrap();
        // Clean now; after the plan, an untracked file where the backup's commit has a tracked
        // one (before it, the plan refuses: `g_s1_reset_hard_over_a_file_in_the_way_…`).
        let top = dir.to_str().unwrap();
        let record = backup::group(&runner, &Handle::new(), top, &stamp)
            .unwrap()
            .unwrap();
        let repo = repo_of(&runner, &dir);
        let entries = backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
        assert!(
            entries.iter().filter(|e| e.chosen).count() == 2,
            "{lane}: HEAD and the work tree chosen: {entries:?}"
        );
        let plan =
            super::actions::planned(&runner, &dir, &Action::Restore { stamp, entries }).unwrap();
        super::actions::has_line(&plan, "git reset --hard HEAD");
        std::fs::write(dir.join("p.txt"), "untracked\n").unwrap();
        let outcome = crate::git::actions::execute(&plan, top, &runner, &Handle::new());
        let at = outcome
            .steps
            .iter()
            .position(|s| s.outcome == StepOutcome::Failed)
            .unwrap_or_else(|| panic!("{lane}: the move was not refused"));
        assert_eq!(
            outcome.steps[at].line,
            format!("git reset --keep {}", with_p.as_str())
        );
        let text = super::actions::failed_at(&lane, &outcome, at);
        assert!(!text.contains("set aside"), "{lane}: {text}");
        assert_eq!(outcome.warning(), None, "{lane}");
        assert_eq!(fx.oid(&dir, "HEAD"), start, "{lane}");
    }
}

/// The planner opened on `base` (as `tests/rebase.rs`' `opened`): the repository with its reads
/// made, and the model.
fn opened(runner: &GitRunner, dir: &Path, base: &Base) -> (Repo, Model) {
    let mut repo = repo_of(runner, dir);
    for _ in 0..6 {
        let missing = history::open_needs(base, &repo);
        if missing.is_empty() {
            break;
        }
        inspect(runner, &Handle::new(), &mut repo, &missing)
            .unwrap_or_else(|e| panic!("inspect: {}", e.reason));
    }
    let model = history::open(base, &repo).unwrap_or_else(|r| panic!("open refused: {}", r.0));
    (repo, model)
}

/// A history edit of `model` dropping `commit` with `--update-refs` and `--autostash` (03
/// §7.8.2), run: its group holds HEAD, the branch in the range and the work tree.
fn drop_with_update_refs(
    lane: &str,
    runner: &GitRunner,
    dir: &Path,
    (mut repo, mut model): (Repo, Model),
    commit: &Oid,
) -> String {
    assert!(model.autostash, "{lane}: on with uncommitted changes");
    model.update_refs = true;
    model
        .apply(Op::Verb {
            commit: commit.clone(),
            verb: Verb::Drop,
        })
        .unwrap();
    let edit = Edit::Plan(model);
    for _ in 0..6 {
        let missing = history::needs(&edit, &repo);
        if missing.is_empty() {
            break;
        }
        inspect(runner, &Handle::new(), &mut repo, &missing)
            .unwrap_or_else(|e| panic!("inspect: {}", e.reason));
    }
    let plan = history::plan(&edit, &repo).unwrap_or_else(|r| panic!("refused: {}", r.0));
    let outcome =
        crate::git::actions::execute(&plan, dir.to_str().unwrap(), runner, &Handle::new());
    done(lane, &outcome);
    outcome.backup.clone().expect("a group")
}

/// The restore plan of group `stamp`, every entry chosen.
fn restore_all(runner: &GitRunner, dir: &Path, stamp: &str) -> Plan {
    let top = dir.to_str().unwrap();
    let record = backup::group(runner, &Handle::new(), top, stamp)
        .unwrap()
        .unwrap();
    let repo = repo_of(runner, dir);
    let mut entries = backup::entries(runner, &Handle::new(), top, &record, &repo.head).unwrap();
    for e in &mut entries {
        e.chosen = true;
    }
    super::actions::planned(
        runner,
        dir,
        &Action::Restore {
            stamp: stamp.into(),
            entries,
        },
    )
    .unwrap_or_else(|r| panic!("refused: {}", r.0))
}

/// The confirmation's `git update-ref --stdin` lines (HEAD's checks, the ref transaction) all
/// come before the work tree's reset (03 §8.3's order); the index of that reset.
#[track_caller]
fn refusals_first(lane: &str, plan: &Plan, updates: usize) -> usize {
    let lines = super::actions::commands(plan);
    let wipe = lines
        .iter()
        .position(|l| l == "git reset --hard HEAD")
        .unwrap_or_else(|| panic!("{lane}: {lines:#?}"));
    let before: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("git update-ref --stdin"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(before.len(), updates, "{lane}: {lines:#?}");
    assert!(before.iter().all(|i| *i < wipe), "{lane}: {lines:#?}");
    wipe
}

/// A `reference-transaction` hook that sets `refname` to `oid` once the restore's own group is
/// written (its `manifest` created): what moves between that backup, which checks the values
/// the dialog read, and the restore's own checks and ref transaction.
fn moves_after_the_backup(dir: &Path, refname: &str, oid: &Oid) {
    super::actions::script(
        &dir.join(".git/hooks/reference-transaction"),
        &format!(
            "[ \"$1\" = committed ] || exit 0\n\
             if grep -q 'refs/sopen-backup/.*/manifest$'; then git update-ref {refname} {}; fi\n\
             exit 0",
            oid.as_str()
        ),
    );
}

/// HEAD, the index's tree and the working files' tracked state, as a restore must leave them
/// when it stops before the work tree's reset.
fn untouched(fx: &Fx, dir: &Path) -> (String, Option<Vec<u8>>, String) {
    (
        fx.git(dir, &["write-tree"]),
        bytes(dir, "README"),
        fx.git(dir, &["diff", "--stat"]),
    )
}

/// 03 §8.3's order, what can refuse first: the undo of an `--update-refs` history edit run with
/// `--autostash` (HEAD, another branch and the work tree). Its ref transaction comes before the
/// work tree's reset: when the other branch moves after the restore's own backup (which checks
/// the values the dialog read), the compare-and-swap fails there and the work tree, the index
/// and HEAD stay as they were.
#[test]
fn a_refused_ref_transaction_leaves_the_work_tree_alone() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("cas-first-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        let c1 = fx.commit(&dir, "f1", "1\n", "one");
        fx.commit(&dir, "f2", "2\n", "two");
        fx.git(&dir, &["branch", "mid"]);
        fx.commit(&dir, "f3", "3\n", "three");
        std::fs::write(dir.join("README"), "edited\n").unwrap();
        let opened = opened(&runner, &dir, &Base::Commit(base.clone()));
        let stamp = drop_with_update_refs(&lane, &runner, &dir, opened, &c1);
        assert_eq!(
            group_names(&fx, &dir, &stamp),
            ["heads/main", "heads/mid", "manifest", "worktree"]
        );
        let plan = restore_all(&runner, &dir, &stamp);
        let wipe = refusals_first(&lane, &plan, 2);
        moves_after_the_backup(&dir, "refs/heads/mid", &base);
        let head = fx.rev(&dir, "HEAD").unwrap();
        let before = untouched(&fx, &dir);
        let outcome =
            crate::git::actions::execute(&plan, dir.to_str().unwrap(), &runner, &Handle::new());
        let at = outcome
            .steps
            .iter()
            .position(|s| s.outcome == StepOutcome::Failed)
            .unwrap_or_else(|| panic!("{lane}: the ref transaction did not refuse"));
        assert!(
            at == 2 && outcome.steps[at].line.starts_with("git update-ref --stdin"),
            "{lane}: step {at}: {}",
            outcome.steps[at].line
        );
        assert!(at < wipe, "{lane}: stopped before the work tree's reset");
        super::actions::failed_at(&lane, &outcome, at);
        assert_eq!(fx.rev(&dir, "HEAD").unwrap(), head, "{lane}: HEAD");
        assert_eq!(
            fx.oid(&dir, "mid"),
            base,
            "{lane}: mid where the hook put it"
        );
        assert_eq!(untouched(&fx, &dir), before, "{lane}: the work tree");
        assert_eq!(outcome.warning(), None, "{lane}");
    }
}

/// 03 §8.3's order with two HEAD items chosen — a detached HEAD's and the branch checked out
/// since (a detached `--update-refs` edit run with `--autostash`, then that branch checked
/// out): both checks come before the work tree's reset. Such a restore never gets past its own
/// backup: that one transaction verifies `HEAD` and the branch `HEAD` points to, which git
/// refuses (`multiple updates for 'refs/heads/side' (including one via symref 'HEAD') are not
/// allowed`), so it stops before anything else runs, the work tree, the index and HEAD as they
/// were.
#[test]
fn both_head_checks_come_before_the_work_tree_reset() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("two-heads-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        fx.git(&dir, &["checkout", "-q", "--detach"]);
        let c1 = fx.commit(&dir, "f1", "1\n", "one");
        fx.commit(&dir, "f2", "2\n", "two");
        fx.git(&dir, &["branch", "side"]);
        fx.commit(&dir, "f3", "3\n", "three");
        std::fs::write(dir.join("README"), "edited\n").unwrap();
        let opened = opened(&runner, &dir, &Base::Commit(base));
        let stamp = drop_with_update_refs(&lane, &runner, &dir, opened, &c1);
        assert_eq!(
            group_names(&fx, &dir, &stamp),
            ["detached", "heads/side", "manifest", "worktree"]
        );
        fx.git(&dir, &["checkout", "-q", "side"]);
        let plan = restore_all(&runner, &dir, &stamp);
        let lines = super::actions::commands(&plan);
        let moves = lines
            .iter()
            .filter(|l| l.starts_with("git reset --keep "))
            .count();
        assert_eq!(moves, 2, "{lane}: two HEAD items: {lines:#?}");
        refusals_first(&lane, &plan, 2);
        let head = fx.rev(&dir, "HEAD").unwrap();
        let before = untouched(&fx, &dir);
        let outcome =
            crate::git::actions::execute(&plan, dir.to_str().unwrap(), &runner, &Handle::new());
        let text = super::actions::failed_at(&lane, &outcome, 0);
        assert!(
            text.contains("multiple updates for 'refs/heads/side'"),
            "{lane}: {text}"
        );
        assert_eq!(outcome.backup, None, "{lane}: no group written");
        assert_eq!(fx.rev(&dir, "HEAD").unwrap(), head, "{lane}: HEAD");
        assert_eq!(untouched(&fx, &dir), before, "{lane}: the work tree");
    }
}

/// The inverse of a reset uses the reset's own mode (coordinator, 2026-11-23): a mixed or soft
/// reset — of a checked-out branch and of a detached HEAD — undone puts HEAD (and with mixed the
/// index) back, leaves every working file byte for byte as it is, and refuses nothing; redo
/// moves them forward again the same way.
#[test]
fn a_mixed_or_soft_reset_is_undone_in_its_own_mode() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        for mode in [ResetMode::Mixed, ResetMode::Soft] {
            for detached in [false, true] {
                let name = format!("own-mode-{lane}-{mode:?}-{detached}");
                let dir = fx.repo(&name);
                let c1 = fx.oid(&dir, "HEAD");
                std::fs::write(dir.join("README"), "changed\r\n").unwrap();
                let c2 = fx.commit(&dir, "a", "added\n", "second");
                if detached {
                    fx.git(&dir, &["checkout", "-q", "--detach", c2.as_str()]);
                }
                let tree = |dir: &Path| fx.git(dir, &["write-tree"]).trim().to_string();
                let c2_tree = tree(&dir);
                let mut window = Undo::default();
                act(
                    &lane,
                    &runner,
                    &dir,
                    &mut window,
                    Action::Reset {
                        to: c1.clone(),
                        mode,
                    },
                );
                let files = (bytes(&dir, "README"), bytes(&dir, "a"));
                assert_eq!(files.0.as_deref(), Some(&b"changed\r\n"[..]), "{name}");
                let index_after = tree(&dir);
                // Undo: HEAD (and the index) back, the files untouched.
                let stamp = window.newest().unwrap().to_string();
                let (entries, plan, outcome) = restore(&runner, &dir, &stamp);
                done(&name, &outcome);
                window.push(outcome.backup.clone().unwrap());
                assert!(entries.iter().any(|e| e.subject == Subject::Head
                    || matches!(&e.subject, Subject::Ref(r) if r == "refs/heads/main")));
                let flag = if mode == ResetMode::Mixed {
                    "--mixed"
                } else {
                    "--soft"
                };
                assert!(
                    super::actions::commands(&plan)
                        .iter()
                        .any(|l| l == &format!("git reset {flag} {}", c2.as_str())),
                    "{name}: {:?}",
                    super::actions::commands(&plan)
                );
                let notes = plan.confirm.as_ref().unwrap().changes.join("\n");
                assert!(
                    notes.contains("working files are left as they are"),
                    "{name}: {notes}"
                );
                assert_eq!(
                    fx.rev(&dir, "HEAD").unwrap(),
                    c2.as_str(),
                    "{name}: HEAD back"
                );
                assert_eq!(tree(&dir), c2_tree, "{name}: the index back");
                assert_eq!(
                    (bytes(&dir, "README"), bytes(&dir, "a")),
                    files,
                    "{name}: files untouched"
                );
                assert_eq!(
                    fx.output(&dir, &["symbolic-ref", "-q", "HEAD"])
                        .status
                        .success(),
                    !detached,
                    "{name}"
                );
                // Redo: forward again, the same way.
                undo(&name, &runner, &dir, &mut window);
                assert_eq!(fx.rev(&dir, "HEAD").unwrap(), c1.as_str(), "{name}: redo");
                assert_eq!(tree(&dir), index_after, "{name}: redo's index");
                assert_eq!(
                    (bytes(&dir, "README"), bytes(&dir, "a")),
                    files,
                    "{name}: redo's files"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Review round 1 (PR #83)

/// Review 3: a group without its `after` record (its seal failed) cannot tell "moved since" from
/// "changed by the action": unknown counts as moved, so a ref that moved later is not chosen.
#[test]
fn r3_no_after_record_counts_as_moved() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("r3-{lane}"));
        let c1 = fx.oid(&dir, "HEAD");
        let c2 = fx.commit(&dir, "a", "a\n", "second");
        fx.git(&dir, &["branch", "gone", c2.as_str()]);
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::DeleteBranch {
                branch: "gone".into(),
                force: true,
                remotes: vec![],
            },
        );
        done(&lane, &outcome);
        let stamp = outcome.backup.clone().unwrap();
        fx.git(
            &dir,
            &[
                "update-ref",
                "-d",
                &format!("refs/sopen-backup/{stamp}/after"),
            ],
        );
        fx.git(&dir, &["branch", "gone", c1.as_str()]);
        let top = dir.to_str().unwrap();
        let record = backup::group(&runner, &Handle::new(), top, &stamp)
            .unwrap()
            .unwrap();
        let repo = repo_of(&runner, &dir);
        let entries = backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
        let gone = entries
            .iter()
            .find(|e| e.subject == Subject::Ref("refs/heads/gone".into()))
            .unwrap();
        assert!(gone.moved_since && !gone.chosen, "{lane}: {gone:?}");
    }
}

/// Review 5: a path holding a carriage return — at its end, or as CRLF inside — reads back from
/// the manifest exactly.
#[test]
fn r5_the_manifest_keeps_carriage_returns() {
    let oid = Oid::parse(&"e".repeat(40)).unwrap();
    for path in ["name\r", "a\r\nb", "\r", "x\\r"] {
        let manifest = Manifest {
            entries: vec![ManifestEntry {
                subject: Subject::Path(path.into()),
                before: Value::Object(oid.clone()),
            }],
            moved_by: None,
        };
        let message = manifest.message("sopen: x");
        assert_eq!(Manifest::parse(&message), Some(manifest), "{path:?}");
    }
}

/// Review 6: the restore's delete of a path the manifest says was absent names that one path:
/// redo of an untracked file named `*` removes it and leaves its untracked neighbour alone.
#[test]
fn r6_a_path_is_never_a_pattern_in_the_restore() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("r6-{lane}"));
        std::fs::write(dir.join("*"), "star\n").unwrap();
        let mut window = Undo::default();
        act(
            &lane,
            &runner,
            &dir,
            &mut window,
            Action::Clean { folders: true },
        );
        undo(&lane, &runner, &dir, &mut window);
        assert_eq!(bytes(&dir, "*").as_deref(), Some(&b"star\n"[..]));
        std::fs::write(dir.join("keep.txt"), "a neighbour\n").unwrap();
        undo(&lane, &runner, &dir, &mut window);
        assert_eq!(bytes(&dir, "*"), None, "{lane}: redo removes `*`");
        assert_eq!(
            bytes(&dir, "keep.txt").as_deref(),
            Some(&b"a neighbour\n"[..]),
            "{lane}: the neighbour is left alone"
        );
    }
}

/// Review 7: the work tree's restore checks what `stash apply --index` writes against the
/// untracked and ignored files first: an ignored file at a path the backup adds (outside the
/// reset's footprint) refuses it, naming the file, which is left alone.
#[test]
fn r7_the_work_tree_restore_never_writes_over_an_ignored_file() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("r7-{lane}"));
        std::fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
        fx.git(&dir, &["add", ".gitignore"]);
        fx.git(&dir, &["commit", "-q", "-m", "ignore"]);
        std::fs::write(dir.join("n.log"), "staged\n").unwrap();
        fx.git(&dir, &["add", "-f", "n.log"]);
        let (_, outcome) = run(&runner, &dir, &Action::ResetChanges { hard: true });
        done(&lane, &outcome);
        let stamp = outcome.backup.clone().unwrap();
        assert_eq!(bytes(&dir, "n.log"), None);
        std::fs::write(dir.join("n.log"), "mine, ignored\n").unwrap();
        let top = dir.to_str().unwrap();
        let record = backup::group(&runner, &Handle::new(), top, &stamp)
            .unwrap()
            .unwrap();
        let repo = repo_of(&runner, &dir);
        let entries = backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
        let refused = super::actions::planned(&runner, &dir, &Action::Restore { stamp, entries })
            .map(|p| p.confirm.map(|c| c.commands))
            .unwrap_err();
        assert!(refused.0.contains("n.log"), "{lane}: {}", refused.0);
        assert_eq!(
            bytes(&dir, "n.log").as_deref(),
            Some(&b"mine, ignored\n"[..]),
            "{lane}"
        );
    }
}

/// Review 8: when the action's steps are done but the group's `after` record cannot be written
/// (a `reference-transaction` hook refuses it), the outcome says so in its warning.
#[test]
fn r8_an_incomplete_backup_is_said() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let dir = fx.repo(&format!("r8-{lane}"));
        fx.git(&dir, &["branch", "gone"]);
        super::actions::script(
            &dir.join(".git/hooks/reference-transaction"),
            "[ \"$1\" = prepared ] || exit 0\nif grep -q '/after$'; then exit 1; fi\nexit 0",
        );
        let (_, outcome) = run(
            &runner,
            &dir,
            &Action::DeleteBranch {
                branch: "gone".into(),
                force: true,
                remotes: vec![],
            },
        );
        done(&lane, &outcome);
        let stamp = outcome.backup.clone().expect("the group");
        let warning = outcome.warning().unwrap_or_default();
        assert!(
            warning.starts_with(&format!("backup {stamp} incomplete: ")),
            "{lane}: {warning:?}"
        );
        assert!(fx.rev(&dir, "refs/heads/gone").is_none());
    }
}
