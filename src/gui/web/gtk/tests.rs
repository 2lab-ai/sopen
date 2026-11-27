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

/// `GInputStream`'s layout as declared here, on a 64-bit machine. This only pins the declaration:
/// the check that matters runs at the engine's start, against GLib's own `g_type_query`, and
/// refuses the engine on a mismatch (`stream_type`; the engine tests see it pass).
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
    assert_eq!(device_size(rect, 1.0, MAX_SIDE), (714, 580));
    assert_eq!(device_size(rect, 2.0, MAX_SIDE), (1428, 1159));
    assert_eq!(device_size(rect, 1.25, MAX_SIDE), (893, 724));
    assert_eq!(device_size(egui::Rect::NOTHING, 2.0, MAX_SIDE), (1, 1));
    // Never past the renderer's largest texture side (egui refuses a larger picture).
    let wide = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(9000.0, 300.0));
    assert_eq!(device_size(wide, 1.0, 8192), (8192, 300));
    assert_eq!(device_size(wide, 2.0, 99_999), (MAX_SIDE as i32, 600));
    assert_eq!(device_size(wide, 1.0, 0), (1, 1));
    // Positions over GTK's own factor, as the widget's size is.
    let at = Input::Button {
        x: 200.0,
        y: 922.0,
        button: 1,
        pressed: true,
        state: 0,
    };
    assert_eq!(at.clone().over(1.0), at);
    assert_eq!(
        at.over(2.0),
        Input::Button {
            x: 100.0,
            y: 461.0,
            button: 1,
            pressed: true,
            state: 0
        }
    );
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
    assert_eq!(
        navigation(&page, "about:srcdoc", false, false),
        Nav::Refuse { clicked: false },
        "about:srcdoc too: a main frame going there reached the screen"
    );
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
    // A file view's main frame: held at its main resource for `data:` and `blob:`; its own
    // documents go.
    assert!(main_out(&page, "data:text/html,<p>x"));
    assert!(main_out(&page, "blob:sopen-page://h/1"));
    assert!(!main_out(&page, &own));
    assert!(!main_out(&page, ""), "no address yet: nothing to hold");
    // `about:` fails closed in a file view, every frame, every spelling.
    for about in [
        "about:blank",
        "ABOUT:srcdoc",
        "about:srcdoc#x",
        "about:srcdoc?x",
    ] {
        assert_eq!(
            navigation(&page, about, false, false),
            Nav::Refuse { clicked: false },
            "{about}"
        );
    }
    // The commit's last line: any document outside the view's scope.
    assert!(commit_out(&page, "about:srcdoc"));
    assert!(commit_out(&page, "data:text/html,x"));
    assert!(!commit_out(&page, &own));
    assert!(!commit_out(&page, ""));
    assert!(commit_out(&address, "http://127.0.0.1:8081/"));
    assert!(!commit_out(&address, "http://127.0.0.1:8080/x"));
    assert_eq!(
        navigation(&address, "about:blank", false, false),
        Nav::Allow
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
    let why = sandbox_runs(missing, PROBE_WAIT).unwrap_err();
    assert!(
        why.starts_with("WebKitGTK's web process sandbox: "),
        "{why}"
    );
    let why = sandbox_runs("/usr/bin/false", PROBE_WAIT).unwrap_err();
    assert_eq!(
        why,
        "WebKitGTK's web process sandbox cannot start here (/usr/bin/false: failed)"
    );
    assert_eq!(
        DEAD_PROXY, "http://127.0.0.1:0",
        "port 0: nothing can listen there"
    );
    // A probe that does not end is given up on, on time.
    let stuck = temp.path().join("stuck-bwrap");
    std::fs::write(&stuck, "#!/bin/sh\nexec sleep 30\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o755)).unwrap();
    let at = Instant::now();
    let why = sandbox_runs(stuck.to_str().unwrap(), Duration::from_millis(300)).unwrap_err();
    assert!(why.ends_with("did not end within 300ms"), "{why}");
    assert!(at.elapsed() < Duration::from_secs(2), "{:?}", at.elapsed());
}

/// The GTK thread starts at most once per process: the first view spawns it and returns at
/// once; a view opened while it starts does not spawn another and is queued; a start that never
/// ends is given up on at its deadline — every waiting view told §4's line — and every later
/// view gets the same reason, still with no second spawn.
#[test]
fn the_gtk_thread_starts_once_and_a_stuck_start_fails_its_views() {
    let life: &'static Mutex<Life> = Box::leak(Box::new(Mutex::new(Life::Unstarted)));
    let spawns = Arc::new(AtomicUsize::new(0));
    let spawn = |spawns: Arc<AtomicUsize>| {
        move |rx: mpsc::Receiver<Cmd>, _tx: mpsc::Sender<Cmd>| {
            spawns.fetch_add(1, Ordering::SeqCst);
            // A start that never ends: the receiver kept, nothing ever said.
            std::mem::forget(rx);
            Ok(())
        }
    };
    let deadline = Duration::from_millis(300);
    let (first, first_rx) = Events::new();
    let at = Instant::now();
    assert!(sender(life, &first, deadline, spawn(spawns.clone())).is_ok());
    let (second, second_rx) = Events::new();
    assert!(sender(life, &second, deadline, spawn(spawns.clone())).is_ok());
    assert!(
        at.elapsed() < Duration::from_millis(100),
        "the windows' thread is not held: {:?}",
        at.elapsed()
    );
    assert_eq!(spawns.load(Ordering::SeqCst), 1, "one spawn");
    let line = "no web engine here (GTK did not start within 300ms)";
    for rx in [&first_rx, &second_rx] {
        let said = rx.recv_timeout(Duration::from_secs(5)).expect("told");
        assert_eq!(said, Event::Failed(line.to_string()));
    }
    let (third, _) = Events::new();
    assert_eq!(
        sender(life, &third, deadline, spawn(spawns.clone())).err(),
        Some("GTK did not start within 300ms".to_string())
    );
    assert_eq!(spawns.load(Ordering::SeqCst), 1, "never a second spawn");
    // The thread coming up after all serves nothing.
    assert!(!started(life));
}

/// A start that fails (GTK's own word) fails the views waiting for it and every later one; one
/// that comes up runs, later views get its sender with no spawn, and its watchdog says nothing.
#[test]
fn a_failed_start_fails_every_later_view_and_a_good_one_runs() {
    let why = "GTK did not start (gtk_init_check: no display)";
    let never = |_: mpsc::Receiver<Cmd>, _: mpsc::Sender<Cmd>| -> std::io::Result<()> {
        panic!("no second spawn")
    };
    let life: &'static Mutex<Life> = Box::leak(Box::new(Mutex::new(Life::Unstarted)));
    let (first, first_rx) = Events::new();
    let failing = move |_rx: mpsc::Receiver<Cmd>, _tx: mpsc::Sender<Cmd>| {
        std::thread::spawn(move || fail(life, why));
        Ok(())
    };
    assert!(sender(life, &first, Duration::from_secs(30), failing).is_ok());
    let said = first_rx.recv_timeout(Duration::from_secs(5)).expect("told");
    assert_eq!(said, Event::Failed(format!("no web engine here ({why})")));
    let (later, _) = Events::new();
    assert_eq!(
        sender(life, &later, Duration::from_secs(30), never).err(),
        Some(why.to_string())
    );

    let life: &'static Mutex<Life> = Box::leak(Box::new(Mutex::new(Life::Unstarted)));
    let (first, first_rx) = Events::new();
    // `started` takes the lock the spawn is called under: from a thread of its own.
    let up = move |rx: mpsc::Receiver<Cmd>, _tx: mpsc::Sender<Cmd>| {
        std::thread::spawn(move || {
            let _rx = rx;
            assert!(started(life), "runs");
        });
        Ok(())
    };
    assert!(sender(life, &first, Duration::from_millis(300), up).is_ok());
    let ran = spin(5.0, || {
        matches!(*life.lock().unwrap(), Life::Running { .. })
    });
    assert!(ran, "it runs");
    let (later, _) = Events::new();
    assert!(sender(life, &later, Duration::from_millis(300), never).is_ok());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        first_rx.try_recv().is_err(),
        "the watchdog says nothing of a thread that runs"
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
    // The page stream's class was registered: GLib's `g_type_query` agreed with its layout.
    assert!(
        matches!(STREAM_TYPE.get(), Some(Ok(_))),
        "{:?}",
        STREAM_TYPE.get()
    );
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

/// What a main-frame input should come to.
#[derive(Clone, Copy, Debug)]
enum Comes {
    /// Refused and said: a `Refused` whose target starts with this.
    Refused(&'static str),
    /// Nothing visible: after a fixed second the page is still there.
    Stays,
    /// The page's own script replaces its content (a `javascript:` URL's result, as
    /// `document.write` would): no navigation, the document still the scheme's — at its address,
    /// nothing outside reached — only no longer red.
    Rewritten,
}

/// §3.4: a file view's main frame never leaves its scheme host — a meta refresh, a script's
/// `location`, a form, `window.open`, `history.back()`, to `about:`, `data:` or `blob:`
/// documents, `about:srcdoc` in every spelling included — the page stays on screen (its red), its
/// address the scheme's, no document outside the scheme said, nothing reaching an outside
/// server, and the policy and the main resource's hold alone hold every input (the commit's check
/// never had to act); a `javascript:` URL's result rewrites the page in place, still the
/// scheme's; a `data:` frame still shows (its green), a `srcdoc` frame never.
#[test]
fn wv_b_a_file_views_main_frame_keeps_to_its_scheme() {
    let Some((_turn, engine)) = turn("wv_b_a_file_views_main_frame_keeps_to_its_scheme") else {
        return;
    };
    let outside = Http::start();
    let data = format!(
        "data:text/html,<body style='background:%2300f'><img src='http://127.0.0.1:{}/d.png'>",
        outside.port
    );
    // A green page as a frame's document.
    let green = "data:text/html,<body style='margin:0;background:%230f0'>";
    let frames = format!(
        "<iframe src=\"{green}\" style='position:fixed;left:0;top:150px;width:90px;height:40px;\
         border:0'></iframe><iframe srcdoc=\"<body style='margin:0;background:#0f0'>\" \
         style='position:fixed;left:100px;top:150px;width:90px;height:40px;border:0'></iframe>"
    );
    let later = |js: &str| format!("<script>setTimeout(() => {{ {js} }}, 50)</script>");
    let blob = later(
        "location = URL.createObjectURL(new Blob(['<body style=\"background:#00f\">'], \
         {type: 'text/html'}))",
    );
    let back = "<script>setTimeout(() => { if (!history.state) { history.replaceState(1, ''); \
        location = 'q.html' } }, 50)</script>";
    let cases: Vec<(&str, bool, String, Comes)> = vec![
        (
            "meta about:blank",
            false,
            "<meta http-equiv=refresh content='0;url=about:blank'>".into(),
            Comes::Refused("about:blank"),
        ),
        (
            "meta data:",
            false,
            format!("<meta http-equiv=refresh content=\"0;url={data}\">"),
            Comes::Refused("data:"),
        ),
        (
            "script data:",
            true,
            later(&format!("location = \"{data}\"")),
            Comes::Refused("data:"),
        ),
        (
            "script about:blank",
            true,
            later("location = 'about:blank'"),
            Comes::Refused("about:blank"),
        ),
        ("script blob:", true, blob, Comes::Refused("blob:")),
        (
            "meta about:srcdoc",
            false,
            "<meta http-equiv=refresh content='0;url=about:srcdoc'>".into(),
            Comes::Stays,
        ),
        (
            "script about:srcdoc",
            true,
            later("location = 'about:srcdoc'"),
            Comes::Stays,
        ),
        (
            "script about:SRCDOC",
            true,
            later("location = 'about:SRCDOC'"),
            Comes::Stays,
        ),
        (
            "script about:srcdoc#x",
            true,
            later("location = 'about:srcdoc#x'"),
            Comes::Stays,
        ),
        (
            "script about:srcdoc?x",
            true,
            later("location = 'about:srcdoc?x'"),
            Comes::Stays,
        ),
        (
            "script javascript:",
            true,
            later("location = 'javascript:\"<h1>x</h1>\"'"),
            Comes::Rewritten,
        ),
        (
            "window.open data:",
            true,
            later(&format!("window.open(\"{data}\")")),
            Comes::Stays,
        ),
        (
            "window.open _self",
            true,
            later("window.open('', '_self')"),
            Comes::Stays,
        ),
        (
            "form data:",
            true,
            format!(
                "<form id=f action=\"data:text/html,<body style='background:%2300f'>\"></form>{}",
                later("f.submit()")
            ),
            Comes::Refused("data:"),
        ),
        ("history.back", true, back.into(), Comes::Stays),
    ];
    let mut failed = vec![];
    let sent_home = COMMITS_SENT_HOME.load(Ordering::SeqCst);
    for (name, scripts, head, comes) in cases {
        let page =
            format!("<!doctype html>{head}<style>body{{margin:0;background:#f00}}</style>{frames}");
        let q = "<!doctype html><style>body{background:#00f}</style>\
            <script>setTimeout(() => history.back(), 50)</script>";
        let site = Site::with(&[
            ("/p.html", "text/html", page.into_bytes().leak()),
            ("/q.html", "text/html", q.as_bytes()),
        ]);
        let mut shown = Shown::page(&engine, &site, scripts, Contain::ALL);
        shown.place(200.0, 200.0, 1.0);
        shown.load("p.html");
        match comes {
            Comes::Refused(target) => shown.until(&format!("{name}: refused"), 10.0, |e| {
                e.iter().any(|e| {
                    matches!(e, Event::Refused { target: t, clicked: false } if t.starts_with(target))
                })
            }),
            Comes::Stays | Comes::Rewritten => shown.finished(),
        }
        std::thread::sleep(Duration::from_millis(1000));
        shown.drain();
        println!("main frame {name}: said {:?}", shown.seen);
        let mut wrong = vec![];
        let rewritten = matches!(comes, Comes::Rewritten);
        if !rewritten && shown.colour(100.0, 60.0) != "red" {
            wrong.push(format!("the page left ({})", shown.colour(100.0, 60.0)));
        }
        if !rewritten && shown.colour(45.0, 170.0) != "green" {
            wrong.push("no data: frame".to_string());
        }
        // SAFETY: the view's address, on the GTK thread.
        let at = shown
            .view
            .probe(|api, web| unsafe { text((api.webkit_web_view_get_uri)(web)) });
        if !shown
            .host
            .path_of(&at)
            .is_some_and(|p| p.ends_with(".html"))
        {
            wrong.push(format!("the view's address left the scheme: {at}"));
        }
        if COMMITS_SENT_HOME.load(Ordering::SeqCst) != sent_home {
            wrong.push("the commit's check had to act: the policy missed it".to_string());
        }
        if !rewritten && shown.colour(145.0, 170.0) != "red" {
            wrong.push("a srcdoc frame".to_string());
        }
        // A document under the root is a path; one outside the scheme is said as its address.
        if shown
            .seen
            .iter()
            .any(|e| matches!(e, Event::Document(d) if d.contains(':')))
        {
            wrong.push("a document outside the scheme".to_string());
        }
        if outside.accepted() > 0 {
            wrong.push(format!("the outside server reached: {:?}", outside.log()));
        }
        if !wrong.is_empty() {
            failed.push(format!("{name}: {wrong:?} — said {:?}", shown.seen));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// The commit's check, the last line: with the policy's `about:` refusal taken away (the tests'
/// hook), a page whose script leaves for `about:blank` at every load is sent home once — its page
/// loaded back — and the second time only stopped and said: no loop, and the screen never shows
/// the outside document (the copied picture stays the page's red). A user's reload (the real
/// reload command) starts over: home once more, then stopped again.
#[test]
fn wv_b_a_document_outside_the_scheme_is_sent_home_once() {
    let Some((_turn, engine)) = turn("wv_b_a_document_outside_the_scheme_is_sent_home_once") else {
        return;
    };
    /// The hook off again however the test ends.
    struct Held;
    impl Drop for Held {
        fn drop(&mut self) {
            ALLOW_ABOUT.store(false, Ordering::SeqCst);
        }
    }
    let held = Held;
    ALLOW_ABOUT.store(true, Ordering::SeqCst);
    let page = b"<!doctype html><style>body{margin:0;background:#f00}</style>\
        <script>setTimeout(() => { location = 'about:blank' }, 300)</script>";
    let site = Site::with(&[("/p.html", "text/html", page)]);
    let before = COMMITS_SENT_HOME.load(Ordering::SeqCst);
    let mut shown = Shown::page(&engine, &site, true, Contain::ALL);
    shown.place(100.0, 100.0, 1.0);
    shown.load("p.html");
    let out = Event::Refused {
        target: "about:blank".to_string(),
        clicked: false,
    };
    let outs = |e: &[Event]| e.iter().filter(|e| **e == out).count();
    shown.until("sent home, then stopped", 10.0, |e| outs(e) >= 2);
    std::thread::sleep(Duration::from_millis(1500));
    shown.drain();
    println!("first load: said {:?}", shown.seen);
    assert_eq!(
        COMMITS_SENT_HOME.load(Ordering::SeqCst),
        before + 1,
        "home once"
    );
    assert_eq!(outs(&shown.seen), 2, "no loop: {:?}", shown.seen);
    assert_eq!(
        shown.colour(50.0, 50.0),
        "red",
        "the outside document never reaches the screen"
    );
    only_red_after_red(&shown.view.posted(), "sent home, then stopped");
    // A user's reload: the check starts over.
    shown.seen.clear();
    shown.view.reload();
    shown.until("after the reload: home, then stopped", 10.0, |e| {
        outs(e) >= 2
    });
    std::thread::sleep(Duration::from_millis(1500));
    shown.drain();
    drop(held);
    println!("after the reload: said {:?}", shown.seen);
    assert_eq!(
        COMMITS_SENT_HOME.load(Ordering::SeqCst),
        before + 2,
        "home once more"
    );
    assert_eq!(outs(&shown.seen), 2, "no loop: {:?}", shown.seen);
    assert_eq!(shown.colour(50.0, 50.0), "red");
    only_red_after_red(&shown.view.posted(), "stopped, then Cmd/Ctrl+R");
    assert!(
        !shown.seen.contains(&Event::Document("about:blank".into())),
        "never said as the document: {:?}",
        shown.seen
    );
}

/// The same in an address window, no hook needed: a page whose script leaves for `about:blank`
/// at every load is sent home once, then stopped; Cmd/Ctrl+R starts over; no picture but the
/// page's reaches the screen.
#[test]
fn wv_b_an_address_window_leaving_for_about_blank_never_shows_it() {
    let Some((_turn, engine)) =
        turn("wv_b_an_address_window_leaving_for_about_blank_never_shows_it")
    else {
        return;
    };
    let server = Http::serving(|_, _| {
        "<!doctype html><style>body{margin:0;background:#f00}</style>\
         <script>setTimeout(() => { location = 'about:blank' }, 300)</script>"
            .to_string()
    });
    let origin = format!("http://127.0.0.1:{}", server.port);
    let mut shown = Shown::address(&engine, &origin);
    shown.place(100.0, 100.0, 1.0);
    shown.view.load(&format!("{origin}/"));
    let out = Event::Refused {
        target: "about:blank".to_string(),
        clicked: false,
    };
    let outs = |e: &[Event]| e.iter().filter(|e| **e == out).count();
    shown.until("sent home, then stopped", 10.0, |e| outs(e) >= 2);
    std::thread::sleep(Duration::from_millis(1500));
    shown.drain();
    assert_eq!(outs(&shown.seen), 2, "no loop: {:?}", shown.seen);
    only_red_after_red(&shown.view.posted(), "address: sent home, then stopped");
    shown.seen.clear();
    shown.view.reload();
    shown.until("after Cmd/Ctrl+R: home, then stopped", 10.0, |e| {
        outs(e) >= 2
    });
    std::thread::sleep(Duration::from_millis(1500));
    shown.drain();
    assert_eq!(outs(&shown.seen), 2, "no loop: {:?}", shown.seen);
    only_red_after_red(&shown.view.posted(), "address: stopped, then Cmd/Ctrl+R");
}

/// Every picture posted after the first red one is red: nothing else reached the screen.
fn only_red_after_red(posted: &[egui::Color32], what: &str) {
    let names: Vec<&str> = posted.iter().map(|c| colour_of(*c)).collect();
    let first = names.iter().position(|c| *c == "red");
    println!("{what}: posted {names:?}");
    let first = first.unwrap_or_else(|| panic!("{what}: never red: {names:?}"));
    assert!(
        names[first..].iter().all(|c| *c == "red"),
        "{what}: a picture not the page's reached the screen: {names:?}"
    );
}

/// §3.2: WebKit's variable that turns its sandbox off refuses the engine, but for `0`.
#[test]
fn the_sandbox_off_variable_refuses_the_engine() {
    assert_eq!(sandbox_variable(None), Ok(()));
    assert_eq!(sandbox_variable(Some("0")), Ok(()));
    for value in ["1", "", "yes"] {
        let why = sandbox_variable(Some(value)).unwrap_err();
        assert!(
            why.starts_with("WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS is set"),
            "{value:?}: {why}"
        );
    }
}

/// The commit's check: a load sopen asked for that shows a document out of the view's scope
/// before any of its own failed (WebKit's `about:blank` for a refused port); after a load failed,
/// a document out is only stopped; a navigation away from a page on screen is sent home once per
/// load sopen asked for, then only stopped — never a loop.
#[test]
fn the_commit_check_fails_once_and_sends_home_once() {
    let mut home = Home::default();
    home.asked();
    assert_eq!(home.commit_out(), Out::Failed, "never reached its page");
    assert_eq!(home.commit_out(), Out::Quiet, "after that, only stopped");
    home.asked();
    home.failed();
    assert_eq!(
        home.commit_out(),
        Out::Quiet,
        "WebKit's failure said it already"
    );
    home.asked();
    home.reached();
    assert_eq!(
        home.commit_out(),
        Out::Home,
        "away from a page on screen: home"
    );
    home.reached();
    assert_eq!(
        home.commit_out(),
        Out::Stop,
        "away again: stopped, not loaded back"
    );
    assert_eq!(home.commit_out(), Out::Stop);
    home.asked();
    home.reached();
    assert_eq!(
        home.commit_out(),
        Out::Home,
        "a new load asked: home once again"
    );
    // A failure that is not a load sopen asked for (one of the page's reads, its own reload)
    // never silences the check; a page reached clears it. (A user's reload is one sopen asked
    // for: the engine test below drives it through the real reload command.)
    let mut home = Home::default();
    home.asked();
    home.reached();
    home.failed();
    home.reached();
    assert_eq!(
        home.commit_out(),
        Out::Home,
        "failed, then reached again: home"
    );
    let mut home = Home::default();
    home.asked();
    home.reached();
    home.failed();
    assert_eq!(
        home.commit_out(),
        Out::Home,
        "a failure after the page was reached: home"
    );
    // What the line shows of the address is redacted where it is made.
    let why = not_reached(None, "http://user:pw@127.0.0.1:8080/x?token=SECRET");
    assert!(!why.contains("pw@") && !why.contains("SECRET"), "{why}");
    // The reason a load that never reached its page gives.
    assert_eq!(
        not_reached(Some("http://127.0.0.1:9/"), "about:blank"),
        "port 9 is one WebKit never connects to (a restricted port)"
    );
    assert_eq!(
        not_reached(Some("http://user:pw@127.0.0.1:6000/x?t=1"), "about:blank"),
        "port 6000 is one WebKit never connects to (a restricted port)"
    );
    assert_eq!(
        not_reached(Some("http://127.0.0.1:8080/"), "about:blank"),
        "WebKit showed about:blank instead of the page"
    );
    assert_eq!(
        not_reached(None, "data:x"),
        "WebKit showed data:x instead of the page"
    );
    // An authentication request, as the address window reads it.
    use crate::gui::web::address::{failure, Failure};
    for (scheme, kind) in [
        (2, Failure::Login),
        (1, Failure::Login),
        (6, Failure::Login),
        (100, Failure::Login),
        (7, Failure::ClientCertificate),
        (9, Failure::ClientCertificate),
        (8, Failure::Untrusted),
    ] {
        assert_eq!(failure(&asks(scheme)), kind, "{}", asks(scheme));
    }
}

/// §7 on Linux: an address on one of WebKit's restricted ports (9, the discard port) fails once,
/// said as its failure line's text; WebKit commits `about:blank` after refusing it, which is not
/// sent home again and again — at most two loads in two seconds.
#[test]
fn wv_b_a_restricted_port_fails_once() {
    let Some((_turn, engine)) = turn("wv_b_a_restricted_port_fails_once") else {
        return;
    };
    let before = COMMITS_SENT_HOME.load(Ordering::SeqCst);
    let mut shown = Shown::address(&engine, "http://127.0.0.1:9");
    shown.place(300.0, 200.0, 1.0);
    shown.view.load("http://127.0.0.1:9/");
    std::thread::sleep(Duration::from_secs(2));
    shown.drain();
    let sent = COMMITS_SENT_HOME.load(Ordering::SeqCst) - before;
    let loads = shown.seen.iter().filter(|e| **e == Event::Loading).count();
    let failed: Vec<&String> = shown
        .seen
        .iter()
        .filter_map(|e| match e {
            Event::Failed(why) => Some(why),
            _ => None,
        })
        .collect();
    let said: Vec<&Event> = shown.seen.iter().take(12).collect();
    println!("restricted port: {loads} loads, {sent} sent home, failed {failed:?}; said {said:?}");
    assert!(loads <= 2, "{loads} loads: {said:?}");
    assert!(sent <= 1, "{sent} sent home");
    assert_eq!(failed.len(), 1, "one failure: {said:?}");
    assert!(
        !shown
            .seen
            .iter()
            .any(|e| matches!(e, Event::Refused { .. })),
        "the failure is not hidden by a refusal: {said:?}"
    );
    assert_eq!(
        failed[0], "port 9 is one WebKit never connects to (a restricted port)",
        "WebKit says nothing of it: the engine names the port"
    );
}

/// §7 on Linux: a page asking for an HTTP login (Basic) is answered with none and said once, as a
/// login the address window names.
#[test]
fn wv_b_a_login_is_said_as_one() {
    let Some((_turn, engine)) = turn("wv_b_a_login_is_said_as_one") else {
        return;
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let body = "<title>login</title>login please";
            let _ = write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"sopen\"\r\n\
                 Content-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n\
                 {body}",
                body.len()
            );
        }
    });
    let origin = format!("http://127.0.0.1:{port}");
    let mut shown = Shown::address(&engine, &origin);
    shown.place(300.0, 200.0, 1.0);
    shown.view.load(&format!("{origin}/"));
    shown.until("the login said", 10.0, |e| {
        e.iter().any(|e| matches!(e, Event::Failed(_)))
    });
    std::thread::sleep(Duration::from_millis(500));
    shown.drain();
    println!("login: said {:?}", shown.seen);
    let failed: Vec<&String> = shown
        .seen
        .iter()
        .filter_map(|e| match e {
            Event::Failed(why) => Some(why),
            _ => None,
        })
        .collect();
    assert_eq!(failed, [&asks(2)], "{:?}", shown.seen);
    use crate::gui::web::address::{failure, Failure};
    assert_eq!(failure(failed[0]), Failure::Login);
}

/// §2.3, §3.2: a context whose sandbox reads back off is refused — the view says why and loads
/// nothing (the tests' hook skips the request); the next view, asked as always, is sandboxed and
/// loads.
#[test]
fn wv_b_a_view_without_its_sandbox_loads_nothing() {
    let Some((_turn, engine)) = turn("wv_b_a_view_without_its_sandbox_loads_nothing") else {
        return;
    };
    let site = Site::with(&[("/index.html", "text/html", QUADRANTS)]);
    /// The hook off again however the test ends.
    struct Asked;
    impl Drop for Asked {
        fn drop(&mut self) {
            SKIP_SANDBOX.store(false, Ordering::SeqCst);
        }
    }
    let asked = Asked;
    SKIP_SANDBOX.store(true, Ordering::SeqCst);
    let mut shown = Shown::page(&engine, &site, false, Contain::ALL);
    shown.place(200.0, 120.0, 1.0);
    shown.load("index.html");
    shown.until("the view says why", 10.0, |e| {
        e.iter()
            .any(|e| matches!(e, Event::Failed(why) if why.contains("sandbox")))
    });
    drop(asked);
    std::thread::sleep(Duration::from_millis(500));
    shown.drain();
    println!(
        "no sandbox: said {:?}; the server was asked {:?}",
        shown.seen,
        site.said()
    );
    assert!(site.said().is_empty(), "nothing loaded: {:?}", site.said());
    assert!(
        !shown.seen.iter().any(|e| matches!(e, Event::Document(_))),
        "{:?}",
        shown.seen
    );
    drop(shown);
    let mut again = Shown::page(&engine, &site, false, Contain::ALL);
    again.place(200.0, 120.0, 1.0);
    again.load("index.html");
    again.finished();
    again.shows("the next view", 50.0, 30.0, "red");
}

/// A view's requests are forgotten as they end: a page that reloads itself every 200 ms keeps
/// the GTK thread's bookkeeping bounded.
#[test]
fn wv_b_ended_requests_are_forgotten() {
    let Some((_turn, engine)) = turn("wv_b_ended_requests_are_forgotten") else {
        return;
    };
    let page = b"<!doctype html><meta http-equiv=refresh content='0.2'>\
        <link rel=stylesheet href=s.css><title>again</title>";
    let site = Site::with(&[
        ("/again.html", "text/html", page),
        ("/s.css", "text/css", b"body { background: #f00 }"),
    ]);
    let mut shown = Shown::page(&engine, &site, false, Contain::ALL);
    shown.place(100.0, 100.0, 1.0);
    shown.load("again.html");
    std::thread::sleep(Duration::from_secs(3));
    let starts = site.said().len();
    let counts = shown.view.counts();
    println!("after 3 s of reloads: {starts} things said to the server, {counts:?}");
    assert!(starts >= 10, "it reloaded: {starts}");
    assert!(counts.ids <= 4, "{counts:?}");
}

/// A view closed while WebKit waits on a read and the server still holds the request: the read
/// is completed, the view gone, and the server's late bytes reach nothing.
#[test]
fn wv_b_a_view_closed_mid_read_leaves_nothing() {
    let Some((_turn, engine)) = turn("wv_b_a_view_closed_mid_read_leaves_nothing") else {
        return;
    };
    let site = Site::default();
    site.hold("/b.html");
    let mut shown = Shown::page(&engine, &site, false, Contain::ALL);
    shown.place(100.0, 100.0, 1.0);
    shown.load("b.html");
    let (b, reply) = site.take_held("/b.html");
    let html = vec![b'x'; 256 * 1024];
    let mut body = reply.head(Head::whole("text/html", html.len() as u64));
    body.data(html[..64 * 1024].to_vec()).unwrap();
    let waiting = spin(5.0, || shown.view.counts().reads == 1);
    assert!(waiting, "WebKit waits on a read: {:?}", shown.view.counts());
    let probe = shown.view.tx.clone();
    let api = shown.view.api;
    drop(shown);
    let told = spin(5.0, || site.stopped(b));
    assert!(told, "the server is told at the close: {:?}", site.said());
    let late = body.data(html[64 * 1024..].to_vec());
    assert_eq!(
        late,
        Err(crate::scheme::Refused::Stopped),
        "its late bytes go nowhere"
    );
    let (tx, rx) = mpsc::channel();
    post(api, &probe, Cmd::Count(tx));
    let counts = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    println!("closed mid-read: {counts:?}");
    assert_eq!(counts.reads, 0, "{counts:?}");
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
    // §2.3: the web process runs under bwrap, in a PID namespace of its own.
    for (pid, _, _) in during.iter().filter(|d| d.1.starts_with("WebKitWebProces")) {
        let chain = ancestors(*pid);
        let nspid = std::fs::read_to_string(format!("/proc/{pid}/status"))
            .unwrap_or_default()
            .lines()
            .find(|l| l.starts_with("NSpid:"))
            .unwrap_or("")
            .to_string();
        println!("web process {pid}: parents {chain:?}; {nspid}");
        assert!(chain.iter().any(|n| n == "bwrap"), "under bwrap: {chain:?}");
        assert!(
            nspid.split_whitespace().count() >= 3,
            "a PID namespace of its own: {nspid}"
        );
    }
    let probe = shown.view.tx.clone();
    let at = Instant::now();
    drop(shown);
    let (tx, rx) = mpsc::channel();
    post(engine.api, &probe, Cmd::Count(tx));
    let counts = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(
        counts.views, 0,
        "the GTK thread has no view left: {counts:?}"
    );
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

/// The names of `pid`'s parents, up to this process.
fn ancestors(pid: i32) -> Vec<String> {
    let me = std::process::id() as i32;
    let mut out = vec![];
    let mut at = pid;
    while at > 1 && at != me {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{at}/stat")) else {
            break;
        };
        let close = stat.rfind(')').unwrap_or(0);
        at = stat[close + 1..]
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        let name = std::fs::read_to_string(format!("/proc/{at}/comm")).unwrap_or_default();
        out.push(name.trim().to_string());
    }
    out
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
