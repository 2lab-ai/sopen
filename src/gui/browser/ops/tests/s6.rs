//! S6-B: the make-folder (F7) and delete (F8, Shift+F8) dialogs (04 §5.4, §5.5), driven by real
//! egui events in S1-D's stand-in host: what each says, Enter gives the job, Esc cancels, a name
//! holding `/` is refused. F7 and F8 themselves, and running the jobs on the engine, are S6-C's.

use super::*;

/// The error colour inside a dialog (`chrome::dialog_visuals`).
fn error_color() -> Color32 {
    chrome::PALETTE.dialog_error
}

fn jobs(log: &Shared) -> Vec<Job> {
    log.lock().unwrap().jobs.clone()
}

/// F7 (here [`Ops::open_make_folder`]): a one-line dialog naming the folder the new one goes in
/// — `host:/abs` on a remote, the plain path here — that holds the keyboard; Esc cancels and
/// nothing runs; the list never sees its keys.
#[test]
fn the_make_folder_dialog_names_its_folder_and_esc_cancels() {
    let (mut h, log) = host(&[]);
    let mut drv = Driver::new(900.0, 500.0);
    assert!(h.ops.open_make_folder(&h.panel, Some("me@box")).is_none());
    drv.idle(&mut h, 2);
    assert!(h.ops.holds_keyboard());
    assert!(
        drv.find_text("Make a folder in me@box:/srv/src").is_some(),
        "the remote folder, in sopen's command-line form"
    );
    type_text(&mut drv, &mut h, "new");
    assert!(drv.find_text("new").is_some(), "the field took the typing");
    drv.key(&mut h, Key::Escape, Modifiers::NONE);
    assert!(!h.ops.holds_keyboard(), "Esc closed it");
    assert!(drv.find_text("Make a folder in me@box:/srv/src").is_none());
    drv.idle(&mut h, 3);
    assert!(jobs(&log).is_empty(), "Esc ran nothing");

    h.ops.open_make_folder(&h.panel, None);
    drv.idle(&mut h, 2);
    assert!(drv.find_text("Make a folder in /srv/src").is_some());
    assert!(drv.find_text("[ Cancel ]").is_some());
    drv.click_text_on_top(&mut h, "[ Cancel ]");
    assert!(!h.ops.holds_keyboard(), "Cancel closed it");
    drv.idle(&mut h, 2);
    assert!(jobs(&log).is_empty());
    assert_eq!(h.list_enters, 0);
}

/// K-K1, the dialog half: `new`, Enter → the job is that folder and that name (never the
/// selection, 04 §4); once made, the bar goes on it ([`Changed::bar`]) and the status row says
/// so. `new` again → the engine's "exists" is an error, read whole in the report, and the bar
/// stays. The list never sees the dialog's Enter.
#[test]
fn the_make_folder_dialog_gives_the_job_on_enter_and_the_bar_goes_on_it() {
    let (mut h, log) = host(&[]);
    let mut drv = Driver::new(900.0, 500.0);
    h.panel.select_group("*.rs", true);
    at(&mut h.panel, "main.rs");
    h.ops.open_make_folder(&h.panel, None);
    drv.idle(&mut h, 2);
    type_text(&mut drv, &mut h, "new");
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    drv.until(&mut h, 5, |h| !h.changed.is_empty());
    drv.idle(&mut h, 2);
    let want = Job::MakeFolder {
        dir: here("/srv/src"),
        name: "new".to_string(),
    };
    assert_eq!(jobs(&log), std::slice::from_ref(&want));
    let changed = &h.changed[0];
    assert_eq!(changed.dirs, [here("/srv/src")]);
    assert_eq!(changed.bar.as_deref(), Some("new"), "the bar goes on it");
    assert_eq!(changed.said.as_ref().unwrap().text, "made folder new");
    assert!(!h.ops.holds_keyboard());
    assert_eq!(selected(&h.panel), ["lib.rs", "main.rs"], "untouched");

    h.ops.open_make_folder(&h.panel, None);
    drv.idle(&mut h, 2);
    type_text(&mut drv, &mut h, "new");
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    drv.until(&mut h, 5, |h| h.changed.len() == 2);
    drv.idle(&mut h, 2);
    assert_eq!(jobs(&log), [want.clone(), want]);
    let changed = &h.changed[1];
    assert_eq!(changed.bar, None, "nothing made: the bar stays");
    assert!(changed.said.is_none(), "a failure is read whole");
    assert!(drv
        .find_text("Make folder new in /srv/src: 1 failed")
        .is_some());
    assert!(drv
        .find_text("failed      new — new: a folder of that name exists")
        .is_some());
    assert_eq!(h.list_enters, 0, "the dialog's Enter");
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    assert!(!h.ops.holds_keyboard(), "Enter closed the report");
}

/// One level only (04 §5.4): a name with `/` is refused in the dialog, with the reason, and
/// nothing is asked of the engine; the name is selected again, so typing replaces it. An empty
/// name and `..` are refused the same way (04 §5.1).
#[test]
fn the_make_folder_dialog_refuses_a_slash() {
    let (mut h, log) = host(&[]);
    let mut drv = Driver::new(900.0, 500.0);
    h.ops.open_make_folder(&h.panel, None);
    drv.idle(&mut h, 2);
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    drv.idle(&mut h, 2);
    assert!(h.ops.holds_keyboard(), "an empty name: still open");
    assert!(drv.find_text("a name cannot be empty").is_some());
    type_text(&mut drv, &mut h, "a/b");
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    drv.idle(&mut h, 2);
    assert!(h.ops.holds_keyboard(), "still open");
    assert!(
        drv.find_text("a/b: a name cannot hold a /").is_some(),
        "the reason shows in the dialog"
    );
    type_text(&mut drv, &mut h, "..");
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    drv.idle(&mut h, 2);
    assert!(drv.find_text(".. cannot be a name").is_some(), "replaced");
    assert!(jobs(&log).is_empty(), "nothing asked of the engine");
    type_text(&mut drv, &mut h, "새 폴더");
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    drv.until(&mut h, 5, |h| !h.changed.is_empty());
    assert_eq!(
        jobs(&log),
        [Job::MakeFolder {
            dir: here("/srv/src"),
            name: "새 폴더".to_string(),
        }]
    );
    assert_eq!(h.list_enters, 0);
}

/// What the delete dialog says (04 §5.5), each opened and closed with Esc — nothing runs.
struct Said {
    question: &'static str,
    /// Where it goes, and whether that line is in the error colour.
    removal: &'static str,
    permanent: bool,
}

fn delete_says(drv: &mut Driver, h: &mut Host, host: Option<&str>, asked: Removal, trash: bool) {
    assert!(h.ops.open_delete(&h.panel, host, asked, trash).is_none());
    drv.idle(h, 2);
    assert!(h.ops.holds_keyboard());
}

fn check(drv: &Driver, said: &Said) {
    assert!(
        drv.find_text(said.question).is_some(),
        "{:?} not on screen",
        said.question
    );
    let colors = drv
        .colors_of(said.removal)
        .unwrap_or_else(|| panic!("{:?} not on screen", said.removal));
    assert_eq!(
        colors.contains(&error_color()),
        said.permanent,
        "{:?}: {colors:?}",
        said.removal
    );
    for other in [
        "to the Trash",
        "permanent",
        "permanent — no Trash here",
        "permanent — no Trash on me@box",
    ] {
        if other != said.removal {
            assert!(drv.find_text(other).is_none(), "{other:?} as well");
        }
    }
}

/// The dialog's question for one file, a folder (`and everything inside it`), N items (`F files,
/// D folders`) and a link to a folder (a link goes as a link); where it goes: this machine's
/// Trash; `permanent — no Trash on <host>` on a remote; `permanent — no Trash here` where this
/// machine has none; Shift+F8, `permanent` in the error colour. Esc cancels each.
#[test]
fn the_delete_dialog_says_what_and_where_it_goes() {
    let (mut h, log) = host(&[]);
    let mut drv = Driver::new(1000.0, 500.0);
    let trash = Said {
        question: "",
        removal: "to the Trash",
        permanent: false,
    };
    let cases: Vec<(&str, Option<&str>, Removal, bool, Said)> = vec![
        (
            "main.rs",
            None,
            Removal::Trash,
            true,
            Said {
                question: "Delete main.rs?",
                ..trash
            },
        ),
        (
            "docs",
            None,
            Removal::Trash,
            true,
            Said {
                question: "Delete docs and everything inside it?",
                ..trash
            },
        ),
        (
            "docs",
            Some("me@box"),
            Removal::Trash,
            true,
            Said {
                question: "Delete docs and everything inside it?",
                removal: "permanent — no Trash on me@box",
                permanent: true,
            },
        ),
        (
            "main.rs",
            Some("me@box"),
            Removal::Permanent,
            true,
            Said {
                question: "Delete main.rs?",
                removal: "permanent — no Trash on me@box",
                permanent: true,
            },
        ),
        (
            "main.rs",
            None,
            Removal::Trash,
            false,
            Said {
                question: "Delete main.rs?",
                removal: "permanent — no Trash here",
                permanent: true,
            },
        ),
        (
            "main.rs",
            None,
            Removal::Permanent,
            true,
            Said {
                question: "Delete main.rs?",
                removal: "permanent",
                permanent: true,
            },
        ),
    ];
    for (name, on_host, asked, trash_here, said) in &cases {
        at(&mut h.panel, name);
        delete_says(&mut drv, &mut h, *on_host, *asked, *trash_here);
        check(&drv, said);
        assert!(drv.find_text("[ Delete ]").is_some(), "its button");
        drv.key(&mut h, Key::Escape, Modifiers::NONE);
        assert!(!h.ops.holds_keyboard(), "Esc closed it: {}", said.question);
        assert!(drv.find_text(said.question).is_none());
    }

    // N items: the selection, files and folders counted apart.
    h.panel.select_group("*.rs", true);
    delete_says(&mut drv, &mut h, None, Removal::Trash, true);
    check(
        &drv,
        &Said {
            question: "Delete 2 items (2 files, 0 folders)?",
            ..trash
        },
    );
    drv.key(&mut h, Key::Escape, Modifiers::NONE);
    h.panel.select_group("docs", true);
    delete_says(&mut drv, &mut h, Some("me@box"), Removal::Trash, true);
    check(
        &drv,
        &Said {
            question: "Delete 3 items (2 files, 1 folder) and everything inside the folder?",
            removal: "permanent — no Trash on me@box",
            permanent: true,
        },
    );
    drv.key(&mut h, Key::Escape, Modifiers::NONE);

    // A link to a folder goes as a link: nothing inside it is deleted.
    h.panel.clear_selection();
    h.panel.entries[1].link = true;
    at(&mut h.panel, "docs");
    delete_says(&mut drv, &mut h, None, Removal::Trash, true);
    check(
        &drv,
        &Said {
            question: "Delete docs?",
            ..trash
        },
    );
    assert!(drv
        .find_text("a link goes as a link, never what it points at")
        .is_some());
    drv.key(&mut h, Key::Escape, Modifiers::NONE);

    drv.idle(&mut h, 3);
    assert!(jobs(&log).is_empty(), "Esc ran nothing");
    assert_eq!(h.list_enters, 0);
}

/// K-D1, the dialog half: Enter gives the job — the folder, the names in the panel's order and
/// where they go, as the dialog said: the Trash only where it said so; permanent on a remote,
/// with Shift+F8 and where there is no Trash here. What was done is unselected; one item done
/// is a status line, more is the report.
#[test]
fn the_delete_dialog_gives_the_job_on_enter() {
    let (mut h, log) = host(&[]);
    let mut drv = Driver::new(1000.0, 500.0);
    h.panel.select_group("*.rs", true);
    h.ops.open_delete(&h.panel, None, Removal::Trash, true);
    drv.idle(&mut h, 2);
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    drv.until(&mut h, 5, |h| !h.changed.is_empty());
    drv.idle(&mut h, 2);
    let names = vec!["lib.rs".to_string(), "main.rs".to_string()];
    assert_eq!(
        jobs(&log),
        [Job::Delete {
            dir: here("/srv/src"),
            names,
            removal: Removal::Trash,
        }]
    );
    let changed = &h.changed[0];
    assert_eq!(changed.dirs, [here("/srv/src")]);
    assert_eq!(changed.bar, None);
    assert!(changed.said.is_none(), "two items: the report says it");
    assert!(drv
        .find_text("Delete from /srv/src to the Trash: 2 done")
        .is_some());
    assert!(selected(&h.panel).is_empty(), "what was done is unselected");
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    assert!(!h.ops.holds_keyboard(), "Enter closed the report");

    let cases = [
        (Some("me@box"), Removal::Trash, true, "deleted main.rs"),
        (None, Removal::Permanent, true, "deleted main.rs"),
        (None, Removal::Trash, false, "deleted main.rs"),
        (None, Removal::Trash, true, "moved main.rs to the Trash"),
    ];
    for (i, (on_host, asked, trash_here, line)) in cases.into_iter().enumerate() {
        at(&mut h.panel, "main.rs");
        h.ops.open_delete(&h.panel, on_host, asked, trash_here);
        drv.idle(&mut h, 2);
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        drv.until(&mut h, 5, |h| h.changed.len() == i + 2);
        let removal = match line {
            "moved main.rs to the Trash" => Removal::Trash,
            _ => Removal::Permanent,
        };
        let dir = OpsTarget {
            host: on_host.map(str::to_string),
            dir: "/srv/src".to_string(),
        };
        assert_eq!(
            jobs(&log).last(),
            Some(&Job::Delete {
                dir: dir.clone(),
                names: vec!["main.rs".to_string()],
                removal,
            }),
            "{line}"
        );
        let changed = &h.changed[i + 1];
        assert_eq!(changed.dirs, [dir]);
        assert_eq!(changed.said.as_ref().unwrap().text, line);
    }
    assert_eq!(h.list_enters, 0, "the dialog's Enter");
}

/// Nothing to delete — the bar on `..` with nothing selected, or a name that is not UTF-8 —
/// opens no dialog and says why on the status row.
#[test]
fn the_delete_dialog_needs_something_to_delete() {
    let (mut h, _) = host(&[]);
    h.panel.home();
    let said = h
        .ops
        .open_delete(&h.panel, None, Removal::Trash, true)
        .expect("refused");
    assert!(!h.ops.holds_keyboard());
    assert!(said.error && said.text.contains(".."), "{said:?}");
    at(&mut h.panel, "main.rs");
    h.panel.entries[h.panel.highlight].lossy = true;
    let said = h
        .ops
        .open_delete(&h.panel, None, Removal::Trash, true)
        .expect("refused");
    assert!(said.text.contains("not valid UTF-8"), "{said:?}");
    assert!(!h.ops.holds_keyboard());
}

/// The dialogs over a list that leaves the keyboard alone while one is open, as the browser's
/// does (04 §3 rule 4) — the host above takes it back on every pass, which undoes a Tab.
struct Bare {
    panel: Panel,
    ops: Ops,
    changed: Vec<Changed>,
}

impl Window for Bare {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(changed) = self.ops.ui(&ctx, &mut self.panel) {
            self.changed.push(changed);
        }
    }

    fn ready(&self) -> bool {
        true
    }
}

/// The delete dialog keeps the keyboard on a holder of its own, not on its Delete button: Space
/// deletes nothing. Tab goes to its buttons, and Enter clicks the one that has the focus —
/// Cancel cancels, Delete deletes.
#[test]
fn the_delete_dialog_enter_clicks_the_focused_button_and_space_deletes_nothing() {
    let log = Shared::default();
    let mut w = Bare {
        panel: panel(),
        ops: Ops::new(engine(log.clone(), &[])),
        changed: Vec::new(),
    };
    let mut drv = Driver::new(1000.0, 500.0);
    at(&mut w.panel, "main.rs");
    w.ops.open_delete(&w.panel, None, Removal::Trash, true);
    drv.idle(&mut w, 2);
    drv.key(&mut w, Key::Space, Modifiers::NONE);
    drv.idle(&mut w, 2);
    assert!(w.ops.holds_keyboard(), "Space: still asking");
    assert!(jobs(&log).is_empty(), "Space deleted nothing");
    drv.key(&mut w, Key::Tab, Modifiers::NONE);
    drv.key(&mut w, Key::Tab, Modifiers::NONE);
    drv.key(&mut w, Key::Enter, Modifiers::NONE);
    drv.idle(&mut w, 3);
    assert!(!w.ops.holds_keyboard(), "Enter clicked Cancel");
    assert!(jobs(&log).is_empty(), "nothing deleted");

    w.ops.open_delete(&w.panel, None, Removal::Trash, true);
    drv.idle(&mut w, 2);
    drv.key(&mut w, Key::Tab, Modifiers::NONE);
    drv.key(&mut w, Key::Enter, Modifiers::NONE);
    drv.until(&mut w, 5, |w| !w.changed.is_empty());
    assert_eq!(
        jobs(&log),
        [Job::Delete {
            dir: here("/srv/src"),
            names: vec!["main.rs".to_string()],
            removal: Removal::Trash,
        }],
        "Enter clicked Delete"
    );
}
