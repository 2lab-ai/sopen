//! WV-E2's tests (09 §7, §10 WV-A1–A3, §11 "unit level, no engine"): the way an address opens,
//! the window's cues, screen, keys and navigation, on the test kit with the recording
//! [`FakeEngine`].

use super::*;
use crate::gui::chrome::PALETTE;
use crate::gui::testkit::{command, Driver};
use crate::gui::web::fake::{Did, FakeEngine};
use crate::gui::web::{use_engine, Event, OFF};
use eframe::egui::{Event as Input_, Key, Modifiers, Pos2};
use std::cell::Cell;
use std::sync::Mutex;

/// An address with a password, a query and a fragment: none of them may be shown.
const SECRET: &str = "http://me:s3cret@127.0.0.1:8000/x?token=abc#frag9";
/// [`SECRET`] as sopen shows it.
const SECRET_SHOWN: &str = "http://me:***@127.0.0.1:8000/x?…#…";

fn parsed(text: &str) -> Url {
    match crate::url::parse(text) {
        Some(Ok(url)) => url,
        other => panic!("{text}: {other:?}"),
    }
}

/// A window on the test kit with everything it says recorded: its cues, the links it sent to
/// the system browser (the stand-in answers `answer`), its end.
struct Rig {
    win: AddressWindow,
    drv: Driver,
    cues: Rc<RefCell<Vec<Cue>>>,
    opened: Arc<Mutex<Vec<String>>>,
    report: Rc<RefCell<Report>>,
    input: Input,
}

fn rig_with(address: &str, scripted: bool, answer: Result<(), String>) -> Rig {
    let opened: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = opened.clone();
    let opener: Opener = Arc::new(move |url: &Url| {
        record.lock().unwrap().push(url.as_str().to_string());
        answer.clone()
    });
    rig_opening(address, scripted, opener, opened)
}

/// A window whose clicked links out go to `opener` (`opened`: what it recorded, if it does).
fn rig_opening(
    address: &str,
    scripted: bool,
    opener: Opener,
    opened: Arc<Mutex<Vec<String>>>,
) -> Rig {
    let cues: Rc<RefCell<Vec<Cue>>> = Rc::default();
    let said = cues.clone();
    let report: Rc<RefCell<Report>> = Rc::default();
    let input = Input::default();
    let win = AddressWindow::new(
        parsed(address),
        Box::new(move |cue| said.borrow_mut().push(cue)),
        opener,
        Some(input.clone()),
        scripted,
        report.clone(),
    );
    Rig {
        win,
        // Wide enough for the longest line whole: a narrower one cuts it with "…".
        drv: Driver::new(1600.0, 480.0),
        cues,
        opened,
        report,
        input,
    }
}

fn rig(address: &str) -> Rig {
    rig_with(address, false, Ok(()))
}

impl Rig {
    fn idle(&mut self, n: usize) {
        self.drv.idle(&mut self.win, n);
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.drv.key(&mut self.win, key, modifiers);
    }

    /// One pass with `events`, as a backend sends several keys in one frame.
    fn frame(&mut self, events: Vec<Input_>, modifiers: Modifiers) {
        self.drv.frame(&mut self.win, events, modifiers);
    }

    fn cues(&self) -> Vec<Cue> {
        self.cues.borrow().clone()
    }

    fn shows(&self, text: &str) -> bool {
        self.drv.find_text(text).is_some()
    }

    /// Every text the last pass drew.
    fn texts(&self) -> Vec<String> {
        self.drv.texts().into_iter().map(|(_, _, t)| t).collect()
    }
}

fn press(key: Key, pressed: bool, modifiers: Modifiers) -> Input_ {
    Input_::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    }
}

fn no_secrets(texts: &[String]) {
    for text in texts {
        for secret in ["s3cret", "token=abc", "frag9"] {
            assert!(!text.contains(secret), "{secret} shown: {text}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The way (09 §7, §4, §8).

/// HOST's side decides the same on every route (here, HOST's half of a tunnel, the address
/// passed as written: `way` takes no route; the routes themselves are `tests/cli.rs`'s,
/// `without_a_web_engine_an_address_opens_in_the_system_browser_after_a_line`): `--browser` is the system browser as before, whatever the display or the
/// engine, and so is no display (§4, §8: only a terminal) — the engine not even asked for in
/// either; else the window, or §4's fallback with the engine's reason.
#[test]
fn wv_e2_the_way_an_address_opens() {
    let asked = Cell::new(0);
    let here = || {
        asked.set(asked.get() + 1);
        Ok(())
    };
    let none = || {
        asked.set(asked.get() + 1);
        Err(Unavailable::NoEngine(OFF.into()))
    };
    for display in [true, false] {
        assert_eq!(way(true, display, here), Way::Browser);
        assert_eq!(way(true, display, none), Way::Browser);
    }
    assert_eq!(way(false, false, here), Way::Browser);
    assert_eq!(way(false, false, none), Way::Browser);
    assert_eq!(asked.get(), 0, "the engine is not asked for");
    assert_eq!(way(false, true, here), Way::Window);
    assert_eq!(
        way(false, true, none),
        Way::Fallback(Unavailable::NoEngine(OFF.into()))
    );
    assert_eq!(asked.take(), 2);
    // The lines it says, the address shown without its secrets.
    let off = Unavailable::NoEngine(OFF.into());
    assert_eq!(
        fallback_line(&off, SECRET_SHOWN),
        format!(
            "no web engine here (turned off (SOPEN_WEB=off)); opening {SECRET_SHOWN} in the \
             system browser"
        )
    );
    assert_eq!(
        opened_line(SECRET_SHOWN),
        format!("opened {SECRET_SHOWN} in xtools")
    );
    assert_eq!(title(SECRET_SHOWN), format!("xtools — {SECRET_SHOWN}"));
}

/// The engine this process would show an address with: any engine, a backend that does not
/// contain file views included (§4: the address window is not affected); none, its reason.
#[test]
fn wv_e2_any_engine_shows_an_address() {
    let fake = FakeEngine::native();
    fake.install().set_uncontained(false, Some("DNS"));
    assert_eq!(engine_here(), Ok(()));
    use_engine(Err(Unavailable::NoEngine(OFF.into())));
    assert_eq!(engine_here(), Err(Unavailable::NoEngine(OFF.into())));
}

/// The view's origin is the address up to its path, as written (a user and password kept: the
/// engine gets the address as the browser would); the rest is loaded on it.
#[test]
fn wv_e2_the_origin_and_the_rest_as_written() {
    for (text, origin, rest) in [
        (
            "http://127.0.0.1:8000/a/b?x#y",
            "http://127.0.0.1:8000",
            "/a/b?x#y",
        ),
        ("HTTP://me:pw@box", "HTTP://me:pw@box", ""),
        ("http://box?q=/x", "http://box", "?q=/x"),
        ("http://[::1]:9/x", "http://[::1]:9", "/x"),
        ("https://box#a/b", "https://box", "#a/b"),
    ] {
        assert_eq!(split_origin(text), (origin, rest), "{text}");
    }
}

// ---------------------------------------------------------------------------------------------
// The cues (09 §7 route 2, R5, R6, decision 13).

/// The window cue only once `Web::new` made the view, then the open cue once its load started —
/// each once; the view opens with scripts on and loads the address on its origin.
#[test]
fn wv_e2_the_window_cue_then_the_open_cue_once_the_view_is_made() {
    let fake = FakeEngine::native();
    fake.install();
    let mut r = rig("http://127.0.0.1:8000/a?t=1");
    assert_eq!(r.cues(), [], "nothing before the first pass");
    r.idle(1);
    assert_eq!(r.cues(), [Cue::Window, Cue::Open]);
    assert_eq!(
        fake.calls(0)[..2],
        [
            Did::Open {
                page: false,
                preview: false,
                scripts: true,
            },
            Did::Load("http://127.0.0.1:8000/a?t=1".into()),
        ]
    );
    r.idle(3);
    assert_eq!(r.cues(), [Cue::Window, Cue::Open], "once each");
    assert_eq!(fake.views(), 1, "one view");
}

/// §4: an engine that refuses the view, or none at all, says no cue; the window asks to close
/// and the run falls back with the reason. Nothing to say at its end.
#[test]
fn wv_e2_no_view_no_cue() {
    let fake = FakeEngine::native();
    fake.install().refuse_open(Some("gtk_init_check failed"));
    let mut r = rig("http://127.0.0.1:8000/");
    r.drv.as_shell();
    r.idle(2);
    assert_eq!(r.cues(), []);
    assert!(r.drv.window_closed, "it closes");
    assert_eq!(
        r.report.borrow().failed,
        Some(Unavailable::NoEngine("gtk_init_check failed".into()))
    );
    assert_eq!(r.report.borrow().ended(), None);
    assert!(r.win.ready(), "nothing to wait for");
    use_engine(Err(Unavailable::NoEngine(OFF.into())));
    let mut r = rig("http://127.0.0.1:8000/");
    r.drv.as_shell();
    r.idle(2);
    assert_eq!(r.cues(), []);
    assert!(r.drv.window_closed);
    assert_eq!(
        r.report.borrow().failed,
        Some(Unavailable::NoEngine(OFF.into()))
    );
}

/// Closing the window says the closed cue (route 2's HOST half: the other side closes the
/// tunnel) — only for a view that was made, closed by the owner: never once the input ended
/// (the other side is gone and the window closed for it), which a scripted run does not heed.
#[test]
fn wv_e2_the_closed_cue_only_after_the_owners_close() {
    let fake = FakeEngine::native();
    fake.install();
    let mut r = rig("http://127.0.0.1:8000/");
    r.drv.as_shell();
    r.idle(1);
    assert_eq!(
        r.report.borrow().ended(),
        Some(Cue::Closed),
        "if it closed now"
    );
    r.key(Key::Escape, Modifiers::NONE);
    assert!(
        r.drv.window_closed,
        "Esc closes it (sopen has the keyboard)"
    );
    assert_eq!(r.report.borrow().ended(), Some(Cue::Closed));
    // The input's end: the window closes, no cue.
    let mut r = rig("http://127.0.0.1:8000/");
    r.drv.as_shell();
    r.idle(1);
    r.input.end();
    r.idle(2);
    assert!(r.drv.window_closed, "the input's end closes it");
    assert_eq!(r.report.borrow().ended(), None);
    assert_eq!(r.cues(), [Cue::Window, Cue::Open]);
    // A scripted run ends by its hook.
    let mut r = rig_with("http://127.0.0.1:8000/", true, Ok(()));
    r.drv.as_shell();
    r.idle(1);
    r.input.end();
    r.idle(3);
    assert!(
        !r.drv.window_closed,
        "a scripted run ignores its input's end"
    );
}

/// What each cue writes: HOST's half of a tunnel the fixed lines, one each; any other route the
/// opened line once the window is up, and nothing else. A closed output is no panic.
#[test]
fn wv_e2_what_the_cues_write() {
    let mut out = Vec::new();
    for cue in [Cue::Window, Cue::Open, Cue::Closed] {
        speak(&mut out, true, SECRET_SHOWN, cue);
    }
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "sopen-tunnel-window\nsopen-tunnel-open\nsopen-tunnel-closed\n"
    );
    let mut out = Vec::new();
    for cue in [Cue::Window, Cue::Open, Cue::Closed] {
        speak(&mut out, false, SECRET_SHOWN, cue);
    }
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("opened {SECRET_SHOWN} in xtools\n")
    );
    struct Gone;
    impl std::io::Write for Gone {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }
    for hold in [true, false] {
        speak(&mut Gone, hold, SECRET_SHOWN, Cue::Window);
    }
}

// ---------------------------------------------------------------------------------------------
// The screen (09 §7 "The address window's screen").

/// The title row is the program and the address; the bar the document on screen, `scripts ON`
/// in the warn colour and loading; the F-key row `1Help 2JS off 3Exit` — all in cells, and no
/// address anywhere with its password, query or fragment.
#[test]
fn wv_e2_the_screen_in_cells_never_a_secret() {
    let fake = FakeEngine::native();
    fake.install();
    let mut r = rig(SECRET);
    r.idle(2);
    assert!(r.shows("xtools"), "the program's block");
    let shown = r.texts().iter().filter(|t| *t == SECRET_SHOWN).count();
    assert_eq!(shown, 2, "the title row and the bar: {:?}", r.texts());
    assert!(r.shows("scripts ON"));
    assert_eq!(r.drv.colors_of("scripts ON"), Some(vec![PALETTE.warn]));
    for label in ["Help", "JS off", "Exit"] {
        assert!(r.shows(label), "{label}: {:?}", r.texts());
    }
    assert!(!r.shows("JS on"));
    assert!(r.shows(TITLE), "the page's frame");
    no_secrets(&r.texts());
    fake.events(0).send(Event::Loading);
    r.idle(1);
    assert!(r.shows("scripts ON · loading…"), "{:?}", r.texts());
    // Another page of the origin on screen: the bar follows it, shown the same way.
    fake.events(0).send(Event::Document(
        "http://me:s3cret@127.0.0.1:8000/next?token=abc".into(),
    ));
    fake.events(0).send(Event::Finished);
    r.idle(1);
    assert!(
        r.shows("http://me:***@127.0.0.1:8000/next?…"),
        "{:?}",
        r.texts()
    );
    assert!(r.shows("scripts ON"));
    no_secrets(&r.texts());
    assert!(r.win.ready(), "the main frame finished");
}

/// §2.4: the page's frame is single-lined while sopen has the keyboard, double-lined (the active
/// line colour) while the page has it.
#[test]
fn wv_e2_the_frame_says_who_has_the_keyboard() {
    let fake = FakeEngine::native();
    fake.install();
    let mut r = rig("http://127.0.0.1:8000/");
    r.idle(2);
    assert_eq!(r.drv.colors_of(TITLE), Some(vec![PALETTE.frame_idle.color]));
    fake.set_page_keyboard(0, true);
    r.idle(1);
    assert_eq!(
        r.drv.colors_of(TITLE),
        Some(vec![PALETTE.frame_active.color])
    );
}

/// The failure lines (09 §7 "Failures"), told apart by the engine's error: a load that fails
/// says so and how to try again, and goes once the next load starts; an untrusted certificate,
/// a login and a client certificate name `--browser`; the web process's end says Cmd/Ctrl+R.
#[test]
fn wv_e2_the_failure_lines() {
    for (why, kind) in [
        (
            "Could not connect to the server. (NSURLErrorDomain -1004)",
            Failure::Load,
        ),
        (
            "The certificate for this server is invalid. (NSURLErrorDomain -1202)",
            Failure::Untrusted,
        ),
        ("x (NSURLErrorDomain -1201)", Failure::Untrusted),
        ("x (NSURLErrorDomain -1203)", Failure::Untrusted),
        ("x (NSURLErrorDomain -1204)", Failure::Untrusted),
        ("x (NSURLErrorDomain -1205)", Failure::ClientCertificate),
        ("x (NSURLErrorDomain -1206)", Failure::ClientCertificate),
        ("x (NSURLErrorDomain -1012)", Failure::Login),
        ("x (NSURLErrorDomain -1013)", Failure::Login),
        ("the page answered 401", Failure::Login),
        ("the page answered 407", Failure::Login),
        ("the page answered 404", Failure::Load),
        ("x (NSURLErrorDomain -12020)", Failure::Load),
        ("x (WebKitErrorDomain -1202)", Failure::Load),
        ("cannot show application/zip", Failure::Load),
    ] {
        assert_eq!(failure(why), kind, "{why}");
    }
    let fake = FakeEngine::native();
    fake.install();
    let mut r = rig(SECRET);
    r.idle(1);
    let events = fake.events(0);
    let refused = "Could not connect to the server. (NSURLErrorDomain -1004)";
    events.send(Event::Failed(refused.into()));
    r.idle(1);
    assert!(
        r.shows(&format!(
            "cannot load {SECRET_SHOWN}: {refused} — Cmd/Ctrl+R tries again"
        )),
        "{:?}",
        r.texts()
    );
    assert!(r.win.ready(), "the main frame failed");
    events.send(Event::Loading);
    r.idle(1);
    assert!(!r.texts().iter().any(|t| t.starts_with("cannot load")));
    for (why, line) in [
        (
            "x (NSURLErrorDomain -1202)",
            "the certificate of 127.0.0.1 is not trusted — --browser opens it in the system \
             browser"
                .to_string(),
        ),
        (
            "the page answered 401",
            format!(
                "{SECRET_SHOWN} asks for a login, which this window does not give — --browser \
                 opens it in the system browser"
            ),
        ),
        (
            "x (NSURLErrorDomain -1206)",
            format!(
                "{SECRET_SHOWN} asks for a client certificate, which this window does not give \
                 — --browser opens it in the system browser"
            ),
        ),
    ] {
        events.send(Event::Failed(why.into()));
        r.idle(1);
        assert!(r.shows(&line), "{why}: {:?}", r.texts());
    }
    events.send(Event::ProcessEnded);
    r.idle(1);
    assert!(r.shows("the page's process ended — Cmd/Ctrl+R reloads"));
    // The engine's own words can carry an address (macOS: a URL it could not make): shown
    // without its secrets too, the reason's code kept.
    events.send(Event::Failed(format!("not an address: {SECRET}")));
    r.idle(1);
    assert!(
        r.shows(&format!(
            "cannot load {SECRET_SHOWN}: not an address: {SECRET_SHOWN} — Cmd/Ctrl+R tries again"
        )),
        "{:?}",
        r.texts()
    );
    no_secrets(&r.texts());
}

// ---------------------------------------------------------------------------------------------
// Navigation (09 §7 "Navigation", WV-A3).

/// A clicked link out of the origin goes to the system browser, whole — only an http or https
/// one; any other scheme, or an address sopen refuses, is said and not opened. Any other
/// navigation out is refused with a line. Every target is shown without its secrets.
#[test]
fn wv_e2_a_clicked_link_out_goes_to_the_system_browser() {
    let fake = FakeEngine::native();
    fake.install();
    let mut r = rig("http://127.0.0.1:8000/");
    r.idle(1);
    let events = fake.events(0);
    let link = "https://example.com/p?token=abc";
    events.send(Event::Refused {
        target: link.into(),
        clicked: true,
    });
    let opened = r.opened.clone();
    r.drv
        .until(&mut r.win, 10, |_| opened.lock().unwrap().len() == 1);
    assert_eq!(*r.opened.lock().unwrap(), [link]);
    let line = "https://example.com/p?…: opened in the system browser";
    let mut tries = 0;
    while !r.shows(line) {
        tries += 1;
        assert!(tries < 500, "{:?}", r.texts());
        r.idle(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    events.send(Event::Refused {
        target: "http://elsewhere.example/x?token=abc".into(),
        clicked: false,
    });
    r.idle(1);
    assert!(
        r.shows(
            "http://elsewhere.example/x?…: a navigation away from http://127.0.0.1:8000, refused"
        ),
        "{:?}",
        r.texts()
    );
    for (target, line) in [
        (
            "file:///etc/passwd",
            "file:///etc/passwd: not opened — only an http:// or https:// link goes to the \
             system browser"
                .to_string(),
        ),
        (
            "mailto:me@example.com",
            "mailto:me@example.com: not opened — only an http:// or https:// link goes to the \
             system browser"
                .to_string(),
        ),
        (
            "http://127.1/?token=abc",
            "not opened: '127.1' is read by browsers as an IPv4 address (write the host plainly \
             (127.0.0.1)) in 'http://127.1/?…'"
                .to_string(),
        ),
    ] {
        events.send(Event::Refused {
            target: target.into(),
            clicked: true,
        });
        r.idle(1);
        assert!(r.shows(&line), "{target}: {:?}", r.texts());
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(r.opened.lock().unwrap().len(), 1, "nothing else opened");
    no_secrets(&r.texts());
}

/// The system browser refusing the link is said as it said it.
#[test]
fn wv_e2_a_link_the_system_browser_refuses_is_said() {
    let fake = FakeEngine::native();
    fake.install();
    let why = "cannot open https://example.com/ in the browser: open exited with 1".to_string();
    let mut r = rig_with("http://127.0.0.1:8000/", false, Err(why.clone()));
    r.idle(1);
    fake.events(0).send(Event::Refused {
        target: "https://example.com/".into(),
        clicked: true,
    });
    let mut tries = 0;
    while !r.shows(&why) {
        tries += 1;
        assert!(tries < 500, "{:?}", r.texts());
        r.idle(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------------------------------------
// Keys (09 §2.4, §7).

/// Esc twice (a page drawn as a texture, Linux's): the first takes the keyboard back from the
/// page, the window stays; the second closes it. Both in one frame do both.
#[test]
fn wv_e2_esc_twice() {
    let fake = FakeEngine::texture();
    fake.install();
    let mut r = rig("http://127.0.0.1:8000/");
    r.drv.as_shell();
    r.idle(2);
    let page = Pos2::new(400.0, 240.0);
    r.drv.click(&mut r.win, page);
    assert_eq!(
        r.drv.colors_of(TITLE),
        Some(vec![PALETTE.frame_active.color]),
        "the page has the keyboard"
    );
    r.key(Key::Escape, Modifiers::NONE);
    assert!(!r.drv.window_closed, "the first Esc only takes it back");
    assert_eq!(r.drv.colors_of(TITLE), Some(vec![PALETTE.frame_idle.color]));
    r.key(Key::Escape, Modifiers::NONE);
    assert!(r.drv.window_closed, "the second closes it");
    // Two in one frame.
    let mut r = rig("http://127.0.0.1:8000/");
    r.drv.as_shell();
    r.idle(2);
    r.drv.click(&mut r.win, page);
    let none = Modifiers::NONE;
    r.frame(
        vec![
            press(Key::Escape, true, none),
            press(Key::Escape, false, none),
            press(Key::Escape, true, none),
        ],
        none,
    );
    r.frame(vec![press(Key::Escape, false, none)], none);
    assert!(r.drv.window_closed, "both Escs of the frame");
}

/// F2 turns scripts off and on, reloading (before anything was on screen, loading the address
/// again: there is nothing to reload yet); the row and the bar follow. Twice in one frame is
/// twice. F3 closes the window, F1 shows the keys.
#[test]
fn wv_e2_f2_toggles_scripts_and_f3_closes() {
    let fake = FakeEngine::native();
    fake.install();
    let mut r = rig("http://127.0.0.1:8000/a");
    r.idle(2);
    fake.take(0);
    let url = "http://127.0.0.1:8000/a".to_string();
    r.key(Key::F2, Modifiers::NONE);
    assert_eq!(
        fake.take(0),
        [Did::Scripts(false), Did::Reload, Did::Load(url.clone())]
    );
    assert!(r.shows("JS on") && !r.shows("JS off"), "{:?}", r.texts());
    assert!(r.shows("scripts off"));
    assert_eq!(r.drv.colors_of("scripts off"), Some(vec![PALETTE.dim]));
    // On screen now: a reload is enough.
    fake.events(0)
        .send(Event::Document("http://127.0.0.1:8000/a".into()));
    r.idle(1);
    let none = Modifiers::NONE;
    r.frame(
        vec![
            press(Key::F2, true, none),
            press(Key::F2, false, none),
            press(Key::F2, true, none),
        ],
        none,
    );
    r.frame(vec![press(Key::F2, false, none)], none);
    assert_eq!(
        fake.take(0),
        [
            Did::Scripts(true),
            Did::Reload,
            Did::Scripts(false),
            Did::Reload
        ]
    );
    assert!(r.shows("scripts off"));
    r.key(Key::F1, Modifiers::NONE);
    assert!(r.shows("Keys"), "F1's help: {:?}", r.texts());
    r.key(Key::Escape, Modifiers::NONE);
    r.drv.as_shell();
    r.key(Key::F3, Modifiers::NONE);
    assert!(r.drv.window_closed, "F3 closes it");
}

/// Cmd/Ctrl+R reloads — before anything was on screen (the dev server not up yet), it loads the
/// address again; twice in one frame is twice.
#[test]
fn wv_e2_cmd_r_reloads_or_tries_again() {
    let fake = FakeEngine::native();
    fake.install();
    let mut r = rig("http://127.0.0.1:8000/");
    r.idle(1);
    fake.events(0).send(Event::Failed(
        "Could not connect to the server. (NSURLErrorDomain -1004)".into(),
    ));
    r.idle(1);
    fake.take(0);
    r.key(Key::R, command(false));
    assert_eq!(fake.take(0), [Did::Load("http://127.0.0.1:8000/".into())]);
    fake.events(0)
        .send(Event::Document("http://127.0.0.1:8000/".into()));
    r.idle(1);
    let cmd = command(false);
    r.frame(
        vec![
            press(Key::R, true, cmd),
            press(Key::R, false, cmd),
            press(Key::R, true, cmd),
        ],
        cmd,
    );
    r.frame(vec![press(Key::R, false, cmd)], cmd);
    assert_eq!(fake.take(0), [Did::Reload, Did::Reload]);
}

// ---------------------------------------------------------------------------------------------
// Review round 1.

/// The input ends while the window is starting (the remote sopen gone before the first pass):
/// no view is made — nothing loads toward a port the tunnel no longer forwards — no cue is
/// said, and the window closes.
#[test]
fn wv_e2_the_inputs_end_before_the_view_makes_none() {
    let fake = FakeEngine::native();
    fake.install();
    let mut r = rig("http://127.0.0.1:8000/");
    r.drv.as_shell();
    r.input.end();
    r.idle(2);
    assert_eq!(r.cues(), []);
    assert_eq!(fake.views(), 0, "no view");
    assert!(r.drv.window_closed);
    assert_eq!(r.report.borrow().ended(), None);
}

/// The input watched as `run` watches stdin — a real pipe, read to its end on a thread of its
/// own: while it is open the window stays; once its writer is gone the window closes, no closed
/// cue. (A subprocess would need a real window: a scripted run ignores the input's end.)
#[test]
fn wv_e2_the_watched_inputs_end_closes_the_window() {
    use std::io::Write as _;
    let fake = FakeEngine::native();
    fake.install();
    let mut r = rig("http://127.0.0.1:8000/");
    r.drv.as_shell();
    r.idle(1);
    let (reader, mut writer) = std::io::pipe().unwrap();
    r.input.watch(reader).unwrap();
    writer.write_all(b"held\n").unwrap();
    r.idle(3);
    std::thread::sleep(std::time::Duration::from_millis(50));
    r.idle(1);
    assert!(!r.drv.window_closed, "its input is still open");
    drop(writer);
    let mut tries = 0;
    while !r.drv.window_closed {
        tries += 1;
        assert!(tries < 1000, "the input's end did not close it");
        r.idle(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(r.cues(), [Cue::Window, Cue::Open]);
    assert_eq!(r.report.borrow().ended(), None);
}

/// What the window's end writes, by how it went: the closed cue on HOST's half of a tunnel after
/// the owner's close of a view that was made; nothing on another route, after the input's end,
/// or without a view.
#[test]
fn wv_e2_the_windows_end_writes_as_it_went() {
    let off = Some(Unavailable::NoEngine(OFF.into()));
    for (made, by_input, failed, hold, want) in [
        (true, false, None, true, "sopen-tunnel-closed\n"),
        (true, false, None, false, ""),
        (true, true, None, true, ""),
        (false, false, off.clone(), true, ""),
        (false, false, None, true, ""),
    ] {
        let report = Report {
            made,
            failed,
            by_input,
        };
        let mut out = Vec::new();
        report.finish(&mut out, hold, SECRET_SHOWN);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            want,
            "{made} {by_input} {hold}"
        );
    }
}

/// The cues against the engine's calls: the window cue once the view is open and before its
/// load, the open cue once the load was asked for.
#[test]
fn wv_e2_the_cues_against_the_engines_calls() {
    let fake = FakeEngine::native();
    fake.install();
    // Each cue with the engine's calls so far.
    type Seen = Vec<(Cue, Vec<Did>)>;
    let seen: Rc<RefCell<Seen>> = Rc::default();
    let (record, engine) = (seen.clone(), fake.clone());
    let opener: Opener = Arc::new(|_: &Url| Ok(()));
    let mut win = AddressWindow::new(
        parsed("http://127.0.0.1:8000/a"),
        Box::new(move |cue| {
            let calls = match engine.views() {
                0 => vec![],
                _ => engine.calls(0),
            };
            record.borrow_mut().push((cue, calls));
        }),
        opener,
        None,
        false,
        Rc::default(),
    );
    Driver::new(800.0, 480.0).idle(&mut win, 1);
    let open = Did::Open {
        page: false,
        preview: false,
        scripts: true,
    };
    let load = Did::Load("http://127.0.0.1:8000/a".into());
    assert_eq!(
        *seen.borrow(),
        [
            (Cue::Window, vec![open.clone()]),
            (Cue::Open, vec![open, load])
        ]
    );
}

/// An opener that says when it was called and answers only once released (within 20 s).
fn held_opener() -> (Opener, mpsc::Receiver<String>, mpsc::Sender<()>) {
    let (called, calls) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let (called, released) = (Mutex::new(called), Mutex::new(released));
    let opener: Opener = Arc::new(move |url: &Url| {
        let _ = called.lock().unwrap().send(url.as_str().to_string());
        released
            .lock()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(20))
            .map_err(|_| "never released".to_string())
    });
    (opener, calls, release)
}

/// The system browser's opener runs off the window's thread — the window goes on drawing while
/// it waits — and one link opens at a time: another click out meanwhile (a script's `a.click()`
/// may count as one) is said and not opened; once the first has opened, the next one goes.
#[test]
fn wv_e2_one_link_opens_at_a_time_off_the_windows_thread() {
    let fake = FakeEngine::native();
    fake.install();
    let (opener, calls, release) = held_opener();
    let mut r = rig_opening("http://127.0.0.1:8000/", false, opener, Arc::default());
    r.idle(1);
    let events = fake.events(0);
    let click = |target: &str| Event::Refused {
        target: target.into(),
        clicked: true,
    };
    let (first, second) = ("https://example.com/a?token=abc", "https://example.com/b");
    events.send(click(first));
    r.idle(1);
    let wait = std::time::Duration::from_secs(10);
    assert_eq!(calls.recv_timeout(wait).unwrap(), first);
    r.idle(2);
    let opening = "https://example.com/a?…: opening in the system browser…";
    assert!(r.shows(opening), "the window drew on: {:?}", r.texts());
    events.send(click(second));
    r.idle(1);
    assert!(
        r.shows(
            "https://example.com/b: not opened — a link is still opening in the system browser"
        ),
        "{:?}",
        r.texts()
    );
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(calls.try_recv().is_err(), "the second was not opened");
    release.send(()).unwrap();
    let opened = "https://example.com/a?…: opened in the system browser";
    let mut tries = 0;
    while !r.shows(opened) {
        tries += 1;
        assert!(tries < 1000, "{:?}", r.texts());
        r.idle(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    events.send(click(second));
    r.idle(1);
    assert_eq!(
        calls.recv_timeout(wait).unwrap(),
        second,
        "the next one goes"
    );
    release.send(()).unwrap();
    no_secrets(&r.texts());
}
