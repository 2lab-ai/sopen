//! WV-A's tests (09 §10–§11, "Engines"): the WKWebView engine in a real window. Each scenario runs
//! in a child process — this test binary again — on that process's main thread, where AppKit and
//! WebKit must run: libtest runs every test on a thread of its own, so a Mach-O initializer
//! ([`CHILD_MAIN`]) takes the child over before libtest's `main` and exits with the scenario's
//! result. The window is borderless and off every screen, and the child never becomes the active
//! app, so it takes no focus from anyone.
//!
//! The window tests run only with `SOPEN_REQUIRE_WEBKIT=1` (09 §11; macOS CI sets it) and skip with
//! a note without it: WebKit is always on a Mac, a window opening under the developer's hands is
//! what the variable asks for. A scenario that panics, crashes — a call on a stopped task throws
//! in WebKit and aborts the child — or outlives its watchdog fails its test. The tests below the
//! window tests need no window and always run.

use super::*;
use crate::gui::web::{
    after_pass, compose, set_parent, snapshots, Content, Settings, Snapshot, Web,
};
use crate::scheme::{Head, Reply, Request, Server};
use objc2::rc::Weak as ObjcWeak;
use objc2::ClassType;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSWindowStyleMask,
};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop};
use std::io::{Read as _, Write as _};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Set in the child: the scenario it runs.
const CHILD: &str = "SOPEN_WEBKIT_CHILD";
/// The child's last line when its scenario passed.
const PASSED: &str = "sopen-webkit-scenario-passed";
/// A child still running after this long ends, failing its test.
const WATCHDOG: Duration = Duration::from_secs(90);

#[used]
#[link_section = "__DATA,__mod_init_func"]
static CHILD_MAIN: extern "C" fn() = child_main;

/// Before libtest's `main`, on the main thread: in a child, its scenario, then the process ends.
extern "C" fn child_main() {
    let Ok(name) = std::env::var(CHILD) else {
        return;
    };
    std::thread::spawn(|| {
        std::thread::sleep(WATCHDOG);
        eprintln!("the scenario did not end within {WATCHDOG:?}");
        std::process::exit(124);
    });
    let passed = std::panic::catch_unwind(|| scenario(&name)).is_ok();
    if passed {
        println!("{PASSED}");
    }
    let _ = std::io::stdout().flush();
    std::process::exit(if passed { 0 } else { 101 });
}

/// The scenario `name` in a child, its output (`None`: skipped without `SOPEN_REQUIRE_WEBKIT=1`).
fn webkit(name: &str) -> Option<String> {
    let temp = tempfile::tempdir().unwrap();
    webkit_in(name, temp.path())
}

/// [`webkit`] with `temp` as the child's temp folder (`TMPDIR`): the rule-list folders it makes
/// and sweeps are there, and the test sees what the child left in it.
fn webkit_in(name: &str, temp: &std::path::Path) -> Option<String> {
    if std::env::var("SOPEN_REQUIRE_WEBKIT").as_deref() != Ok("1") {
        eprintln!("skipped: {name} opens a WebKit window; SOPEN_REQUIRE_WEBKIT=1 runs it");
        return None;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        // Should the initializer not run, libtest finds no test by this name and runs nothing.
        .args(["--exact", "sopen-webkit-child"])
        .env(CHILD, name)
        .env("TMPDIR", temp)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let text = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
    println!("{text}");
    assert!(out.status.success(), "{name}: {:?}\n{text}", out.status);
    assert!(
        stdout.lines().last() == Some(PASSED),
        "{name} did not run in the child:\n{text}"
    );
    Some(stdout)
}

// ---------------------------------------------------------------------------------------------
// The window tests.

/// A file page served through the scheme from the test's server renders in the view, at its
/// frame: its document and title read back, its stylesheet asked through the scheme, and its
/// snapshot — four colours at their corners, at the window's scale — composed where the frame is.
#[test]
fn wv_a_a_page_served_through_the_scheme_renders() {
    webkit("render");
}

/// X-W1's rule: no call on a task after WebKit stopped it — a stop before its head, mid-body,
/// and after its last byte before its end; what the server sends after the stop is dropped.
#[test]
fn wv_a_no_task_call_after_a_stop() {
    webkit("stops");
}

/// §3.3: a file view loads nothing outside the scheme — an image, a stylesheet, a frame of an
/// outside server never reach it, and the row counts them; a meta refresh or a load out is
/// refused and said; preconnect, dns-prefetch and prefetch hints open no connection — the same
/// pages without the rule list do reach it (the control). Nothing loads before the list is on
/// the view; the list's folder is the process's own, a killed sopen's is swept, and it is gone
/// after the exit.
#[test]
fn wv_a_a_request_outside_the_scheme_is_blocked() {
    let temp = tempfile::tempdir().unwrap();
    let Some(out) = webkit_in("rules", temp.path()) else {
        return;
    };
    let folder = out
        .lines()
        .find_map(|l| l.strip_prefix("rules-folder "))
        .expect("the child names its rule-list folder");
    let folder = std::path::Path::new(folder);
    assert!(folder.starts_with(temp.path()), "{}", folder.display());
    assert!(
        !folder.exists(),
        "{} is left after the exit",
        folder.display()
    );
    let left: Vec<_> = std::fs::read_dir(temp.path()).unwrap().flatten().collect();
    assert!(left.is_empty(), "the child left {left:?}");
}

/// §3.3: scripts are off in a file view — its `<script>` does not run; on (F2's engine half),
/// the same page's does (the control); off again, not.
#[test]
fn wv_a_scripts_off_means_a_script_does_not_run() {
    webkit("scripts");
}

/// §2.5: no cookie is shared between two views — the view that was given a cookie sends it
/// back (the control), another view of the same origin does not.
#[test]
fn wv_a_two_views_do_not_share_cookies() {
    webkit("cookies");
}

/// §2.2 through [`Web`] on the process's own engine: the view goes into winit's view at its rect
/// on the first frame that draws it, hides on a frame that does not or under something egui
/// paints above it (taking the keyboard back first), and while held unshown; the screenshot hook
/// gets its picture; closed, dropped or torn down with its window, it leaves the window, is
/// released, its web process ends, and the key monitor goes with the last view.
#[test]
fn wv_a_the_view_follows_web() {
    webkit("follow");
}

/// §2.4 and the preview (§2.2): the key monitor hands Esc, the F-keys and Cmd+W/Q/digits back to
/// sopen while the page has the keyboard — Esc consumed with its repeats and release, the
/// others passed on — and nothing else; installed, AppKit runs it, removed, it does not (the
/// control). A preview's shield is hit instead of the page and its view never keeps the keyboard;
/// handed over, the page is hit.
#[test]
fn wv_a_the_monitor_and_the_shield() {
    webkit("keys");
}

/// §2.2's ATS measurement: plain `http://` to an IP, to `localhost` and to dotted names, each
/// load's outcome and time printed. The IP and `localhost` load. App Transport Security refuses
/// before any lookup (`-1022`); `sopen-ats.invalid` — a dotted name nothing resolves, so no
/// network — must fail at the lookup instead (`-1003`): ATS let plain http to a name through.
/// `sopen.localhost` (a dotted name the system resolves to loopback) is printed, not required.
#[test]
fn wv_a_ats_and_plain_http() {
    webkit("ats");
}

// ---------------------------------------------------------------------------------------------
// The scenarios (the child, on its main thread).

fn scenario(name: &str) {
    let mtm = MainThreadMarker::new().expect("the initializer runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // No Dock icon, never active: the child takes no focus.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    autoreleasepool(|_| match name {
        "render" => render(mtm),
        "stops" => stops(mtm),
        "rules" => rules(mtm),
        "scripts" => scripts(mtm),
        "cookies" => cookies(mtm),
        "follow" => follow(mtm),
        "keys" => keys(mtm),
        "ats" => ats(mtm),
        other => panic!("no scenario {other}"),
    });
}

fn render(mtm: MainThreadMarker) {
    let rig = Rig::new(mtm);
    let engine = WebKit::start(mtm);
    let site = Site::with(&[
        ("/index.html", "text/html", QUADRANTS.as_bytes()),
        (
            "/style.css",
            "text/css",
            b"body { margin: 0; background: #fff }",
        ),
    ]);
    let start = Instant::now();
    let mut view = rig.page(&engine, &site, false);
    let rect = egui::Rect::from_min_size(egui::pos2(20.0, 10.0), egui::vec2(200.0, 120.0));
    view.show(&rig, rect);
    view.page.load(&view.host.url("index.html"));
    view.until("the page finished", |e| e.contains(&Event::Finished));
    let finished = start.elapsed();
    assert!(
        view.seen.contains(&Event::Document("index.html".into())),
        "{:?}",
        view.seen
    );
    wait("its title", 5.0, || view.title() == "sopen page");
    assert!(site.started("/style.css").is_some(), "{:?}", site.said());
    let frame = view.web().frame();
    assert_eq!(
        (
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height
        ),
        (20.0, 10.0, 200.0, 120.0),
        "the frame is the rect, in points"
    );
    let scale = rig.scale();
    let start = Instant::now();
    let picture = view.snapshot().expect("a picture");
    let took = start.elapsed();
    let [w, h] = picture.size;
    assert_eq!(
        [w, h],
        [(200.0 * scale) as usize, (120.0 * scale) as usize],
        "the window's scale"
    );
    for (x, y, colour) in [
        (w / 4, h / 4, "red"),
        (3 * w / 4, h / 4, "green"),
        (w / 4, 3 * h / 4, "blue"),
        (3 * w / 4, 3 * h / 4, "yellow"),
    ] {
        assert_eq!(colour_of(picture[(x, y)]), colour, "at {x},{y}");
    }
    // The screenshot hook's compose: the read-back is empty where the page is.
    let size = [(400.0 * scale) as usize, (300.0 * scale) as usize];
    let mut window = egui::ColorImage::filled(size, egui::Color32::WHITE);
    compose(
        &mut window,
        scale as f32,
        &[Snapshot {
            rect,
            image: picture,
        }],
    );
    let at = |x: f32, y: f32| window[((x * scale as f32) as usize, (y * scale as f32) as usize)];
    assert_eq!(colour_of(at(30.0, 20.0)), "red");
    assert_eq!(colour_of(at(210.0, 120.0)), "yellow");
    assert_eq!(at(10.0, 5.0), egui::Color32::WHITE, "outside the frame");
    println!(
        "rendered: finished {} ms after the view was made; snapshot {w}x{h} in {:.1} ms",
        finished.as_millis(),
        took.as_secs_f64() * 1e3
    );
    view.page.close();
}

fn stops(mtm: MainThreadMarker) {
    let rig = Rig::new(mtm);
    let engine = WebKit::start(mtm);
    let site = Site::with(&[
        ("/next.html", "text/html", b"<title>next</title>"),
        ("/n1.html", "text/html", b"<title>n1</title>"),
        ("/n2.html", "text/html", b"<title>n2</title>"),
        ("/n3.html", "text/html", b"<title>n3</title>"),
    ]);
    for held in ["/a.html", "/b.html", "/c.html"] {
        site.hold(held);
    }
    let mut view = rig.page(&engine, &site, false);
    view.show(&rig, rig.full());
    let html = big_page(256 * 1024);
    let mut next = 0;
    // A later live request: once its head is called, the drain has gone past every message
    // sent before it — the stopped request's late ones among them.
    let mut later = |view: &mut Shown| {
        next += 1;
        let path = format!("/n{next}.html");
        view.page.load(&view.host.url(&path));
        let ok = spin(5.0, || site.starts(&path) > 0);
        assert!(
            ok,
            "a later request: {:?}\n{:?}\n{:?}",
            site.said(),
            view.log(),
            {
                view.drain_events();
                &view.seen
            }
        );
        let id = site.last_start(&path);
        wait("the later request's head", 5.0, || {
            view.log().contains(&Glue::Call(id, "head"))
        });
    };

    // Before its head.
    view.page.load(&view.host.url("a.html"));
    let (a, reply) = site.take_held("/a.html");
    let raw = reply.raw();
    view.page.load(&view.host.url("next.html"));
    wait("WebKit stops a before its head", 5.0, || {
        view.log().contains(&Glue::Stop(a))
    });
    assert!(site.stopped(a), "the server was told");
    raw.head(Head::whole("text/html", 4));
    raw.data(b"late".to_vec());
    raw.done();
    drop(reply);
    later(&mut view);
    assert_no_call_after_stop(&view.log(), a);

    // Mid-body.
    view.page.load(&view.host.url("b.html"));
    let (b, reply) = site.take_held("/b.html");
    let raw = reply.raw();
    let mut body = reply.head(Head::whole("text/html", html.len() as u64));
    body.data(html[..64 * 1024].to_vec()).unwrap();
    wait("b's first piece reaches WebKit", 5.0, || {
        view.log().contains(&Glue::Call(b, "data"))
    });
    view.page.load(&view.host.url("next.html"));
    wait("WebKit stops b mid-body", 5.0, || {
        view.log().contains(&Glue::Stop(b))
    });
    assert!(site.stopped(b), "the server was told");
    raw.data(html[64 * 1024..].to_vec());
    raw.done();
    drop(body);
    later(&mut view);
    assert_no_call_after_stop(&view.log(), b);

    // After its last byte, before its end.
    view.page.load(&view.host.url("c.html"));
    let (c, reply) = site.take_held("/c.html");
    let raw = reply.raw();
    let mut body = reply.head(Head::whole("text/html", html.len() as u64));
    body.data(html[..WINDOW_PIECE].to_vec()).unwrap();
    body.data(html[WINDOW_PIECE..].to_vec()).unwrap();
    assert_eq!(body.left(), 0, "every byte sent");
    wait("c's last piece reaches WebKit", 5.0, || {
        view.log()
            .iter()
            .filter(|g| **g == Glue::Call(c, "data"))
            .count()
            == 2
    });
    view.page.load(&view.host.url("next.html"));
    wait("WebKit stops c after its last byte", 5.0, || {
        view.log().contains(&Glue::Stop(c))
    });
    assert!(site.stopped(c), "the server was told");
    raw.done();
    drop(body);
    later(&mut view);
    assert_no_call_after_stop(&view.log(), c);
    assert!(!view.log().contains(&Glue::Call(c, "done")));
    println!("stops: before the head, mid-body (64 KiB of 256 KiB), after the last byte: no call after any");
    view.page.close();
}

fn rules(mtm: MainThreadMarker) {
    // A sopen that died left its folder; this one's is made at the start.
    let temp = std::env::temp_dir();
    let stale = temp.join(format!("{RULES_PREFIX}{}", dead_pid()));
    std::fs::create_dir_all(&stale).unwrap();
    let rig = Rig::new(mtm);
    let start = Instant::now();
    let engine = WebKit::start(mtm);
    assert!(!stale.exists(), "a dead process's folder is swept");
    let outside = Http::start();
    let page = format!(
        "<title>out</title><img src='http://127.0.0.1:{p}/i.png'>\
         <link rel=stylesheet href='http://127.0.0.1:{p}/s.css'>\
         <iframe src='http://127.0.0.1:{p}/f.html'></iframe>",
        p = outside.port
    );
    let refresh = format!(
        "<title>refresh</title><meta http-equiv=refresh content='0;url=http://127.0.0.1:{}/meta'>",
        outside.port
    );
    // Hints that open a connection, or look a name up, without a request.
    let hints = Http::start();
    let pre = format!(
        "<title>pre</title><link rel=preconnect href='http://127.0.0.1:{p}/'>\
         <link rel=dns-prefetch href='//127.0.0.1:{p}/'>\
         <link rel=prefetch href='http://127.0.0.1:{p}/pf'>",
        p = hints.port
    );
    let site = Site::with(&[
        ("/out.html", "text/html", page.as_bytes()),
        ("/refresh.html", "text/html", refresh.as_bytes()),
        ("/pre.html", "text/html", pre.as_bytes()),
    ]);
    // The first view, made while the list compiles: its load waits for it.
    let mut view = rig.page(&engine, &site, false);
    view.show(&rig, rig.full());
    view.page.load(&view.host.url("out.html"));
    assert!(view.log().is_empty(), "nothing asked before the list");
    view.until("the page finished", |e| e.contains(&Event::Finished));
    let finished = start.elapsed();
    let compiled = view.log().iter().position(|g| *g == Glue::Ruled);
    let first = view.log().iter().position(|g| matches!(g, Glue::Start(_)));
    assert!(
        compiled.is_some() && compiled < first,
        "the list is on the view before its first request: {:?}",
        view.log()
    );
    spin(1.0, || false);
    view.drain_events();
    assert_eq!(outside.accepted(), 0, "{:?}", outside.log());
    let blocked = view.seen.iter().rev().find_map(|e| match e {
        Event::Blocked(n) => Some(*n),
        _ => None,
    });
    assert_eq!(blocked, Some(3), "the row's count: {:?}", view.seen);
    let ours = rules_dir(&temp);
    let files = std::fs::read_dir(&ours).unwrap().count();
    assert!(files > 0, "the compiled list is in {}", ours.display());
    println!(
        "rules: compiled and the page finished {} ms after the engine started; {blocked:?} outside requests blocked",
        finished.as_millis()
    );
    println!("rules-folder {}", ours.display());
    // Navigations of the main frame out of the scheme — the page's own (a meta refresh) and a
    // load asked of the view — are refused and said, and nothing reaches the outside server: not
    // even the bare connect WebKit makes for a load it is asked before its policy decides.
    let meta = format!("http://127.0.0.1:{}/meta", outside.port);
    view.page.load(&view.host.url("refresh.html"));
    view.until("the page's navigation out refused", |e| {
        e.contains(&Event::Refused {
            target: meta.clone(),
            clicked: false,
        })
    });
    spin(0.5, || false);
    assert_eq!(outside.accepted(), 0, "the refresh: {:?}", outside.log());
    let away = format!("http://127.0.0.1:{}/away", outside.port);
    view.page.load(&away);
    view.until("the load out refused", |e| {
        e.contains(&Event::Refused {
            target: away.clone(),
            clicked: false,
        })
    });
    spin(0.5, || false);
    assert_eq!(outside.accepted(), 0, "{:?}", outside.log());
    view.page.load(&view.host.url("pre.html"));
    view.until("the hints page", |e| {
        e.iter().filter(|e| **e == Event::Finished).count() >= 2
    });
    spin(1.0, || false);
    let ruled_hints = hints.accepted();
    view.page.close();

    // The control: the same page without the list reaches the outside server.
    let (events, rx) = Events::new();
    let host = Host::new();
    let control = engine
        .open_unruled(Opening {
            content: Opened::Page {
                host: host.clone(),
                server: site.server(),
            },
            preview: false,
            scripts: false,
            events,
        })
        .unwrap();
    let mut control = Shown::new(control, host, rx);
    control.show(&rig, rig.full());
    control.page.load(&control.host.url("out.html"));
    wait("the control reaches the outside server", 10.0, || {
        outside.accepted() >= 2
    });
    assert_eq!(
        ruled_hints,
        0,
        "a hint opened a connection: {:?}",
        hints.log()
    );
    control.page.load(&control.host.url("pre.html"));
    wait("the control's hints connect", 5.0, || hints.accepted() > 0);
    println!(
        "rules: hints (preconnect, dns-prefetch, prefetch): {ruled_hints} connections accepted \
         with the list, {} without (the control)",
        hints.accepted()
    );
    control.page.close();
}

fn scripts(mtm: MainThreadMarker) {
    let rig = Rig::new(mtm);
    let engine = WebKit::start(mtm);
    let site = Site::with(&[(
        "/js.html",
        "text/html",
        b"<title>before</title><script>document.title = 'ran'</script>",
    )]);
    let mut view = rig.page(&engine, &site, false);
    view.show(&rig, rig.full());
    view.page.load(&view.host.url("js.html"));
    let title = |view: &mut Shown, loads: usize| {
        view.until("the page finished", |e| {
            e.iter().filter(|e| **e == Event::Finished).count() >= loads
        });
        spin(0.5, || false);
        view.title()
    };
    assert_eq!(title(&mut view, 1), "before", "scripts off");
    view.page.set_scripts(true);
    view.page.reload();
    assert_eq!(title(&mut view, 2), "ran", "scripts on (the control)");
    view.page.set_scripts(false);
    view.page.reload();
    assert_eq!(title(&mut view, 3), "before", "off again");
    view.page.close();
}

fn cookies(mtm: MainThreadMarker) {
    let rig = Rig::new(mtm);
    let engine = WebKit::start(mtm);
    let server = Http::start();
    let origin = format!("http://127.0.0.1:{}", server.port);
    let mut one = rig.address(&engine, &origin);
    let mut two = rig.address(&engine, &origin);
    one.show(&rig, rig.full());
    two.show(&rig, rig.full());
    let visit = |view: &mut Shown, path: &str, title: &str| {
        let loads = view.count(&Event::Finished);
        view.page.load(&format!("{origin}{path}"));
        view.until(path, |e| {
            e.iter().filter(|e| **e == Event::Finished).count() > loads
        });
        let ok = spin(5.0, || view.title() == title);
        assert!(ok, "{path}: the title {:?}, not {title:?}", view.title());
    };
    visit(&mut one, "/set", "cookie[set]");
    visit(&mut one, "/check", "cookie[sopen=1]");
    visit(&mut two, "/check", "cookie[]");
    assert_eq!(
        server.log(),
        [
            "GET /set HTTP/1.1 | ",
            "GET /check HTTP/1.1 | sopen=1",
            "GET /check HTTP/1.1 | ",
        ],
        "the view given the cookie sends it back (the control), the other view does not"
    );
    one.page.close();
    two.page.close();
}

fn follow(mtm: MainThreadMarker) {
    let rig = Rig::new(mtm);
    let ctx = egui::Context::default();
    set_parent(&ctx, rig.parent());
    let site = Site::with(&[("/index.html", "text/html", QUADRANTS.as_bytes())]);
    let rect = egui::Rect::from_min_size(egui::pos2(40.0, 30.0), egui::vec2(160.0, 100.0));
    let mut web = Web::new(
        Content::Page {
            server: site.server(),
        },
        Settings::default(),
    )
    .expect("this process's engine, on its main thread");
    assert!(!web.scripts(), "a file view starts with scripts off");
    web.load("index.html");
    // What touches the WKWebView is in a pool of its own, so that nothing autoreleased keeps it.
    let (gone, pid) = autoreleasepool(|_| {
        frame(&ctx, Some(&mut web), rect, false);
        let view = rig.web_views().pop().expect("a WKWebView in winit's view");
        assert_eq!(rig.web_views().len(), 1);
        let f = view.frame();
        assert_eq!(
            (f.origin.x, f.origin.y, f.size.width, f.size.height),
            (40.0, 30.0, 160.0, 100.0)
        );
        assert!(!view.isHidden());
        assert!(MONITOR.with(|m| m.borrow().is_some()), "the monitor is on");
        let mut seen = vec![];
        wait("the page finished", 10.0, || {
            seen.extend(web.events());
            seen.contains(&Event::Finished)
        });
        // A frame that does not draw it.
        frame(&ctx, None, rect, false);
        assert!(view.isHidden(), "not drawn");
        frame(&ctx, Some(&mut web), rect, false);
        assert!(!view.isHidden(), "drawn again");
        // Something painted above it — the page holding the keyboard first.
        rig.window.makeFirstResponder(Some(&view));
        assert_eq!(web.keyboard(), Keyboard::Page);
        frame(&ctx, Some(&mut web), rect, true);
        frame(&ctx, Some(&mut web), rect, true);
        assert!(view.isHidden(), "under a layer above it");
        assert!(rig.host_has_keyboard(), "the keyboard came back first");
        frame(&ctx, Some(&mut web), rect, false);
        assert!(!view.isHidden());
        // Held unshown.
        web.set_shown(false);
        frame(&ctx, Some(&mut web), rect, false);
        assert!(view.isHidden(), "held unshown");
        web.set_shown(true);
        frame(&ctx, Some(&mut web), rect, false);
        assert!(!view.isHidden());
        // The screenshot hook.
        let shots = snapshots(&ctx);
        wait("the hook's picture", 5.0, || shots.ready());
        let pictures = shots.take();
        assert_eq!(pictures.len(), 1);
        assert_eq!(pictures[0].rect, rect);
        assert_eq!(colour_of(pictures[0].image[(10, 10)]), "red");
        // Closed: out of the window, the monitor off.
        let handles = handles(&view);
        drop(view);
        web.close();
        assert!(rig.web_views().is_empty(), "out of winit's view");
        assert!(
            MONITOR.with(|m| m.borrow().is_none()),
            "the monitor went with the last view"
        );
        handles
    });
    // Released, its web process gone.
    released(gone, pid);
    // Dropped, and torn down with its window.
    for teardown in [false, true] {
        let (gone, pid) = autoreleasepool(|_| {
            let server = site.server();
            let mut web = Web::new(Content::Page { server }, Settings::default()).unwrap();
            web.load("index.html");
            frame(&ctx, Some(&mut web), rect, false);
            let view = rig.web_views().pop().expect("a WKWebView");
            wait("loaded", 10.0, || web.events().contains(&Event::Finished));
            let handles = handles(&view);
            drop(view);
            if teardown {
                crate::gui::web::teardown(&ctx);
            } else {
                drop(web);
            }
            assert!(rig.web_views().is_empty());
            handles
        });
        released(gone, pid);
    }
    assert!(PAGES.with(|p| p.borrow().is_empty()), "no view is left");
}

fn keys(mtm: MainThreadMarker) {
    let rig = Rig::new(mtm);
    let engine = WebKit::start(mtm);
    let site = Site::with(&[("/index.html", "text/html", QUADRANTS.as_bytes())]);
    let mut view = rig.page(&engine, &site, false);
    view.show(&rig, rig.full());
    view.page.load(&view.host.url("index.html"));
    view.until("the page finished", |e| e.contains(&Event::Finished));
    let web = view.web();
    let focus = || {
        assert!(rig.window.makeFirstResponder(Some(&web)));
        assert!(view.page.page_has_keyboard());
    };
    let key = |code: u16, chars: &str, flags: NSEventModifierFlags, repeat: bool, down: bool| {
        rig.key(code, chars, flags, repeat, down)
    };
    let none = NSEventModifierFlags::empty();
    let cmd = NSEventModifierFlags::Command;

    // The page has the keyboard.
    focus();
    assert!(
        on_key(&key(0, "a", none, false, true)),
        "typing is the page's"
    );
    assert!(view.page.page_has_keyboard());
    assert!(
        on_key(&key(48, "\t", none, false, true)),
        "Tab is the page's"
    );
    assert!(
        on_key(&key(8, "c", cmd, false, true)),
        "Cmd+C is the page's"
    );
    assert!(view.page.page_has_keyboard());
    assert!(
        !on_key(&key(KEY_ESCAPE, "\u{1b}", none, false, true)),
        "Esc is consumed"
    );
    assert!(rig.host_has_keyboard(), "Esc took the keyboard back");
    assert!(
        !on_key(&key(KEY_ESCAPE, "\u{1b}", none, true, true)),
        "its repeat too"
    );
    assert!(
        !on_key(&key(KEY_ESCAPE, "\u{1b}", none, false, false)),
        "and its release"
    );
    for (code, chars, flags, what) in [
        (99, "\u{F706}", none, "F3"),
        (96, "\u{F708}", NSEventModifierFlags::Shift, "Shift+F5"),
        (13, "w", cmd, "Cmd+W"),
        (12, "q", cmd, "Cmd+Q"),
        (18, "1", cmd, "Cmd+1"),
    ] {
        focus();
        assert!(
            on_key(&key(code, chars, flags, false, true)),
            "{what} goes on"
        );
        assert!(rig.host_has_keyboard(), "{what} took the keyboard back");
    }
    focus();
    let cmd_shift = cmd | NSEventModifierFlags::Shift;
    assert!(on_key(&key(13, "w", cmd_shift, false, true)));
    assert!(view.page.page_has_keyboard(), "Shift+Cmd+W is the page's");
    // sopen has the keyboard: every key goes on, Esc included.
    rig.window.makeFirstResponder(Some(&rig.host));
    assert!(on_key(&key(KEY_ESCAPE, "\u{1b}", none, false, true)));
    assert!(on_key(&key(KEY_ESCAPE, "\u{1b}", none, false, false)));

    // Installed, AppKit runs it on what the application sends; removed, it does not.
    let app = NSApplication::sharedApplication(mtm);
    engine.monitor(true);
    focus();
    app.sendEvent(&key(96, "\u{F708}", none, false, true));
    assert!(
        rig.host_has_keyboard(),
        "the installed monitor took F5's keyboard back"
    );
    engine.monitor(false);
    focus();
    app.sendEvent(&key(96, "\u{F708}", none, false, true));
    assert!(
        view.page.page_has_keyboard(),
        "without the monitor nothing takes it back"
    );

    view.page.close();

    // A preview: its shield is hit, and its view never keeps the keyboard.
    let mut preview = rig.page(&engine, &site, true);
    let rect = egui::Rect::from_min_size(egui::pos2(50.0, 40.0), egui::vec2(200.0, 150.0));
    preview.show(&rig, rect);
    preview.page.load(&preview.host.url("index.html"));
    preview.until("the preview finished", |e| e.contains(&Event::Finished));
    let centre = egui::pos2(150.0, 115.0);
    assert!(is::<Shield>(&rig.hit(centre)), "the shield is hit");
    assert!(is::<HostView>(&rig.hit(egui::pos2(10.0, 10.0))));
    let pweb = preview.web();
    rig.window.makeFirstResponder(Some(&pweb));
    assert!(
        on_key(&key(0, "a", none, false, true)),
        "the key is sopen's"
    );
    assert!(
        rig.host_has_keyboard(),
        "a preview's view never keeps the keyboard"
    );
    // Handed over.
    preview.page.set_preview(false);
    let hit = rig.hit(centre);
    assert!(
        hit.isDescendantOf(&pweb),
        "the page is hit: {}",
        hit.class().name().to_string_lossy()
    );
    preview.page.close();
}

fn ats(mtm: MainThreadMarker) {
    let rig = Rig::new(mtm);
    let engine = WebKit::start(mtm);
    let server = Http::start();
    let port = server.port;
    for (what, url) in [
        ("an IP", format!("http://127.0.0.1:{port}/ip")),
        ("localhost", format!("http://localhost:{port}/localhost")),
        (
            "a dotted loopback name",
            format!("http://sopen.localhost:{port}/dotted"),
        ),
        ("a dotted name", "http://sopen-ats.invalid/".to_string()),
    ] {
        let mut view = rig.address(&engine, &url);
        view.show(&rig, rig.full());
        let start = Instant::now();
        view.page.load(&url);
        view.until(&url, |e| {
            e.iter()
                .any(|e| matches!(e, Event::Finished | Event::Failed(_)))
        });
        let ms = start.elapsed().as_millis();
        let outcome = view
            .seen
            .iter()
            .find_map(|e| match e {
                Event::Finished => Some("loaded".to_string()),
                Event::Failed(why) => Some(format!("failed: {why}")),
                _ => None,
            })
            .unwrap();
        println!("ATS plain http to {what} ({url}): {outcome} in {ms} ms");
        assert!(!outcome.contains("-1022"), "ATS refused {url}");
        match what {
            "a dotted name" => assert!(outcome.contains("-1003"), "{url}: {outcome}"),
            "a dotted loopback name" => {}
            _ => assert_eq!(outcome, "loaded", "{url}"),
        }
        view.page.close();
    }
    let log = server.log();
    assert!(log.iter().any(|l| l.starts_with("GET /ip ")), "{log:?}");
    assert!(
        log.iter().any(|l| l.starts_with("GET /localhost ")),
        "{log:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// The child's stage.

/// What a test page is: four colours at its corners, its title.
const QUADRANTS: &str = "<!doctype html><title>sopen page</title>\
    <link rel=stylesheet href=style.css>\
    <div style='position:fixed;left:0;top:0;width:50%;height:50%;background:#f00'></div>\
    <div style='position:fixed;right:0;top:0;width:50%;height:50%;background:#0f0'></div>\
    <div style='position:fixed;left:0;bottom:0;width:50%;height:50%;background:#00f'></div>\
    <div style='position:fixed;right:0;bottom:0;width:50%;height:50%;background:#ff0'></div>";

/// The first of two pieces of [`stops`]'s last page.
const WINDOW_PIECE: usize = 128 * 1024;

define_class!(
    /// The stand-in for winit's view: flipped, as winit's is, and able to take the keyboard.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    struct HostView;

    impl HostView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }
    }
);

/// The app's window, borderless and off every screen, its content view the host.
struct Rig {
    window: Retained<NSWindow>,
    host: Retained<HostView>,
}

impl Rig {
    fn new(mtm: MainThreadMarker) -> Self {
        let size = NSSize::new(400.0, 300.0);
        let frame = NSRect::new(NSPoint::new(-30000.0, -30000.0), size);
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), size);
        let host: Retained<HostView> =
            unsafe { msg_send![HostView::alloc(mtm), initWithFrame: bounds] };
        window.setContentView(Some(&host));
        window.orderFrontRegardless();
        Rig { window, host }
    }

    /// The window's handle, as the shell gives it.
    fn parent(&self) -> Parent {
        Parent::AppKit(NonNull::new(Retained::as_ptr(&self.host) as *mut c_void).unwrap())
    }

    fn scale(&self) -> f64 {
        self.window.backingScaleFactor()
    }

    fn full(&self) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0))
    }

    fn page(&self, engine: &WebKit, site: &Site, preview: bool) -> Shown {
        let (events, rx) = Events::new();
        let host = Host::new();
        let page = engine
            .open_page(Opening {
                content: Opened::Page {
                    host: host.clone(),
                    server: site.server(),
                },
                preview,
                scripts: false,
                events,
            })
            .unwrap();
        Shown::new(page, host, rx)
    }

    fn address(&self, engine: &WebKit, origin: &str) -> Shown {
        let (events, rx) = Events::new();
        let page = engine
            .open_page(Opening {
                content: Opened::Address {
                    origin: origin.to_string(),
                },
                preview: false,
                scripts: true,
                events,
            })
            .unwrap();
        Shown::new(page, Host::new(), rx)
    }

    /// The WKWebViews in the host, in order.
    fn web_views(&self) -> Vec<Retained<WKWebView>> {
        self.host
            .subviews()
            .iter()
            .filter_map(|v| v.downcast::<WKWebView>().ok())
            .collect()
    }

    fn host_has_keyboard(&self) -> bool {
        self.window.firstResponder().is_some_and(|r| {
            std::ptr::eq(
                Retained::as_ptr(&r).cast::<HostView>(),
                Retained::as_ptr(&self.host),
            )
        })
    }

    /// The view AppKit's hit test finds at `at` (the host's points).
    fn hit(&self, at: egui::Pos2) -> Retained<NSView> {
        let point = NSPoint::new(f64::from(at.x), f64::from(at.y));
        let sup = unsafe { self.host.superview() }.expect("the window's frame view");
        let point = self.host.convertPoint_toView(point, Some(&sup));
        sup.hitTest(point).expect("something is hit")
    }

    /// A key event of the window.
    fn key(
        &self,
        code: u16,
        chars: &str,
        flags: NSEventModifierFlags,
        repeat: bool,
        down: bool,
    ) -> Retained<NSEvent> {
        let chars = NSString::from_str(chars);
        let kind = if down {
            NSEventType::KeyDown
        } else {
            NSEventType::KeyUp
        };
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind,
            NSPoint::new(0.0, 0.0),
            flags,
            0.0,
            self.window.windowNumber(),
            None,
            &chars,
            &chars,
            repeat,
            code,
        )
        .expect("a key event")
    }
}

/// A view of the engine's, drawn at a rect, and what it said.
struct Shown {
    page: Page,
    host: Host,
    events: Receiver<Event>,
    seen: Vec<Event>,
}

impl Shown {
    fn new(page: Page, host: Host, events: Receiver<Event>) -> Self {
        Shown {
            page,
            host,
            events,
            seen: vec![],
        }
    }

    fn show(&mut self, rig: &Rig, rect: egui::Rect) {
        self.page.attach(rig.parent());
        self.page.place(rect, rig.scale() as f32);
        self.page.set_hidden(false);
    }

    fn web(&self) -> Retained<WKWebView> {
        self.page.inner().web().expect("open")
    }

    fn log(&self) -> Vec<Glue> {
        self.page.inner().log.borrow().clone()
    }

    fn title(&self) -> String {
        unsafe { self.web().title() }.map_or_else(String::new, |t| t.to_string())
    }

    fn drain_events(&mut self) {
        self.seen.extend(self.events.try_iter());
    }

    fn count(&mut self, event: &Event) -> usize {
        self.drain_events();
        self.seen.iter().filter(|e| *e == event).count()
    }

    /// Until what it said so far passes `done`.
    fn until(&mut self, what: &str, mut done: impl FnMut(&[Event]) -> bool) {
        let ok = spin(10.0, || {
            self.seen.extend(self.events.try_iter());
            done(&self.seen)
        });
        assert!(ok, "{what}: {:?}", self.seen);
    }

    fn snapshot(&mut self) -> Option<egui::ColorImage> {
        let got: Rc<RefCell<Option<Option<egui::ColorImage>>>> = Rc::default();
        let into = got.clone();
        self.page
            .snapshot(Box::new(move |picture| *into.borrow_mut() = Some(picture)));
        wait("the snapshot", 5.0, || got.borrow().is_some());
        let picture = got.borrow_mut().take().flatten();
        picture
    }
}

/// One frame of a window drawing `web` (or not) at `rect`, with a layer painted over it (`over`),
/// and the shell's after-frame step.
fn frame(ctx: &egui::Context, web: Option<&mut Web>, rect: egui::Rect, over: bool) {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(400.0, 300.0),
        )),
        ..Default::default()
    };
    let mut web = web;
    let mut out = ctx.run_ui(raw, |ui| {
        if let Some(web) = web.as_mut() {
            web.ui(ui, rect);
        }
        if over {
            egui::Area::new(egui::Id::unique("over the page"))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.center())
                .show(ui.ctx(), |ui| {
                    ui.label("a dialog");
                });
        }
        after_pass(ui.ctx());
    });
    out.textures_delta.clear();
}

/// A weak reference to `view` and its web content process (WebKit's own word, for this test).
fn handles(view: &WKWebView) -> (ObjcWeak<WKWebView>, i32) {
    let pid: i32 = unsafe { msg_send![view, _webProcessIdentifier] };
    assert!(pid > 0, "a web process");
    (ObjcWeak::new(view), pid)
}

/// The view is released and its web process ends, within 2 s (09 WV-L1).
fn released(view: ObjcWeak<WKWebView>, pid: i32) {
    wait("the WKWebView released", 2.0, || view.load().is_none());
    let start = Instant::now();
    wait("its web process ended", 2.0, || !alive(pid));
    println!(
        "closed: released, its web process ended {} ms later",
        start.elapsed().as_millis()
    );
}

/// The main run loop, turned until `done` or `secs` passed; whether `done`.
fn spin(secs: f64, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        autoreleasepool(|_| unsafe {
            let until = NSDate::dateWithTimeIntervalSinceNow(0.005);
            NSRunLoop::currentRunLoop().runMode_beforeDate(NSDefaultRunLoopMode, &until);
        });
    }
}

fn wait(what: &str, secs: f64, done: impl FnMut() -> bool) {
    assert!(spin(secs, done), "{what}: not within {secs} s");
}

/// No call on `id` after its stop, and the stop came.
fn assert_no_call_after_stop(log: &[Glue], id: RequestId) {
    let stop = log
        .iter()
        .position(|g| *g == Glue::Stop(id))
        .expect("the stop");
    let after: Vec<_> = log[stop..]
        .iter()
        .filter(|g| matches!(g, Glue::Call(i, _) if *i == id))
        .collect();
    assert!(
        after.is_empty(),
        "calls after the stop: {after:?} in {log:?}"
    );
}

/// Whether `view` is a `T`.
fn is<T: ClassType>(view: &NSView) -> bool {
    view.isKindOfClass(T::class())
}

/// The colour name of a pixel of a test page.
fn colour_of(c: egui::Color32) -> &'static str {
    let [r, g, b, _] = c.to_array();
    let (hi, lo) = (|v: u8| v > 180, |v: u8| v < 90);
    match (hi(r), hi(g), hi(b), lo(r), lo(g), lo(b)) {
        (true, _, _, _, true, true) => "red",
        (_, true, _, true, _, true) => "green",
        (_, _, true, true, true, _) => "blue",
        (true, true, _, _, _, true) => "yellow",
        (true, true, true, _, _, _) => "white",
        _ => "other",
    }
}

/// An HTML page of `len` bytes.
fn big_page(len: usize) -> Vec<u8> {
    let mut page = b"<!doctype html><title>big</title><p>".to_vec();
    page.resize(len, b'x');
    page
}

/// The pid of a process that has ended.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
    child.wait().unwrap();
    child.id()
}

// ---------------------------------------------------------------------------------------------
// The test's page server and its outside HTTP server.

/// What the page server was told.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Said {
    Start(RequestId, String),
    Stop(RequestId),
}

#[derive(Default)]
struct SiteState {
    files: HashMap<String, (&'static str, Vec<u8>)>,
    /// Paths whose requests are kept for the scenario to answer.
    hold: Vec<String>,
    held: Vec<(Request, Reply)>,
    said: Vec<Said>,
}

/// A page server answering from memory at once, or keeping a request for the scenario.
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

    fn starts(&self, path: &str) -> usize {
        self.said()
            .iter()
            .filter(|s| matches!(s, Said::Start(_, p) if p == path))
            .count()
    }

    fn started(&self, path: &str) -> Option<RequestId> {
        self.said().iter().rev().find_map(|s| match s {
            Said::Start(id, p) if p == path => Some(*id),
            _ => None,
        })
    }

    fn last_start(&self, path: &str) -> RequestId {
        self.started(path).expect("started")
    }

    fn stopped(&self, id: RequestId) -> bool {
        self.said().contains(&Said::Stop(id))
    }

    /// The kept request for `path`, once WebKit asked for it.
    fn take_held(&self, path: &str) -> (RequestId, Reply) {
        let mut got = None;
        wait(&format!("WebKit asks for {path}"), 5.0, || {
            let mut state = self.0.lock().unwrap();
            if let Some(at) = state.held.iter().position(|(r, _)| r.path == path) {
                let (request, reply) = state.held.remove(at);
                got = Some((request.id, reply));
            }
            got.is_some()
        });
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

/// An HTTP server on 127.0.0.1 that logs every connection it accepts (at the accept, a request
/// or not) and each request's first line and cookie: `/set` sets a cookie, every page's title
/// says the cookie it was sent.
struct Http {
    port: u16,
    accepted: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<String>>>,
}

impl Http {
    fn start() -> Self {
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
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut buf = vec![0u8; 8192];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let Some(first) = head.lines().next().filter(|l| !l.is_empty()) else {
                        return;
                    };
                    let cookie = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Cookie: ").or(l.strip_prefix("cookie: ")))
                        .unwrap_or("")
                        .to_string();
                    lines.lock().unwrap().push(format!("{first} | {cookie}"));
                    let set = if first.starts_with("GET /set ") {
                        "Set-Cookie: sopen=1; Path=/\r\n"
                    } else {
                        ""
                    };
                    let body = match set {
                        "" => format!("<title>cookie[{cookie}]</title>"),
                        _ => "<title>cookie[set]</title>".to_string(),
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n{set}Content-Length: {}\r\n\
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

// ---------------------------------------------------------------------------------------------
// Without a window.

/// The engine is made on the main thread only: anywhere else `Web::new` gets the reason.
#[test]
fn the_engine_is_not_made_off_the_main_thread() {
    let why = engine().err().expect("not on a test's thread");
    assert!(why.contains("main thread"), "{why}");
}

/// §3.3's list: every URL blocked, then the scheme, `data:` and `blob:` let through — in that
/// order, and nothing else.
#[test]
fn the_rule_list_lets_through_only_the_scheme_data_and_blob() {
    let rules: serde_json::Value = serde_json::from_str(RULES).unwrap();
    let rules: Vec<(String, String)> = rules
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["trigger"]["url-filter"].as_str().unwrap().to_string(),
                r["action"]["type"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let rules: Vec<(&str, &str)> = rules
        .iter()
        .map(|(f, a)| (f.as_str(), a.as_str()))
        .collect();
    assert_eq!(
        rules,
        [
            (".*", "block"),
            ("^sopen-page:", "ignore-previous-rules"),
            ("^data:", "ignore-previous-rules"),
            ("^blob:", "ignore-previous-rules"),
        ]
    );
    assert_eq!(crate::scheme::SCHEME, "sopen-page");
}

/// A rule-list folder of a process that is gone is swept; a live process's, and anything not
/// named as one, stay.
#[test]
fn a_dead_processs_rule_folder_is_swept() {
    let temp = tempfile::tempdir().unwrap();
    let dead = temp.path().join(format!("{RULES_PREFIX}{}", dead_pid()));
    let mine = rules_dir(temp.path());
    let other = temp.path().join(format!("{RULES_PREFIX}notapid"));
    let unrelated = temp.path().join("sopen-something-else");
    for dir in [&dead, &mine, &other, &unrelated] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("ContentRuleList-x"), b"x").unwrap();
    }
    sweep(temp.path());
    assert!(!dead.exists());
    assert!(mine.exists() && other.exists() && unrelated.exists());
}

/// The monitor reads the keys [`route`] tells apart: Esc and the F-keys by their key codes, W
/// and Q by their characters, the digit row; ⌘ as egui-winit reports it.
#[test]
fn the_monitor_reads_keys_as_the_route_does() {
    use egui::Key::*;
    let codes = [
        (53, Escape),
        (122, F1),
        (120, F2),
        (99, F3),
        (118, F4),
        (96, F5),
        (97, F6),
        (98, F7),
        (100, F8),
        (101, F9),
        (109, F10),
        (103, F11),
        (111, F12),
        (18, Num1),
        (29, Num0),
    ];
    for (code, key) in codes {
        assert_eq!(key_of(code, ""), Some(key), "{code}");
    }
    assert_eq!(key_of(13, "w"), Some(W));
    assert_eq!(
        key_of(6, "W"),
        Some(W),
        "by the character, wherever the layout puts it"
    );
    assert_eq!(key_of(12, "q"), Some(Q));
    assert_eq!(key_of(0, "a"), None);
    let cmd = modifiers_of(NSEventModifierFlags::Command);
    assert!(cmd.mac_cmd && cmd.command && !cmd.ctrl);
    assert_eq!(route(Keyboard::Page, W, cmd), Route::TakeBackAndAct);
    let ctrl = modifiers_of(NSEventModifierFlags::Control);
    assert_eq!(
        route(Keyboard::Page, W, ctrl),
        Route::Page,
        "a Mac's ⌃W is the page's"
    );
}

/// An address window keeps to its origin — scheme, host and port, a default port spelled or not,
/// no user info counted; a file view keeps to its own scheme host, and its subframes may show
/// `data:`, `blob:` and `about:`.
#[test]
fn a_view_keeps_to_its_scope() {
    let address = Scope::Address(origin("http://127.0.0.1:8080/app/"));
    assert!(address.keeps("http://127.0.0.1:8080/other?q#f", true));
    assert!(address.keeps("HTTP://user:pw@127.0.0.1:8080/", true));
    assert!(!address.keeps("http://127.0.0.1:8081/", true));
    assert!(!address.keeps("https://127.0.0.1:8080/", true));
    assert!(!address.keeps("http://localhost:8080/", true));
    assert!(
        address.keeps("https://elsewhere.example/", false),
        "subframes load"
    );
    let default = Scope::Address(origin("https://example.test/"));
    assert!(default.keeps("https://example.test:443/x", true));
    assert!(!default.keeps("https://example.test.evil/", true));
    let v6 = origin("http://[::1]:9000/").unwrap();
    assert_eq!((v6.host.as_str(), v6.port), ("[::1]", Some(9000)));
    let host = Host::new();
    let page = Scope::Page(host.clone());
    assert!(page.keeps(&host.url("sub/a b.html"), true));
    assert!(
        !page.keeps(&Host::new().url("a.html"), true),
        "another view's"
    );
    assert!(!page.keeps("https://example.test/", true));
    assert!(
        !page.keeps("data:text/html,x", true),
        "a link to data: is not followed"
    );
    assert!(page.keeps("data:text/html,x", false));
    assert!(page.keeps("about:srcdoc", false));
    assert!(!page.keeps("https://example.test/", false));
    assert_eq!(page.document(&host.url("sub/a b.html")), "sub/a b.html");
    assert_eq!(page.document(&host.url("%E2%82%AC.html")), "%E2%82%AC.html");
    assert_eq!(decoded("%E2%82%AC.html"), "€.html");
}
