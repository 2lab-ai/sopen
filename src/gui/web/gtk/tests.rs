//! WV-B's tests (09 §10–§11, "Engines"): the WebKitGTK engine. The tests above the line need no
//! display and always run. The engine tests below it run only with `SOPEN_REQUIRE_WEBKIT=1`, under
//! a private compositor of their own (09 §11: a private kwin or `xvfb-run`, never the session's
//! display), and skip with a note without it; there they fail when WebKitGTK does not load or
//! GTK does not start. GTK runs on the engine's own thread, so the tests drive it from theirs;
//! they take turns ([`turn`]), as the engine's one GTK thread per process would make them anyway.

use super::*;
use crate::gui::web::{after_pass, choose, use_engine, Content, Settings, Unavailable, Web};
use crate::scheme::{Head, Reply, Request, Server};
use std::io::{Read as _, Write as _};
use std::sync::atomic::AtomicUsize;

// ---------------------------------------------------------------------------------------------
// Without a display.

/// §4, WV-7: a library that is not there fails in `dlopen`, at once, with its reason; the
/// process's pick is then §4's line.
#[test]
fn a_missing_library_fails_fast_with_its_reason() {
    let soname = "libwebkit2gtk-sopen-missing.so.0";
    let at = Instant::now();
    let why = api::load(soname).err().expect("no such library");
    let took = at.elapsed();
    assert!(why.starts_with("cannot load "), "{why}");
    assert!(why.contains(soname), "{why}");
    println!(
        "missing library: {:.3} ms, \"{why}\"",
        took.as_secs_f64() * 1e3
    );
    assert!(took < Duration::from_millis(100), "{took:?}");
    let picked = choose(None, || api::load(soname).map(|_| unreachable!()));
    let Err(why) = picked else {
        panic!("no engine without the library");
    };
    assert!(
        why.line().starts_with("no web engine here (cannot load "),
        "{}",
        why.line()
    );
    assert!(matches!(why, Unavailable::NoEngine(_)));
}

/// A test build outside the private compositor never starts GTK: it would reach the session's
/// own display and bus.
#[test]
fn a_test_build_starts_no_gtk_unless_asked() {
    if std::env::var("SOPEN_REQUIRE_WEBKIT").as_deref() == Ok("1") {
        return;
    }
    let why = engine().err().expect("no engine in an ungated test build");
    assert_eq!(why, UNGATED);
}

/// `GInputStream`'s layout as declared: GLib's `g_type_query` must agree before the page stream
/// is registered on it (checked at the engine's start; here the declaration itself).
#[test]
fn the_stream_layout_is_glibs_on_a_64_bit_machine() {
    assert_eq!(api::stream_layout(), (248, 32));
    assert_eq!(api::page_stream_size(), 48);
}

/// §3.2: every view's settings — acceleration `NEVER` (the default aborts offscreen), devtools,
/// file URL access, WebRTC, DNS prefetching, windows and the page cache off; scripts as asked.
#[test]
fn every_view_gets_the_settings() {
    for scripts in [false, true] {
        let all = settings_for(scripts);
        let value = |s: Setting| {
            all.iter()
                .find(|(setting, _)| *setting == s)
                .map(|(_, v)| *v)
                .unwrap_or_else(|| panic!("{s:?} is set"))
        };
        assert_eq!(value(Setting::HardwareAcceleration), 2);
        assert_eq!(value(Setting::Javascript), c_int::from(scripts));
        assert_eq!(value(Setting::JavascriptMarkup), c_int::from(scripts));
        for off in [
            Setting::OpenWindows,
            Setting::DeveloperExtras,
            Setting::FileAccess,
            Setting::UniversalAccess,
            Setting::Webrtc,
            Setting::DnsPrefetching,
            Setting::PageCache,
        ] {
            assert_eq!(value(off), 0, "{off:?}");
        }
    }
    assert_eq!(
        scripts_settings(true),
        vec![(Setting::Javascript, 1), (Setting::JavascriptMarkup, 1)]
    );
}

/// §3.3: the filter is macOS's rule list — every URL blocked, then only the scheme, `data:` and
/// `blob:` let through.
#[test]
fn the_filter_lets_through_only_the_scheme_data_and_blob() {
    let rules: serde_json::Value = serde_json::from_str(RULES).unwrap();
    let rules = rules.as_array().unwrap();
    assert_eq!(rules.len(), 4);
    assert_eq!(rules[0]["trigger"]["url-filter"], ".*");
    assert_eq!(rules[0]["action"]["type"], "block");
    let through: Vec<&str> = rules[1..]
        .iter()
        .inspect(|r| assert_eq!(r["action"]["type"], "ignore-previous-rules"))
        .map(|r| r["trigger"]["url-filter"].as_str().unwrap())
        .collect();
    assert_eq!(through, ["^sopen-page:", "^data:", "^blob:"]);
}

/// The filter's folder: new, this user's, mode 0700, named for this process; a dead process's
/// is swept, a live one's and anything else stay.
#[test]
fn the_filter_folder_is_new_private_and_swept_when_its_process_is_gone() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let dir = make_rules_dir(temp.path()).unwrap();
    let name = dir.file_name().unwrap().to_str().unwrap().to_string();
    assert!(name.starts_with(&format!("{RULES_PREFIX}{}-", std::process::id())));
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    let mut child = std::process::Command::new("true").spawn().unwrap();
    child.wait().unwrap();
    let dead = temp
        .path()
        .join(format!("{RULES_PREFIX}{}-0123", child.id()));
    std::fs::create_dir(&dead).unwrap();
    let other = temp.path().join("sopen-web-rules-notapid");
    std::fs::create_dir(&other).unwrap();
    sweep(temp.path());
    assert!(!dead.exists(), "a dead process's folder is swept");
    assert!(dir.exists(), "this process's stays");
    assert!(other.exists(), "anything else stays");
}

/// §2.3's HiDPI: the view is its rect × the scale in device pixels and its page zoomed by the
/// scale; GTK's own factor, were it ever above 1, takes its share of both.
#[test]
fn the_view_is_its_rect_times_the_scale() {
    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(714.0, 579.5));
    assert_eq!(device_size(rect, 1.0), (714, 580));
    assert_eq!(device_size(rect, 2.0), (1428, 1159));
    assert_eq!(device_size(rect, 1.25), (893, 724));
    assert_eq!(device_size(egui::Rect::NOTHING, 2.0), (1, 1));
    assert_eq!(zoom(2.0, 1), 2.0);
    assert_eq!(zoom(1.5, 1), 1.5);
    assert_eq!(zoom(2.0, 2), 1.0);
    assert_eq!(zoom(f32::NAN, 1), 1.0);
    assert_eq!(widget_size(1428, 1159, 1), (1428, 1159));
    assert_eq!(widget_size(1428, 1159, 2), (714, 580));
    assert_eq!(widget_size(0, 0, 0), (1, 1));
}

/// §2.3's input: pointer positions scaled to device pixels; the first press gives the page focus
/// and the buttons held go into the state; a wheel at the pointer, its direction GDK's.
#[test]
fn the_pointer_reaches_the_page_in_device_pixels() {
    let mut forward = Forward {
        scale: 2.0,
        height: 300.0,
        ..Forward::default()
    };
    assert_eq!(
        forward.translate(&egui::Event::PointerMoved(egui::pos2(100.0, 461.0))),
        vec![Input::Motion {
            x: 200.0,
            y: 922.0,
            state: 0
        }]
    );
    let press = |pressed| egui::Event::PointerButton {
        pos: egui::pos2(10.0, 5.0),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::SHIFT,
    };
    assert_eq!(
        forward.translate(&press(true)),
        vec![
            Input::Focus(true),
            Input::Button {
                x: 20.0,
                y: 10.0,
                button: 1,
                pressed: true,
                state: api::GDK_SHIFT_MASK
            }
        ]
    );
    assert_eq!(
        forward.translate(&egui::Event::PointerMoved(egui::pos2(11.0, 5.0))),
        vec![Input::Motion {
            x: 22.0,
            y: 10.0,
            state: api::GDK_BUTTON1_MASK
        }],
        "a drag holds the button"
    );
    assert_eq!(
        forward.translate(&press(false)),
        vec![Input::Button {
            x: 20.0,
            y: 10.0,
            button: 1,
            pressed: false,
            state: api::GDK_SHIFT_MASK | api::GDK_BUTTON1_MASK
        }]
    );
    let wheel = egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -80.0),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
    };
    assert_eq!(
        forward.translate(&wheel),
        vec![Input::Scroll {
            x: 20.0,
            y: 10.0,
            dx: 0.0,
            dy: 4.0,
            state: 0
        }],
        "80 points down at 2x: 160 content pixels, 4 of WebKit's 40-pixel units"
    );
    assert_eq!(forward.blur(), Some(Input::Focus(false)));
    assert_eq!(forward.blur(), None);
    assert_eq!(button_number(egui::PointerButton::Secondary), 3);
    assert_eq!(
        smooth_delta(egui::MouseWheelUnit::Line, egui::vec2(1.0, 2.0), 1.0, 0.0),
        (-1.0, -2.0)
    );
}

/// §2.4 on Linux: keys that type text are typed by their text event (once), the others by their
/// keyval; any key with Ctrl or Alt is a shortcut; text, an IME's commit and a paste are typed
/// key by key, Hangul as GDK's Unicode keyvals; Copy and Cut are Ctrl+C and Ctrl+X.
#[test]
fn keys_reach_the_page_once_as_gdk_keyvals() {
    use egui::Key;
    let none = egui::Modifiers::NONE;
    let ctrl = egui::Modifiers::CTRL;
    assert_eq!(key_keyval(Key::Enter, none), Some(0xff0d));
    assert_eq!(key_keyval(Key::Tab, none), Some(0xff09));
    assert_eq!(key_keyval(Key::Tab, egui::Modifiers::SHIFT), Some(0xfe20));
    assert_eq!(key_keyval(Key::Backspace, none), Some(0xff08));
    assert_eq!(key_keyval(Key::PageDown, none), Some(0xff56));
    assert_eq!(key_keyval(Key::ArrowLeft, none), Some(0xff51));
    assert_eq!(key_keyval(Key::A, none), None, "its text types it");
    assert_eq!(key_keyval(Key::Space, none), None, "its text types it");
    assert_eq!(key_keyval(Key::A, ctrl), Some(u32::from(b'a')));
    assert_eq!(key_keyval(Key::Num5, ctrl), Some(u32::from(b'5')));
    assert_eq!(key_keyval(Key::Space, ctrl), Some(0x20));
    assert_eq!(key_keyval(Key::F5, none), None, "never the page's");
    assert_eq!(key_keyval(Key::Escape, none), None, "never the page's");
    assert_eq!(char_keyval('a'), Some(0x61));
    assert_eq!(char_keyval('é'), Some(0xe9));
    assert_eq!(char_keyval('한'), Some(0x0100_d55c));
    assert_eq!(char_keyval('\n'), Some(0xff0d));
    assert_eq!(char_keyval('\u{7}'), None);

    let mut forward = Forward::default();
    let keyvals = |inputs: Vec<Input>| -> Vec<(u32, bool, u32)> {
        inputs
            .into_iter()
            .map(|i| match i {
                Input::Key {
                    keyval,
                    pressed,
                    state,
                } => (keyval, pressed, state),
                other => panic!("{other:?}"),
            })
            .collect()
    };
    assert_eq!(
        keyvals(forward.translate(&egui::Event::Text("ab".into()))),
        [
            (0x61, true, 0),
            (0x61, false, 0),
            (0x62, true, 0),
            (0x62, false, 0)
        ]
    );
    let commit = egui::Event::Ime(egui::ImeEvent::Commit("한글".into()));
    assert_eq!(
        keyvals(forward.translate(&commit)),
        [
            (0x0100_d55c, true, 0),
            (0x0100_d55c, false, 0),
            (0x0100_ae00, true, 0),
            (0x0100_ae00, false, 0)
        ]
    );
    let preedit = egui::Event::Ime(egui::ImeEvent::Preedit {
        text: "하".into(),
        active_range_chars: None,
    });
    assert_eq!(
        forward.translate(&preedit),
        vec![],
        "no preedit reaches the page"
    );
    let ctrl_mask = api::GDK_CONTROL_MASK;
    assert_eq!(
        keyvals(forward.translate(&egui::Event::Copy)),
        [(0x63, true, ctrl_mask), (0x63, false, ctrl_mask)]
    );
    assert_eq!(
        keyvals(forward.translate(&egui::Event::Key {
            key: Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: none,
        })),
        []
    );
}

/// §3.4: a file view's every frame keeps to its scheme host; a frame's `data:`, `blob:` and
/// `about:` documents go, unless clicked; a click out is refused as a click; a new window inside
/// the scope opens in the view. An address window's frames load as in a browser, its main frame
/// held to its origin; a click out is refused.
#[test]
fn a_view_keeps_to_its_scope() {
    let host = Host::new();
    let page = Scope::Page(host.clone());
    let own = host.url("sub/a b.html");
    assert_eq!(navigation(&page, &own, false, false), Nav::Allow);
    assert_eq!(navigation(&page, &own, true, false), Nav::Allow);
    assert_eq!(navigation(&page, &own, true, true), Nav::InView);
    assert_eq!(page.document(&own), "sub/a b.html");
    let other = Host::new().url("a.html");
    assert_eq!(
        navigation(&page, &other, false, false),
        Nav::Refuse { clicked: false }
    );
    assert_eq!(
        navigation(&page, "https://example.com/", true, false),
        Nav::Refuse { clicked: true }
    );
    assert_eq!(
        navigation(&page, "data:text/html,<p>x", false, false),
        Nav::Allow
    );
    assert_eq!(navigation(&page, "about:srcdoc", false, false), Nav::Allow);
    assert_eq!(
        navigation(&page, "data:text/html,<p>x", true, false),
        Nav::Refuse { clicked: true }
    );
    assert_eq!(
        navigation(&page, "file:///etc/hostname", false, false),
        Nav::Refuse { clicked: false }
    );

    let address = Scope::Address(origin("http://127.0.0.1:8080/app/"));
    assert_eq!(
        navigation(&address, "http://127.0.0.1:8080/other?q=1", false, false),
        Nav::Allow
    );
    assert_eq!(
        navigation(&address, "http://127.0.0.1:8081/", false, false),
        Nav::Allow,
        "a frame of another origin loads; the main frame is held at its start"
    );
    assert!(main_out(&address, "http://127.0.0.1:8081/"));
    assert!(!main_out(&address, "http://127.0.0.1:8080/x"));
    assert!(
        !main_out(&page, "https://example.com/"),
        "a file view's policy holds it"
    );
    assert_eq!(
        navigation(&address, "http://127.0.0.1:8081/", false, true),
        Nav::Refuse { clicked: false },
        "a new window out"
    );
    assert_eq!(
        navigation(&address, "https://127.0.0.1:8080/", true, false),
        Nav::Refuse { clicked: true }
    );
    assert!(address.keeps("HTTP://user:pw@127.0.0.1:8080/x"));
    assert!(!Scope::Address(None).keeps("http://127.0.0.1:8080/"));
    assert_eq!(origin("http://[::1]/"), origin("http://[::1]:80/x"));
    assert_eq!(
        without_fragment("sopen-page://h/a.html#top"),
        "sopen-page://h/a.html"
    );
}

/// The row's lines for a response the engine cannot show, and the load failures that are only
/// cancels.
#[test]
fn what_a_refused_response_and_a_cancel_say() {
    assert_eq!(cannot_show(404, ""), "the page answered 404");
    assert_eq!(
        cannot_show(200, "application/zip"),
        "cannot show application/zip"
    );
    assert_eq!(
        cannot_show(200, ""),
        "cannot show a response without a type"
    );
    assert!(cancelled("WebKitNetworkError", 302));
    assert!(cancelled("WebKitPolicyError", 102));
    assert!(!cancelled("WebKitNetworkError", 399));
}

/// §3.2: the sandbox's tools must be there, or there is no engine (never one unsandboxed).
#[test]
fn without_the_sandboxs_tools_there_is_no_engine() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("bwrap");
    let missing = missing.to_str().unwrap();
    assert_eq!(
        sandbox_tools(&["/bin/sh", missing]),
        Err(format!("WebKitGTK's web process sandbox needs {missing}"))
    );
    let plain = temp.path().join("plain");
    std::fs::write(&plain, "").unwrap();
    assert!(
        sandbox_tools(&[plain.to_str().unwrap()]).is_err(),
        "not executable"
    );
    assert_eq!(sandbox_tools(&["/bin/sh"]), Ok(()));
    let why = sandbox_runs(missing).unwrap_err();
    assert!(
        why.starts_with("WebKitGTK's web process sandbox: "),
        "{why}"
    );
    let why = sandbox_runs("/usr/bin/false").unwrap_err();
    assert_eq!(
        why,
        "WebKitGTK's web process sandbox cannot start here (/usr/bin/false: failed)"
    );
    assert_eq!(
        DEAD_PROXY, "http://127.0.0.1:0",
        "port 0: nothing can listen there"
    );
}

// ---------------------------------------------------------------------------------------------
// The engine tests (SOPEN_REQUIRE_WEBKIT=1, a private compositor).

/// One engine test at a time.
static TURN: Mutex<()> = Mutex::new(());

/// The engine and this test's turn; `None` (skipped, with a note) without `SOPEN_REQUIRE_WEBKIT=1`.
fn turn(name: &str) -> Option<(std::sync::MutexGuard<'static, ()>, WebKitGtk)> {
    if std::env::var("SOPEN_REQUIRE_WEBKIT").as_deref() != Ok("1") {
        eprintln!("skipped: {name} starts WebKitGTK; SOPEN_REQUIRE_WEBKIT=1 runs it");
        return None;
    }
    let guard = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let api = checked().unwrap_or_else(|why| panic!("{name}: no WebKitGTK: {why}"));
    Some((guard, WebKitGtk::new(api)))
}

/// A view of the engine's and what it says.
struct Shown {
    view: GtkView,
    host: Host,
    events: mpsc::Receiver<Event>,
    seen: Vec<Event>,
}

impl Shown {
    fn page(engine: &WebKitGtk, site: &Site, scripts: bool, contain: Contain) -> Shown {
        let host = Host::new();
        let (events, received) = Events::new();
        let opening = Opening {
            content: Opened::Page {
                host: host.clone(),
                server: site.server(),
            },
            preview: false,
            scripts,
            events,
        };
        let view = engine.open_view(opening, contain).expect("a view");
        Shown {
            view,
            host,
            events: received,
            seen: vec![],
        }
    }

    /// An address window's view of `origin`, scripts on (§7).
    fn address(engine: &WebKitGtk, origin: &str) -> Shown {
        let (events, received) = Events::new();
        let opening = Opening {
            content: Opened::Address {
                origin: origin.to_string(),
            },
            preview: false,
            scripts: true,
            events,
        };
        let view = engine.open_view(opening, Contain::ALL).expect("a view");
        Shown {
            view,
            host: Host::new(),
            events: received,
            seen: vec![],
        }
    }

    fn place(&mut self, w: f32, h: f32, scale: f32) {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h));
        self.view.place(rect, scale);
    }

    fn load(&mut self, path: &str) {
        let url = self.host.url(path);
        self.view.load(&url);
    }

    fn drain(&mut self) {
        self.seen.extend(self.events.try_iter());
    }

    fn until(&mut self, what: &str, secs: f64, mut done: impl FnMut(&[Event]) -> bool) {
        let ok = spin(secs, || {
            self.seen.extend(self.events.try_iter());
            done(&self.seen)
        });
        assert!(ok, "{what}: {:?}", self.seen);
    }

    fn finished(&mut self) {
        self.until("the page finished", 15.0, |e| e.contains(&Event::Finished));
    }

    /// A click at `x`, `y`, the pointer moved there first, and time for WebKit to take it: WebKit
    /// sends a view's mouse events to its web process one at a time, each after the last was
    /// answered, and its keys at once — so a key sent in the same instant as a click can reach
    /// the page before the click does (measured: typing right after a move and a click went to
    /// the body).
    fn click(&mut self, x: f32, y: f32) {
        self.view
            .input(&egui::Event::PointerMoved(egui::pos2(x, y)));
        for pressed in [true, false] {
            self.view.input(&egui::Event::PointerButton {
                pos: egui::pos2(x, y),
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        std::thread::sleep(CLICK_TAKEN);
    }

    /// `keyval` pressed and released (a key egui sends as a key, not as text).
    fn key(&mut self, key: egui::Key) {
        for pressed in [true, false] {
            self.view.input(&egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
    }

    /// The colour at `x`, `y` (points) of the newest picture.
    fn colour(&self, x: f32, y: f32) -> &'static str {
        let Some((picture, scale)) = self.view.picture() else {
            return "none";
        };
        let (px, py) = ((x * scale) as usize, (y * scale) as usize);
        if px >= picture.size[0] || py >= picture.size[1] {
            return "outside";
        }
        colour_of(picture[(px, py)])
    }

    /// Until the newest picture has `colour` at `x`, `y`.
    fn shows(&self, what: &str, x: f32, y: f32, colour: &str) {
        let ok = spin(10.0, || self.colour(x, y) == colour);
        assert!(ok, "{what}: {} at {x},{y}, not {colour}", self.colour(x, y));
    }
}

/// How long a click is given before the next input ([`Shown::click`]).
const CLICK_TAKEN: Duration = Duration::from_millis(300);

/// Until `done`, for at most `secs`.
fn spin(secs: f64, mut done: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs_f64(secs);
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn colour_of(c: egui::Color32) -> &'static str {
    match (
        c.r() > 200,
        c.g() > 200,
        c.b() > 200,
        c.r() < 60,
        c.g() < 60,
        c.b() < 60,
    ) {
        (true, _, _, _, true, true) => "red",
        (_, true, _, true, _, true) => "green",
        (_, _, true, true, true, _) => "blue",
        (true, true, _, _, _, true) => "yellow",
        (true, true, true, _, _, _) => "white",
        (_, _, _, true, true, true) => "black",
        _ => "other",
    }
}

/// Four fixed quadrants, red, green, blue and yellow, and a stylesheet through the scheme.
const QUADRANTS: &[u8] = b"<!doctype html><title>sopen page</title>\
    <link rel=stylesheet href=style.css>\
    <div style='position:fixed;left:0;top:0;width:50%;height:50%;background:#f00'></div>\
    <div style='position:fixed;right:0;top:0;width:50%;height:50%;background:#0f0'></div>\
    <div style='position:fixed;left:0;bottom:0;width:50%;height:50%;background:#00f'></div>\
    <div style='position:fixed;right:0;bottom:0;width:50%;height:50%;background:#ff0'></div>";

/// A field whose `pattern` is `want`: the box beside it turns green once its value is exactly
/// that (no script reads it), red before.
fn field_page(want: &str) -> Vec<u8> {
    format!(
        "<!doctype html><meta charset=utf-8><style>body{{margin:0}}\
         #f{{position:fixed;left:10px;top:10px;width:200px;height:30px;font-size:20px}}\
         #m{{position:fixed;left:10px;top:60px;width:60px;height:40px;background:#f00}}\
         #f:valid~#m{{background:#0f0}}</style>\
         <input id=f required pattern='{want}'><div id=m></div>"
    )
    .into_bytes()
}

/// §2.3: a file page served through the scheme renders — its stylesheet asked through the
/// scheme, its document said, four colours where they are — at 1× and at 2×, where the picture
/// is twice the size and the layout the same; the copies' time printed.
#[test]
fn wv_b_a_page_served_through_the_scheme_renders() {
    let Some((_turn, engine)) = turn("wv_b_a_page_served_through_the_scheme_renders") else {
        return;
    };
    let site = Site::with(&[
        ("/index.html", "text/html", QUADRANTS),
        (
            "/style.css",
            "text/css",
            b"body { margin: 0; background: #fff }",
        ),
    ]);
    let started = Instant::now();
    let mut shown = Shown::page(&engine, &site, false, Contain::ALL);
    shown.place(200.0, 120.0, 1.0);
    shown.load("index.html");
    shown.finished();
    let finished = started.elapsed();
    assert!(
        shown.seen.contains(&Event::Document("index.html".into())),
        "{:?}",
        shown.seen
    );
    assert!(site.started("/style.css").is_some(), "{:?}", site.said());
    for (x, y, colour) in [
        (50.0, 30.0, "red"),
        (150.0, 30.0, "green"),
        (50.0, 90.0, "blue"),
        (150.0, 90.0, "yellow"),
    ] {
        shown.shows("1x", x, y, colour);
    }
    let (picture, scale) = shown.view.picture().expect("a picture");
    assert_eq!((picture.size, scale), ([200, 120], 1.0));
    assert_eq!(shown.view.title(), "sopen page");
    // 2×: twice the pixels, the same layout.
    shown.place(200.0, 120.0, 2.0);
    let ok = spin(10.0, || {
        shown
            .view
            .picture()
            .is_some_and(|(p, s)| p.size == [400, 240] && s == 2.0)
    });
    assert!(
        ok,
        "a 2x picture: {:?}",
        shown.view.picture().map(|(p, s)| (p.size, s))
    );
    for (x, y, colour) in [
        (50.0, 30.0, "red"),
        (150.0, 30.0, "green"),
        (50.0, 90.0, "blue"),
        (150.0, 90.0, "yellow"),
    ] {
        shown.shows("2x", x, y, colour);
    }
    println!(
        "rendered: finished {} ms after the view was made; {} pictures copied",
        finished.as_millis(),
        shown.view.pictures()
    );
    // §3.2: the view's context runs its web process in WebKit's sandbox.
    let sandbox = shown.view.probe(|api, web| unsafe {
        (api.webkit_web_context_get_sandbox_enabled)((api.webkit_web_view_get_context)(web)) != 0
    });
    assert!(sandbox, "the view's context is sandboxed");
}

/// §2.3's input: a forwarded click toggles a checkbox — its `:checked` style shows it, no
/// script involved.
#[test]
fn wv_b_a_click_toggles_a_checkbox() {
    let Some((_turn, engine)) = turn("wv_b_a_click_toggles_a_checkbox") else {
        return;
    };
    let page = b"<!doctype html><style>body{margin:0}\
        #c{position:fixed;left:10px;top:10px;width:40px;height:40px;margin:0}\
        #m{position:fixed;left:100px;top:10px;width:50px;height:50px;background:#f00}\
        #c:checked~#m{background:#0f0}</style><input type=checkbox id=c><div id=m></div>";
    let site = Site::with(&[("/box.html", "text/html", page)]);
    let mut shown = Shown::page(&engine, &site, false, Contain::ALL);
    shown.place(300.0, 200.0, 1.0);
    shown.load("box.html");
    shown.finished();
    shown.shows("unchecked", 125.0, 35.0, "red");
    shown.click(30.0, 30.0);
    shown.shows("checked by the click", 125.0, 35.0, "green");
    shown.click(30.0, 30.0);
    shown.shows("unchecked by a second click", 125.0, 35.0, "red");
}

/// §2.3's input and its IME limit: text typed after a click into a field lands there — `abc` as
/// egui's text events, Hangul as egui's text and as an IME's committed text (GDK Unicode
/// keyvals); the field's `pattern` reads the value back, no script involved.
#[test]
fn wv_b_typed_text_lands_in_a_field() {
    let Some((_turn, engine)) = turn("wv_b_typed_text_lands_in_a_field") else {
        return;
    };
    for (want, events) in [
        ("abc", vec![egui::Event::Text("abc".into())]),
        (
            "한글",
            vec![
                egui::Event::Text("한".into()),
                egui::Event::Text("글".into()),
            ],
        ),
        (
            "한글",
            vec![egui::Event::Ime(egui::ImeEvent::Commit("한글".into()))],
        ),
    ] {
        let site = Site::with(&[("/field.html", "text/html", &field_page(want))]);
        let mut shown = Shown::page(&engine, &site, false, Contain::ALL);
        shown.place(300.0, 200.0, 1.0);
        shown.load("field.html");
        shown.finished();
        shown.shows("empty: invalid", 40.0, 80.0, "red");
        shown.click(50.0, 25.0);
        for event in &events {
            shown.view.input(event);
        }
        shown.shows(&format!("{want} typed ({events:?})"), 40.0, 80.0, "green");
        println!("typed {want:?} through {events:?}: the field holds it");
    }
}

/// §2.3's input: a wheel over the page scrolls it.
#[test]
fn wv_b_a_wheel_scrolls() {
    let Some((_turn, engine)) = turn("wv_b_a_wheel_scrolls") else {
        return;
    };
    let page = b"<!doctype html><style>body{margin:0}</style>\
        <div style='height:400px;background:#f00'></div>\
        <div style='height:3000px;background:#0f0'></div>";
    let site = Site::with(&[("/long.html", "text/html", page)]);
    let mut shown = Shown::page(&engine, &site, false, Contain::ALL);
    shown.place(200.0, 150.0, 1.0);
    shown.load("long.html");
    shown.finished();
    shown.shows("the top", 100.0, 20.0, "red");
    shown
        .view
        .input(&egui::Event::PointerMoved(egui::pos2(100.0, 75.0)));
    for _ in 0..10 {
        shown.view.input(&egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -60.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
        });
    }
    shown.shows("scrolled 600 points down", 100.0, 20.0, "green");
}

/// §2.4 through [`Web`]: a click in the page gives it the keyboard, its typing goes in; Esc takes
/// the keyboard back and is not the page's; typing after it is sopen's, not the page's.
#[test]
fn wv_b_esc_gives_the_keyboard_back_to_sopen() {
    let Some((_turn, engine)) = turn("wv_b_esc_gives_the_keyboard_back_to_sopen") else {
        return;
    };
    use_engine(Ok(Rc::new(engine) as Rc<dyn Engine>));
    let site = Site::with(&[("/field.html", "text/html", &field_page("a"))]);
    let mut web = Web::new(
        Content::Page {
            server: site.server(),
        },
        Settings::default(),
    )
    .expect("a view");
    let ctx = egui::Context::default();
    let rect = egui::Rect::from_min_size(egui::pos2(20.0, 10.0), egui::vec2(300.0, 200.0));
    let mut events = vec![];
    frame(&ctx, &mut web, rect, vec![]);
    web.load("field.html");
    let ok = spin(15.0, || {
        frame(&ctx, &mut web, rect, vec![]);
        events.extend(web.events());
        events.contains(&Event::Finished)
    });
    assert!(ok, "finished: {events:?}");
    let at = rect.min + egui::vec2(50.0, 25.0);
    let click = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let ime = frame(
        &ctx,
        &mut web,
        rect,
        vec![egui::Event::PointerMoved(at), click(true), click(false)],
    );
    assert_eq!(web.keyboard(), crate::gui::web::Keyboard::Page);
    let ime = ime.expect("the page has the keyboard: the window's IME is asked for");
    assert_eq!(ime.rect, rect);
    assert_eq!(ime.cursor_rect.min, at, "where the pointer was");
    std::thread::sleep(CLICK_TAKEN);
    frame(&ctx, &mut web, rect, vec![egui::Event::Text("a".into())]);
    let esc = |pressed| egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let ime = frame(&ctx, &mut web, rect, vec![esc(true), esc(false)]);
    assert_eq!(web.keyboard(), crate::gui::web::Keyboard::Sopen);
    assert_eq!(ime, None, "sopen has the keyboard: no IME asked for");
    frame(&ctx, &mut web, rect, vec![egui::Event::Text("b".into())]);
    // The field holds `a` (valid: green), never `ab`; read back from the view's picture.
    let green =
        |web: &mut Web| snapshot_of(web).is_some_and(|image| colour_of(image[(40, 80)]) == "green");
    let ok = spin(10.0, || green(&mut web));
    let seen = snapshot_of(&mut web).map(|i| {
        [(40, 80), (50, 25), (100, 25), (5, 5)]
            .map(|(x, y)| (x, y, colour_of(i[(x, y)]), i[(x, y)]))
    });
    assert!(ok, "the field holds what was typed before Esc: {seen:?}");
    std::thread::sleep(Duration::from_millis(500));
    frame(&ctx, &mut web, rect, vec![]);
    assert!(green(&mut web), "nothing typed after Esc reached the page");
    web.close();
}

/// `web`'s picture, as the view gives it.
fn snapshot_of(web: &mut Web) -> Option<egui::ColorImage> {
    let got: Rc<RefCell<Option<egui::ColorImage>>> = Rc::default();
    let into = got.clone();
    web.snapshot(Box::new(move |p| *into.borrow_mut() = p));
    let image = got.borrow_mut().take();
    image
}

/// §3.3: a file view loads nothing outside the scheme — an outside image, stylesheet, frame,
/// preconnect and prefetch never reach the server (not even a bare connection); the scheme's own
/// image and a `data:` image show. WebKitGTK says nothing of a block, so no count is said. Each
/// layer alone holds the page's requests — the filter without the proxy, the proxy without the
/// filter — and without both the same page reaches the server (the control).
#[test]
fn wv_b_a_request_outside_the_scheme_is_blocked() {
    let Some((_turn, engine)) = turn("wv_b_a_request_outside_the_scheme_is_blocked") else {
        return;
    };
    for (contain, reached) in [
        (Contain::ALL, false),
        (
            Contain {
                filter: true,
                proxy: false,
            },
            false,
        ),
        (
            Contain {
                filter: false,
                proxy: true,
            },
            false,
        ),
        (
            Contain {
                filter: false,
                proxy: false,
            },
            true,
        ),
    ] {
        let outside = Http::start();
        let site = outside_page(outside.port);
        let mut shown = Shown::page(&engine, &site, false, contain);
        shown.place(200.0, 160.0, 1.0);
        shown.load("out.html");
        shown.finished();
        shown.shows("the data: image", 85.0, 25.0, "green");
        shown.shows("the scheme's image", 145.0, 25.0, "blue");
        if reached {
            let got = spin(5.0, || outside.accepted() >= 1);
            assert!(got, "{contain:?}: the page reaches the server");
        } else {
            std::thread::sleep(Duration::from_millis(1000));
        }
        shown.drain();
        println!(
            "{contain:?}: {} connections to the outside server {:?}",
            outside.accepted(),
            outside.log()
        );
        if !reached {
            assert_eq!(outside.accepted(), 0, "{contain:?}: {:?}", outside.log());
        }
        assert!(
            !shown.seen.iter().any(|e| matches!(e, Event::Blocked(_))),
            "no count is said on Linux"
        );
    }
}

/// A page of outside requests — an image, a stylesheet, hints, a frame — beside a `data:` image
/// and one of the scheme's own.
fn outside_page(port: u16) -> Site {
    // A green square as an SVG.
    let green = "data:image/svg+xml;base64,PHN2ZyB4bWxucz0naHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmcnIHdp\
        ZHRoPScxMCcgaGVpZ2h0PScxMCc+PHJlY3Qgd2lkdGg9JzEwJyBoZWlnaHQ9JzEwJyBmaWxsPScjMGYwJy8+PC9zdmc+";
    let page = format!(
        "<!doctype html><style>body{{margin:0}} img{{position:fixed;width:50px;height:50px}}</style>\
         <img src='http://127.0.0.1:{port}/i.png' style='left:0;top:0'>\
         <link rel=stylesheet href='http://127.0.0.1:{port}/s.css'>\
         <link rel=preconnect href='http://127.0.0.1:{port}/'>\
         <link rel=prefetch href='http://127.0.0.1:{port}/next'>\
         <iframe src='http://127.0.0.1:{port}/f.html' style='position:fixed;top:60px'></iframe>\
         <img src='{green}' style='left:60px;top:0'>\
         <img src='blue.svg' style='left:120px;top:0'>"
    );
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'>\
        <rect width='10' height='10' fill='#00f'/></svg>";
    Site::with(&[
        ("/out.html", "text/html", page.into_bytes().leak()),
        ("/blue.svg", "image/svg+xml", svg),
    ])
}

/// §3.3: a file view's scripts are off — its `<script>` does not run; on (the engine asked), the
/// same page's does (the control).
#[test]
fn wv_b_scripts_off_means_a_script_does_not_run() {
    let Some((_turn, engine)) = turn("wv_b_scripts_off_means_a_script_does_not_run") else {
        return;
    };
    let page = b"<!doctype html><style>body{margin:0;background:#f00}</style><body>\
        <script>document.body.style.background = '#0f0'</script>";
    let site = Site::with(&[("/js.html", "text/html", page)]);
    for (scripts, colour) in [(false, "red"), (true, "green")] {
        let mut shown = Shown::page(&engine, &site, scripts, Contain::ALL);
        shown.place(100.0, 100.0, 1.0);
        shown.load("js.html");
        shown.finished();
        shown.shows(&format!("scripts {scripts}"), 50.0, 50.0, colour);
    }
}

/// §3.4: a link out of the scheme is not followed — clicked, or Enter on it — refused as a
/// click and the outside server never reached, not even by the bare connection WebKit makes to a
/// link's host when its navigation starts: the dead proxy holds it, and without the proxy (the
/// control) that connection is made.
#[test]
fn wv_b_a_link_out_is_not_followed() {
    let Some((_turn, engine)) = turn("wv_b_a_link_out_is_not_followed") else {
        return;
    };
    let filter_only = Contain {
        filter: true,
        proxy: false,
    };
    for (by, contain, connections) in [
        ("click", Contain::ALL, 0),
        ("enter", Contain::ALL, 0),
        ("click", filter_only, 1),
    ] {
        let outside = Http::start();
        let target = format!("http://127.0.0.1:{}/away", outside.port);
        let page = format!(
            "<!doctype html><style>body{{margin:0}}a{{display:block;width:200px;height:80px;\
             background:#00f}}</style><div style='height:20px'></div><a href='{target}'>out</a>"
        );
        let site = Site::with(&[("/link.html", "text/html", page.into_bytes().leak())]);
        let mut shown = Shown::page(&engine, &site, false, contain);
        shown.place(300.0, 200.0, 1.0);
        shown.load("link.html");
        shown.finished();
        match by {
            "click" => shown.click(50.0, 60.0),
            _ => {
                shown.click(50.0, 5.0);
                shown.key(egui::Key::Tab);
                shown.key(egui::Key::Enter);
            }
        }
        let refused = Event::Refused {
            target: target.clone(),
            clicked: true,
        };
        shown.until(&format!("the link refused ({by})"), 10.0, |e| {
            e.contains(&refused)
        });
        if connections > 0 {
            spin(3.0, || outside.accepted() >= connections);
        } else {
            std::thread::sleep(Duration::from_millis(500));
        }
        println!(
            "link out by {by}, {contain:?}: {} connections {:?}",
            outside.accepted(),
            outside.log()
        );
        assert_eq!(outside.accepted(), connections, "{by} {contain:?}");
        assert!(
            outside.log().is_empty(),
            "never a request: {:?}",
            outside.log()
        );
    }
}

/// §7 on Linux: an address window loads its origin's page with scripts on; a link inside the
/// origin is followed; a cross-origin frame loads, as in a browser; a link out is refused as a
/// click; a redirect and a script sending the main frame out are stopped as the main resource
/// asks and said, their documents never on screen. A file view stays live beside it: two views,
/// two contexts, both drawn.
#[test]
fn wv_b_an_address_window_keeps_to_its_origin() {
    let Some((_turn, engine)) = turn("wv_b_an_address_window_keeps_to_its_origin") else {
        return;
    };
    let other = Http::start();
    let o = other.port;
    let home = format!(
        "<!doctype html><title>own</title><style>body{{margin:0;background:#f00}}\
         a{{display:block;height:40px}}</style>\
         <script>addEventListener('load', () => document.body.style.background = '#0f0')</script>\
         <a href='/second' style='background:#00f'>in</a>\
         <a href='http://127.0.0.1:{o}/clicked' style='background:#ff0'>out</a>\
         <iframe src='http://127.0.0.1:{o}/frame' style='height:20px'></iframe>"
    );
    let away = format!(
        "<!doctype html><title>away</title><script>location = 'http://127.0.0.1:{o}/main'</script>"
    );
    let own = Http::serving(move |path, port| match path {
        "/" => home.clone(),
        "/second" => "<!doctype html><title>second</title>".to_string(),
        "/away" => away.clone(),
        "/redir" => format!("{REDIRECT}http://127.0.0.1:{o}/redirected"),
        _ => format!("<title>{path} on {port}</title>"),
    });
    let origin = format!("http://127.0.0.1:{}", own.port);
    // A file view first, kept live.
    let site = Site::with(&[("/index.html", "text/html", QUADRANTS)]);
    let mut file = Shown::page(&engine, &site, false, Contain::ALL);
    file.place(200.0, 120.0, 1.0);
    file.load("index.html");
    file.finished();

    let mut shown = Shown::address(&engine, &origin);
    shown.place(300.0, 200.0, 1.0);
    shown.view.load(&format!("{origin}/"));
    shown.finished();
    shown.shows("its script ran", 250.0, 150.0, "green");
    let frame = spin(5.0, || {
        other.log().iter().any(|l| l.starts_with("GET /frame "))
    });
    assert!(frame, "the cross-origin frame loaded: {:?}", other.log());
    // A link out: refused as a click, nothing asked of the other server.
    shown.click(50.0, 60.0);
    let out = Event::Refused {
        target: format!("http://127.0.0.1:{}/clicked", other.port),
        clicked: true,
    };
    shown.until("the link out refused", 10.0, |e| e.contains(&out));
    // A link inside: followed.
    shown.click(50.0, 20.0);
    let second = Event::Document(format!("{origin}/second"));
    shown.until("the link inside followed", 10.0, |e| e.contains(&second));
    let clicked_accepts = other.accepted();
    // A redirect of the main frame out: held at the main resource's next request.
    shown.view.load(&format!("{origin}/redir"));
    let redirected = format!("http://127.0.0.1:{o}/redirected");
    let held = Event::Refused {
        target: redirected.clone(),
        clicked: false,
    };
    shown.until("the main frame's redirect out stopped", 10.0, |e| {
        e.contains(&held)
    });
    // A script sending the main frame out: stopped and said, never on screen.
    shown.view.load(&format!("{origin}/away"));
    let main = format!("http://127.0.0.1:{}/main", other.port);
    let went = Event::Refused {
        target: main.clone(),
        clicked: false,
    };
    shown.until("the main frame's navigation out stopped", 10.0, |e| {
        e.contains(&went)
    });
    std::thread::sleep(Duration::from_millis(500));
    shown.drain();
    for out in [&main, &redirected] {
        assert!(
            !shown.seen.contains(&Event::Document(out.to_string())),
            "never on screen: {out} {:?}",
            shown.seen
        );
    }
    println!(
        "address window: the other origin accepted {} connections ({clicked_accepts} by the \
         frame and the click out), asked {:?}; said {:?}",
        other.accepted(),
        other.log(),
        shown.seen
    );
    assert!(
        !other.log().iter().any(|l| l.starts_with("GET /clicked ")),
        "a link out asks nothing: {:?}",
        other.log()
    );
    // The file view is still drawn beside it.
    file.shows("the file view beside it", 50.0, 30.0, "red");
}

/// X-W1's stops on Linux: a request stopped before its head (the view's resource failing names
/// it) and one stopped mid-body (its read's cancel) are both stopped at the server.
#[test]
fn wv_b_a_stop_reaches_the_server() {
    let Some((_turn, engine)) = turn("wv_b_a_stop_reaches_the_server") else {
        return;
    };
    let site = Site::with(&[("/next.html", "text/html", b"<title>next</title>")]);
    site.hold("/a.html");
    site.hold("/b.html");
    let mut shown = Shown::page(&engine, &site, false, Contain::ALL);
    shown.place(100.0, 100.0, 1.0);
    // Before its head.
    shown.load("a.html");
    let (a, reply) = site.take_held("/a.html");
    shown.load("next.html");
    let ok = spin(5.0, || site.stopped(a));
    assert!(ok, "a stopped before its head: {:?}", site.said());
    drop(reply);
    // Mid-body.
    shown.load("b.html");
    let (b, reply) = site.take_held("/b.html");
    let html = vec![b'x'; 256 * 1024];
    let mut body = reply.head(Head::whole("text/html", html.len() as u64));
    body.data(html[..64 * 1024].to_vec()).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    shown.load("next.html");
    let ok = spin(5.0, || site.stopped(b));
    assert!(ok, "b stopped mid-body: {:?}", site.said());
    drop(body);
    shown.until("next finished", 10.0, |e| {
        e.iter().filter(|e| **e == Event::Finished).count() >= 1
    });
}

/// §2.5, WV-L1's close: a view's web processes are its own and end when it closes; the GTK
/// thread stays for the next view, which renders.
#[test]
fn wv_b_teardown_leaves_no_web_process() {
    let Some((_turn, engine)) = turn("wv_b_teardown_leaves_no_web_process") else {
        return;
    };
    let before = descendants();
    let site = Site::with(&[("/index.html", "text/html", QUADRANTS)]);
    let mut shown = Shown::page(&engine, &site, false, Contain::ALL);
    shown.place(200.0, 120.0, 1.0);
    shown.load("index.html");
    shown.finished();
    let during: Vec<(i32, String, char)> = descendants()
        .into_iter()
        .filter(|d| !before.contains(d))
        .collect();
    println!("the view's processes: {during:?}");
    assert!(
        during
            .iter()
            .any(|(_, name, _)| name.starts_with("WebKitWebProces")),
        "a web process of the view's: {during:?}"
    );
    let at = Instant::now();
    drop(shown);
    assert_eq!(open_views(), 0, "the GTK thread has no view left");
    let gone = spin(2.0, || {
        descendants()
            .iter()
            .all(|d| before.contains(d) || d.2 == 'Z')
    });
    let left: Vec<_> = descendants()
        .into_iter()
        .filter(|d| !before.contains(d))
        .collect();
    println!(
        "after the close: {left:?} ({} ms)",
        at.elapsed().as_millis()
    );
    assert!(gone, "the view's processes are gone within 2 s: {left:?}");
    // Again, on the same GTK thread.
    let mut again = Shown::page(&engine, &site, false, Contain::ALL);
    again.place(200.0, 120.0, 1.0);
    again.load("index.html");
    again.finished();
    again.shows("after a restart", 50.0, 30.0, "red");
}

/// Every process below this one: pid, name, state.
fn descendants() -> Vec<(i32, String, char)> {
    let me = std::process::id() as i32;
    let mut all: Vec<(i32, i32, String, char)> = vec![];
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // pid (name) state ppid …
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let name = stat[open + 1..close].to_string();
        let mut rest = stat[close + 1..].split_whitespace();
        let state = rest.next().and_then(|s| s.chars().next()).unwrap_or('?');
        let ppid = rest.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        all.push((pid, ppid, name, state));
    }
    let mut out = vec![];
    let mut parents = vec![me];
    while let Some(parent) = parents.pop() {
        for (pid, ppid, name, state) in &all {
            if *ppid == parent {
                out.push((*pid, name.clone(), *state));
                parents.push(*pid);
            }
        }
    }
    out
}

/// One pass of a window drawing `web` at `rect` with `events`, and the shell's after-frame step;
/// the IME the pass asked for.
fn frame(
    ctx: &egui::Context,
    web: &mut Web,
    rect: egui::Rect,
    events: Vec<egui::Event>,
) -> Option<egui::output::IMEOutput> {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(400.0, 300.0),
        )),
        events,
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| {
        web.ui(ui, rect);
        after_pass(ui.ctx());
    });
    out.textures_delta.clear();
    out.platform_output.ime
}

// ---------------------------------------------------------------------------------------------
// The test's page server and its outside HTTP server.

#[derive(Clone, Debug, PartialEq, Eq)]
enum Said {
    Start(RequestId, String),
    Stop(RequestId),
}

#[derive(Default)]
struct SiteState {
    files: HashMap<String, (&'static str, Vec<u8>)>,
    hold: Vec<String>,
    held: Vec<(Request, Reply)>,
    said: Vec<Said>,
}

/// A page server answering from memory at once, or keeping a request for the test.
#[derive(Clone, Default)]
struct Site(Arc<Mutex<SiteState>>);

impl Site {
    fn with(files: &[(&str, &'static str, &[u8])]) -> Self {
        let site = Site::default();
        site.0.lock().unwrap().files = files
            .iter()
            .map(|(path, mime, bytes)| (path.to_string(), (*mime, bytes.to_vec())))
            .collect();
        site
    }

    fn hold(&self, path: &str) {
        self.0.lock().unwrap().hold.push(path.to_string());
    }

    fn server(&self) -> Box<dyn Server> {
        Box::new(self.clone())
    }

    fn said(&self) -> Vec<Said> {
        self.0.lock().unwrap().said.clone()
    }

    fn started(&self, path: &str) -> Option<RequestId> {
        self.said().iter().rev().find_map(|s| match s {
            Said::Start(id, p) if p == path => Some(*id),
            _ => None,
        })
    }

    fn stopped(&self, id: RequestId) -> bool {
        self.said().contains(&Said::Stop(id))
    }

    fn take_held(&self, path: &str) -> (RequestId, Reply) {
        let mut got = None;
        let ok = spin(10.0, || {
            let mut state = self.0.lock().unwrap();
            if let Some(at) = state.held.iter().position(|(r, _)| r.path == path) {
                let (request, reply) = state.held.remove(at);
                got = Some((request.id, reply));
            }
            got.is_some()
        });
        assert!(ok, "WebKit asks for {path}: {:?}", self.said());
        got.unwrap()
    }
}

impl Server for Site {
    fn start(&mut self, request: Request, reply: Reply) {
        let mut state = self.0.lock().unwrap();
        state
            .said
            .push(Said::Start(request.id, request.path.clone()));
        if state.hold.contains(&request.path) {
            state.held.push((request, reply));
            return;
        }
        let file = state.files.get(&request.path).cloned();
        drop(state);
        match file {
            Some((mime, bytes)) => {
                let mut body = reply.head(Head::whole(mime, bytes.len() as u64));
                body.data(bytes).unwrap();
                body.done().unwrap();
            }
            None => reply.head(Head::not_found()).done().unwrap(),
        }
    }

    fn stop(&mut self, id: RequestId) {
        self.0.lock().unwrap().said.push(Said::Stop(id));
    }

    fn credit(&mut self, _id: RequestId) {}
}

/// A page body starting with this is sent as a 302 to the address after it.
const REDIRECT: &str = "redirect:";

/// An HTTP server on 127.0.0.1 that logs every connection it accepts (a request or not).
struct Http {
    port: u16,
    accepted: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<String>>>,
}

impl Http {
    fn start() -> Self {
        Http::serving(|_, _| "<title>outside</title>".to_string())
    }

    /// A server answering each `GET` with `page(path, port)` as HTML.
    fn serving(page: impl Fn(&str, u16) -> String + Send + Sync + 'static) -> Self {
        let page = Arc::new(page);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let log = Arc::new(Mutex::new(vec![]));
        let (count, lines) = (accepted.clone(), log.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                count.fetch_add(1, Ordering::SeqCst);
                let lines = lines.clone();
                let page = page.clone();
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut buf = vec![0u8; 8192];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let Some(first) = head.lines().next().filter(|l| !l.is_empty()) else {
                        return;
                    };
                    lines.lock().unwrap().push(first.to_string());
                    let path = first.split(' ').nth(1).unwrap_or("/");
                    let body = page(path, port);
                    if let Some(to) = body.strip_prefix(REDIRECT) {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\n\
                             Connection: close\r\n\r\n"
                        );
                        return;
                    }
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                         Connection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        Http {
            port,
            accepted,
            log,
        }
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}
