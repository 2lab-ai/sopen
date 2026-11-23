//! The history edits' tests (03 §7.8, §8.5's edit stop; G-H1–G-H5), each run through `LocalGit`
//! and `ShellGit`, asserting the commits, messages and refs after, the confirmation's command
//! lines and the backup refs. S4-A writes them. The fixture is `tests/actions.rs`'.
//!
//! The first half is pure: the ids, the verbs, the model's ops and checks, the todo's lines byte
//! for byte. The second half runs real git on fixture repositories, in every lane.

use super::actions::{commands, done, failed_at, fixture, repo_of, run as act, script, Fx};
use crate::git::actions::{
    execute, inspect, Action, ActionId, Banner, Outcome, Plan, Proceed, RebaseOptions, Refused,
    Repo, Rev, Seen, StepOutcome,
};
use crate::git::backup;
use crate::git::model::{
    BranchStatus, Change, CommitRow, GitVersion, Head, InProgress, Oid, Operation, Status,
    StatusEntry, StatusState,
};
use crate::git::rebase::{
    needs, open, open_needs, plan, split_of, Author, AuthorLine, Base, BranchTip, Commit,
    CommitterDate, Date, Edit, EditId, Exec, Model, Named, Op, Problem, Quick, Run, Split,
    TodoLine, Toward, Verb, HEAD_MESSAGE, SEQUENCE_EDITOR,
};
use crate::git::runner::{GitRunner, Handle};
use std::path::Path;

// =============================================================================================
// Pure: the tables S4-0 wrote in full, the model, the todo

fn o(c: char) -> Oid {
    Oid::parse(&c.to_string().repeat(40)).unwrap()
}

fn date(seconds: i64, offset_minutes: i32) -> Date {
    Date {
        seconds,
        offset_minutes,
    }
}

fn someone(name: &str) -> Author {
    Author {
        name: name.into(),
        email: format!("{}@example.invalid", name.to_lowercase()),
        date: date(1_000, 0),
        committer: CommitterDate::Keep,
    }
}

/// A commit `id` whose parent is `parent`, its message `<subject>\n`.
fn commit(id: char, parent: char, subject: &str) -> Commit {
    Commit {
        row: CommitRow {
            oid: o(id),
            parents: vec![o(parent)],
            author_name: "Ann".into(),
            author_email: "ann@example.invalid".into(),
            author_time: 1_000,
            commit_time: 2_000,
            subject: subject.into(),
        },
        message: format!("{subject}\n"),
        message_lossy: false,
        author: someone("Ann"),
        committed: date(2_000, 0),
    }
}

/// The model of a chain above `0`: `ids` oldest first, each the parent of the next.
fn chain(ids: &str) -> Model {
    let mut parent = '0';
    let mut commits = Vec::new();
    for id in ids.chars() {
        commits.push(commit(id, parent, &format!("subject {id}")));
        parent = id;
    }
    commits.reverse();
    Model::new(Base::Commit(o('0')), commits)
}

/// A clean repository whose HEAD is `main` at `head`.
fn clean_repo(head: char) -> Repo {
    Repo {
        top: "/nowhere".into(),
        git_dir: "/nowhere/.git".into(),
        version: GitVersion {
            major: 2,
            minor: 56,
            patch: 1,
            text: "2.56.1".into(),
        },
        head: Head::Branch {
            refname: "refs/heads/main".into(),
            lossy: false,
            oid: o(head),
        },
        refs: Vec::new(),
        stashes: Vec::new(),
        status: Status {
            branch: BranchStatus::default(),
            entries: Vec::new(),
        },
        in_progress: InProgress::default(),
        remotes: Vec::new(),
        seen: Seen::default(),
    }
}

fn entry(path: &str, state: StatusState) -> StatusEntry {
    StatusEntry {
        path: path.into(),
        old_path: None,
        state,
        lossy: false,
    }
}

/// S4-0's id table: every id round-trips through `parse`, and no spelling is an `ActionId`'s
/// (02 §9: `SOPEN_STATE dialog=` tries `ActionId::parse`, then `EditId::parse`).
#[test]
fn edit_ids_round_trip_and_are_never_action_ids() {
    let mut seen = std::collections::HashSet::new();
    for id in EditId::ALL {
        assert!(seen.insert(id.as_str()), "{} twice", id.as_str());
        assert_eq!(EditId::parse(id.as_str()), Some(id));
        assert_eq!(ActionId::parse(id.as_str()), None, "{}", id.as_str());
    }
    for id in ActionId::ALL {
        assert_eq!(EditId::parse(id.as_str()), None, "{}", id.as_str());
    }
    assert_eq!(seen.len(), 11);
    assert_eq!(EditId::parse("move_up"), None);
    assert_eq!(EditId::parse("Reword"), None);
}

/// 03 §7.8.2: `P R E S F D` set the verbs, typed in either case; no other letter does (`A` is
/// the author key, not a verb).
#[test]
fn verbs_take_their_letter_in_either_case() {
    let mut words = std::collections::HashSet::new();
    for verb in Verb::ALL {
        assert!(words.insert(verb.word()));
        assert_eq!(Verb::of_letter(verb.letter()), Some(verb));
        assert_eq!(
            Verb::of_letter(verb.letter().to_ascii_uppercase()),
            Some(verb)
        );
    }
    assert_eq!(
        Verb::ALL.map(|v| v.letter()),
        ['p', 'r', 'e', 's', 'f', 'd']
    );
    for other in ['a', 'A', 'x', 'q', '1', ' ', 'é'] {
        assert_eq!(Verb::of_letter(other), None, "{other:?}");
    }
    assert_eq!(Verb::default(), Verb::Pick);
}

/// 03 §7.8.1: each quick edit's id.
#[test]
fn quick_ids() {
    let c = o('a');
    let cases = [
        (
            Quick::Reword {
                commit: c.clone(),
                message: "m".into(),
            },
            EditId::Reword,
        ),
        (
            Quick::Author {
                commit: c.clone(),
                author: someone("Ann"),
            },
            EditId::Author,
        ),
        (
            Quick::Squash {
                commit: c.clone(),
                message: "m".into(),
            },
            EditId::Squash,
        ),
        (Quick::Fixup { commit: c.clone() }, EditId::Fixup),
        (
            Quick::Move {
                commit: c.clone(),
                toward: Toward::Up,
            },
            EditId::MoveUp,
        ),
        (
            Quick::Move {
                commit: c.clone(),
                toward: Toward::Down,
            },
            EditId::MoveDown,
        ),
        (Quick::Drop { commit: c.clone() }, EditId::Drop),
        (Quick::Edit { commit: c.clone() }, EditId::Edit),
        (Quick::Split { commit: c.clone() }, EditId::Split),
        (Quick::Fold { into: c.clone() }, EditId::Fold),
    ];
    for (quick, id) in cases {
        assert_eq!(quick.id(), id, "{quick:?}");
    }
}

/// `Model::new`: every row a `pick` as the commit is, no option on, no branch, nothing changed.
#[test]
fn a_new_model_is_the_history_as_it_is() {
    let model = chain("123");
    assert_eq!(model.base, Base::Commit(o('0')));
    assert_eq!(
        model
            .rows
            .iter()
            .map(|r| r.commit.row.oid.clone())
            .collect::<Vec<_>>(),
        vec![o('3'), o('2'), o('1')]
    );
    for row in &model.rows {
        assert_eq!(row.verb, Verb::Pick);
        assert_eq!(row.message, None);
        assert_eq!(row.author, None);
    }
    assert!(!model.autostash);
    assert!(!model.update_refs);
    assert_eq!(model.read.order, vec![o('3'), o('2'), o('1')]);
    assert!(!model.read.autostash && !model.read.update_refs);
    assert_eq!(model.fold, None);
    assert!(model.branches.is_empty());
    assert!(model.left_out.is_empty());
    assert!(!model.changed());
    assert_eq!(model.problems(&clean_repo('3')), vec![]);
}

/// 03 §7.8.2: dates as `@<unix> <±hhmm>`; §7.8.1's committer-date choice.
#[test]
fn dates_and_the_author_line_values() {
    assert_eq!(date(1_700_000_000, 540).word(), "@1700000000 +0900");
    assert_eq!(date(1_700_000_000, -330).word(), "@1700000000 -0530");
    assert_eq!(date(0, 0).word(), "@0 +0000");
    assert_eq!(date(5, -45).word(), "@5 -0045");
    let committed = date(2_000, 60);
    let mut a = someone("Ann");
    a.date = date(1_000, -150);
    let line = a.line(&committed);
    assert_eq!(line.name, "Ann");
    assert_eq!(line.email, "ann@example.invalid");
    assert_eq!(line.date, date(1_000, -150));
    assert_eq!(line.committer, Some(committed));
    assert_eq!(line.message, None);
    a.committer = CommitterDate::Now;
    assert_eq!(a.line(&committed).committer, None);
    a.committer = CommitterDate::Author;
    assert_eq!(a.line(&committed).committer, Some(date(1_000, -150)));
}

/// The awkward message of G-H1: quotes, `\n` as text, `%`, `$`, backquotes, `\0101`, `\c`, a `#`
/// line, Hangul, a leading blank line, trailing spaces and blank lines, a tab, CR and ESC.
const HARD: &str = "\nsay \"hi\" and 'bye': \\n stays text, 100% $HOME `date` \\0101 \\c\n\
                    # a hash line\n한글 메시지\twith a tab\rCR \u{1b}[1mESC   \n\n\n";

/// 03 §7.8.2's todo lines as git reads them: the message line with `\` doubled and each newline
/// `\n`, quoted as one shell word, every other byte as it is; the author line with
/// `--cleanup=verbatim`, carrying the message when it knows it (S4-A: `--no-edit` drops a
/// leading blank line, and the last line decides the committer date); `update-ref`; `fold`'s
/// line naming the commit `--fixup` made as `HEAD`.
#[test]
fn todo_lines_as_git_reads_them() {
    let a = "a".repeat(40);
    let pick = TodoLine::Pick {
        commit: o('a'),
        subject: "one two".into(),
    };
    assert_eq!(pick.text(), format!("pick {a} one two"));
    let drop = TodoLine::Drop {
        commit: o('a'),
        subject: "x\ry\tz\u{1b}".into(),
    };
    assert_eq!(drop.text(), format!("drop {a} x y z "));
    let edit = TodoLine::Edit {
        commit: o('a'),
        subject: String::new(),
    };
    assert_eq!(edit.text(), format!("edit {a}"));
    let fixup = TodoLine::Fixup {
        commit: Named::Commit(o('a')),
        subject: "s".into(),
    };
    assert_eq!(fixup.text(), format!("fixup {a} s"));
    let folded = TodoLine::Fixup {
        commit: Named::Folded,
        subject: "fixup! one".into(),
    };
    assert_eq!(folded.text(), "fixup HEAD fixup! one");
    let update = TodoLine::UpdateRef {
        refname: "refs/heads/mid".into(),
    };
    assert_eq!(update.text(), "update-ref refs/heads/mid");

    let message = TodoLine::Exec(Exec::Message("it's a \\ back\nslash\t\r\u{1b}%$`#".into()));
    assert_eq!(
        message.text(),
        "exec printf '%b' 'it'\\''s a \\\\ back\\nslash\t\r\u{1b}%$`#' | git commit --amend \
         --only --allow-empty --cleanup=verbatim -F -"
    );
    let line = |message: Option<&str>, committer: Option<Date>| AuthorLine {
        name: "Ann O'Ther".into(),
        email: "ann@example.invalid".into(),
        date: date(1_600_000_000, -150),
        committer,
        message: message.map(str::to_string),
    };
    assert_eq!(
        TodoLine::Exec(Exec::Author(line(None, Some(date(1_700_000_000, 540))))).text(),
        format!(
            "exec {HEAD_MESSAGE} | GIT_COMMITTER_DATE='@1700000000 +0900' git commit --amend \
             --only --allow-empty --cleanup=verbatim -F - --author='Ann O'\\''Ther \
             <ann@example.invalid>' --date='@1600000000 -0230'"
        )
    );
    assert_eq!(
        TodoLine::Exec(Exec::Author(line(Some("m\n"), None))).text(),
        "exec printf '%b' 'm\\n' | git commit --amend --only --allow-empty --cleanup=verbatim \
         -F - --author='Ann O'\\''Ther <ann@example.invalid>' --date='@1600000000 -0230'"
    );
    let run = Run {
        base: Base::Root,
        todo: vec![pick.clone(), update.clone()],
        autostash: false,
        update_refs: true,
        moves: Vec::new(),
        left_out: Vec::new(),
    };
    assert_eq!(
        run.todo_text(),
        format!("pick {a} one two\nupdate-ref refs/heads/mid\n")
    );
    assert_eq!(SEQUENCE_EDITOR, r#"f(){ cat >"$1";};f"#);
}

/// 03 §7.8.2: the message line's `printf '%b'` gives the message back byte for byte under
/// `/bin/sh` (git's own shell for `exec` lines) and dash.
#[test]
fn the_message_line_comes_back_byte_for_byte_in_sh_and_dash() {
    for message in [HARD, "plain\n", "", "\\", "'", "a\\nb\\\\c%%d\n\n"] {
        let text = TodoLine::Exec(Exec::Message(message.into())).text();
        let printf = text
            .strip_prefix("exec ")
            .and_then(|t| t.split_once(" | git commit"))
            .map(|(p, _)| p.to_string())
            .expect("a message line");
        for shell in ["/bin/sh", "/bin/dash"] {
            if !Path::new(shell).exists() {
                continue;
            }
            let out = std::process::Command::new(shell)
                .arg("-c")
                .arg(&printf)
                .output()
                .unwrap();
            assert!(out.status.success(), "{shell}: {printf}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                message,
                "{shell}: {printf}"
            );
        }
    }
}

/// The author line's read of HEAD's message ([`HEAD_MESSAGE`]'s body after the header's blank
/// line) gives the message back byte for byte under `/bin/sh` and dash: a leading blank line, a
/// header line that continues (`gpgsig`), CR, no final newline.
#[test]
fn the_head_message_comes_back_byte_for_byte_in_sh_and_dash() {
    let body = HEAD_MESSAGE
        .split_once(" | ")
        .map(|(_, b)| b)
        .expect("the body part");
    for message in [
        &b"\n\nlead\r\n# x  \n\n"[..],
        b"plain\n",
        b"no newline",
        b"",
    ] {
        let mut object = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a@b> 1 +0000\n\
                           gpgsig -----BEGIN-----\n line two\n -----END-----\n\n"
            .to_vec();
        object.extend_from_slice(message);
        for shell in ["/bin/sh", "/bin/dash"] {
            if !Path::new(shell).exists() {
                continue;
            }
            use std::io::Write as _;
            let mut child = std::process::Command::new(shell)
                .arg("-c")
                .arg(body)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(&object).unwrap();
            let out = child.wait_with_output().unwrap();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(message),
                "{shell}"
            );
        }
    }
}

/// 03 §7.8.2's ops: what `apply` refuses, with the reason for the status row.
#[test]
fn apply_refuses_what_it_cannot_do() {
    let mut model = chain("123");
    let refused = |model: &mut Model, op: Op| model.apply(op).unwrap_err().0;
    let up = Op::Move {
        commit: o('3'),
        toward: Toward::Up,
    };
    assert!(refused(&mut model, up).contains("top"));
    let down = Op::Move {
        commit: o('1'),
        toward: Toward::Down,
    };
    assert!(refused(&mut model, down).contains("bottom"));
    let stranger = Op::Verb {
        commit: o('9'),
        verb: Verb::Drop,
    };
    assert!(refused(&mut model, stranger).contains("not in the range"));
    let message = |m: &str| Op::Message {
        commit: o('2'),
        message: m.into(),
    };
    assert!(refused(&mut model, message("m")).contains("reword or squash"));
    model
        .apply(Op::Verb {
            commit: o('2'),
            verb: Verb::Reword,
        })
        .unwrap();
    assert!(refused(&mut model, message("a\0b")).contains("NUL"));
    assert!(refused(&mut model, message("")).contains("empty"));
    let mut bad = someone("Ann");
    bad.name = "A <b>".into();
    let op = Op::Author {
        commit: o('2'),
        author: Some(bad),
    };
    assert!(refused(&mut model, op).contains('<'));
    let mut bad = someone("Ann");
    bad.email = "a\nb".into();
    let op = Op::Author {
        commit: o('2'),
        author: Some(bad),
    };
    assert!(refused(&mut model, op).contains("line"));
    // 03 §3.3, G-H3: a message that is not UTF-8 is never rewritten.
    model.rows[2].commit.message_lossy = true;
    for verb in [Verb::Reword, Verb::Squash] {
        let why = refused(
            &mut model,
            Op::Verb {
                commit: o('1'),
                verb,
            },
        );
        assert!(why.contains("not valid UTF-8"), "{why}");
    }
    let why = refused(
        &mut model,
        Op::Author {
            commit: o('1'),
            author: Some(someone("Bo")),
        },
    );
    assert!(why.contains("not valid UTF-8"), "{why}");
    for verb in [Verb::Pick, Verb::Drop, Verb::Edit, Verb::Fixup] {
        model
            .apply(Op::Verb {
                commit: o('1'),
                verb,
            })
            .unwrap();
    }
}

/// 03 §7.8.2's checks: a merge in the range named, a squash or fixup with no kept row below it,
/// uncommitted changes without `--autostash`, an operation in progress; and `changed` comes back
/// to false when every edit is undone.
#[test]
fn problems_and_changed() {
    let verb = |model: &mut Model, c: char, verb: Verb| {
        model.apply(Op::Verb { commit: o(c), verb }).unwrap()
    };
    let repo = clean_repo('3');
    let mut model = chain("123");
    verb(&mut model, '1', Verb::Squash);
    assert_eq!(model.problems(&repo), vec![Problem::NothingBelow(o('1'))]);
    verb(&mut model, '1', Verb::Drop);
    verb(&mut model, '2', Verb::Fixup);
    assert_eq!(model.problems(&repo), vec![Problem::NothingBelow(o('2'))]);
    let text = Problem::NothingBelow(o('2')).text();
    assert!(text.contains("22222222"), "{text}");
    assert!(model.run(&repo).unwrap_err().0.contains("22222222"));

    let mut model = chain("123");
    model.rows[1].commit.row.parents.push(o('f'));
    let problems = model.problems(&repo);
    assert_eq!(problems, vec![Problem::Merges(vec![o('2')])]);
    let text = problems[0].text();
    assert!(
        text.contains("22222222") && text.contains("merge"),
        "{text}"
    );

    let mut dirty = clean_repo('3');
    dirty.status.entries.push(entry(
        "f",
        StatusState::Changed {
            staged: None,
            unstaged: Some(Change::Modified),
        },
    ));
    let mut model = chain("123");
    assert_eq!(model.problems(&dirty), vec![Problem::Uncommitted]);
    assert!(Problem::Uncommitted.text().contains("uncommitted"));
    model.autostash = true;
    assert_eq!(model.problems(&dirty), vec![]);
    let mut untracked = clean_repo('3');
    untracked
        .status
        .entries
        .push(entry("u", StatusState::Untracked));
    model.autostash = false;
    assert_eq!(model.problems(&untracked), vec![]);
    let mut busy = clean_repo('3');
    busy.in_progress.operation = Some(Operation::Merge);
    let problems = model.problems(&busy);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].text().contains("in progress"), "{problems:?}");

    let mut model = chain("123");
    assert!(!model.changed());
    let toward = |toward| Op::Move {
        commit: o('2'),
        toward,
    };
    model.apply(toward(Toward::Up)).unwrap();
    assert!(model.changed());
    model.apply(toward(Toward::Down)).unwrap();
    assert!(!model.changed());
    verb(&mut model, '2', Verb::Edit);
    assert!(model.changed());
    verb(&mut model, '2', Verb::Pick);
    assert!(!model.changed());
    model
        .apply(Op::Author {
            commit: o('2'),
            author: Some(someone("Bo")),
        })
        .unwrap();
    assert!(model.changed());
    model
        .apply(Op::Author {
            commit: o('2'),
            author: None,
        })
        .unwrap();
    assert!(!model.changed());

    // `fold`: nothing staged is a problem; a fold into a dropped commit is refused (its
    // commit's line would be left out, and git would drop it with the staged changes).
    let mut model = chain("12");
    Quick::Fold { into: o('1') }.apply(&mut model).unwrap();
    assert!(model.changed());
    assert_eq!(
        model.problems(&clean_repo('2')),
        vec![Problem::NothingStaged]
    );
    let mut staged = clean_repo('2');
    staged.status.entries.push(entry(
        "f",
        StatusState::Changed {
            staged: Some(Change::Modified),
            unstaged: None,
        },
    ));
    assert_eq!(model.problems(&staged), vec![]);
    verb(&mut model, '1', Verb::Drop);
    let why = model.run(&staged).unwrap_err().0;
    assert!(why.contains("fold"), "{why}");
}

/// S4-B's Esc asks when something changed: a run option counts as well as the rows, against the
/// options as read (`open` turns `--autostash` on with uncommitted changes, which is no change).
#[test]
fn changed_counts_the_run_options() {
    let mut model = chain("12");
    model.autostash = true;
    assert!(model.changed());
    model.autostash = false;
    assert!(!model.changed());
    model.update_refs = true;
    assert!(model.changed());
    model.update_refs = false;
    assert!(!model.changed());
    // A typed message left on a row whose verb went back to `pick` is lost on close: a change.
    model
        .apply(Op::Verb {
            commit: o('2'),
            verb: Verb::Reword,
        })
        .unwrap();
    model
        .apply(Op::Message {
            commit: o('2'),
            message: "typed\n".into(),
        })
        .unwrap();
    model
        .apply(Op::Verb {
            commit: o('2'),
            verb: Verb::Pick,
        })
        .unwrap();
    assert!(model.changed());
    model.rows[0].message = None;
    assert!(!model.changed());
    // As `open` makes it on a dirty tree: on, and read on.
    model.autostash = true;
    model.read.autostash = true;
    assert!(!model.changed());
    model.autostash = false;
    assert!(model.changed());
}

/// A squash group's message is S4-B's (`planner::squash_message` and `group_message` on branch
/// `feat/browser-s4-planner`, read for this test): typed, else the head's (a reword's own) then
/// each squash row's, oldest first, a fixup's and a dropped row's left out, each whole, a blank
/// line after the older one's last line. The pane shows what the todo writes.
#[test]
fn the_squash_message_is_the_planners() {
    use crate::git::rebase::squash_message;
    assert_eq!(squash_message("a\n", "b\n"), "a\n\nb\n");
    assert_eq!(squash_message("a", "b"), "a\n\nb");
    assert_eq!(squash_message("a\n\n", "b"), "a\n\n\nb");
    assert_eq!(squash_message("", "b"), "\n\nb");

    let last_message = |model: &Model| match model.run(&clean_repo('5')).unwrap().todo.last() {
        Some(TodoLine::Exec(Exec::Message(m))) => m.clone(),
        other => panic!("not a message line: {other:?}"),
    };
    let verb = |model: &mut Model, c: char, verb: Verb| {
        model.apply(Op::Verb { commit: o(c), verb }).unwrap()
    };
    // Oldest first: 1 the head, 2 squash, 3 fixup, 4 dropped, 5 squash.
    let mut model = chain("12345");
    model.rows[4].commit.message = "one".into();
    model.rows[3].commit.message = "two\n\n".into();
    verb(&mut model, '2', Verb::Squash);
    verb(&mut model, '3', Verb::Fixup);
    verb(&mut model, '4', Verb::Drop);
    verb(&mut model, '5', Verb::Squash);
    assert_eq!(
        last_message(&model),
        squash_message(&squash_message("one", "two\n\n"), "subject 5\n")
    );
    assert_eq!(last_message(&model), "one\n\ntwo\n\n\nsubject 5\n");
    // A reword head gives its own message.
    verb(&mut model, '1', Verb::Reword);
    model
        .apply(Op::Message {
            commit: o('1'),
            message: "reworded".into(),
        })
        .unwrap();
    assert_eq!(last_message(&model), "reworded\n\ntwo\n\n\nsubject 5\n");
    // A typed message is the group's, on each squash row.
    model
        .apply(Op::Message {
            commit: o('5'),
            message: "typed\n".into(),
        })
        .unwrap();
    assert_eq!(model.rows[0].message.as_deref(), Some("typed\n"));
    assert_eq!(model.rows[3].message.as_deref(), Some("typed\n"));
    assert_eq!(last_message(&model), "typed\n");
    // Two groups' typed messages meet in one group when the row between them becomes a fixup:
    // the newest squash row's is the group's, as the pane scans (newest first).
    let mut model = chain("1234");
    verb(&mut model, '2', Verb::Squash);
    verb(&mut model, '4', Verb::Squash);
    for (c, m) in [('2', "older typed\n"), ('4', "newer typed\n")] {
        model
            .apply(Op::Message {
                commit: o(c),
                message: m.into(),
            })
            .unwrap();
    }
    verb(&mut model, '3', Verb::Fixup);
    assert_eq!(
        model.run(&clean_repo('4')).unwrap().todo.last(),
        Some(&TodoLine::Exec(Exec::Message("newer typed\n".into())))
    );
    // A squash row's message that is not UTF-8 is not joined.
    let mut model = chain("12");
    verb(&mut model, '2', Verb::Squash);
    model.rows[0].commit.message_lossy = true;
    let why = model.run(&clean_repo('2')).unwrap_err().0;
    assert!(why.contains("not valid UTF-8"), "{why}");
}

/// The todo, oldest first: a squash group's lines and its one message line, the author line
/// after the group (carrying the group's message), a branch's `update-ref` after the group it
/// points into, or after a dropped commit's line (it moves to the nearest kept commit below),
/// another worktree's branch left out. A squash row's message is its group's.
#[test]
fn the_todo_of_groups_authors_and_branches() {
    let verb = |model: &mut Model, c: char, verb: Verb| {
        model.apply(Op::Verb { commit: o(c), verb }).unwrap()
    };
    let tip = |b: &str, c: char| BranchTip {
        refname: format!("refs/heads/{b}"),
        tip: o(c),
    };
    let mut model = chain("12345");
    model.update_refs = true;
    model.branches = vec![
        tip("b2", '2'),
        tip("b3", '3'),
        tip("wt", '3'),
        tip("b4", '4'),
    ];
    model.left_out = vec![tip("wt", '3')];
    verb(&mut model, '2', Verb::Squash);
    verb(&mut model, '3', Verb::Fixup);
    model
        .apply(Op::Message {
            commit: o('2'),
            message: "group\n".into(),
        })
        .unwrap();
    assert_eq!(model.rows[3].message.as_deref(), Some("group\n"));
    let mut ann = someone("Ann");
    ann.committer = CommitterDate::Now;
    model
        .apply(Op::Author {
            commit: o('1'),
            author: Some(ann.clone()),
        })
        .unwrap();
    verb(&mut model, '4', Verb::Drop);
    verb(&mut model, '5', Verb::Reword);
    let run = model.run(&clean_repo('5')).unwrap();
    let mut line = ann.line(&date(2_000, 0));
    line.message = Some("group\n".into());
    let subject = |c: char| format!("subject {c}");
    assert_eq!(
        run.todo,
        vec![
            TodoLine::Pick {
                commit: o('1'),
                subject: subject('1')
            },
            TodoLine::Fixup {
                commit: Named::Commit(o('2')),
                subject: subject('2')
            },
            TodoLine::Fixup {
                commit: Named::Commit(o('3')),
                subject: subject('3')
            },
            TodoLine::Exec(Exec::Author(line)),
            TodoLine::UpdateRef {
                refname: "refs/heads/b2".into()
            },
            TodoLine::UpdateRef {
                refname: "refs/heads/b3".into()
            },
            TodoLine::Drop {
                commit: o('4'),
                subject: subject('4')
            },
            TodoLine::UpdateRef {
                refname: "refs/heads/b4".into()
            },
            TodoLine::Pick {
                commit: o('5'),
                subject: subject('5')
            },
            TodoLine::Exec(Exec::Message("subject 5\n".into())),
        ]
    );
    assert_eq!(
        run.moves
            .iter()
            .map(|m| m.refname.as_str())
            .collect::<Vec<_>>(),
        ["refs/heads/b2", "refs/heads/b3", "refs/heads/b4"]
    );
    assert_eq!(run.moves[0].from, o('2'));
    assert_eq!(run.moves[0].after, o('1'), "into a group: its head");
    assert_eq!(
        run.moves[2].after,
        o('1'),
        "a dropped tip: the kept commit below"
    );
    assert_eq!(run.left_out, vec![tip("wt", '3')]);
    assert!(run.update_refs);

    // A squash group with no message of its own: both messages, the older first, a blank
    // line between (03 §7.8.1's prefill).
    let mut model = chain("12");
    verb(&mut model, '2', Verb::Squash);
    let run = model.run(&clean_repo('2')).unwrap();
    assert_eq!(
        run.todo.last(),
        Some(&TodoLine::Exec(Exec::Message(
            "subject 1\n\nsubject 2\n".into()
        )))
    );
}

/// A message too long for one line of the todo (git runs an `exec` line as one `sh -c`
/// argument): typed, it is refused; an author line keeps a long message of the commit's own with
/// `--no-edit`; a joined squash message that long is refused, asking for a shorter one.
#[test]
fn a_message_too_long_for_the_todo() {
    let long = "x".repeat(70 * 1024);
    let mut model = chain("12");
    model
        .apply(Op::Verb {
            commit: o('2'),
            verb: Verb::Reword,
        })
        .unwrap();
    let why = model
        .apply(Op::Message {
            commit: o('2'),
            message: long.clone(),
        })
        .unwrap_err()
        .0;
    assert!(why.contains("too long"), "{why}");

    let mut model = chain("12");
    model.rows[1].commit.message = long.clone();
    model
        .apply(Op::Author {
            commit: o('1'),
            author: Some(someone("Bo")),
        })
        .unwrap();
    let run = model.run(&clean_repo('2')).unwrap();
    assert!(
        matches!(&run.todo[1], TodoLine::Exec(Exec::Author(l)) if l.message.is_none()),
        "{:?}",
        run.todo[1].text().len()
    );

    let half = "y".repeat(35 * 1024);
    let mut model = chain("12");
    model.rows[0].commit.message = half.clone();
    model.rows[1].commit.message = half;
    model
        .apply(Op::Verb {
            commit: o('2'),
            verb: Verb::Squash,
        })
        .unwrap();
    let why = model.run(&clean_repo('2')).unwrap_err().0;
    assert!(why.contains("too long"), "{why}");
}

/// git silently drops a commit its todo leaves out: after any ops, every row's commit is on
/// exactly one line of the todo, in the rows' order.
#[test]
fn every_row_is_on_exactly_one_line() {
    let repo = clean_repo('6');
    let ids = ['1', '2', '3', '4', '5', '6'];
    let mut seed: u64 = 0x5eed;
    let mut next = |n: usize| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((seed >> 33) as usize) % n
    };
    let mut ran = 0;
    for _ in 0..200 {
        let mut model = chain("123456");
        for _ in 0..8 {
            let c = o(ids[next(ids.len())]);
            let op = match next(3) {
                0 => Op::Move {
                    commit: c,
                    toward: if next(2) == 0 {
                        Toward::Up
                    } else {
                        Toward::Down
                    },
                },
                1 => Op::Verb {
                    commit: c,
                    verb: Verb::ALL[next(Verb::ALL.len())],
                },
                _ => Op::Author {
                    commit: c,
                    author: Some(someone("Bo")),
                },
            };
            let _ = model.apply(op);
        }
        let Ok(run) = model.run(&repo) else {
            assert!(!model.problems(&repo).is_empty());
            continue;
        };
        ran += 1;
        let named: Vec<Oid> = run
            .todo
            .iter()
            .filter_map(|l| match l {
                TodoLine::Pick { commit, .. }
                | TodoLine::Drop { commit, .. }
                | TodoLine::Edit { commit, .. } => Some(commit.clone()),
                TodoLine::Fixup {
                    commit: Named::Commit(c),
                    ..
                } => Some(c.clone()),
                _ => None,
            })
            .collect();
        let rows: Vec<Oid> = model
            .rows
            .iter()
            .rev()
            .map(|r| r.commit.row.oid.clone())
            .collect();
        assert_eq!(named, rows, "{:#?}", run.todo);
    }
    assert!(ran > 50, "{ran} of 200 ran");
}

// =============================================================================================
// On real git: helpers

/// `repo` with every read `edit` needs.
fn ready(runner: &GitRunner, repo: &mut Repo, edit: &Edit) {
    for _ in 0..6 {
        let missing = needs(edit, repo);
        if missing.is_empty() {
            return;
        }
        inspect(runner, &Handle::new(), repo, &missing)
            .unwrap_or_else(|e| panic!("inspect: {}", e.reason));
    }
    panic!("needs did not settle for {edit:?}");
}

/// The plan of `edit` on `dir` as it is now.
fn planned(runner: &GitRunner, dir: &Path, edit: &Edit) -> Result<Plan, Refused> {
    let mut repo = repo_of(runner, dir);
    ready(runner, &mut repo, edit);
    plan(edit, &repo)
}

/// Plans `edit` against `repo` (with whatever it already read) and runs it.
fn run_on(runner: &GitRunner, dir: &Path, mut repo: Repo, edit: &Edit) -> (Plan, Outcome) {
    ready(runner, &mut repo, edit);
    let plan = plan(edit, &repo).unwrap_or_else(|r| panic!("refused: {}", r.0));
    let outcome = execute(&plan, dir.to_str().unwrap(), runner, &Handle::new());
    (plan, outcome)
}

fn run(runner: &GitRunner, dir: &Path, edit: &Edit) -> (Plan, Outcome) {
    run_on(runner, dir, repo_of(runner, dir), edit)
}

/// The planner opened on `base`: the repository with the reads made, and the model.
fn opened(runner: &GitRunner, dir: &Path, base: &Base) -> (Repo, Model) {
    let mut repo = repo_of(runner, dir);
    for _ in 0..6 {
        let missing = open_needs(base, &repo);
        if missing.is_empty() {
            break;
        }
        inspect(runner, &Handle::new(), &mut repo, &missing)
            .unwrap_or_else(|e| panic!("inspect: {}", e.reason));
    }
    let model = open(base, &repo).unwrap_or_else(|r| panic!("open refused: {}", r.0));
    (repo, model)
}

/// A commit's message as git stores it, byte for byte.
fn raw_message(fx: &Fx, dir: &Path, rev: &str) -> Vec<u8> {
    let out = fx.output(dir, &["cat-file", "commit", rev]);
    assert!(out.status.success(), "cat-file {rev}");
    let at = out
        .stdout
        .windows(2)
        .position(|w| w == b"\n\n")
        .expect("a message");
    out.stdout[at + 2..].to_vec()
}

/// `%an`, `%ae`, `%ad` and `%cd` (raw dates) of `rev`.
fn idents(fx: &Fx, dir: &Path, rev: &str) -> [String; 4] {
    let out = fx.git(
        dir,
        &[
            "log",
            "-1",
            "--date=raw",
            "--format=%an%x00%ae%x00%ad%x00%cd",
            rev,
        ],
    );
    let parts: Vec<String> = out.trim_end().split('\0').map(str::to_string).collect();
    [
        parts[0].clone(),
        parts[1].clone(),
        parts[2].clone(),
        parts[3].clone(),
    ]
}

/// Commits the index — `file` written and added first, when given — with `message` byte for
/// byte, its dates fixed at `when` (`<unix> <±hhmm>`) when given.
fn commit_raw(
    fx: &Fx,
    dir: &Path,
    file: Option<(&str, &str)>,
    message: &[u8],
    when: Option<&str>,
) -> Oid {
    if let Some((path, content)) = file {
        let f = dir.join(path);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, content).unwrap();
        fx.git(dir, &["add", "-f", "--", path]);
    }
    let m = fx.root.join("message.txt");
    std::fs::write(&m, message).unwrap();
    let mut c = fx.command(
        dir,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "--cleanup=verbatim",
            "-F",
            m.to_str().unwrap(),
        ],
    );
    if let Some(at) = when {
        c.env("GIT_AUTHOR_DATE", at).env("GIT_COMMITTER_DATE", at);
    }
    let out = c.output().unwrap();
    assert!(
        out.status.success(),
        "commit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    fx.oid(dir, "HEAD")
}

/// A commit of the index — `file` written and added first — whose message is `message` byte for
/// byte, made with `hash-object`: `commit` and `commit-tree` take bytes that are not UTF-8 as
/// Latin-1 and convert them. The branch moves to it.
fn commit_object(fx: &Fx, dir: &Path, file: (&str, &str), message: &[u8]) -> Oid {
    use std::io::Write as _;
    std::fs::write(dir.join(file.0), file.1).unwrap();
    fx.git(dir, &["add", "--", file.0]);
    let tree = fx.git(dir, &["write-tree"]).trim().to_string();
    let parent = fx.oid(dir, "HEAD");
    let who = "A U Thor <a@example.invalid> 1500000000 +0000";
    let mut object = format!(
        "tree {tree}\nparent {}\nauthor {who}\ncommitter {who}\n\n",
        parent.as_str()
    )
    .into_bytes();
    object.extend_from_slice(message);
    let mut c = fx.command(dir, &["hash-object", "-t", "commit", "-w", "--stdin"]);
    c.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(&object).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "hash-object");
    let oid = String::from_utf8(out.stdout).unwrap().trim().to_string();
    fx.git(dir, &["update-ref", "HEAD", &oid]);
    Oid::parse(&oid).unwrap()
}

/// The subjects of `range`, oldest first.
fn subjects(fx: &Fx, dir: &Path, range: &str) -> Vec<String> {
    fx.git(dir, &["log", "--reverse", "--format=%s", range])
        .lines()
        .map(str::to_string)
        .collect()
}

/// Nothing in progress (the probe's markers).
fn idle(runner: &GitRunner, dir: &Path) -> bool {
    repo_of(runner, dir).in_progress.operation.is_none()
}

/// The names of the group `stamp` (the `after` record left out), sorted.
fn group_names(fx: &Fx, dir: &Path, stamp: &str) -> Vec<String> {
    let mut names: Vec<String> = fx
        .git(
            dir,
            &[
                "for-each-ref",
                "--format=%(refname)",
                &format!("refs/sopen-backup/{stamp}/"),
            ],
        )
        .lines()
        .map(|l| l.rsplit_once(&format!("{stamp}/")).unwrap().1.to_string())
        .filter(|n| n != "after")
        .collect();
    names.sort();
    names
}

/// Restores the group `stamp` with its entries as the dialog first shows them.
fn restore(runner: &GitRunner, dir: &Path, stamp: &str) -> (Vec<backup::RestoreEntry>, Outcome) {
    let (entries, _, outcome) = restore_shown(runner, dir, stamp);
    (entries, outcome)
}

/// [`restore`], with the plan its dialog showed.
fn restore_shown(
    runner: &GitRunner,
    dir: &Path,
    stamp: &str,
) -> (Vec<backup::RestoreEntry>, Plan, Outcome) {
    let top = dir.to_str().unwrap();
    let record = backup::group(runner, &Handle::new(), top, stamp)
        .unwrap()
        .expect("the group");
    let repo = repo_of(runner, dir);
    let entries = backup::entries(runner, &Handle::new(), top, &record, &repo.head).unwrap();
    let (plan, outcome) = act(
        runner,
        dir,
        &Action::Restore {
            stamp: stamp.into(),
            entries: entries.clone(),
        },
    );
    (entries, plan, outcome)
}

/// The restore's confirmation lists the work tree's item (03 §8.3), so the owner sees that Undo
/// resets the tracked changes too, and its own group backs up what is there first.
#[track_caller]
fn shows_the_work_tree(lane: &str, plan: &Plan) {
    let confirm = plan.confirm.as_ref().expect("a confirmation");
    assert!(
        confirm
            .changes
            .iter()
            .any(|c| c == "the tracked changes as they were (staged and unstaged)"),
        "{lane}: {:?}",
        confirm.changes
    );
    assert!(
        confirm.backups.iter().any(|b| b == "worktree"),
        "{lane}: {:?}",
        confirm.backups
    );
}

/// The confirmation's line of a planner run, its options `extra`, its base `base`.
fn rebase_line(extra: &str, base: &str) -> String {
    format!(
        "'GIT_SEQUENCE_EDITOR=f(){{ cat >\"$1\";}};f' git -c 'core.commentChar=#' rebase -i \
         --reschedule-failed-exec {extra}{base}"
    )
}

fn backups_of(plan: &Plan) -> Vec<String> {
    plan.confirm
        .as_ref()
        .map(|c| c.backups.clone())
        .unwrap_or_default()
}

// =============================================================================================
// G-H1: one run of every verb

/// The reworded empty commit's message.
const EMPTY_REWORDED: &str = "three, reworded\n\nbody   \n";
/// The reword a squash group's message shadows (asserted absent from the todo).
const SHADOWED: &str = "shadowed reword of two\n";

/// G-H1 on one lane. 03's list of ops cannot all show on distinct rows of five commits, so the
/// commits are numbered oldest first and each op's positions are counted as the ops before it
/// left them: reword the 2nd (shadowed by the group's message, as the todo shows), reword the
/// empty one, move the 4th below the 3rd, drop the 5th, squash the (now) 3rd into the 2nd with a
/// new message, change the 1st's author and dates.
fn g_h1_in(fx: &Fx, lane: &str, runner: &GitRunner, name: &str, hostile_editor: bool) {
    let dir = fx.repo(name);
    if hostile_editor {
        fx.git(&dir, &["config", "sequence.editor", "false"]);
    }
    let base = fx.oid(&dir, "HEAD");
    let c1 = fx.commit(&dir, "f1", "1\n", "one");
    let c2 = fx.commit(&dir, "f2", "2\n", "two");
    let c3 = commit_raw(fx, &dir, None, b"three (empty)\n", None);
    let c4 = fx.commit(&dir, "f4", "4\n", "four");
    let c5 = fx.commit(&dir, "f5", "5\n", "five");
    let (repo, mut model) = opened(runner, &dir, &Base::Commit(base.clone()));
    let oids: Vec<Oid> = model
        .rows
        .iter()
        .map(|r| r.commit.row.oid.clone())
        .collect();
    assert_eq!(oids, [&c5, &c4, &c3, &c2, &c1].map(Oid::clone), "{lane}");
    let ops = [
        Op::Verb {
            commit: c2.clone(),
            verb: Verb::Reword,
        },
        Op::Message {
            commit: c2.clone(),
            message: SHADOWED.into(),
        },
        Op::Verb {
            commit: c3.clone(),
            verb: Verb::Reword,
        },
        Op::Message {
            commit: c3.clone(),
            message: EMPTY_REWORDED.into(),
        },
        Op::Move {
            commit: c4.clone(),
            toward: Toward::Down,
        },
        Op::Verb {
            commit: c5.clone(),
            verb: Verb::Drop,
        },
        Op::Verb {
            commit: c4.clone(),
            verb: Verb::Squash,
        },
        Op::Message {
            commit: c4.clone(),
            message: HARD.into(),
        },
        Op::Author {
            commit: c1.clone(),
            author: Some(Author {
                name: "Ann O'Ther".into(),
                email: "ann@example.invalid".into(),
                date: date(1_600_000_000, -150),
                committer: CommitterDate::Author,
            }),
        },
    ];
    for op in ops {
        model
            .apply(op.clone())
            .unwrap_or_else(|r| panic!("{lane}: {op:?}: {}", r.0));
    }
    assert!(model.changed());
    assert_eq!(model.problems(&repo), vec![], "{lane}");
    let todo = model.run(&repo).unwrap();
    let author = AuthorLine {
        name: "Ann O'Ther".into(),
        email: "ann@example.invalid".into(),
        date: date(1_600_000_000, -150),
        committer: Some(date(1_600_000_000, -150)),
        message: Some("one\n".into()),
    };
    assert_eq!(
        todo.todo,
        vec![
            TodoLine::Pick {
                commit: c1.clone(),
                subject: "one".into()
            },
            TodoLine::Exec(Exec::Author(author)),
            TodoLine::Pick {
                commit: c2.clone(),
                subject: "two".into()
            },
            TodoLine::Fixup {
                commit: Named::Commit(c4.clone()),
                subject: "four".into()
            },
            TodoLine::Exec(Exec::Message(HARD.into())),
            TodoLine::Pick {
                commit: c3.clone(),
                subject: "three (empty)".into()
            },
            TodoLine::Exec(Exec::Message(EMPTY_REWORDED.into())),
            TodoLine::Drop {
                commit: c5.clone(),
                subject: "five".into()
            },
        ],
        "{lane}"
    );
    assert!(
        !todo.todo_text().contains("shadowed"),
        "{lane}: the group's message shadows the 2nd's reword"
    );
    let (plan, outcome) = run_on(runner, &dir, repo, &Edit::Plan(model));
    assert_eq!(
        commands(&plan),
        vec![
            "backup heads/main, manifest under refs/sopen-backup/<stamp>/".to_string(),
            rebase_line("--no-update-refs ", base.as_str()),
        ],
        "{lane}"
    );
    done(lane, &outcome);
    let stamp = outcome.backup.clone().expect("a group");
    assert_eq!(group_names(fx, &dir, &stamp), ["heads/main", "manifest"]);
    let new: Vec<String> = fx
        .git(
            &dir,
            &["rev-list", "--reverse", &format!("{}..HEAD", base.as_str())],
        )
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(new.len(), 3, "{lane}: {new:?}");
    assert_eq!(raw_message(fx, &dir, &new[0]), b"one\n", "{lane}");
    assert_eq!(
        idents(fx, &dir, &new[0]),
        [
            "Ann O'Ther",
            "ann@example.invalid",
            "1600000000 -0230",
            "1600000000 -0230"
        ]
        .map(String::from),
        "{lane}"
    );
    assert_eq!(
        String::from_utf8_lossy(&raw_message(fx, &dir, &new[1])),
        HARD,
        "{lane}"
    );
    assert_eq!(raw_message(fx, &dir, &new[1]), HARD.as_bytes(), "{lane}");
    assert_eq!(
        raw_message(fx, &dir, &new[2]),
        EMPTY_REWORDED.as_bytes(),
        "{lane}"
    );
    let tree = |rev: &str| fx.git(&dir, &["rev-parse", &format!("{rev}^{{tree}}")]);
    assert_eq!(tree(&new[2]), tree(&new[1]), "{lane}: still empty");
    let files = fx.git(&dir, &["ls-tree", "--name-only", "HEAD"]);
    assert_eq!(files, "README\nf1\nf2\nf4\n", "{lane}");
    assert!(idle(runner, &dir), "{lane}");
}

#[test]
fn g_h1_one_run_of_every_verb() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        g_h1_in(&fx, &lane, &runner, &format!("h1-{lane}"), false);
    }
}

/// G-H1, again with `GIT_SEQUENCE_EDITOR=false` exported to sopen and `sequence.editor = false`
/// in the repository's config: the plan still applies.
#[test]
fn g_h1_the_owners_sequence_editor_is_never_used() {
    let Some(fx) = fixture() else { return };
    let lanes = [
        (
            "local",
            GitRunner::local(fx.local_git().with_env("GIT_SEQUENCE_EDITOR", "false")),
        ),
        (
            "sh",
            GitRunner::shell(fx.shell_git().with_env("GIT_SEQUENCE_EDITOR", "false")),
        ),
    ];
    for (lane, runner) in lanes {
        g_h1_in(&fx, lane, &runner, &format!("h1-editor-{lane}"), true);
    }
}

// =============================================================================================
// G-H2: a conflict, Continue, Abort

/// G-H2: a plan whose reorder conflicts → the banner `REBASE …`; resolve, Mark resolved,
/// Continue → the remaining message line still applies; the group's `after` record was written
/// at the stop, so the branch reads as moved since; a second plan → Abort → HEAD back.
#[test]
fn g_h2_a_conflict_then_continue_then_abort() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("h2-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "f", "1\n", "one");
        fx.commit(&dir, "f", "2\n", "two");
        let c3 = fx.commit(&dir, "f", "3\n", "three");
        let c4 = fx.commit(&dir, "h", "h\n", "four");
        let (repo, mut model) = opened(&runner, &dir, &Base::Commit(base.clone()));
        for op in [
            Op::Move {
                commit: c3.clone(),
                toward: Toward::Down,
            },
            Op::Verb {
                commit: c4.clone(),
                verb: Verb::Reword,
            },
            Op::Message {
                commit: c4.clone(),
                message: "four, reworded\n\n  kept  \n".into(),
            },
        ] {
            model.apply(op).unwrap();
        }
        let (_, outcome) = run_on(&runner, &dir, repo, &Edit::Plan(model));
        let error = failed_at(lane, &outcome, 1);
        assert!(error.contains("onflict"), "{lane}: {error}");
        let stamp = outcome.backup.clone().expect("a group");
        assert!(
            outcome.warning().is_none(),
            "{lane}: {:?}",
            outcome.warning()
        );
        let mut rounds = 0;
        while !idle(&runner, &dir) {
            rounds += 1;
            assert!(rounds <= 3, "{lane}: more stops than expected");
            let repo = repo_of(&runner, &dir);
            let banner = Banner::of(&repo.in_progress, &repo.status).expect("a banner");
            assert!(
                banner.text().starts_with("REBASE"),
                "{lane}: {}",
                banner.text()
            );
            assert_eq!(banner.conflicts, 1, "{lane}");
            std::fs::write(dir.join("f"), format!("r{rounds}\n")).unwrap();
            let (_, resolved) = act(
                &runner,
                &dir,
                &Action::Resolved {
                    paths: vec!["f".into()],
                },
            );
            done(lane, &resolved);
            let (_, cont) = act(&runner, &dir, &Action::Proceed(Proceed::Continue));
            if cont.steps[0].outcome != StepOutcome::Done {
                assert!(
                    !idle(&runner, &dir),
                    "{lane}: a Continue failed and stopped"
                );
            }
        }
        assert_eq!(rounds, 2, "{lane}");
        assert_eq!(
            raw_message(&fx, &dir, "HEAD"),
            b"four, reworded\n\n  kept  \n",
            "{lane}: the message line after the stops"
        );
        assert_eq!(
            subjects(&fx, &dir, &format!("{}..HEAD", base.as_str())),
            ["one", "three", "two", "four, reworded"],
            "{lane}"
        );
        // The group's `after` record was written at the stop (the executor seals once the
        // steps end): the branch moved by Continue reads as moved since, not chosen.
        let top = dir.to_str().unwrap();
        let record = backup::group(&runner, &Handle::new(), top, &stamp)
            .unwrap()
            .unwrap();
        let repo = repo_of(&runner, &dir);
        let entries = backup::entries(&runner, &Handle::new(), top, &record, &repo.head).unwrap();
        let main = entries
            .iter()
            .find(|e| e.subject == backup::Subject::Ref("refs/heads/main".into()))
            .expect("main's entry");
        assert!(main.moved_since && !main.chosen, "{lane}: {main:?}");

        // A second plan that conflicts, then Abort: HEAD back.
        let before = fx.oid(&dir, "HEAD");
        let (repo, mut model) = opened(&runner, &dir, &Base::Commit(base.clone()));
        let two = model.rows[1].commit.row.oid.clone();
        model
            .apply(Op::Move {
                commit: two,
                toward: Toward::Down,
            })
            .unwrap();
        let (_, outcome) = run_on(&runner, &dir, repo, &Edit::Plan(model));
        failed_at(lane, &outcome, 1);
        assert!(!idle(&runner, &dir));
        let (_, abort) = act(&runner, &dir, &Action::Proceed(Proceed::Abort));
        done(lane, &abort);
        assert!(idle(&runner, &dir), "{lane}");
        assert_eq!(fx.oid(&dir, "HEAD"), before, "{lane}");
        assert_eq!(fx.oid(&dir, "refs/heads/main"), before, "{lane}");
    }
}

// =============================================================================================
// G-H3: split, edit, fold, reword of HEAD, a message that is not UTF-8

/// The Split at `c`'s edit stop: its plan run, and the split the window keeps.
fn split_at(lane: &str, runner: &GitRunner, dir: &Path, c: &Oid) -> Split {
    let stop = Edit::SplitStop { commit: c.clone() };
    let mut repo = repo_of(runner, dir);
    ready(runner, &mut repo, &stop);
    let split = split_of(c, &repo).expect("the paths read");
    let plan = plan(&stop, &repo).unwrap_or_else(|r| panic!("{lane}: {}", r.0));
    let outcome = execute(&plan, dir.to_str().unwrap(), runner, &Handle::new());
    done(lane, &outcome);
    split
}

/// G-H3: `split` a commit that modifies one file and adds another into two commits — Continue
/// is refused while the added file is untracked (and while the modified one is not committed),
/// then succeeds with both in history.
#[test]
fn g_h3_split_and_its_continue() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("h3-split-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        let c1 = fx.commit(&dir, "f", "1\n", "one");
        std::fs::write(dir.join("n"), "new\n").unwrap();
        fx.git(&dir, &["add", "n"]);
        let c2 = fx.commit(&dir, "f", "2\n", "two");
        fx.commit(&dir, "g", "g\n", "three");
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Split { commit: c2.clone() }),
        );
        assert!(
            commands(&plan)[1].ends_with(c1.as_str()),
            "{lane}: {:?}",
            commands(&plan)
        );
        done(lane, &outcome);
        let repo = repo_of(&runner, &dir);
        let banner = Banner::of(&repo.in_progress, &repo.status).expect("the stop");
        assert_eq!(banner.edit_stop(), Some(c2.short()), "{lane}");
        assert_eq!(fx.oid(&dir, "HEAD"), c2, "{lane}");

        let split = split_at(lane, &runner, &dir, &c2);
        assert_eq!(split.commit, c2);
        assert_eq!(split.paths, ["f", "n"], "{lane}");
        assert_eq!(fx.oid(&dir, "HEAD"), c1, "{lane}");
        assert_eq!(fx.git(&dir, &["status", "--porcelain"]), " M f\n?? n\n");
        let cont = Edit::ContinueSplit(split.clone());
        let why = planned(&runner, &dir, &cont).unwrap_err().0;
        assert!(why.contains("f, n"), "{lane}: {why}");
        fx.git(&dir, &["add", "f"]);
        fx.git(&dir, &["commit", "-q", "-m", "two: f"]);
        let why = planned(&runner, &dir, &cont).unwrap_err().0;
        assert!(
            why.contains(": n.") || why.contains(": n "),
            "{lane}: {why}"
        );
        fx.git(&dir, &["add", "n"]);
        fx.git(&dir, &["commit", "-q", "-m", "two: n"]);
        let (plan, outcome) = run(&runner, &dir, &cont);
        assert_eq!(
            commands(&plan),
            ["GIT_EDITOR=true git rebase --continue"],
            "{lane}"
        );
        done(lane, &outcome);
        assert!(idle(&runner, &dir), "{lane}");
        assert_eq!(
            subjects(&fx, &dir, &format!("{}..HEAD", base.as_str())),
            ["one", "two: f", "two: n", "three"],
            "{lane}"
        );
        assert_eq!(
            fx.git(&dir, &["ls-tree", "--name-only", "HEAD"]),
            "README\nf\ng\nn\n"
        );
    }
}

/// G-H3's refusals: `split` on the root commit (it has no `HEAD^`); Split with no edit stop.
#[test]
fn g_h3_split_is_refused_where_it_cannot_be() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("h3-root-{lane}"));
        let root = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "f", "1\n", "one");
        let why = planned(
            &runner,
            &dir,
            &Edit::Quick(Quick::Split {
                commit: root.clone(),
            }),
        )
        .unwrap_err()
        .0;
        assert!(why.contains("root"), "{lane}: {why}");
        let head = fx.oid(&dir, "HEAD");
        let why = planned(&runner, &dir, &Edit::SplitStop { commit: head })
            .unwrap_err()
            .0;
        assert!(why.contains("edit stop"), "{lane}: {why}");
    }
}

/// Split records both names of a rename (03's `diff --name-only` names only the new one, so the
/// deletion would be left out of history), and a path named `*` is a literal path.
#[test]
fn split_records_both_names_of_a_rename() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("split-rename-{lane}"));
        fx.commit(&dir, "a", "alpha\nbeta\ngamma\ndelta\n", "add a");
        fx.commit(&dir, "*", "star\n", "add star");
        fx.git(&dir, &["mv", "a", "b"]);
        std::fs::write(dir.join("*"), "star 2\n").unwrap();
        fx.git(&dir, &["add", "--", "*"]);
        fx.git(&dir, &["commit", "-q", "-m", "rename"]);
        let c = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "z", "z\n", "after");
        let (_, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Split { commit: c.clone() }),
        );
        done(lane, &outcome);
        let split = split_at(lane, &runner, &dir, &c);
        assert_eq!(split.paths, ["*", "a", "b"], "{lane}");
        let cont = Edit::ContinueSplit(split.clone());
        // Only `b` committed: `a`'s deletion and `*` still differ.
        fx.git(&dir, &["add", "b"]);
        fx.git(&dir, &["commit", "-q", "-m", "b"]);
        let why = planned(&runner, &dir, &cont).unwrap_err().0;
        assert!(why.contains("*, a"), "{lane}: {why}");
        fx.git(&dir, &["add", "-A", "--", "a", "*"]);
        fx.git(&dir, &["commit", "-q", "-m", "a and star"]);
        let (_, outcome) = run(&runner, &dir, &cont);
        done(lane, &outcome);
        assert!(fx.rev(&dir, "HEAD:a").is_none(), "{lane}: a is gone");
        assert_eq!(fx.git(&dir, &["show", "HEAD:*"]), "star 2\n");
    }
}

/// Continue after a split waits for an added file that is ignored too: `status` shows an
/// untracked ignored file only with `--ignored`, and git would continue and leave it out of
/// history.
#[test]
fn continue_waits_for_an_ignored_added_file() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("split-ignored-{lane}"));
        fx.commit(&dir, ".gitignore", "*.log\n", "ignore logs");
        fx.commit(&dir, "x.log", "kept\n", "add a log");
        let c = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "z", "z\n", "after");
        let (_, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Split { commit: c.clone() }),
        );
        done(lane, &outcome);
        let split = split_at(lane, &runner, &dir, &c);
        assert_eq!(split.paths, ["x.log"], "{lane}");
        let cont = Edit::ContinueSplit(split.clone());
        let why = planned(&runner, &dir, &cont)
            .map(|p| format!("planned: {:?}", commands(&p)))
            .unwrap_or_else(|r| r.0);
        assert!(why.contains("x.log"), "{lane}: {why}");
        fx.git(&dir, &["add", "-f", "x.log"]);
        fx.git(&dir, &["commit", "-q", "-m", "the log"]);
        let (_, outcome) = run(&runner, &dir, &cont);
        done(lane, &outcome);
        assert_eq!(fx.git(&dir, &["show", "HEAD~1:x.log"]), "kept\n", "{lane}");
    }
}

/// G-H3: `edit` a commit and amend it (§7.6's `amend` at the stop), then Continue.
#[test]
fn g_h3_edit_then_amend() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("h3-edit-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "f", "1\n", "one");
        let c2 = fx.commit(&dir, "g", "2\n", "two");
        fx.commit(&dir, "h", "3\n", "three");
        let (_, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Edit { commit: c2.clone() }),
        );
        done(lane, &outcome);
        let repo = repo_of(&runner, &dir);
        let banner = Banner::of(&repo.in_progress, &repo.status).unwrap();
        assert_eq!(banner.edit_stop(), Some(c2.short()), "{lane}");
        std::fs::write(dir.join("g"), "2, amended\n").unwrap();
        fx.git(&dir, &["add", "g"]);
        let (_, amend) = act(
            &runner,
            &dir,
            &Action::Amend {
                message: Some("two, amended\n".into()),
            },
        );
        done(lane, &amend);
        let (_, cont) = act(&runner, &dir, &Action::Proceed(Proceed::Continue));
        done(lane, &cont);
        assert!(idle(&runner, &dir), "{lane}");
        assert_eq!(
            subjects(&fx, &dir, &format!("{}..HEAD", base.as_str())),
            ["one", "two, amended", "three"],
            "{lane}"
        );
        assert_eq!(fx.git(&dir, &["show", "HEAD~1:g"]), "2, amended\n");
    }
}

/// G-H3: `fold` staged changes into an older commit — `commit --fixup=<c>` (git's `fixup!
/// <subject>`, no editor), then that commit's line right after `<c>`'s as `fixup`, named `HEAD`
/// (git reads the todo before the rebase checks anything out; 03's estimate, settled here). With
/// a partly staged file the rest is stashed for the run and comes back. Then: into HEAD itself,
/// whose Undo puts the staged change back staged; and on a detached HEAD.
#[test]
fn g_h3_fold_staged_changes_into_an_older_commit() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("h3-fold-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        let c1 = fx.commit(&dir, "f1", "1\n", "one");
        let c2 = fx.commit(&dir, "f2", "2\n", "two");
        std::fs::write(dir.join("f1"), "1 staged\n").unwrap();
        fx.git(&dir, &["add", "f1"]);
        std::fs::write(dir.join("f1"), "1 staged\nunstaged\n").unwrap();

        // The model of the quick edit: the folded line right after `<c>`'s.
        let (repo, mut model) = opened(&runner, &dir, &Base::Commit(base.clone()));
        Quick::Fold { into: c1.clone() }.apply(&mut model).unwrap();
        model.autostash = true;
        let todo = model.run(&repo).unwrap().todo;
        assert_eq!(
            todo,
            vec![
                TodoLine::Pick {
                    commit: c1.clone(),
                    subject: "one".into()
                },
                TodoLine::Fixup {
                    commit: Named::Folded,
                    subject: "fixup! one".into()
                },
                TodoLine::Pick {
                    commit: c2.clone(),
                    subject: "two".into()
                },
            ],
            "{lane}"
        );

        let (plan, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Fold { into: c1.clone() }),
        );
        let lines = commands(&plan);
        assert_eq!(
            lines[1],
            format!("git commit --fixup={}", c1.as_str()),
            "{lane}"
        );
        assert_eq!(
            lines[2],
            rebase_line("--autostash --no-update-refs ", base.as_str()),
            "{lane}"
        );
        done(lane, &outcome);
        assert!(idle(&runner, &dir), "{lane}");
        assert_eq!(
            subjects(&fx, &dir, &format!("{}..HEAD", base.as_str())),
            ["one", "two"],
            "{lane}"
        );
        assert_eq!(fx.git(&dir, &["show", "HEAD~1:f1"]), "1 staged\n", "{lane}");
        assert_eq!(raw_message(&fx, &dir, "HEAD~1"), b"one\n", "{lane}");
        assert_eq!(fx.git(&dir, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(
            std::fs::read_to_string(dir.join("f1")).unwrap(),
            "1 staged\nunstaged\n",
            "{lane}: the unstaged part came back"
        );
        assert_eq!(fx.git(&dir, &["stash", "list"]), "", "{lane}");

        // Into HEAD itself, only staged changes; then Undo: the staged change back, staged.
        fx.git(&dir, &["checkout", "--", "f1"]);
        let head = fx.oid(&dir, "HEAD");
        std::fs::write(dir.join("f2"), "2 staged\n").unwrap();
        fx.git(&dir, &["add", "f2"]);
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Fold { into: head.clone() }),
        );
        assert_eq!(backups_of(&plan), ["heads/main", "worktree"], "{lane}");
        done(lane, &outcome);
        assert_eq!(fx.git(&dir, &["show", "HEAD:f2"]), "2 staged\n", "{lane}");
        assert_eq!(subjects(&fx, &dir, "HEAD~1..HEAD"), ["two"], "{lane}");
        let stamp = outcome.backup.clone().unwrap();
        let (_, shown, undone) = restore_shown(&runner, &dir, &stamp);
        shows_the_work_tree(lane, &shown);
        done(lane, &undone);
        assert_eq!(fx.oid(&dir, "HEAD"), head, "{lane}");
        assert_eq!(
            fx.git(&dir, &["diff", "--cached", "--name-only"]),
            "f2\n",
            "{lane}: staged again"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("f2")).unwrap(),
            "2 staged\n"
        );

        // On a detached HEAD.
        fx.git(&dir, &["commit", "-q", "-m", "three"]);
        fx.git(&dir, &["checkout", "-q", "--detach"]);
        std::fs::write(dir.join("f1"), "1 detached\n").unwrap();
        fx.git(&dir, &["add", "f1"]);
        let into = fx.oid(&dir, "HEAD~2");
        let (plan, outcome) = run(&runner, &dir, &Edit::Quick(Quick::Fold { into }));
        assert_eq!(backups_of(&plan), ["detached", "worktree"], "{lane}");
        done(lane, &outcome);
        assert_eq!(fx.git(&dir, &["show", "HEAD~2:f1"]), "1 detached\n");
        assert!(
            fx.output(&dir, &["symbolic-ref", "-q", "HEAD"])
                .stdout
                .is_empty(),
            "{lane}: still detached"
        );
    }
}

/// A `fold` whose run stops at a conflict, then Abort: git puts HEAD back where the rebase
/// started, at the commit `--fixup` made, so the staged changes are in that commit; Undo puts
/// the branch back and the changes back staged (measured: what 03 asks of `fold`'s backup,
/// `heads/<current>`, would give the branch back but not the staged changes).
#[test]
fn a_fold_that_stops_then_abort_and_undo() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("fold-abort-{lane}"));
        let c1 = fx.commit(&dir, "f", "1\n", "one");
        let c2 = fx.commit(&dir, "f", "2\n", "two");
        std::fs::write(dir.join("f"), "3\n").unwrap();
        fx.git(&dir, &["add", "f"]);
        let (_, outcome) = run(&runner, &dir, &Edit::Quick(Quick::Fold { into: c1 }));
        failed_at(lane, &outcome, 2);
        let folded = fx.oid(&dir, "refs/heads/main");
        assert_eq!(
            subjects(&fx, &dir, &format!("{}..main", c2.as_str())),
            ["fixup! one"],
            "{lane}"
        );
        let (_, abort) = act(&runner, &dir, &Action::Proceed(Proceed::Abort));
        done(lane, &abort);
        assert_eq!(
            fx.oid(&dir, "HEAD"),
            folded,
            "{lane}: back at the fold commit"
        );
        assert_eq!(fx.git(&dir, &["show", "HEAD:f"]), "3\n", "{lane}");
        assert_eq!(fx.git(&dir, &["status", "--porcelain"]), "", "{lane}");
        let (entries, shown, undone) =
            restore_shown(&runner, &dir, &outcome.backup.clone().unwrap());
        assert!(entries.iter().all(|e| e.chosen), "{lane}: {entries:?}");
        shows_the_work_tree(lane, &shown);
        done(lane, &undone);
        assert_eq!(fx.oid(&dir, "HEAD"), c2, "{lane}");
        assert_eq!(
            fx.git(&dir, &["diff", "--cached", "--name-only"]),
            "f\n",
            "{lane}"
        );
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "3\n");
    }
}

/// G-H3: quick `reword` of HEAD keeps a staged file staged (`commit --amend --only`) and writes
/// the message byte for byte; no rebase runs.
#[test]
fn g_h3_reword_of_head_keeps_a_staged_file_staged() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("h3-head-{lane}"));
        let head = fx.commit(&dir, "f", "1\n", "one");
        std::fs::write(dir.join("s"), "staged\n").unwrap();
        fx.git(&dir, &["add", "s"]);
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Reword {
                commit: head.clone(),
                message: HARD.into(),
            }),
        );
        assert_eq!(
            commands(&plan),
            [
                "backup heads/main, manifest under refs/sopen-backup/<stamp>/",
                "git commit --amend --only --allow-empty --cleanup=verbatim -F -"
            ],
            "{lane}"
        );
        done(lane, &outcome);
        assert_eq!(raw_message(&fx, &dir, "HEAD"), HARD.as_bytes(), "{lane}");
        assert_eq!(fx.git(&dir, &["diff", "--cached", "--name-only"]), "s\n");
        assert!(fx.rev(&dir, "HEAD:s").is_none(), "{lane}");
        assert_eq!(
            fx.oid(&dir, "HEAD~1"),
            fx.oid(&dir, &format!("{}~1", head.as_str()))
        );
    }
}

/// G-H3: reword of a commit whose message is not valid UTF-8 is refused with the reason — HEAD's
/// and an older one's, and the planner's `R`.
#[test]
fn g_h3_a_message_not_utf8_is_refused() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("h3-latin1-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        let old = commit_object(&fx, &dir, ("f", "1\n"), b"caf\xe9 au lait\n");
        let head = commit_object(&fx, &dir, ("g", "2\n"), b"na\xefve\n");
        for c in [&old, &head] {
            let why = planned(
                &runner,
                &dir,
                &Edit::Quick(Quick::Reword {
                    commit: c.clone(),
                    message: "x\n".into(),
                }),
            )
            .unwrap_err()
            .0;
            assert!(why.contains("not valid UTF-8"), "{lane}: {why}");
        }
        let (_, mut model) = opened(&runner, &dir, &Base::Commit(base));
        assert!(model.rows.iter().all(|r| r.commit.message_lossy), "{lane}");
        let why = model
            .apply(Op::Verb {
                commit: old.clone(),
                verb: Verb::Reword,
            })
            .unwrap_err()
            .0;
        assert!(why.contains("not valid UTF-8"), "{lane}: {why}");
    }
}

/// `author` on HEAD and on an older commit keeps the message byte for byte — a leading blank
/// line, a `#` line, trailing spaces and blank lines — and sets the committer date per the
/// choice (03 §7.8.1); a row with both a new message and an author change gets one line that
/// sets both, so its committer date is the one chosen (S4-A: `--no-edit` drops a leading blank
/// line even with `--cleanup=verbatim`, measured with git 2.56.1).
#[test]
fn the_author_line_keeps_the_message_bytes() {
    const OLD: &str = "1500000000 +0100";
    const KEPT: &[u8] = b"\n  leading blank line\n# hash\n\ntrailing   \n\n\n";
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("author-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        commit_raw(&fx, &dir, Some(("f", "1\n")), KEPT, Some(OLD));
        commit_raw(&fx, &dir, Some(("g", "2\n")), KEPT, Some(OLD));
        let who = |committer| Author {
            name: "Bo".into(),
            email: "bo@example.invalid".into(),
            date: date(1_600_000_000, 330),
            committer,
        };

        // HEAD, three choices in turn.
        for (choice, cd) in [
            (CommitterDate::Keep, Some(OLD)),
            (CommitterDate::Author, Some("1600000000 +0530")),
            (CommitterDate::Now, None),
        ] {
            let head = fx.oid(&dir, "HEAD");
            let (plan, outcome) = run(
                &runner,
                &dir,
                &Edit::Quick(Quick::Author {
                    commit: head,
                    author: who(choice),
                }),
            );
            done(lane, &outcome);
            assert_eq!(raw_message(&fx, &dir, "HEAD"), KEPT, "{lane}: {choice:?}");
            assert!(
                commands(&plan)[1].contains(
                    "git commit --amend --only --allow-empty --cleanup=verbatim -F - \
                     '--author=Bo <bo@example.invalid>' '--date=@1600000000 +0530'"
                ),
                "{lane}: {:?}",
                commands(&plan)
            );
            let [an, ae, ad, got_cd] = idents(&fx, &dir, "HEAD");
            assert_eq!((an.as_str(), ae.as_str()), ("Bo", "bo@example.invalid"));
            assert_eq!(ad, "1600000000 +0530", "{lane}");
            match cd {
                Some(cd) => assert_eq!(got_cd, cd, "{lane}: {choice:?}"),
                None => assert_ne!(got_cd, OLD, "{lane}: now"),
            }
        }

        // An older commit: the author alone, then a reword with an author on one row.
        let head = fx.oid(&dir, "HEAD");
        let (repo, mut model) = opened(&runner, &dir, &Base::Commit(base.clone()));
        let older = model.rows[1].commit.row.oid.clone();
        model
            .apply(Op::Author {
                commit: older.clone(),
                author: Some(who(CommitterDate::Keep)),
            })
            .unwrap();
        let (_, outcome) = run_on(&runner, &dir, repo, &Edit::Plan(model));
        done(lane, &outcome);
        assert_eq!(
            raw_message(&fx, &dir, "HEAD~1"),
            KEPT,
            "{lane}: author alone"
        );
        assert_eq!(idents(&fx, &dir, "HEAD~1")[3], OLD, "{lane}: kept");
        assert_ne!(fx.oid(&dir, "HEAD"), head);

        let reworded: &[u8] = b"\n\nreworded with a leading blank line  \n\n";
        let (repo, mut model) = opened(&runner, &dir, &Base::Commit(base.clone()));
        let older = model.rows[1].commit.row.oid.clone();
        for op in [
            Op::Verb {
                commit: older.clone(),
                verb: Verb::Reword,
            },
            Op::Message {
                commit: older.clone(),
                message: String::from_utf8(reworded.to_vec()).unwrap(),
            },
            Op::Author {
                commit: older.clone(),
                author: Some(who(CommitterDate::Author)),
            },
        ] {
            model.apply(op).unwrap();
        }
        let todo = model.run(&repo).unwrap().todo;
        assert_eq!(
            todo.iter()
                .filter(|l| matches!(l, TodoLine::Exec(_)))
                .count(),
            1,
            "{lane}: one line sets both: {todo:#?}"
        );
        let (_, outcome) = run_on(&runner, &dir, repo, &Edit::Plan(model));
        done(lane, &outcome);
        assert_eq!(raw_message(&fx, &dir, "HEAD~1"), reworded, "{lane}");
        let [_, _, ad, cd] = idents(&fx, &dir, "HEAD~1");
        assert_eq!(
            (ad.as_str(), cd.as_str()),
            ("1600000000 +0530", "1600000000 +0530"),
            "{lane}"
        );
    }
}

// =============================================================================================
// G-H4: a merge in the range

/// G-H4: a range holding a merge → the planner refuses, naming the merge; a base above the
/// newest merge works; plain Rebase over the same range → the confirmation lists the merge as
/// flattened.
#[test]
fn g_h4_a_merge_in_the_range() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("h4-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "m", "m\n", "on main");
        fx.git(&dir, &["checkout", "-q", "-b", "side", base.as_str()]);
        fx.commit(&dir, "s", "s\n", "on side");
        fx.git(&dir, &["checkout", "-q", "main"]);
        fx.git(
            &dir,
            &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
        );
        let merge = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "t", "t\n", "after the merge");
        let (repo, model) = opened(&runner, &dir, &Base::Commit(base.clone()));
        let problems = model.problems(&repo);
        assert_eq!(
            problems,
            vec![Problem::Merges(vec![merge.clone()])],
            "{lane}"
        );
        assert!(problems[0].text().contains(merge.short()), "{lane}");
        let why = planned(&runner, &dir, &Edit::Plan(model)).unwrap_err().0;
        assert!(why.contains(merge.short()), "{lane}: {why}");
        let (repo, model) = opened(&runner, &dir, &Base::Commit(merge.clone()));
        assert_eq!(model.problems(&repo), vec![], "{lane}: above the merge");
        assert_eq!(model.rows.len(), 1);
        let plan = super::actions::planned(
            &runner,
            &dir,
            &Action::Rebase {
                onto: Rev::Commit(base.clone()),
                options: RebaseOptions::default(),
            },
        )
        .unwrap();
        let notes = plan.confirm.unwrap().notes;
        assert!(
            notes
                .iter()
                .any(|n| n.contains("flattens 1 merge") && n.contains(merge.short())),
            "{lane}: {notes:?}"
        );
    }
}

// =============================================================================================
// G-H5: --update-refs

/// G-H5: `--update-refs`: a plan dropping a commit below another local branch's tip → that
/// branch moves to the rewritten commit, its old value is in the backup group, and Restore puts
/// both branches back; the same with plain Rebase. A branch in the range checked out in a linked
/// worktree → left out of the todo, named in the plan view, still at its commit after the run.
#[test]
fn g_h5_update_refs() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("h5-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        let c1 = fx.commit(&dir, "f1", "1\n", "one");
        let c2 = fx.commit(&dir, "f2", "2\n", "two");
        fx.git(&dir, &["branch", "mid"]);
        let wt = fx.root.join(format!("h5-wt-{lane}"));
        fx.git(
            &dir,
            &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
        );
        let c3 = fx.commit(&dir, "f3", "3\n", "three");

        let (repo, mut model) = opened(&runner, &dir, &Base::Commit(base.clone()));
        let tip = |b: &str| BranchTip {
            refname: format!("refs/heads/{b}"),
            tip: c2.clone(),
        };
        assert_eq!(model.branches, vec![tip("mid"), tip("wt")], "{lane}");
        assert_eq!(model.left_out, vec![tip("wt")], "{lane}");
        model.update_refs = true;
        model
            .apply(Op::Verb {
                commit: c1.clone(),
                verb: Verb::Drop,
            })
            .unwrap();
        let todo = model.run(&repo).unwrap().todo_text();
        assert_eq!(
            todo,
            format!(
                "drop {} one\npick {} two\nupdate-ref refs/heads/mid\npick {} three\n",
                c1.as_str(),
                c2.as_str(),
                c3.as_str()
            ),
            "{lane}"
        );
        let (plan, outcome) = run_on(&runner, &dir, repo, &Edit::Plan(model));
        let confirm = plan.confirm.clone().unwrap();
        assert_eq!(confirm.backups, ["heads/main", "heads/mid"], "{lane}");
        assert_eq!(
            confirm.commands[1],
            rebase_line("--update-refs ", base.as_str()),
            "{lane}"
        );
        assert!(
            confirm
                .notes
                .iter()
                .any(|n| n == "wt is checked out in another worktree; it stays where it is"),
            "{lane}: {:?}",
            confirm.notes
        );
        done(lane, &outcome);
        let stamp = outcome.backup.clone().unwrap();
        assert_eq!(
            fx.oid(&dir, "mid"),
            fx.oid(&dir, "HEAD~1"),
            "{lane}: mid moved"
        );
        assert_ne!(fx.oid(&dir, "mid"), c2);
        assert_eq!(fx.oid(&dir, "wt"), c2, "{lane}: wt stays");
        assert_eq!(
            group_names(&fx, &dir, &stamp),
            ["heads/main", "heads/mid", "manifest"]
        );
        assert_eq!(
            fx.oid(&dir, &format!("refs/sopen-backup/{stamp}/heads/mid")),
            c2
        );
        let (entries, undone) = restore(&runner, &dir, &stamp);
        assert!(entries.iter().all(|e| e.chosen), "{lane}: {entries:?}");
        done(lane, &undone);
        assert_eq!(fx.oid(&dir, "HEAD"), c3, "{lane}");
        assert_eq!(fx.oid(&dir, "mid"), c2, "{lane}");

        // The same with plain Rebase onto another branch.
        fx.git(&dir, &["worktree", "remove", wt.to_str().unwrap()]);
        fx.git(&dir, &["branch", "-D", "wt"]);
        fx.git(&dir, &["branch", "other", base.as_str()]);
        let other = fx.root.join(format!("h5-other-{lane}"));
        fx.git(
            &dir,
            &["worktree", "add", "-q", other.to_str().unwrap(), "other"],
        );
        let o1 = fx.commit(&other, "o", "o\n", "on other");
        fx.git(&dir, &["worktree", "remove", other.to_str().unwrap()]);
        let (plan, outcome) = act(
            &runner,
            &dir,
            &Action::Rebase {
                onto: Rev::Branch("other".into()),
                options: RebaseOptions {
                    update_refs: true,
                    ..RebaseOptions::default()
                },
            },
        );
        assert_eq!(backups_of(&plan), ["heads/main", "heads/mid"], "{lane}");
        done(lane, &outcome);
        assert_eq!(fx.oid(&dir, "HEAD~3"), o1, "{lane}");
        assert_eq!(fx.oid(&dir, "mid"), fx.oid(&dir, "HEAD~1"), "{lane}");
        let stamp = outcome.backup.clone().unwrap();
        let (_, undone) = restore(&runner, &dir, &stamp);
        done(lane, &undone);
        assert_eq!(fx.oid(&dir, "HEAD"), c3, "{lane}");
        assert_eq!(fx.oid(&dir, "mid"), c2, "{lane}");
    }
}

// =============================================================================================
// The data-loss classes of S3-A's review, for history edits

/// A todo line is never read as a comment: with `core.commentChar = p` git reads every `pick`
/// line as a comment, so a todo of picks and a message line drops the picks and amends the base
/// instead, and says it succeeded (measured with git 2.56.1, before the run's `-c
/// core.commentChar=#`). Since review round 1 the run fails closed on any comment character but
/// `#`, `core.commentString` too (a Continue would read the todo under the repository's own
/// setting; `r1_comment_char_fails_closed`): refused, nothing changed.
#[test]
fn a_todo_line_is_never_read_as_a_comment() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        for (key, value) in [("core.commentChar", "p"), ("core.commentString", "pi")] {
            let lane = format!("{lane} {key}");
            let dir = fx.repo(&format!("comment-{}", lane.replace([' ', '.'], "-")));
            let base = fx.oid(&dir, "HEAD");
            fx.commit(&dir, "f1", "1\n", "one");
            let c2 = fx.commit(&dir, "f2", "2\n", "two");
            fx.commit(&dir, "f3", "3\n", "three");
            fx.git(&dir, &["config", key, value]);
            let (repo, mut model) = opened(&runner, &dir, &Base::Commit(base.clone()));
            model
                .apply(Op::Verb {
                    commit: c2.clone(),
                    verb: Verb::Reword,
                })
                .unwrap();
            model
                .apply(Op::Message {
                    commit: c2.clone(),
                    message: "two, reworded\n".into(),
                })
                .unwrap();
            let edit = Edit::Plan(model);
            let mut repo = repo;
            ready(&runner, &mut repo, &edit);
            let why = plan(&edit, &repo).unwrap_err().0;
            assert_eq!(
                why,
                format!("{key} is '{value}': start or finish this rebase in a terminal"),
                "{lane}"
            );
            assert_eq!(
                subjects(&fx, &dir, "HEAD"),
                ["first", "one", "two", "three"],
                "{lane}"
            );
            assert_eq!(fx.oid(&dir, "HEAD~3"), base, "{lane}: the base untouched");
        }
    }
}

/// `rebase.updateRefs = true` in the config never moves a branch the plan did not back up: with
/// the option off the run passes `--no-update-refs` (S3-A review round 1's rule).
#[test]
fn the_update_refs_config_never_moves_a_branch() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("update-refs-config-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        let c1 = fx.commit(&dir, "f1", "1\n", "one");
        let c2 = fx.commit(&dir, "f2", "2\n", "two");
        fx.git(&dir, &["branch", "mid"]);
        fx.commit(&dir, "f3", "3\n", "three");
        fx.git(&dir, &["config", "rebase.updateRefs", "true"]);
        let (plan, outcome) = run(&runner, &dir, &Edit::Quick(Quick::Drop { commit: c1 }));
        done(lane, &outcome);
        assert_eq!(fx.oid(&dir, "mid"), c2, "{lane}: mid stays");
        assert_eq!(subjects(&fx, &dir, "HEAD"), ["first", "two", "three"]);
        assert_eq!(backups_of(&plan), ["heads/main"], "{lane}");
        assert_eq!(
            commands(&plan)[1],
            rebase_line("--no-update-refs ", base.as_str()),
            "{lane}"
        );
    }
}

/// 03 §8.6: a history edit is refused before it runs when an ignored file sits at a path the
/// run writes, listing it.
#[test]
fn ignored_files_in_the_way_refuse_the_run() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("in-the-way-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        fx.commit(&dir, ".gitignore", "*.log\n", "ignore logs");
        fx.commit(&dir, "x.log", "tracked once\n", "add a log");
        let c3 = fx.commit(&dir, "f", "f\n", "three");
        fx.git(&dir, &["rm", "-q", "--cached", "x.log"]);
        fx.git(&dir, &["commit", "-q", "-m", "untrack the log"]);
        std::fs::write(dir.join("x.log"), "mine, ignored\n").unwrap();
        let why = planned(&runner, &dir, &Edit::Quick(Quick::Drop { commit: c3 }))
            .unwrap_err()
            .0;
        assert!(
            why.contains("x.log") && why.contains("ignored"),
            "{lane}: {why}"
        );
        let (mut repo, model) = opened(&runner, &dir, &Base::Commit(base));
        let edit = Edit::Plan(model);
        ready(&runner, &mut repo, &edit);
        assert!(plan(&edit, &repo).unwrap_err().0.contains("x.log"));
        assert_eq!(
            std::fs::read_to_string(dir.join("x.log")).unwrap(),
            "mine, ignored\n"
        );
    }
}

/// Uncommitted changes: with `--autostash` (a quick edit turns it on, and says so) every change
/// comes back — what was staged unstaged, as git's autostash does — and Undo puts the staged and
/// unstaged parts back exactly (the `worktree` backup); the planner with it off refuses (03
/// §7.8.2). The runner's environment (`GIT_LITERAL_PATHSPECS=1`, `GIT_EDITOR=false`, `LC_ALL=C`)
/// reaches the rebase and its `exec` lines.
#[test]
fn uncommitted_changes_are_stashed_for_the_run_or_refused() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("dirty-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        let c1 = fx.commit(&dir, "f1", "1\n", "one");
        fx.commit(&dir, "f2", "2\n", "two");
        std::fs::write(dir.join("f2"), "2 staged\n").unwrap();
        fx.git(&dir, &["add", "f2"]);
        std::fs::write(dir.join("f2"), "2 staged\n2 unstaged\n").unwrap();
        std::fs::write(dir.join("README"), "unstaged readme\n").unwrap();
        let before = (fx.git(&dir, &["diff", "--cached"]), fx.git(&dir, &["diff"]));
        let whole = fx.git(&dir, &["diff", "HEAD"]);
        let head = fx.oid(&dir, "HEAD");

        let (repo, mut model) = opened(&runner, &dir, &Base::Commit(base.clone()));
        assert!(model.autostash, "{lane}: on with uncommitted changes");
        model.autostash = false;
        assert_eq!(model.problems(&repo), vec![Problem::Uncommitted]);
        let mut repo = repo;
        let edit = Edit::Plan(model);
        ready(&runner, &mut repo, &edit);
        let why = plan(&edit, &repo).unwrap_err().0;
        assert!(why.contains("uncommitted"), "{lane}: {why}");

        let (plan, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Reword {
                commit: c1.clone(),
                message: "one, reworded\n".into(),
            }),
        );
        let confirm = plan.confirm.clone().unwrap();
        assert!(confirm.commands[1].contains(" --autostash "), "{lane}");
        assert!(
            confirm
                .notes
                .iter()
                .any(|n| n.contains("stashed for the run")),
            "{lane}: {:?}",
            confirm.notes
        );
        done(lane, &outcome);
        // git's `--autostash` applies its stash without `--index`: every change comes back,
        // what was staged unstaged; Undo puts both back as they were (the `worktree` backup).
        assert_eq!(
            fx.git(&dir, &["diff", "HEAD"]),
            whole,
            "{lane}: every change back"
        );
        assert_eq!(fx.git(&dir, &["diff", "--cached"]), "", "{lane}");
        assert_eq!(
            subjects(&fx, &dir, &format!("{}..HEAD", base.as_str())),
            ["one, reworded", "two"]
        );
        assert_eq!(fx.git(&dir, &["stash", "list"]), "", "{lane}");
        let (_, shown, undone) = restore_shown(&runner, &dir, &outcome.backup.clone().unwrap());
        shows_the_work_tree(lane, &shown);
        done(lane, &undone);
        assert_eq!(fx.oid(&dir, "HEAD"), head, "{lane}");
        assert_eq!(
            (fx.git(&dir, &["diff", "--cached"]), fx.git(&dir, &["diff"])),
            before,
            "{lane}: staged and unstaged as they were"
        );
        assert_eq!(confirm.backups, ["heads/main", "worktree"], "{lane}");
    }
}

/// A seal failure is the outcome's warning: the `after` record of a history edit's group is
/// refused by a `reference-transaction` hook, and the run says the backup is incomplete.
#[test]
fn a_seal_failure_is_a_warning() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("seal-{lane}"));
        let c1 = fx.commit(&dir, "f1", "1\n", "one");
        fx.commit(&dir, "f2", "2\n", "two");
        script(
            &dir.join(".git/hooks/reference-transaction"),
            "[ \"$1\" = prepared ] || exit 0\nif grep -q '/after$'; then exit 1; fi\nexit 0",
        );
        let (_, outcome) = run(&runner, &dir, &Edit::Quick(Quick::Drop { commit: c1 }));
        done(lane, &outcome);
        let stamp = outcome.backup.clone().expect("the group");
        let warning = outcome.warning().unwrap_or_default();
        assert!(
            warning.starts_with(&format!("backup {stamp} incomplete: ")),
            "{lane}: {warning:?}"
        );
        assert_eq!(subjects(&fx, &dir, "HEAD"), ["first", "two"]);
    }
}

/// Non-ASCII text is never cut inside a character: a subject of 1500 bytes of Hangul is read,
/// shown in a confirmation and written to the todo.
#[test]
fn text_is_never_cut_inside_a_character() {
    let Some(fx) = fixture() else { return };
    let (_, runner) = fx.lanes().remove(0);
    let dir = fx.repo("hangul");
    let base = fx.oid(&dir, "HEAD");
    let long = "가".repeat(500);
    let c1 = fx.commit(&dir, "f1", "1\n", &long);
    fx.commit(&dir, "f2", "2\n", "두번째");
    let (repo, model) = opened(&runner, &dir, &Base::Commit(base));
    let subject = &model.rows[1].commit.row.subject;
    assert!(
        subject.len() <= 1024 && !subject.is_empty() && subject.chars().all(|c| c == '가'),
        "{subject}"
    );
    assert_eq!(model.rows[1].commit.message, format!("{long}\n"));
    assert!(model.run(&repo).unwrap().todo_text().contains('가'));
    let plan = planned(&runner, &dir, &Edit::Quick(Quick::Drop { commit: c1 })).unwrap();
    let confirm = plan.confirm.unwrap();
    assert!(confirm.title.chars().count() < 200, "{}", confirm.title);
    assert!(confirm.title.contains('가'), "{}", confirm.title);
}

/// The plan of a model is refused when HEAD is no longer the model's newest row: git would
/// rebase a commit the todo does not name, and drop it.
#[test]
fn a_plan_is_refused_when_head_moved_since_it_was_read() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("moved-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "f1", "1\n", "one");
        let (_, model) = opened(&runner, &dir, &Base::Commit(base));
        fx.commit(&dir, "f2", "2\n", "two, made meanwhile");
        let why = planned(&runner, &dir, &Edit::Plan(model)).unwrap_err().0;
        assert!(why.contains("HEAD"), "{lane}: {why}");
    }
}

/// A plan that moves or drops the newest row is the owner's plan, not a moved HEAD (S4-C round
/// 2): the check compares HEAD with the newest row as the planner read it (`Model::read`), so
/// Cmd/Ctrl+↓ on the top row and `D` on it plan and run, and the history is exactly that.
#[test]
fn a_plan_that_moves_or_drops_the_newest_row_runs() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("newest-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "f1", "1\n", "one");
        let two = fx.commit(&dir, "f2", "2\n", "two");
        let (mut repo, model) = opened(&runner, &dir, &Base::Commit(base.clone()));
        assert_eq!(
            model.read.order.first(),
            Some(&two),
            "{lane}: read as opened"
        );
        // Both planned: the newest row moved down one place, and dropped.
        let mut moved = model.clone();
        moved
            .apply(Op::Move {
                commit: two.clone(),
                toward: Toward::Down,
            })
            .unwrap();
        let mut dropped = model.clone();
        dropped
            .apply(Op::Verb {
                commit: two.clone(),
                verb: Verb::Drop,
            })
            .unwrap();
        let (moved, dropped) = (Edit::Plan(moved), Edit::Plan(dropped));
        ready(&runner, &mut repo, &moved);
        ready(&runner, &mut repo, &dropped);
        let plans = [
            plan(&moved, &repo).map(|_| ()),
            plan(&dropped, &repo).map(|_| ()),
        ];
        assert!(
            plans.iter().all(Result::is_ok),
            "{lane}: moved {:?}, dropped {:?}",
            plans[0],
            plans[1]
        );
        // Run: the moved one, then (opened again) the newest dropped.
        let (_, outcome) = run_on(&runner, &dir, repo, &moved);
        done(lane, &outcome);
        let range = format!("{}..HEAD", base.as_str());
        assert_eq!(subjects(&fx, &dir, &range), ["two", "one"], "{lane}");
        // The newest row (now `one`) dropped.
        let (repo, mut model) = opened(&runner, &dir, &Base::Commit(base.clone()));
        let newest = model.rows[0].commit.row.oid.clone();
        model
            .apply(Op::Verb {
                commit: newest,
                verb: Verb::Drop,
            })
            .unwrap();
        let (_, outcome) = run_on(&runner, &dir, repo, &Edit::Plan(model));
        done(lane, &outcome);
        assert_eq!(subjects(&fx, &dir, &range), ["two"], "{lane}");
        assert!(!dir.join("f1").exists(), "{lane}: one's file gone");
    }
}

/// The root commit's quick edits run `rebase -i --root` (03 §7.8.2: `<base>|--root`), its
/// footprint read from the whole history (§8.6); a branch at a dropped root has no kept commit
/// to move to, and the run is refused rather than leave it pointing nowhere new.
#[test]
fn the_root_commit_is_rewritten_with_root() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("root-{lane}"));
        let root = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "f", "1\n", "one");
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Reword {
                commit: root.clone(),
                message: "first, reworded\n".into(),
            }),
        );
        assert_eq!(
            commands(&plan)[1],
            rebase_line("--no-update-refs ", "--root"),
            "{lane}"
        );
        done(lane, &outcome);
        assert_eq!(subjects(&fx, &dir, "HEAD"), ["first, reworded", "one"]);
        assert!(fx.rev(&dir, "HEAD~2").is_none(), "{lane}: still one root");

        let root = fx.oid(&dir, "HEAD~1");
        fx.git(&dir, &["branch", "at-root", root.as_str()]);
        let (repo, mut model) = opened(&runner, &dir, &Base::Root);
        model.update_refs = true;
        model
            .apply(Op::Verb {
                commit: root.clone(),
                verb: Verb::Drop,
            })
            .unwrap();
        let why = model.run(&repo).unwrap_err().0;
        assert!(why.contains("at-root"), "{lane}: {why}");
    }
}

/// The quick edits that start below the commit's parent, on real git: `fixup` of a commit whose
/// parent is the root (`--root`), `squash` into the commit below with its message, `move-down`
/// and `move-up` (03 §7.8.1: each a plan with one change).
#[test]
fn quick_squash_fixup_and_moves() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("quick-{lane}"));
        let c1 = fx.commit(&dir, "f1", "1\n", "one");
        fx.commit(&dir, "f2", "2\n", "two");
        fx.commit(&dir, "f3", "3\n", "three");
        fx.commit(&dir, "f4", "4\n", "four");

        let (plan, outcome) = run(&runner, &dir, &Edit::Quick(Quick::Fixup { commit: c1 }));
        assert_eq!(
            commands(&plan)[1],
            rebase_line("--no-update-refs ", "--root"),
            "{lane}"
        );
        done(lane, &outcome);
        assert_eq!(
            subjects(&fx, &dir, "HEAD"),
            ["first", "two", "three", "four"]
        );
        assert_eq!(raw_message(&fx, &dir, "HEAD~3"), b"first\n", "{lane}");
        assert_eq!(fx.git(&dir, &["show", "HEAD~3:f1"]), "1\n", "{lane}");

        let three = fx.oid(&dir, "HEAD~1");
        let root = fx.oid(&dir, "HEAD~3");
        let (plan, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Squash {
                commit: three,
                message: "two and three\n".into(),
            }),
        );
        assert_eq!(
            commands(&plan)[1],
            rebase_line("--no-update-refs ", root.as_str()),
            "{lane}: from the grandparent"
        );
        done(lane, &outcome);
        assert_eq!(
            subjects(&fx, &dir, "HEAD"),
            ["first", "two and three", "four"]
        );
        assert_eq!(raw_message(&fx, &dir, "HEAD~1"), b"two and three\n");

        let four = fx.oid(&dir, "HEAD");
        let (_, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Move {
                commit: four.clone(),
                toward: Toward::Down,
            }),
        );
        done(lane, &outcome);
        assert_eq!(
            subjects(&fx, &dir, "HEAD"),
            ["first", "four", "two and three"]
        );

        let four = fx.oid(&dir, "HEAD~1");
        let (_, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Move {
                commit: four,
                toward: Toward::Up,
            }),
        );
        done(lane, &outcome);
        assert_eq!(
            subjects(&fx, &dir, "HEAD"),
            ["first", "two and three", "four"]
        );
        let why = planned(
            &runner,
            &dir,
            &Edit::Quick(Quick::Move {
                commit: fx.oid(&dir, "HEAD"),
                toward: Toward::Up,
            }),
        )
        .unwrap_err()
        .0;
        assert!(why.contains("top"), "{lane}: {why}");
    }
}

// =============================================================================================
// Review round 1 (PR #89)

/// R1-1: an author set on a row that a squash group folds is lost with the row's commit, in
/// either order (author, then the verb; the verb, then the author): a problem that names the
/// head to set it on, and the run is refused.
#[test]
fn r1_an_author_on_a_folded_row_is_a_problem() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("r1-folded-author-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        let c1 = fx.commit(&dir, "f1", "1\n", "one");
        let c2 = fx.commit(&dir, "f2", "2\n", "two");
        let c3 = fx.commit(&dir, "f3", "3\n", "three");
        let who = Some(someone("Bo"));
        // The author first, then `fixup`; and `squash` first, then the author.
        let orders: [Vec<Op>; 2] = [
            vec![
                Op::Author {
                    commit: c2.clone(),
                    author: who.clone(),
                },
                Op::Verb {
                    commit: c2.clone(),
                    verb: Verb::Fixup,
                },
            ],
            vec![
                Op::Verb {
                    commit: c3.clone(),
                    verb: Verb::Squash,
                },
                Op::Author {
                    commit: c3.clone(),
                    author: who.clone(),
                },
            ],
        ];
        for (ops, (lost, into)) in orders.into_iter().zip([(&c2, &c1), (&c3, &c2)]) {
            let (repo, mut model) = opened(&runner, &dir, &Base::Commit(base.clone()));
            for op in ops {
                model.apply(op).unwrap();
            }
            let problems: Vec<String> = model.problems(&repo).iter().map(Problem::text).collect();
            let want = format!(
                "{}: its author would be lost — it is squashed into {}; set the author on {}",
                lost.short(),
                into.short(),
                into.short()
            );
            assert_eq!(problems, std::slice::from_ref(&want), "{lane}");
            let why = planned(&runner, &dir, &Edit::Plan(model)).unwrap_err().0;
            assert_eq!(why, want, "{lane}");
        }
    }
}

/// R1-2: an author change on an `edit` row keeps the message as it is at the stop, byte for
/// byte — read with `git cat-file commit HEAD` — a leading blank line included: as the commit
/// had it, and as an Amend at the stop made it.
#[test]
fn r1_an_author_at_an_edit_stop_keeps_the_message_at_the_stop() {
    const KEPT: &[u8] = b"\n  a leading blank line\n# hash\n\ntrailing   \n\n";
    const AMENDED: &str = "\n\namended at the stop  \n\n";
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("r1-edit-author-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        fx.commit(&dir, "f1", "1\n", "one");
        commit_raw(&fx, &dir, Some(("f2", "2\n")), KEPT, None);
        fx.commit(&dir, "f3", "3\n", "three");
        for amend in [None, Some(AMENDED)] {
            let (repo, mut model) = opened(&runner, &dir, &Base::Commit(base.clone()));
            let edited = model.rows[1].commit.row.oid.clone();
            for op in [
                Op::Verb {
                    commit: edited.clone(),
                    verb: Verb::Edit,
                },
                Op::Author {
                    commit: edited.clone(),
                    author: Some(someone("Bo")),
                },
            ] {
                model.apply(op).unwrap();
            }
            let (_, outcome) = run_on(&runner, &dir, repo, &Edit::Plan(model));
            done(lane, &outcome);
            assert!(!idle(&runner, &dir), "{lane}: the stop");
            if let Some(m) = amend {
                let (_, amended) = act(
                    &runner,
                    &dir,
                    &Action::Amend {
                        message: Some(m.into()),
                    },
                );
                done(lane, &amended);
            }
            let (_, cont) = act(&runner, &dir, &Action::Proceed(Proceed::Continue));
            done(lane, &cont);
            assert!(idle(&runner, &dir), "{lane}");
            let want = amend.map_or(KEPT.to_vec(), |m| m.as_bytes().to_vec());
            assert_eq!(
                String::from_utf8_lossy(&raw_message(&fx, &dir, "HEAD~1")),
                String::from_utf8_lossy(&want),
                "{lane}: {amend:?}"
            );
            assert_eq!(idents(&fx, &dir, "HEAD~1")[0], "Bo", "{lane}");
        }
    }
}

/// A commit made with a message encoding other than UTF-8 (`i18n.commitEncoding`): git stores
/// the bytes as given and writes an `encoding` header.
fn commit_encoded(fx: &Fx, dir: &Path, file: (&str, &str), encoding: &str, message: &[u8]) -> Oid {
    std::fs::write(dir.join(file.0), file.1).unwrap();
    fx.git(dir, &["add", "--", file.0]);
    let m = fx.root.join("encoded.txt");
    std::fs::write(&m, message).unwrap();
    let key = format!("i18n.commitEncoding={encoding}");
    fx.git(
        dir,
        &["-c", &key, "commit", "-q", "-F", m.to_str().unwrap()],
    );
    fx.oid(dir, "HEAD")
}

/// R1-3: a commit whose message or author is not UTF-8 is never rewritten by sopen's own lines:
/// read raw (`cat-file commit`), an `encoding` header other than UTF-8 (which `git log`
/// re-encodes, so the range read cannot see it) or bytes that are not UTF-8 refuse a reword, an
/// author change and a squash, naming the encoding. A drop elsewhere in the range still runs.
#[test]
fn r1_a_commit_not_in_utf8_is_never_rewritten() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("r1-latin1-{lane}"));
        let base = fx.oid(&dir, "HEAD");
        let latin = commit_encoded(&fx, &dir, ("f1", "1\n"), "ISO-8859-1", b"caf\xe9 au lait\n");
        let raw = fx
            .output(&dir, &["cat-file", "commit", latin.as_str()])
            .stdout;
        assert!(
            raw.windows(20).any(|w| w == b"encoding ISO-8859-1\n"),
            "{lane}: the fixture"
        );
        let plain = fx.commit(&dir, "f2", "2\n", "two");
        let head = commit_encoded(&fx, &dir, ("f3", "3\n"), "ISO-8859-1", b"na\xefve\n");
        let refused = |edit: Edit| planned(&runner, &dir, &edit).unwrap_err().0;
        for (what, edit) in [
            (
                "reword",
                Edit::Quick(Quick::Reword {
                    commit: latin.clone(),
                    message: "x\n".into(),
                }),
            ),
            (
                "reword HEAD",
                Edit::Quick(Quick::Reword {
                    commit: head.clone(),
                    message: "x\n".into(),
                }),
            ),
            (
                "author HEAD",
                Edit::Quick(Quick::Author {
                    commit: head.clone(),
                    author: someone("Bo"),
                }),
            ),
            (
                "squash",
                Edit::Quick(Quick::Squash {
                    commit: plain.clone(),
                    message: "x\n".into(),
                }),
            ),
        ] {
            let why = refused(edit);
            assert!(
                why.contains("is not UTF-8 (ISO-8859-1); sopen rewrites UTF-8 only"),
                "{lane}: {what}: {why}"
            );
        }
        // The planner's model reads the message re-encoded: `R` is allowed there, the run is
        // refused.
        let (_, mut model) = opened(&runner, &dir, &Base::Commit(base.clone()));
        assert!(model.rows.iter().all(|r| !r.commit.message_lossy), "{lane}");
        model
            .apply(Op::Verb {
                commit: latin.clone(),
                verb: Verb::Reword,
            })
            .unwrap();
        let why = planned(&runner, &dir, &Edit::Plan(model)).unwrap_err().0;
        assert!(why.starts_with(latin.short()), "{lane}: {why}");
        // A drop elsewhere: git replays the others itself.
        let (_, outcome) = run(&runner, &dir, &Edit::Quick(Quick::Drop { commit: plain }));
        done(lane, &outcome);

        // An author that is not UTF-8 with no `encoding` header.
        let dir = fx.repo(&format!("r1-latin1-author-{lane}"));
        let c = commit_with_author(&fx, &dir, b"Jos\xe9 <jose@example.invalid>");
        let edit = Edit::Quick(Quick::Author {
            commit: c.clone(),
            author: someone("Bo"),
        });
        let why = planned(&runner, &dir, &edit).unwrap_err().0;
        assert!(why.contains("its author is not UTF-8"), "{lane}: {why}");
    }
}

/// A commit of a new file whose author line is `author` byte for byte (`hash-object`); the
/// branch moves to it.
fn commit_with_author(fx: &Fx, dir: &Path, author: &[u8]) -> Oid {
    use std::io::Write as _;
    std::fs::write(dir.join("a"), "a\n").unwrap();
    fx.git(dir, &["add", "a"]);
    let tree = fx.git(dir, &["write-tree"]).trim().to_string();
    let parent = fx.oid(dir, "HEAD");
    let mut object = format!("tree {tree}\nparent {}\nauthor ", parent.as_str()).into_bytes();
    object.extend_from_slice(author);
    object.extend_from_slice(
        b" 1500000000 +0000\ncommitter A U Thor <a@example.invalid> 1500000000 +0000\n\nplain\n",
    );
    let mut c = fx.command(dir, &["hash-object", "-t", "commit", "-w", "--stdin"]);
    c.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(&object).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "hash-object");
    let oid = String::from_utf8(out.stdout).unwrap().trim().to_string();
    fx.git(dir, &["update-ref", "HEAD", &oid]);
    Oid::parse(&oid).unwrap()
}

/// R1-4: `core.commentChar` set to anything but `#` fails closed: sopen's `-c` covers only the
/// command that starts the rebase, and Continue or Skip reads the todo again under the
/// repository's own setting, where `p` makes the remaining picks comments. So a history-edit
/// run is refused to start, and the banner's Continue and Skip are refused for a rebase; Abort
/// still runs. `#` and unset both work.
#[test]
fn r1_comment_char_fails_closed() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("r1-comment-{lane}"));
        fx.commit(&dir, "f1", "1\n", "one");
        let c2 = fx.commit(&dir, "f2", "2\n", "two");
        fx.commit(&dir, "f3", "3\n", "three");
        let said = "core.commentChar is 'p': start or finish this rebase in a terminal";
        fx.git(&dir, &["config", "core.commentChar", "p"]);
        let why = planned(
            &runner,
            &dir,
            &Edit::Quick(Quick::Drop { commit: c2.clone() }),
        )
        .unwrap_err()
        .0;
        assert_eq!(why, said, "{lane}: the start");

        fx.git(&dir, &["config", "--unset", "core.commentChar"]);
        let (_, outcome) = run(&runner, &dir, &Edit::Quick(Quick::Edit { commit: c2 }));
        done(lane, &outcome);
        assert!(!idle(&runner, &dir), "{lane}: the stop");
        fx.git(&dir, &["config", "core.commentChar", "p"]);
        for p in [Proceed::Continue, Proceed::Skip] {
            let why = super::actions::planned(&runner, &dir, &Action::Proceed(p))
                .unwrap_err()
                .0;
            assert_eq!(why, said, "{lane}: {p:?}");
        }
        assert!(super::actions::planned(&runner, &dir, &Action::Proceed(Proceed::Abort)).is_ok());
        fx.git(&dir, &["config", "core.commentChar", "#"]);
        let (_, cont) = act(&runner, &dir, &Action::Proceed(Proceed::Continue));
        done(lane, &cont);
        assert!(idle(&runner, &dir), "{lane}");
        assert_eq!(
            subjects(&fx, &dir, "HEAD"),
            ["first", "one", "two", "three"]
        );
    }
}

/// R1-5: Split is offered at an edit stop only: a rebase stopped by a failed `exec` line (a
/// `commit-msg` hook refused a message line), clean and at HEAD, is refused.
#[test]
fn r1_split_needs_an_edit_stop() {
    let Some(fx) = fixture() else { return };
    for (lane, runner) in fx.lanes() {
        let lane = lane.as_str();
        let dir = fx.repo(&format!("r1-split-exec-{lane}"));
        let c1 = fx.commit(&dir, "f1", "1\n", "one");
        fx.commit(&dir, "f2", "2\n", "two");
        script(&dir.join(".git/hooks/commit-msg"), "exit 1");
        let (_, outcome) = run(
            &runner,
            &dir,
            &Edit::Quick(Quick::Reword {
                commit: c1.clone(),
                message: "one, reworded\n".into(),
            }),
        );
        failed_at(lane, &outcome, 1);
        assert!(!idle(&runner, &dir), "{lane}: stopped at the exec line");
        let head = fx.oid(&dir, "HEAD");
        let why = planned(&runner, &dir, &Edit::SplitStop { commit: head })
            .unwrap_err()
            .0;
        assert!(why.contains("edit stop"), "{lane}: {why}");
    }
}
