//! The switch to a window on another machine (10 §7.3–§7.4, X2-switch), against a stand-in ssh
//! (AGENTS.md: no ssh or network in tests) that runs the words it is given as sshd's login shell
//! would — the forward's real script, `SOPEN_TARGET_BIN` a stand-in HOST sopen playing HOST's
//! half (its state in, its cues, stderr and exit codes out) — and the terminal scripted as the
//! backend's other tests script it ([`Tape`], its bytes in the strict [`Vt`]).
//!
//! - TW-2: the state in and the cue out, its paths rewritten both ways (a folder on this
//!   machine, on HOST itself and on a third machine; the target's user or spelling aside);
//!   `BatchMode=yes` on ssh's command line and `SSH_ASKPASS_REQUIRE=never` in its environment;
//!   HOST's stderr on the status row; Ctrl+C → the state from before, with the kept line or the
//!   lost-link one; each of §7.3's ends → its line, the terminal staying; an unknown `"v"`; a
//!   running copy → `waiting for…`, then the switch.
//! - TW-4's terminal half: the question before the switch — Cancel stays, Save switches once it
//!   lands, Discard switches with the file as stored; and its HOST half's Cancel and Save (item 7
//!   of 10 §17's X2-switch row: what a prompt's answer leaves on HOST).
//! - TL-1's remote half: still raw and on the alternate screen, the screen printed whole and
//!   dimmed, only Ctrl+C read.

use super::super::tests::{backend, ctrl_q, fixture_with, key, plain, Bytes, Fx, Ink, Tape, Vt};
use super::*;
use crate::gui::browser::ops;
use crate::gui::browser::switch_state::{Mode, PanelState};
use crate::gui::switch_host::{self, Cues, Input};
use crate::gui::term::{to_window, End, Place, ENTER, LEAVE};
use crate::gui::testkit::Driver;
use crate::gui::Window as _;
use crossterm::event::{KeyCode, KeyModifiers};
use eframe::egui::{self, Key, Modifiers};
use std::cell::Cell;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// This machine's name in these tests: the switch tells HOST `here:/abs` for a folder here.
const HOSTNAME: &str = "here.lan";

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A stand-in ssh and HOST sopen in `dir`. The ssh records its arguments (`ssh-args`, a line
/// each) and `SSH_ASKPASS_REQUIRE` (`askpass`), then runs the words after `--` as sshd hands a
/// command to the login shell — the forward's own script, a display set so a Linux runner never
/// asks systemd for one. The sopen (`SOPEN_TARGET_BIN`) records its arguments (`sopen-args`) and
/// its pid (`sopen-pid`), reads the state's line (`state-in`), then runs `body` — the line in
/// `$line`.
fn stand_in(dir: &Path, body: &str) -> PathBuf {
    let file = |name: &str| forward::quote(dir.join(name).to_str().unwrap());
    let ssh = dir.join("ssh");
    executable(
        &ssh,
        &format!(
            "#!/bin/sh\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\"; done > {args}\n\
             printf '%s\\n' \"${{SSH_ASKPASS_REQUIRE-unset}}\" > {askpass}\n\
             while [ $# -gt 0 ] && [ \"$1\" != -- ]; do shift; done\n\
             shift\n\
             DISPLAY=${{DISPLAY:-:99}}; export DISPLAY\n\
             exec /bin/sh -c \"$*\"\n",
            args = file("ssh-args"),
            askpass = file("askpass"),
        ),
    );
    executable(
        &dir.join("sopen"),
        &format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$@\" > {args}\n\
             echo $$ > {pid}\n\
             IFS= read -r line\n\
             printf '%s\\n' \"$line\" > {state}\n\
             {body}\n",
            args = file("sopen-args"),
            pid = file("sopen-pid"),
            state = file("state-in"),
        ),
    );
    ssh
}

/// The lines a stand-in wrote to `name` in `dir`.
fn lines(dir: &Path, name: &str) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    text.lines().map(str::to_string).collect()
}

/// The switch over the stand-in in `dir` (`bin`: what `SOPEN_TARGET_BIN` names), this machine
/// named [`HOSTNAME`], no master (the stand-in needs none); a fresh browser on `root`.
fn remote_for(dir: &Path, bin: &Path, root: &str) -> Remote {
    let bin = bin.to_str().unwrap().to_string();
    let (tmp, root) = (dir.to_path_buf(), root.to_string());
    Remote {
        ssh: dir.join("ssh").into_os_string(),
        connection: Box::new(|_| Ok(vec![])),
        env: Box::new(move |_| Ok(vec![format!("SOPEN_TARGET_BIN={bin}")])),
        install_paths: String::new(),
        hostname: Some(HOSTNAME.to_string()),
        fresh: Box::new(move || Ok(start(&tmp, &root, None))),
        made: RefCell::default(),
    }
}

/// A browser's start on `root` (this machine's files, named `label`), its cache and state in
/// `tmp`.
fn start(tmp: &Path, root: &str, label: Option<&str>) -> browser::Start {
    let connect: browser::Connect =
        std::sync::Arc::new(|| Ok(Box::new(crate::remote::LocalFs::new()) as Box<_>));
    browser::Start {
        fs: Box::new(crate::remote::LocalFs::new()),
        connect,
        label: label.map(str::to_string),
        destination: None,
        abs: root.to_string(),
        cache: tmp.join("cache"),
        state_dir: tmp.join("state"),
        launch: std::sync::Arc::new(|abs: &str, _| Ok(format!("opened {abs}"))),
        git: None,
    }
}

/// When the script's next event may come: the browser settled, or the step's own condition.
type When = Box<dyn Fn() -> bool>;

/// `events` given one at a time, each once its condition holds (`None`: once the browser has
/// settled).
fn script(tape: &Tape, fx: &Fx, events: Vec<(CEvent, Option<When>)>) {
    let browser = fx.b.clone();
    let given = Rc::new(Cell::new(0));
    let mut whens = Vec::new();
    let mut s = tape.0.borrow_mut();
    for (ev, when) in events {
        s.events.push_back(ev);
        whens.push(when);
    }
    s.gate = Some(Box::new(move || {
        let i = given.get();
        let open = match whens.get(i) {
            Some(Some(when)) => when(),
            _ => browser.borrow().ready(),
        };
        if open {
            given.set(i + 1);
        }
        open
    }));
}

/// What a switch's run left: the line it said, how often it ran, its state before.
#[derive(Default)]
struct Ran {
    said: RefCell<Vec<Option<String>>>,
    times: Cell<usize>,
    before: RefCell<Option<SwitchState>>,
    /// How many bytes the terminal had been sent when the switch last returned.
    sent: Cell<usize>,
    /// The status row once the screen after it was printed (the next key puts the line away),
    /// and the browser's state then (Ctrl+Q closes a viewer).
    shown: RefCell<Option<String>>,
    then: RefCell<Option<SwitchState>>,
    /// The whole screen then.
    screen: RefCell<Option<String>>,
}

/// Runs the browser of `fx` in `b` with the switch to `box` over `remote`: what each switch said,
/// and how the run ended.
fn run(b: &mut Backend<Tape, Bytes>, fx: &Fx, remote: &Remote, ran: &Rc<Ran>) -> End {
    let shared = fx.b.clone();
    let mut switch = |b: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
        if ran.before.borrow().is_none() {
            *ran.before.borrow_mut() = Some(shared.borrow().take_state(&b.ctx));
        }
        let host = |b: &mut Backend<Tape, Bytes>, host: &str| to_host(b, &shared, host, remote);
        let said = to_window(b, &shared, Ok(Place::Host("box".into())), host, |_| {
            unreachable!("no window here")
        });
        if let Ok(line) = &said {
            ran.said.borrow_mut().push(line.clone());
        }
        ran.times.set(ran.times.get() + 1);
        ran.sent.set(b.term.out.0.borrow().len());
        said
    };
    let due = fx.b.clone();
    b.switch_due = Some(Box::new(move || due.borrow_mut().switch_due()));
    let mut win = Shared(fx.b.clone());
    b.run(&mut win, &mut switch).unwrap()
}

/// The fixture's browser in dual mode (no preview: each side its list), ready.
fn dual(fx: &Fx) {
    let mut drv = Driver::new(800.0, 480.0);
    let mut win = Shared(fx.b.clone());
    drv.until(&mut win, 20, |w| w.ready());
    drv.key(&mut win, Key::F9, Modifiers::NONE);
    drv.until(&mut win, 20, |w| w.ready());
}

/// The browser's state, made comparable across browsers: what it shows.
fn state_of(fx: &Fx) -> SwitchState {
    fx.b.borrow().take_state(&egui::Context::default())
}

fn cue_of(state: &SwitchState) -> String {
    format!(
        "printf '%s\\n' {}",
        forward::quote(&format!("sopen-xtools-state {}", state.line()))
    )
}

// ---------------------------------------------------------------------------------------------
// The paths (§7.3).

/// The state's paths out to HOST and back, for a folder on this machine, on HOST itself (with the
/// target's user and without), on a third machine, and on a loopback name: out as §4a rewrites
/// SOURCE — FOLDER and the state agree — and back with the terminal browser's own label, which
/// `give_state` compares with; a Copy target this machine typed comes back as typed, one HOST
/// typed as HOST's place.
#[test]
fn the_state_goes_out_as_host_sees_it_and_comes_back_as_this_machine_does() {
    let cases: [(&str, Option<&str>, Option<&str>, &str); 6] = [
        // host, the terminal's label, HOST's label for it, FOLDER
        ("box", None, Some("here"), "here:/r"),
        ("box", Some("box"), None, "/r"),
        ("me@box", Some("box"), None, "/r"),
        ("box", Some("me@box"), Some("me@box"), "me@box:/r"),
        ("box", Some("other"), Some("other"), "other:/r"),
        ("box", Some("me@localhost"), Some("me@here"), "me@here:/r"),
    ];
    for (host, here, there, folder) in cases {
        let what = format!("{host} {here:?}");
        let ways = Ways::new(host, here, Some(HOSTNAME)).unwrap();
        let mut state = state_at(here, "/r");
        state.dialog = Some(switch_state::DialogState {
            kind: switch_state::DialogKind::Copy,
            text: Some("/srv/x".into()),
            caret: Some(6),
            anchor: Some(6),
            error: None,
            entries: vec!["f".into()],
            unplaced: None,
        });
        let sent = state.rewritten(|p| ways.out(p)).unwrap();
        assert_eq!(sent.host.as_deref(), there, "{what}");
        let at = show(sent.host.as_deref(), &sent.panels[sent.active].folder);
        assert_eq!(at, folder, "{what}");
        // A Copy target typed here is this machine's place, wherever the list is.
        assert_eq!(
            sent.dialog.as_ref().unwrap().text.as_deref(),
            Some("here:/srv/x"),
            "{what}"
        );
        let back = sent.rewritten(|p| ways.back(p)).unwrap();
        assert_eq!(back, state, "{what}");
        // HOST typed one of its own.
        let mut theirs = sent.clone();
        theirs.dialog.as_mut().unwrap().text = Some("/opt/y".into());
        let back = theirs.rewritten(|p| ways.back(p)).unwrap();
        // HOST's own place: as the folders come back where they are HOST's (the pair), else
        // HOST's label.
        let want = match there {
            None => show(here, "/opt/y"),
            Some(_) => match crate::target::parse(&format!("{host}:/")) {
                Ok(crate::target::Target::Remote(spec)) => format!("{}:/opt/y", spec.label()),
                _ => unreachable!(),
            },
        };
        assert_eq!(
            back.dialog.unwrap().text.as_deref(),
            Some(want.as_str()),
            "{what}"
        );
    }
}

/// A state of `label`'s (`None`: this machine) on `root` and its folder `a`, a viewer on `f`.
fn state_at(label: Option<&str>, root: &str) -> SwitchState {
    let panel = |folder: String| PanelState {
        folder,
        bar: Some("f".into()),
        selected: vec![],
        top: 0,
        tree: false,
        pin: None,
    };
    SwitchState {
        v: switch_state::VERSION,
        host: label.map(str::to_string),
        mode: Mode::Dual,
        active: 0,
        panels: [panel(root.to_string()), panel(format!("{root}/a"))],
        git_focus: false,
        dialog: None,
        viewer: Some(switch_state::ViewerState {
            file: format!("{root}/f"),
            view: 1,
            place: None,
            encoding: None,
            wrap: None,
            revision: false,
        }),
    }
}

// ---------------------------------------------------------------------------------------------
// TW-2.

/// F12 with a HOST: HOST's half runs over ssh with `-T`, its input kept, `BatchMode=yes`,
/// `SSH_ASKPASS_REQUIRE=never`, the forward's words (`--gui --state-stdin -- here:<folder>`, the
/// script adding `--here`), the state's line on its input — this machine's folders as
/// `here:/abs`. Its cue's state, HOST's own change in it, comes back as this machine's paths in a
/// browser that shows it; nothing is said.
#[test]
fn tw2_hosts_half_runs_with_the_state_and_its_cue_comes_back() {
    let fx = fixture_with(0, &[]);
    dual(&fx);
    let tmp = tempfile::tempdir().unwrap();
    let mut reply = state_at(Some("here"), &fx.root);
    reply.viewer = None;
    reply.active = 1;
    reply.panels[0].bar = Some("beta".into());
    reply.panels[1] = PanelState {
        folder: format!("{}/alpha", fx.root),
        bar: None,
        selected: vec![],
        top: 0,
        tree: false,
        pin: None,
    };
    stand_in(tmp.path(), &cue_of(&reply));
    let remote = remote_for(tmp.path(), &tmp.path().join("sopen"), &fx.root);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let ran = Rc::new(Ran::default());
    script(
        &tape,
        &fx,
        vec![
            (plain(KeyCode::F(12)), None),
            (ctrl_q(), Some(after(&ran, &fx, &bytes, 1))),
        ],
    );
    assert_eq!(run(&mut b, &fx, &remote, &ran), End::Closed);
    assert_eq!(
        *ran.said.borrow(),
        [None],
        "back with HOST's state, nothing said"
    );
    // ssh's command line: the master's options (none here), then the switch's.
    let args = lines(tmp.path(), "ssh-args");
    let at = args.iter().position(|a| a == "box").expect("HOST");
    assert_eq!(
        args[..at],
        [
            "-T",
            "-o",
            "RemoteCommand=none",
            "-o",
            "LogLevel=ERROR",
            "-o",
            "BatchMode=yes"
        ]
    );
    assert_eq!(args[at + 1], "--");
    assert_eq!(args[at + 2], "exec");
    let folder = format!("here:{}", fx.root);
    assert_eq!(
        args[args.len() - 4..],
        [
            "'--gui'",
            "'--state-stdin'",
            "'--'",
            &forward::quote(&folder)
        ]
    );
    assert_eq!(lines(tmp.path(), "askpass"), ["never"]);
    // Through the script: HOST's sopen got `--here` first.
    assert_eq!(
        lines(tmp.path(), "sopen-args"),
        ["--here", "--gui", "--state-stdin", "--", folder.as_str()]
    );
    // The state as HOST read it: this machine's folders as `here:/abs`.
    let sent = switch_state::read(&lines(tmp.path(), "state-in")[0], "HOST").unwrap();
    let before = ran.before.borrow().clone().unwrap();
    assert_eq!(sent.host.as_deref(), Some("here"));
    assert_eq!(
        sent.panels, before.panels,
        "the same folders, of host `here`"
    );
    // HOST's state, back as this machine's.
    let back = state_of(&fx);
    assert_eq!(back.host, None);
    assert_eq!(back.active, 1);
    assert_eq!(back.panels[0].folder, fx.root);
    assert_eq!(back.panels[0].bar.as_deref(), Some("beta"));
    assert_eq!(back.panels[1].folder, format!("{}/alpha", fx.root));
    assert_eq!(
        remote.made.borrow().len(),
        1,
        "a browser made for it, finished at the end"
    );
}

/// A condition that holds once the switch has run `n` times, the browser has settled and the
/// screen after it — its line on the status row — has been printed.
fn after(ran: &Rc<Ran>, fx: &Fx, bytes: &Bytes, n: usize) -> When {
    let (ran, browser, bytes) = (ran.clone(), fx.b.clone(), bytes.clone());
    Box::new(move || {
        let printed = bytes.0.borrow().len() > ran.sent.get();
        let open = ran.times.get() >= n && printed && browser.borrow().ready();
        if open {
            let screen = bytes.last_screen(100, 30);
            *ran.shown.borrow_mut() = Some(screen.row(28).trim().to_string());
            *ran.screen.borrow_mut() = Some(screen.text());
            *ran.then.borrow_mut() = Some(browser.borrow().take_state(&egui::Context::default()));
        }
        open
    })
}

/// A condition that holds once the stand-in in `dir` has written `name`.
fn wrote(dir: &Path, name: &str) -> When {
    let file = dir.join(name);
    Box::new(move || file.exists())
}

/// A condition that holds once the terminal shows `text` (its bytes through the strict VT).
fn says(bytes: &Bytes, text: &str) -> When {
    let (bytes, text) = (bytes.clone(), text.to_string());
    Box::new(move || shows(&bytes, &text))
}

/// Whether the terminal shows `text` now.
fn shows(bytes: &Bytes, text: &str) -> bool {
    bytes.text().contains(ENTER) && bytes.last_screen(100, 30).text().contains(text)
}

fn ctrl_c() -> CEvent {
    key(KeyCode::Char('c'), KeyModifiers::CONTROL)
}

/// Each of §7.3's ends of HOST's half is its line on the status row, and the terminal stays with
/// the browser as it was: 0 without the cue, 1, 2 and 3 with HOST's last stderr line, a signal
/// (128 + n), no sopen there (127), ssh's own failure (255) with ssh's line, ssh ended by a
/// signal (no exit code); a cue of a newer `"v"` is §7.4's line. A cue's state is used whatever
/// the code (3: a refused save, its line said too).
#[test]
fn tw2_each_end_of_hosts_half_is_its_line_and_the_terminal_stays() {
    let lost = "ssh to box ended without an exit code (";
    let cases: [(&str, bool, &str); 8] = [
        (
            "exit 0",
            false,
            "sopen on box ended without handing the screen back",
        ),
        (
            "echo 'it broke' >&2; exit 1",
            false,
            "sopen on box failed: it broke",
        ),
        (
            "echo \"error: unexpected argument '--state-stdin' found\" >&2; exit 2",
            false,
            "sopen on box is too old for the switch, or a usage error: error: unexpected \
             argument '--state-stdin' found",
        ),
        ("kill -TERM $$", false, "sopen on box ended by signal 15"),
        (
            "exit 0",
            true,
            "sopen is not installed on box (brew install 2lab-ai/tap/sopen-preview)",
        ),
        (
            "echo 'ssh: connect to host box port 22: Connection refused' >&2; exit 255",
            false,
            "could not reach box: ssh: connect to host box port 22: Connection refused — run ssh \
             box once in a shell to answer its questions",
        ),
        ("kill -KILL $PPID", false, lost),
        (
            "printf '%s\\n' 'sopen-xtools-state {\"v\":9}'",
            false,
            "box sent switch state v9; this sopen reads up to v1 — update one of them",
        ),
    ];
    for (body, missing, want) in cases {
        let fx = fixture_with(0, &[]);
        dual(&fx);
        let before = state_of(&fx);
        let tmp = tempfile::tempdir().unwrap();
        stand_in(tmp.path(), body);
        let bin = tmp.path().join(if missing { "missing" } else { "sopen" });
        let remote = remote_for(tmp.path(), &bin, &fx.root);
        let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
        let ran = Rc::new(Ran::default());
        let events = vec![
            (plain(KeyCode::F(12)), None),
            (ctrl_q(), Some(after(&ran, &fx, &bytes, 1))),
        ];
        script(&tape, &fx, events);
        assert_eq!(run(&mut b, &fx, &remote, &ran), End::Closed, "{body}");
        let said = ran.said.borrow()[0].clone().unwrap_or_default();
        match want == lost {
            true => assert!(said.starts_with(lost), "{body}: {said}"),
            false => assert_eq!(said, want, "{body}"),
        }
        let row = ran.shown.borrow().clone().unwrap_or_default();
        let shown = row.trim_end_matches('…');
        assert!(
            said.starts_with(shown) && !shown.is_empty(),
            "{body}: on the status row: {row}\n{}",
            bytes.last_screen(100, 30).text()
        );
        assert_eq!(
            state_of(&fx),
            before,
            "the terminal stays as it was: {body}"
        );
        assert!(remote.made.borrow().is_empty(), "{body}");
    }
    // A cue whose state the browser cannot take (another host's): its reason, the browser kept.
    let fx = fixture_with(0, &[]);
    dual(&fx);
    let before = state_of(&fx);
    let tmp = tempfile::tempdir().unwrap();
    let mut theirs = state_at(Some("other"), &fx.root);
    theirs.viewer = None;
    stand_in(tmp.path(), &cue_of(&theirs));
    let remote = remote_for(tmp.path(), &tmp.path().join("sopen"), &fx.root);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let ran = Rc::new(Ran::default());
    let events = vec![
        (plain(KeyCode::F(12)), None),
        (ctrl_q(), Some(after(&ran, &fx, &bytes, 1))),
    ];
    script(&tape, &fx, events);
    assert_eq!(run(&mut b, &fx, &remote, &ran), End::Closed);
    assert_eq!(
        *ran.said.borrow(),
        [Some(
            "the switch's state is of other, this window of this machine".into()
        )]
    );
    assert_eq!(state_of(&fx), before, "the browser as it was");
    assert!(remote.made.borrow().is_empty());

    // 3, with the cue: its state used, the refused save said.
    let fx = fixture_with(0, &[]);
    dual(&fx);
    let tmp = tempfile::tempdir().unwrap();
    let mut reply = state_at(Some("here"), &fx.root);
    reply.viewer = None;
    reply.panels[1].folder = format!("{}/beta", fx.root);
    let body = format!(
        "{}; echo 'refused: it changed on disk' >&2; exit 3",
        cue_of(&reply)
    );
    stand_in(tmp.path(), &body);
    let remote = remote_for(tmp.path(), &tmp.path().join("sopen"), &fx.root);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let ran = Rc::new(Ran::default());
    let events = vec![
        (plain(KeyCode::F(12)), None),
        (ctrl_q(), Some(after(&ran, &fx, &bytes, 1))),
    ];
    script(&tape, &fx, events);
    assert_eq!(run(&mut b, &fx, &remote, &ran), End::Closed);
    assert_eq!(
        *ran.said.borrow(),
        [Some(
            "the window on box ended with a refused save: refused: it changed on disk".into()
        )]
    );
    assert_eq!(state_of(&fx).panels[1].folder, format!("{}/beta", fx.root));
}

/// Meanwhile (TL-1's remote half, T28): the terminal stays raw and on the alternate screen —
/// nothing of it is left — and the screen is printed whole again, dimmed, under the box's line,
/// HOST's stderr line on the status row; a key typed then is not read (F7's dialog never opens);
/// Ctrl+C closes HOST's input, and HOST's kept cue brings the terminal back as it was before the
/// switch, with the kept line. Ctrl+C with the link cut instead: the lost-link line.
#[test]
fn tw2_ctrl_c_ends_hosts_input_and_the_terminal_comes_back_as_before() {
    let body = "echo 'opening the window there' >&2; cat > /dev/null; echo sopen-xtools-kept";
    let fx = fixture_with(0, &[]);
    dual(&fx);
    let before = state_of(&fx);
    let tmp = tempfile::tempdir().unwrap();
    stand_in(tmp.path(), body);
    let remote = remote_for(tmp.path(), &tmp.path().join("sopen"), &fx.root);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let ran = Rc::new(Ran::default());
    // What the terminal was sent, and its raw mode's switches, while HOST's line shows.
    type Meanwhile = Option<(Vec<u8>, Vec<bool>)>;
    let meanwhile: Rc<RefCell<Meanwhile>> = Rc::default();
    let away: When = {
        let (bytes, tape, seen) = (bytes.clone(), tape.clone(), meanwhile.clone());
        Box::new(move || {
            let shown = bytes.text().contains("opening the window there");
            if shown && seen.borrow().is_none() {
                let raws = tape.0.borrow().raws.clone();
                *seen.borrow_mut() = Some((bytes.0.borrow().clone(), raws));
            }
            shown
        })
    };
    let events = vec![
        (plain(KeyCode::F(12)), None),
        (plain(KeyCode::F(7)), Some(away)),
        (ctrl_c(), Some(Box::new(|| true) as When)),
        (ctrl_q(), Some(after(&ran, &fx, &bytes, 1))),
    ];
    script(&tape, &fx, events);
    assert_eq!(run(&mut b, &fx, &remote, &ran), End::Closed);
    assert_eq!(
        *ran.said.borrow(),
        [Some(
            "the window on box stays open: it has unsaved edits".into()
        )]
    );
    assert_eq!(state_of(&fx), before, "the state from before the switch");
    let (sent, raws) = meanwhile.borrow().clone().expect("HOST's line was shown");
    assert_eq!(raws, [true], "raw mode never left");
    let text = String::from_utf8(sent.clone()).unwrap();
    let entered = text.rfind(ENTER).expect("entered");
    assert!(!text[entered..].contains(LEAVE), "the terminal is not left");
    let mut vt = Vt::new(100, 30);
    vt.feed(&sent);
    assert!(vt.alt, "on the alternate screen");
    assert!(vt.modes.contains(&1000), "its modes on");
    assert!(!vt.cursor_shown, "no caret");
    let screen = vt.text();
    assert!(
        screen.contains("xtools is in a window on box — close it or press F12 there; Ctrl+C"),
        "{screen}"
    );
    assert_eq!(vt.row(28).trim(), "opening the window there", "{screen}");
    // Dimmed: the title row's cells in the shadow's colours.
    assert_eq!(vt.cell(0, 0).fg, Ink::Rgb(cga::DARK_GRAY));
    assert_eq!(vt.cell(0, 0).bg, Ink::Rgb(cga::BLACK));
    let after_switch = bytes.last_screen(100, 30).text();
    assert!(
        !after_switch.contains("Make a folder"),
        "F7 was not read:\n{after_switch}"
    );
    assert!(
        !after_switch.contains("xtools is in a window"),
        "{after_switch}"
    );

    // The link cut after Ctrl+C: no kept cue, ssh's own failure.
    let fx = fixture_with(0, &[]);
    dual(&fx);
    let tmp = tempfile::tempdir().unwrap();
    stand_in(tmp.path(), "cat > /dev/null; exit 255");
    let remote = remote_for(tmp.path(), &tmp.path().join("sopen"), &fx.root);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let ran = Rc::new(Ran::default());
    let events = vec![
        (plain(KeyCode::F(12)), None),
        (ctrl_c(), Some(wrote(tmp.path(), "state-in"))),
        (ctrl_q(), Some(after(&ran, &fx, &bytes, 1))),
    ];
    script(&tape, &fx, events);
    assert_eq!(run(&mut b, &fx, &remote, &ran), End::Closed);
    assert_eq!(
        *ran.said.borrow(),
        [Some(
            "lost the link to box; a window there with unsaved edits stays open".into()
        )]
    );
}

/// A HOST that never answers its input's end: the box says Ctrl+C closed it, and a second Ctrl+C
/// ends this run's ssh client (its pid alone) — the link lost, the terminal back.
#[test]
fn a_second_ctrl_c_ends_this_runs_ssh() {
    let fx = fixture_with(0, &[]);
    dual(&fx);
    let tmp = tempfile::tempdir().unwrap();
    stand_in(tmp.path(), "exec sleep 30");
    let remote = remote_for(tmp.path(), &tmp.path().join("sopen"), &fx.root);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let ran = Rc::new(Ran::default());
    let events = vec![
        (plain(KeyCode::F(12)), None),
        (ctrl_c(), Some(wrote(tmp.path(), "state-in"))),
        (ctrl_c(), Some(says(&bytes, "box's input is closed"))),
        (ctrl_q(), Some(after(&ran, &fx, &bytes, 1))),
    ];
    script(&tape, &fx, events);
    let started = std::time::Instant::now();
    assert_eq!(run(&mut b, &fx, &remote, &ran), End::Closed);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "not ssh's 30 s"
    );
    assert_eq!(
        *ran.said.borrow(),
        [Some(
            "lost the link to box; a window there with unsaved edits stays open".into()
        )]
    );
    // The stand-in HOST sopen (`sleep`) outlives the ssh client killed under it, as HOST's would.
    if let Some(pid) = lines(tmp.path(), "sopen-pid")
        .first()
        .and_then(|p| p.parse().ok())
    {
        // SAFETY: the pid this test's stand-in recorded.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// A file operation copy holding until `release` says, `running` set once it runs.
fn held_copy(running: Arc<AtomicBool>, release: std::sync::mpsc::Receiver<()>) -> ops::Run {
    let release = std::sync::Mutex::new(release);
    Arc::new(move |job: ops::Job, _: &ops::Link| {
        running.store(true, Ordering::SeqCst);
        let _ = release.lock().unwrap().recv();
        let names = match job {
            ops::Job::Transfer { names, .. } => names,
            _ => vec![],
        };
        crate::fileops::Report {
            items: (names.into_iter())
                .map(|name| crate::fileops::Item {
                    name,
                    outcome: crate::fileops::ItemOutcome::Done,
                })
                .collect(),
            notes: vec![],
        }
    })
}

/// Keys pressed in a window on the fixture's browser, each once it has settled.
fn keys(fx: &Fx, pressed: &[Key]) {
    let mut drv = Driver::new(800.0, 480.0);
    let mut win = Shared(fx.b.clone());
    drv.until(&mut win, 20, |w| w.ready());
    for &k in pressed {
        drv.key(&mut win, k, Modifiers::NONE);
        drv.until(&mut win, 20, |w| w.ready());
    }
}

/// A copy that runs makes the switch wait: `waiting for the file operation…` on the status row,
/// nothing run on HOST yet; once it has ended the switch goes by itself, the state with it.
#[test]
fn tw2_a_running_copy_makes_the_switch_wait_then_it_goes() {
    let fx = fixture_with(0, &[]);
    dual(&fx);
    // The right side in `beta`, the bar on `alpha` in the left one.
    keys(
        &fx,
        &[
            Key::Tab,
            Key::ArrowDown,
            Key::ArrowDown,
            Key::Enter,
            Key::Tab,
            Key::ArrowDown,
        ],
    );
    let running = Arc::new(AtomicBool::new(false));
    let (release, held) = std::sync::mpsc::channel();
    fx.b.borrow_mut()
        .set_ops_run(held_copy(running.clone(), held));
    let tmp = tempfile::tempdir().unwrap();
    stand_in(tmp.path(), "printf 'sopen-xtools-state %s\\n' \"$line\"");
    let remote = remote_for(tmp.path(), &tmp.path().join("sopen"), &fx.root);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let ran = Rc::new(Ran::default());
    // Ctrl+Q once the switch has gone; the copy let go once the status row says it waits.
    let gone: When = {
        let (bytes, dir, ran, browser) = (
            bytes.clone(),
            tmp.path().to_path_buf(),
            ran.clone(),
            fx.b.clone(),
        );
        Box::new(move || {
            if ran.times.get() == 1 && shows(&bytes, "waiting for the file operation…") {
                assert!(
                    !dir.join("state-in").exists(),
                    "nothing runs on HOST meanwhile"
                );
                let _ = release.send(());
            }
            ran.times.get() >= 2 && browser.borrow().ready()
        })
    };
    let copying = running.clone();
    let events = vec![
        (plain(KeyCode::F(5)), None),
        (plain(KeyCode::Enter), None),
        (
            plain(KeyCode::F(12)),
            Some(Box::new(move || copying.load(Ordering::SeqCst)) as When),
        ),
        (ctrl_q(), Some(gone)),
    ];
    script(&tape, &fx, events);
    assert_eq!(run(&mut b, &fx, &remote, &ran), End::Closed);
    assert!(running.load(Ordering::SeqCst));
    assert_eq!(
        *ran.said.borrow(),
        [None, None],
        "held, then back with the state"
    );
    assert_eq!(ran.times.get(), 2, "asked for again once the copy ended");
    let sent = switch_state::read(&lines(tmp.path(), "state-in")[0], "HOST").unwrap();
    assert_eq!(sent.panels[1].folder, format!("{}/beta", fx.root));
    assert_eq!(state_of(&fx).panels[1].folder, format!("{}/beta", fx.root));
}

/// The fixture with `0.bin` (every byte) first among its files, in a window: its hex viewer
/// open, a byte typed over (`Z` in the ASCII column once Enter has started editing, 08 D35).
fn hex_edited() -> (Fx, Driver, Shared) {
    let fx = fixture_with(0, &["0.bin"]);
    std::fs::write(
        Path::new(&fx.root).join("0.bin"),
        (0..=255u8).collect::<Vec<u8>>(),
    )
    .unwrap();
    let mut drv = Driver::new(800.0, 480.0);
    let mut win = Shared(fx.b.clone());
    drv.until(&mut win, 20, |w| w.ready());
    // `..`, three folders, then it.
    for _ in 0..4 {
        drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    }
    drv.until(&mut win, 20, |w| w.ready());
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    drv.until(&mut win, 20, |w| w.ready());
    drv.idle(&mut win, 2);
    drv.key(&mut win, Key::F8, Modifiers::NONE);
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    drv.frame(
        &mut win,
        vec![egui::Event::Text("Z".into())],
        Modifiers::NONE,
    );
    drv.idle(&mut win, 1);
    assert!(
        drv.text_starting_with("5A 01 02").is_some(),
        "the byte is typed over"
    );
    drv.until(&mut win, 20, |w| w.ready());
    (fx, drv, win)
}

/// Whether a text the window drew holds `text` (a dialog's rows are wrapped).
fn has(drv: &Driver, text: &str) -> bool {
    drv.texts().iter().any(|(_, _, t)| t.contains(text))
}

/// The first byte of `0.bin` on disk.
fn first_byte(fx: &Fx) -> u8 {
    std::fs::read(Path::new(&fx.root).join("0.bin")).unwrap()[0]
}

/// TW-4's terminal half: unsaved edits in the viewer — the switch asks first, `Save the changes
/// to <file> before switching to box?` with 07 §10's four answers. Cancel stays (asked again
/// next time); Save saves through the viewer's own F5 and lets the switch go once it has landed,
/// not before; Discard lets it go at once, the file as stored, the edits still in the viewer.
#[test]
fn tw4_unsaved_edits_ask_before_the_switch() {
    let (fx, mut drv, mut win) = hex_edited();
    let question = "switching to box?";
    assert_eq!(fx.b.borrow_mut().before_switch("box"), Before::Held);
    drv.idle(&mut win, 2);
    assert!(has(&drv, "Save the changes to"), "the question is up");
    assert!(has(&drv, question), "the question is up");
    assert!(
        drv.find_text("[ Discard ]").is_some(),
        "with 07 §10's answers"
    );
    drv.click_text_on_top(&mut win, "[ Cancel ]");
    drv.idle(&mut win, 2);
    assert!(!has(&drv, question), "Cancel closes it");
    assert!(!fx.b.borrow_mut().switch_due(), "and stays");
    assert!(
        drv.text_starting_with("5A 01 02").is_some(),
        "the edit still there"
    );
    // Save: once it has landed.
    assert_eq!(
        fx.b.borrow_mut().before_switch("box"),
        Before::Held,
        "asked again"
    );
    drv.idle(&mut win, 2);
    assert!(has(&drv, question));
    drv.click_text_on_top(&mut win, "[ Save ]");
    assert_eq!(first_byte(&fx), 0, "not saved yet");
    drv.idle(&mut win, 1);
    assert!(
        !fx.b.borrow_mut().switch_due(),
        "not due while the save runs"
    );
    let mut due = false;
    for _ in 0..400 {
        drv.idle(&mut win, 1);
        if fx.b.borrow_mut().switch_due() {
            due = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(due, "due once saved");
    assert_eq!(first_byte(&fx), 0x5A, "saved before it is due");
    assert_eq!(fx.b.borrow_mut().before_switch("box"), Before::Go);

    // Discard: let go at once; the edits go once the switch has started.
    let (fx, mut drv, mut win) = hex_edited();
    assert_eq!(fx.b.borrow_mut().before_switch("box"), Before::Held);
    drv.idle(&mut win, 2);
    drv.click_text_on_top(&mut win, "[ Discard ]");
    drv.idle(&mut win, 1);
    assert!(fx.b.borrow_mut().switch_due());
    assert_eq!(fx.b.borrow_mut().before_switch("box"), Before::Go);
    assert!(
        fx.b.borrow_mut().would_lose(),
        "not before the switch starts"
    );
    fx.b.borrow_mut().discard_for_switch(&drv.ctx);
    drv.idle(&mut win, 2);
    assert!(!fx.b.borrow_mut().would_lose(), "the edits dropped");
    assert!(!has(&drv, "Esc or F3 back to the list"), "the viewer left");
    assert_eq!(first_byte(&fx), 0, "the file as stored");

    // A switch that did not start forgets the answer: the edits stay, and it asks again.
    let (fx, mut drv, mut win) = hex_edited();
    assert_eq!(fx.b.borrow_mut().before_switch("box"), Before::Held);
    drv.idle(&mut win, 2);
    drv.click_text_on_top(&mut win, "[ Discard ]");
    drv.idle(&mut win, 1);
    assert!(fx.b.borrow_mut().switch_due());
    assert_eq!(fx.b.borrow_mut().before_switch("box"), Before::Go);
    fx.b.borrow_mut().switch_not_started();
    fx.b.borrow_mut().discard_for_switch(&drv.ctx);
    drv.idle(&mut win, 1);
    assert!(
        fx.b.borrow_mut().would_lose(),
        "the edit is still in the viewer"
    );
    assert_eq!(fx.b.borrow_mut().before_switch("box"), Before::Held);
}

/// TW-4 through the terminal, every key the terminal's — the viewer opened, a byte typed over,
/// F12 — so the view keys are the terminal context's as the owner's would be: the question asks;
/// Tab and Space press Save; the switch goes once the save has landed, the viewer's file in the
/// state HOST gets, and comes back with the viewer open on it.
#[test]
fn tw4_through_the_terminal_save_then_the_switch() {
    let fx = fixture_with(0, &["0.bin"]);
    std::fs::write(
        Path::new(&fx.root).join("0.bin"),
        (0..=255u8).collect::<Vec<u8>>(),
    )
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    stand_in(tmp.path(), "printf 'sopen-xtools-state %s\\n' \"$line\"");
    let remote = remote_for(tmp.path(), &tmp.path().join("sopen"), &fx.root);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let ran = Rc::new(Ran::default());
    let now: fn() -> When = || Box::new(|| true);
    let mut events: Vec<(CEvent, Option<When>)> =
        (0..4).map(|_| (plain(KeyCode::Down), None)).collect();
    events.extend([
        (plain(KeyCode::Enter), None),
        (plain(KeyCode::F(8)), None),
        (plain(KeyCode::Enter), None),
        (plain(KeyCode::Char('Z')), None),
        (
            plain(KeyCode::F(12)),
            Some(says(&bytes, "0000(0000) 5A 01 02")),
        ),
        (plain(KeyCode::Tab), Some(says(&bytes, "[ Discard ]"))),
        (plain(KeyCode::Char(' ')), Some(now())),
        (ctrl_q(), Some(after(&ran, &fx, &bytes, 2))),
    ]);
    script(&tape, &fx, events);
    assert_eq!(run(&mut b, &fx, &remote, &ran), End::Closed);
    assert_eq!(*ran.said.borrow(), [None, None]);
    assert_eq!(first_byte(&fx), 0x5A, "saved before it went");
    let sent = switch_state::read(&lines(tmp.path(), "state-in")[0], "HOST").unwrap();
    let viewer = sent.viewer.expect("the viewer in the state");
    assert_eq!(viewer.file, format!("{}/0.bin", fx.root));
    let back = ran.then.borrow().clone().expect("the state once back");
    assert_eq!(
        back.viewer.map(|v| v.file),
        Some(format!("{}/0.bin", fx.root))
    );
}

/// TW-4's HOST half, as X2-host's note asked to check (10 §17's X2-switch row): an edit, F12 →
/// HOST's close prompt; Cancel keeps the window with the viewer on screen and usable — a second
/// byte typed; F12 again asks again; Save saves both and returns with the cue, the viewer in its
/// state — the one taken when the prompt came up (the browser's close drops the viewer once the
/// prompt is answered, before the close that hands the state back).
#[test]
fn tw4_on_host_cancel_keeps_the_viewer_and_save_returns() {
    let fx = fixture_with(0, &["0.bin"]);
    std::fs::write(
        Path::new(&fx.root).join("0.bin"),
        (0..=255u8).collect::<Vec<u8>>(),
    )
    .unwrap();
    let mut state = state_of(&fx);
    state.mode = Mode::Dual;
    state.panels[0].bar = Some("0.bin".into());
    state.panels[1].bar = None;
    state.viewer = None;
    let line = format!("{}\n", state.line());
    let sent = switch_host::read_state(&mut line.as_bytes()).unwrap();
    let out = Bytes::default();
    let cues = Cues::new(Box::new(out.clone()), Box::new(|| {}));
    let tmp = tempfile::tempdir().unwrap();
    let typed = |drv: &mut Driver, w: &mut switch_host::Switched, text: &str| {
        drv.frame(w, vec![egui::Event::Text(text.into())], Modifiers::NONE);
        drv.idle(w, 1);
    };
    let ran = switch_host::run_on(
        start(tmp.path(), &fx.root, None),
        &crate::gui::Hooks::default(),
        sent,
        Input::default(),
        cues,
        |_, mut w| {
            let mut drv = Driver::new(1200.0, 480.0);
            drv.as_shell();
            let settle = |drv: &mut Driver, w: &mut switch_host::Switched| {
                drv.until(w, 60, |w| w.ready());
                drv.idle(w, 2);
                drv.until(w, 60, |w| w.ready());
            };
            settle(&mut drv, &mut w);
            drv.key(&mut w, Key::Enter, Modifiers::NONE);
            settle(&mut drv, &mut w);
            drv.key(&mut w, Key::F8, Modifiers::NONE);
            drv.key(&mut w, Key::Enter, Modifiers::NONE);
            typed(&mut drv, &mut w, "Z");
            assert!(drv.text_starting_with("5A 01 02").is_some());
            drv.key(&mut w, Key::F12, Modifiers::NONE);
            drv.idle(&mut w, 2);
            assert!(
                drv.find_text("Unsaved changes").is_some(),
                "HOST's close prompt"
            );
            settle(&mut drv, &mut w);
            drv.click_text_on_top(&mut w, "[ Cancel ]");
            drv.idle(&mut w, 2);
            assert!(!drv.window_closed, "Cancel keeps the window");
            assert!(drv.find_text("Unsaved changes").is_none());
            assert!(
                drv.text_starting_with("5A 01 02").is_some(),
                "the viewer on screen"
            );
            typed(&mut drv, &mut w, "Y");
            assert!(drv.text_starting_with("5A 59 02").is_some(), "and usable");
            drv.key(&mut w, Key::F12, Modifiers::NONE);
            drv.idle(&mut w, 2);
            assert!(drv.find_text("Unsaved changes").is_some(), "asked again");
            settle(&mut drv, &mut w);
            drv.click_text_on_top(&mut w, "[ Save ]");
            for _ in 0..400 {
                drv.idle(&mut w, 1);
                if drv.window_closed {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(drv.window_closed, "Save returns once it has landed");
            Ok(())
        },
    );
    ran.unwrap();
    let saved = std::fs::read(Path::new(&fx.root).join("0.bin")).unwrap();
    assert_eq!(saved[..2], [0x5A, 0x59], "both bytes saved");
    let text = out.text();
    let json = text
        .trim_end()
        .strip_prefix("sopen-xtools-state ")
        .unwrap_or_else(|| panic!("the cue: {text}"));
    let back = switch_state::read(json, "HOST").unwrap();
    assert_eq!(back.panels[0].bar.as_deref(), Some("0.bin"));
    // The state taken when the prompt came up: the viewer in it, at its file, view and place.
    let viewer = back.viewer.expect("the viewer comes back");
    assert_eq!(viewer.file, format!("{}/0.bin", fx.root));
    assert_eq!(viewer.view, 1, "a binary's view 1 is Hex");
    assert_eq!(
        viewer.place,
        Some(2),
        "the cursor after the two bytes typed"
    );
}

/// TW-4's Discard through the terminal: Tab to `[ Discard ]` and Space — the viewer drops its
/// edits as its own Discard does (`Window::discard`: the journal it recorded removed), the switch
/// goes with the file as stored, and comes back with HOST's state, the viewer open on the stored
/// bytes; the file opened here again offers nothing to restore.
#[test]
fn tw4_through_the_terminal_discard_then_the_switch() {
    let (fx, mut drv, mut win) = hex_edited();
    let edits = Path::new(&fx.root).parent().unwrap().join("cache/.edits");
    let journals = |edits: &Path| -> Vec<PathBuf> {
        let all = walk(edits);
        let journal = |p: &&PathBuf| p.to_string_lossy().ends_with("CURRENT");
        all.iter().filter(journal).cloned().collect()
    };
    // The edit kept first (07 §3: its journal written on the schedule's tick).
    for _ in 0..400 {
        if !journals(&edits).is_empty() {
            break;
        }
        drv.idle(&mut win, 1);
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(journals(&edits).len(), 1, "the edit is kept, before");
    let tmp = tempfile::tempdir().unwrap();
    // HOST hands its state back once Ctrl+C ends its input: the screen meanwhile can be read.
    let body = "cat > /dev/null; printf 'sopen-xtools-state %s\\n' \"$line\"";
    stand_in(tmp.path(), body);
    let remote = remote_for(tmp.path(), &tmp.path().join("sopen"), &fx.root);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let ran = Rc::new(Ran::default());
    let now: fn() -> When = || Box::new(|| true);
    let away: Rc<RefCell<Option<String>>> = Rc::default();
    let meanwhile: When = {
        let (bytes, dir, away) = (bytes.clone(), tmp.path().to_path_buf(), away.clone());
        Box::new(move || {
            let on = dir.join("state-in").exists() && shows(&bytes, "xtools is in a window");
            if on {
                *away.borrow_mut() = Some(bytes.last_screen(100, 30).text());
            }
            on
        })
    };
    let events = vec![
        (plain(KeyCode::F(12)), None),
        (plain(KeyCode::Tab), Some(says(&bytes, "[ Discard ]"))),
        (plain(KeyCode::Tab), Some(now())),
        (plain(KeyCode::Tab), Some(now())),
        (plain(KeyCode::Char(' ')), Some(now())),
        (ctrl_c(), Some(meanwhile)),
        (ctrl_q(), Some(after(&ran, &fx, &bytes, 2))),
    ];
    script(&tape, &fx, events);
    assert_eq!(run(&mut b, &fx, &remote, &ran), End::Closed);
    assert_eq!(*ran.said.borrow(), [None, None], "it went, and came back");
    // Meanwhile the dimmed screen is the browser as it went: no question, no dropped edit.
    let away = away.borrow().clone().expect("the screen meanwhile");
    assert!(
        !away.contains("[ Discard ]"),
        "the question answered:\n{away}"
    );
    assert!(!away.contains("5A 01 02"), "the edit dropped:\n{away}");
    assert_eq!(first_byte(&fx), 0, "the file as stored");
    let sent = switch_state::read(&lines(tmp.path(), "state-in")[0], "HOST").unwrap();
    let viewer = sent.viewer.expect("HOST opens the viewer's file");
    assert_eq!(viewer.file, format!("{}/0.bin", fx.root));
    let screen = ran.screen.borrow().clone().unwrap();
    assert!(
        screen.contains("0000(0000) 00 01 02"),
        "the stored bytes:\n{screen}"
    );
    assert!(screen.contains("0 bytes changed"), "{screen}");
    assert!(
        journals(&edits).is_empty(),
        "the journal removed: {:?}",
        walk(&edits)
    );
    // The file opened here again offers nothing to restore.
    let cache = Path::new(&fx.root).parent().unwrap().to_path_buf();
    let again = Rc::new(RefCell::new(Browser::new(start(&cache, &fx.root, None))));
    let mut drv = Driver::new(800.0, 480.0);
    let mut win = Shared(again.clone());
    drv.until(&mut win, 20, |w| w.ready());
    for _ in 0..4 {
        drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    }
    drv.until(&mut win, 20, |w| w.ready());
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    drv.until(&mut win, 20, |w| w.ready());
    drv.idle(&mut win, 5);
    assert!(
        drv.text_starting_with("0000(0000)").is_some(),
        "the viewer is open"
    );
    assert!(!has(&drv, "unsaved edits from"), "nothing offered");
    assert!(drv.find_text("[ Restore ]").is_none());
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.push(p.clone());
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// A switch held by a copy is decided anew once the copy has ended: unsaved edits made meanwhile
/// ask first — nothing crosses unasked.
#[test]
fn tw4_after_a_copy_unsaved_edits_still_ask() {
    let fx = fixture_with(0, &["0.bin"]);
    std::fs::write(
        Path::new(&fx.root).join("0.bin"),
        (0..=255u8).collect::<Vec<u8>>(),
    )
    .unwrap();
    dual(&fx);
    keys(
        &fx,
        &[
            Key::Tab,
            Key::ArrowDown,
            Key::ArrowDown,
            Key::Enter,
            Key::Tab,
            Key::ArrowDown,
        ],
    );
    let running = Arc::new(AtomicBool::new(false));
    let (release, held) = std::sync::mpsc::channel();
    fx.b.borrow_mut()
        .set_ops_run(held_copy(running.clone(), held));
    let mut drv = Driver::new(800.0, 480.0);
    let mut win = Shared(fx.b.clone());
    drv.until(&mut win, 20, |w| w.ready());
    drv.key(&mut win, Key::F5, Modifiers::NONE);
    drv.until(&mut win, 20, |w| w.ready());
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    while !running.load(Ordering::SeqCst) {
        drv.idle(&mut win, 1);
    }
    // The hex viewer on `0.bin` (alpha, beta, the Korean folder, then it), a byte typed over.
    for _ in 0..3 {
        drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    }
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    for _ in 0..400 {
        drv.idle(&mut win, 1);
        if drv.text_starting_with("0000(0000)").is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drv.idle(&mut win, 2);
    drv.key(&mut win, Key::F8, Modifiers::NONE);
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    drv.frame(
        &mut win,
        vec![egui::Event::Text("Z".into())],
        Modifiers::NONE,
    );
    drv.idle(&mut win, 1);
    assert!(
        drv.text_starting_with("5A 01 02").is_some(),
        "the byte is typed over"
    );
    assert_eq!(
        fx.b.borrow_mut().before_switch("box"),
        Before::Held,
        "the copy runs"
    );
    drv.idle(&mut win, 1);
    assert!(has(&drv, "waiting for the file operation…"));
    release.send(()).unwrap();
    let mut due = false;
    for _ in 0..400 {
        drv.idle(&mut win, 1);
        if fx.b.borrow_mut().switch_due() {
            due = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(due, "asked for again once the copy has ended");
    assert_eq!(
        fx.b.borrow_mut().before_switch("box"),
        Before::Held,
        "decided anew: the edit asks"
    );
    drv.idle(&mut win, 2);
    assert!(has(&drv, "switching to box?"), "the question");
}

/// The keys that open `0.bin` (the fixture's first file) in the hex viewer through the terminal
/// and type a byte over: `Z` at offset 0, editing (08 D35).
fn edit_through_the_terminal() -> Vec<(CEvent, Option<When>)> {
    let mut events: Vec<(CEvent, Option<When>)> =
        (0..4).map(|_| (plain(KeyCode::Down), None)).collect();
    events.extend([
        (plain(KeyCode::Enter), None),
        (plain(KeyCode::F(8)), None),
        (plain(KeyCode::Enter), None),
        (plain(KeyCode::Char('Z')), None),
    ]);
    events
}

/// While the switch's question is up its keys are its own, the view keys' too (08 §4): in an
/// editing hex view Esc is the question's Cancel — not the view's end of editing — and in a
/// viewing one a view digit switches nothing under it (no close prompt for the view's switch).
#[test]
fn the_view_keys_are_the_questions_while_it_is_up() {
    for viewing in [false, true] {
        let fx = fixture_with(0, &["0.bin"]);
        let file = Path::new(&fx.root).join("0.bin");
        std::fs::write(&file, (0..=255u8).collect::<Vec<u8>>()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        stand_in(tmp.path(), "exit 0");
        let remote = remote_for(tmp.path(), &tmp.path().join("sopen"), &fx.root);
        let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
        let ran = Rc::new(Ran::default());
        let now: fn() -> When = || Box::new(|| true);
        let mut events = edit_through_the_terminal();
        let mode = if viewing {
            events.push((
                plain(KeyCode::Esc),
                Some(says(&bytes, "0000(0000) 5A 01 02")),
            ));
            "viewing — Enter edits"
        } else {
            "editing — Esc ends"
        };
        events.push((plain(KeyCode::F(12)), Some(says(&bytes, mode))));
        if viewing {
            events.push((plain(KeyCode::Char('3')), Some(says(&bytes, "[ Discard ]"))));
            events.push((plain(KeyCode::Esc), Some(now())));
        } else {
            events.push((plain(KeyCode::Esc), Some(says(&bytes, "[ Discard ]"))));
        }
        // Once the Esc's pass is printed.
        let printed = {
            let bytes = bytes.clone();
            Box::new(move || !shows(&bytes, "[ Discard ]")) as When
        };
        events.push((CEvent::FocusGained, Some(printed)));
        script(&tape, &fx, events);
        tape.0.borrow_mut().stop = Some(libc::SIGTERM);
        assert_eq!(run(&mut b, &fx, &remote, &ran), End::Signal(libc::SIGTERM));
        assert!(lines(tmp.path(), "state-in").is_empty(), "never switched");
        let screen = bytes.last_screen(100, 30).text();
        assert!(
            !screen.contains("before closing?"),
            "no view switch's prompt:\n{screen}"
        );
        assert!(screen.contains(mode), "the view as it was:\n{screen}");
        assert!(
            screen.contains("0000(0000) 5A 01 02"),
            "the edit kept:\n{screen}"
        );
    }
}

/// HOST's half ends without its state: the terminal stays with the browser from before and lists
/// again — a file HOST's window made in the folder meanwhile is listed.
#[test]
fn without_a_cue_the_terminal_lists_again() {
    let fx = fixture_with(0, &[]);
    dual(&fx);
    let tmp = tempfile::tempdir().unwrap();
    let made = Path::new(&fx.root).join("made");
    let body = format!(": > {}; exit 0", forward::quote(made.to_str().unwrap()));
    stand_in(tmp.path(), &body);
    let remote = remote_for(tmp.path(), &tmp.path().join("sopen"), &fx.root);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let ran = Rc::new(Ran::default());
    let events = vec![
        (plain(KeyCode::F(12)), None),
        (ctrl_q(), Some(after(&ran, &fx, &bytes, 1))),
    ];
    script(&tape, &fx, events);
    assert_eq!(run(&mut b, &fx, &remote, &ran), End::Closed);
    assert_eq!(
        *ran.said.borrow(),
        [Some(
            "sopen on box ended without handing the screen back".into()
        )]
    );
    let screen = ran.screen.borrow().clone().unwrap();
    assert!(screen.contains("║made "), "listed again:\n{screen}");
    assert!(remote.made.borrow().is_empty(), "the browser from before");
}
