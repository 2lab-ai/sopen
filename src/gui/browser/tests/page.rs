//! WV-D (09 §5–§6, §10's WV-7, WV-8, WV-P1, WV-P2, WV-S6's preview half): page files in the
//! browser — the page as the preview and as the Enter viewer, its views 1, 2 and 3, F2, its
//! root, its rows, its teardown, the fallbacks to its source, a remote page through its page
//! server, the terminal's hand-off — on the test kit with the recording [`FakeEngine`] (no
//! WebKit): the fake answers as WebKit would ask, through the view's real scheme table, and the
//! real page server (`stream::page`) answers it from the folder.

use super::*;
use crate::gui::web::fake::{Did, FakeEngine};
use crate::gui::web::{use_engine, Event as WebEvent, Unavailable, OFF};
use crate::scheme::{Call, Head, RequestId};

const INDEX: &str = "<!doctype html>\n<html><head><link rel=stylesheet href=\"style.css\"></head>\n<body><h1>Index</h1><img src=\"img.png\"><img src=\"http://127.0.0.1:9/outside.png\"></body></html>\n";
const STYLE: &str = "body { background: #ffd; }\n";
const STYLED: &str =
    "<?xml version=\"1.0\"?>\n<?xml-stylesheet type=\"text/xsl\" href=\"s.xsl\"?>\n<doc/>\n";
const XSL: &str =
    "<xsl:stylesheet version=\"1.0\" xmlns:xsl=\"http://www.w3.org/1999/XSL/Transform\"/>\n";
const FEED: &str =
    "<?xml version=\"1.0\"?>\n<rss version=\"2.0\"><channel><title>feed</title></channel></rss>\n";

/// The page tree: `index.html` (its stylesheet, a picture of the folder and one outside it),
/// a page in `sub/`, an `.htm`, a styled XML and its `.xsl`, a plain XML, text, links named as
/// another kind than their targets; `outside.txt` beside the folder, outside
/// every root; a 33 MiB `big.bin` (sparse) past the preview's budget.
fn page_tree(root: &Path) {
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("index.html"), INDEX).unwrap();
    std::fs::write(root.join("style.css"), STYLE).unwrap();
    std::fs::write(
        root.join("sub/inner.html"),
        "<p>inner <a href=\"../index.html\">up</a></p>\n",
    )
    .unwrap();
    std::fs::write(root.join("styled.xml"), STYLED).unwrap();
    std::fs::write(root.join("s.xsl"), XSL).unwrap();
    std::fs::write(root.join("feed.xml"), FEED).unwrap();
    std::fs::write(root.join("page.htm"), "<p>an htm page</p>\n").unwrap();
    // Links whose names say another kind than their targets (one decision, from the target).
    for (link, to) in [
        ("x.html", "feed.xml"),
        ("y.html", "notes.txt"),
        ("alias.xml", "index.html"),
        ("alias.txt", "index.html"),
    ] {
        std::os::unix::fs::symlink(to, root.join(link)).unwrap();
    }
    std::fs::write(root.join("notes.txt"), "plain notes\n").unwrap();
    image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8 * 30, y as u8 * 30, 90]))
        .save(root.join("img.png"))
        .unwrap();
    std::fs::write(root.parent().unwrap().join("outside.txt"), "outside\n").unwrap();
    let big = std::fs::File::create(root.join("big.bin")).unwrap();
    big.set_len(33 << 20).unwrap();
}

/// The browser on the page tree (`label`: posing as host `box`, every file read as from a
/// remote), opened on `sub` of it when `inside`; this thread's engine the fake.
fn page_fixture(label: Option<&str>, inside: bool) -> (Fx, FakeEngine) {
    page_fixture_with(label, inside, local_connect())
}

/// [`page_fixture`] with the viewers' and the preview's connections made by `connect`.
fn page_fixture_with(label: Option<&str>, inside: bool, connect: Connect) -> (Fx, FakeEngine) {
    let fake = FakeEngine::native();
    fake.install();
    let tmp = tempfile::tempdir().unwrap();
    let top = std::fs::canonicalize(tmp.path()).unwrap().join("root");
    page_tree(&top);
    let opened = if inside { top.join("sub") } else { top };
    let root = opened.to_str().unwrap().to_string();
    let cache = tmp.path().join("cache");
    let launched: Launched = Arc::default();
    let record = launched.clone();
    let b = Browser::new(Start {
        fs: Box::new(LocalFs::new()),
        connect,
        label: label.map(str::to_string),
        destination: label.map(str::to_string),
        abs: root.clone(),
        cache: cache.clone(),
        state_dir: tmp.path().join("state"),
        launch: Arc::new(move |abs: &str, is_dir: bool| {
            record.lock().unwrap().push((abs.to_string(), is_dir));
            Ok(format!("opened {abs}"))
        }),
        git: None,
    });
    let mut fx = Fx {
        _tmp: tmp,
        root,
        cache,
        drv: Driver::new(1000.0, 700.0),
        b,
        launched,
    };
    fx.settle();
    (fx, fake)
}

/// A `Connect` on this machine that counts the connections it makes.
fn counted() -> (Connect, impl Fn() -> usize) {
    let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = n.clone();
    let connect: Connect = Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>)
    });
    (connect, move || n.load(std::sync::atomic::Ordering::SeqCst))
}

/// A window of its own (the terminal's switch opens one: a new context) drawing the browser
/// until `done`, at most 20 passes.
fn passes_until(drv: &mut Driver, fx: &mut Fx, done: impl Fn(&Browser) -> bool) -> bool {
    for _ in 0..20 {
        drv.idle(&mut fx.b, 1);
        if done(&fx.b) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// A terminal's passes over the browser (its own context, marked as the terminal backend's).
fn terminal() -> Driver {
    let t = Driver::new(1000.0, 700.0);
    crate::gui::mark_terminal(&t.ctx);
    t
}

/// [`page_fixture`] in dual mode: Enter opens each file from the start, no preview made first.
fn dual(label: Option<&str>) -> (Fx, FakeEngine) {
    let (mut fx, fake) = page_fixture(label, false);
    fx.b.mode = view::Mode::Dual;
    fx.drv.idle(&mut fx.b, 2);
    (fx, fake)
}

/// View `n` loaded `doc` as WebKit says it: the main frame started, the document on screen,
/// finished; then the window settles.
fn loaded(fx: &mut Fx, fake: &FakeEngine, n: usize, doc: &str) {
    fx.drv.until(&mut fx.b, 20, |_| fake.views() > n);
    let events = fake.events(n);
    events.send(WebEvent::Loading);
    events.send(WebEvent::Document(doc.to_string()));
    events.send(WebEvent::Finished);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
}

/// Enter on `name`; its page is view `n`, loaded.
fn enter_page(fx: &mut Fx, fake: &FakeEngine, name: &str, n: usize) {
    fx.select(name);
    fx.key(Key::Enter);
    loaded(fx, fake, n, name);
    assert!(fx.b.viewer.is_some(), "{name}: {:?}", fx.b.status.text);
}

/// Enter on `name`, whose viewer is not a page: settled as any viewer.
fn enter_other(fx: &mut Fx, name: &str) {
    fx.select(name);
    fx.key(Key::Enter);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.viewer.is_some(), "{name}: {:?}", fx.b.status.text);
}

/// `n` pressed as the keyboard sends it, then settled (a view made, or one loaded).
fn digit(fx: &mut Fx, n: u8) {
    let key = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4][usize::from(n)];
    fx.drv.press_typed(&mut fx.b, key, &n.to_string());
    fx.drv.idle(&mut fx.b, 2);
}

fn strip(fx: &Fx) -> String {
    fx.b.viewer.as_ref().expect("a viewer").views.strip().text()
}

fn loads(calls: &[Did]) -> Vec<String> {
    calls
        .iter()
        .filter_map(|d| match d {
            Did::Load(url) => Some(url.clone()),
            _ => None,
        })
        .collect()
}

fn count(calls: &[Did], what: &Did) -> usize {
    calls.iter().filter(|d| *d == what).count()
}

/// What view `n`'s glue is called for request `id` until it ends (at most 10 s): its head, its
/// bytes, its failure.
fn answer(fake: &FakeEngine, n: usize, id: RequestId) -> (Option<Head>, Vec<u8>, Option<String>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let (mut head, mut body) = (None, Vec::new());
    loop {
        for (rid, call) in fake.pump(n) {
            if rid != id {
                continue;
            }
            match call {
                Call::Head(h) => head = Some(h),
                Call::Data(bytes) => body.extend(bytes),
                Call::Done => return (head, body, None),
                Call::Fail(why) => return (head, body, Some(why)),
                Call::Readable => {}
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "request {id:?} never ended"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

use std::time::Duration;

/// 09 §6, WV-P1: Enter on an `.html` opens its page in the viewer — a file view, scripts off,
/// in its own scheme at its path under the root — on view 1 of `1 Page · 2 Hex · 3 Text`; the
/// page's own requests are answered from its folder (its stylesheet, its picture); the rows say
/// the document, `scripts off` and the outside requests the engine blocked; the F-key row is
/// `2JS on` beside the viewer's own.
#[test]
fn wv_d_enter_shows_a_page_in_its_own_scheme_under_its_root() {
    let (mut fx, fake) = dual(None);
    enter_page(&mut fx, &fake, "index.html", 0);
    let calls = fake.calls(0);
    assert_eq!(
        calls[0],
        Did::Open {
            page: true,
            preview: false,
            scripts: false
        }
    );
    let host = fake.host(0);
    assert_eq!(loads(&calls), [host.url("index.html")]);
    assert_eq!(strip(&fx), "1 Page · 2 Hex · 3 Text");
    let doc = format!("{}/index.html", fx.root);
    assert!(shows(&fx, &doc), "the document on the bar");
    assert!(fx.drv.find_text("scripts off").is_some(), "scripts off");
    assert!(fx.drv.find_text("Page Viewer").is_some(), "its frame");
    for label in ["JS on", "Exit", "Edit", "Help"] {
        assert!(fx.drv.find_text(label).is_some(), "the F-key row: {label}");
    }
    // The page's own: its stylesheet and its picture, from the folder.
    let css = fake.request(0, &host.url("style.css"), None);
    let (head, body, failed) = answer(&fake, 0, css);
    assert_eq!(failed, None);
    let head = head.expect("a head");
    assert_eq!((head.code(), head.mime()), (200, "text/css; charset=utf-8"));
    assert_eq!(body, STYLE.as_bytes());
    let png = fake.request(0, &host.url("img.png"), Some("bytes=0-3"));
    let (head, body, _) = answer(&fake, 0, png);
    assert_eq!(head.map(|h| h.code()), Some(206));
    assert_eq!(body, b"\x89PNG");
    // The outside picture: the engine blocks it and counts it.
    fake.events(0).send(WebEvent::Blocked(1));
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(&fx, "1 outside request blocked"), "the row's count");
    assert!(!shows(&fx, "no web engine"), "no fallback line");
}

/// 09 §3.1, WV-P2: the root is the folder the browser was opened on, the file inside it — a page
/// in `sub/` is served at `sub/inner.html` and its `../index.html` is the folder's — and
/// nothing outside it: `../outside.txt`, in any spelling, is 403 and counted on the row as
/// refused. Opened inside `sub`, the root is `sub`: the page's upward link is refused.
#[test]
fn wv_d_the_root_is_the_folder_opened_and_nothing_outside_it() {
    let (mut fx, fake) = dual(None);
    fx.select("sub");
    fx.key(Key::Enter);
    fx.settle();
    enter_page(&mut fx, &fake, "inner.html", 0);
    let host = fake.host(0);
    assert_eq!(loads(&fake.calls(0)), [host.url("sub/inner.html")]);
    let up = fake.request(0, &host.url("index.html"), None);
    let (head, body, _) = answer(&fake, 0, up);
    assert_eq!(head.map(|h| h.code()), Some(200), "the folder's own index");
    assert_eq!(body, INDEX.as_bytes());
    for spelling in [
        "../outside.txt",
        "%2e%2e/outside.txt",
        "sub/..%2f..%2foutside.txt",
    ] {
        let url = format!("sopen-page://{}/{spelling}", host.as_str());
        let out = fake.request(0, &url, None);
        let (head, body, _) = answer(&fake, 0, out);
        assert_eq!(head.map(|h| h.code()), Some(403), "{spelling}");
        assert!(body.is_empty(), "{spelling}: nothing of it");
    }
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(&fx, "3 refused"), "counted on the row");

    // Opened inside `sub`: its root is `sub`.
    let (mut fx, fake) = page_fixture(None, true);
    fx.b.mode = view::Mode::Dual;
    enter_page(&mut fx, &fake, "inner.html", 0);
    let host = fake.host(0);
    assert_eq!(loads(&fake.calls(0)), [host.url("inner.html")]);
    let url = format!("sopen-page://{}/../index.html", host.as_str());
    let (head, _, _) = answer(&fake, 0, fake.request(0, &url, None));
    assert_eq!(head.map(|h| h.code()), Some(403), "above the root");
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(&fx, "1 refused"));
}

/// 09 §6's table (08 D40), VM-7's keys: a page's 2 is its bytes, 3 its source highlighted, 1 the
/// page again — the same view, shown again, never a second one; plain XML opens on its source
/// with the page as 3, made when chosen; styled XML opens on its page.
#[test]
fn wv_d_views_1_2_3_of_a_page_and_of_plain_xml() {
    let (mut fx, fake) = dual(None);
    enter_page(&mut fx, &fake, "index.html", 0);
    digit(&mut fx, 2);
    fx.settle();
    assert!(
        fx.drv.text_starting_with("3C 21 64 6F").is_some(),
        "view 2: the bytes"
    );
    assert!(
        fake.calls(0).contains(&Did::Shown(false)),
        "the page hidden"
    );
    digit(&mut fx, 3);
    fx.settle();
    assert!(shows(&fx, "<!doctype html>"), "view 3: the source");
    assert!(fx.drv.find_text("Text Viewer").is_some());
    fake.take(0);
    digit(&mut fx, 1);
    fx.drv.idle(&mut fx.b, 3);
    assert_eq!(fake.views(), 1, "the page shown again, not made again");
    let calls = fake.take(0);
    assert!(calls.contains(&Did::Shown(true)), "{calls:?}");
    assert!(calls.contains(&Did::Hidden(false)), "{calls:?}");
    assert!(loads(&calls).is_empty(), "not loaded again: {calls:?}");
    assert_eq!(count(&calls, &Did::Reload), 0);
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());

    // Plain XML: its source first; 3 is its page.
    enter_other(&mut fx, "feed.xml");
    assert_eq!(strip(&fx), "1 Text · 2 Hex · 3 Page");
    assert_eq!(fake.views(), 1, "no page view yet");
    assert!(shows(&fx, "<rss version=\"2.0\">"));
    digit(&mut fx, 3);
    loaded(&mut fx, &fake, 1, "feed.xml");
    assert_eq!(loads(&fake.calls(1)), [fake.host(1).url("feed.xml")]);
    fx.key(Key::Escape);

    // Styled XML: its page first.
    enter_page(&mut fx, &fake, "styled.xml", 2);
    assert_eq!(strip(&fx), "1 Page · 2 Hex · 3 Text");
}

/// 09 §3.3, R1, R11: F2 turns a file view's scripts on only on an engine that contains them,
/// on a version WV-S2 passed — none yet — so here it says why and does nothing: the version
/// line, and on a backend whose scripts are not contained (macOS: WebRTC) that channel.
#[test]
fn wv_d_f2_says_why_a_file_views_scripts_stay_off() {
    let (mut fx, fake) = dual(None);
    enter_page(&mut fx, &fake, "index.html", 0);
    fx.key(Key::F2);
    fx.drv.idle(&mut fx.b, 2);
    assert!(
        shows(
            &fx,
            "scripts are not available with this WebKit (2.52.6): not yet checked"
        ),
        "the version's line"
    );
    fake.set_uncontained(true, Some("WebRTC"));
    fx.key(Key::F2);
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(
        &fx,
        "scripts are not available here: WebRTC is not contained"
    ));
    assert!(fx.drv.find_text("scripts off").is_some());
    assert!(
        fx.drv.find_text("JS on").is_some(),
        "the label names what F2 would do"
    );
    let calls = fake.calls(0);
    assert!(
        !calls.iter().any(|d| matches!(d, Did::Scripts(_))),
        "{calls:?}"
    );
    assert!(!calls.contains(&Did::Reload), "nothing reloaded");
}

/// 09 §3.4: a clicked link out of the page is not followed — the row shows it redacted and the
/// F-key row offers `5Link`, which copies the whole address; any other navigation out is said;
/// Cmd/Ctrl+R reloads.
#[test]
fn wv_d_a_link_out_is_shown_redacted_and_f5_copies_it() {
    let (mut fx, fake) = dual(None);
    enter_page(&mut fx, &fake, "index.html", 0);
    let url = "https://user:secret@example.invalid/a?token=1";
    fake.events(0).send(WebEvent::Refused {
        target: url.to_string(),
        clicked: true,
    });
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(
        &fx,
        "https://user:***@example.invalid/a?…: link not followed from a file view — F5 copies it"
    ));
    assert!(!shows(&fx, "secret"), "never the password");
    assert!(fx.drv.find_text("Link").is_some(), "5Link");
    fx.drv.take_commands();
    fx.key(Key::F5);
    fx.drv.idle(&mut fx.b, 1);
    let copied: Vec<_> = fx
        .drv
        .take_commands()
        .into_iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(copied, [url], "the whole address");
    assert!(shows(&fx, "copied — it may carry what the page read"));
    fake.events(0).send(WebEvent::Refused {
        target: "http://example.invalid/next".to_string(),
        clicked: false,
    });
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(
        &fx,
        "http://example.invalid/next: a navigation out of the page, cancelled"
    ));
    fx.drv
        .key(&mut fx.b, Key::R, crate::gui::testkit::command(false));
    assert_eq!(count(&fake.calls(0), &Did::Reload), 1, "Cmd/Ctrl+R reloads");
}

/// 09 §2.5, §6: the viewer's Esc closes its page view — its requests stop with it — and the list
/// is back; the next Enter makes a new view.
#[test]
fn wv_d_leaving_the_viewer_tears_its_view_down() {
    let (mut fx, fake) = dual(None);
    enter_page(&mut fx, &fake, "index.html", 0);
    fake.request(0, &fake.host(0).url("style.css"), None);
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    fx.drv.idle(&mut fx.b, 2);
    assert_eq!(count(&fake.calls(0), &Did::Close), 1, "closed once");
    assert!(!fake.has_table(0), "its requests go with it");
    enter_page(&mut fx, &fake, "index.html", 1);
    assert_eq!(fake.views(), 2);
}

/// 09 §4, WV-7: with no web engine — SOPEN_WEB=off, or a system without one (Linux before WV-B)
/// — a page file opens as its source, its first row saying why, on 1 Text · 2 Hex · 3 Raw; so
/// does a backend that does not contain a file view, naming the channel. Other files are as
/// they were.
#[test]
fn wv_d_without_an_engine_a_page_shows_its_source_and_why() {
    for (why, line) in [
        (
            Unavailable::NoEngine(OFF.to_string()),
            "no web engine here (turned off (SOPEN_WEB=off)): showing the source",
        ),
        (
            Unavailable::NoEngine("not in this build".to_string()),
            "no web engine here (not in this build): showing the source",
        ),
    ] {
        let (mut fx, _fake) = dual(None);
        use_engine(Err(why));
        enter_other(&mut fx, "index.html");
        assert!(shows(&fx, line), "{line}");
        assert_eq!(strip(&fx), "1 Text · 2 Hex · 3 Raw");
        assert!(shows(&fx, "<h1>Index</h1>"), "the source");
        fx.key(Key::Escape);
        enter_other(&mut fx, "notes.txt");
        assert!(
            !shows(&fx, "showing the source"),
            "a text file says nothing of it"
        );
        fx.key(Key::Escape);
        enter_other(&mut fx, "feed.xml");
        assert!(
            !shows(&fx, "showing the source"),
            "plain XML's view 1 is its source anyway"
        );
    }
    let (mut fx, fake) = dual(None);
    fake.set_uncontained(false, Some("DNS"));
    enter_other(&mut fx, "index.html");
    assert!(shows(
        &fx,
        "the page view is not contained here (DNS): showing the source"
    ));
    assert_eq!(fake.views(), 0, "no view made");
}

/// 09 §4: an engine that refuses to make the view (the macOS content rule list did not compile,
/// a WebKitGTK that did not start) — the viewer shows the source in its place, view 3, its first
/// row saying why; page files open as their source in this window from then on.
#[test]
fn wv_d_a_view_the_engine_refuses_gives_way_to_the_source() {
    let (mut fx, fake) = dual(None);
    fake.refuse_open(Some("the content rule list did not compile: x"));
    fx.select("index.html");
    fx.key(Key::Enter);
    fx.settle();
    fx.drv.idle(&mut fx.b, 3);
    fx.settle();
    let line = "no web engine here (the content rule list did not compile: x): showing the source";
    assert!(shows(&fx, line), "{:?}", fx.b.status.text);
    assert!(shows(&fx, "<h1>Index</h1>"), "the source");
    assert_eq!(fx.b.viewer.as_ref().unwrap().views.strip().on, Some("Text"));
    fx.key(Key::Escape);
    fake.refuse_open(None);
    enter_other(&mut fx, "styled.xml");
    assert!(shows(&fx, line), "the window remembers");
    assert_eq!(strip(&fx), "1 Text · 2 Hex · 3 Raw");
}

/// 09 §6, WV-8: in preview mode the page previews after the bar rests — a preview view, scripts
/// off, loaded once; Enter hands the same view over (no second view, no second load) and F2 in
/// the preview is never the page's; the bar moving on closes it.
#[test]
fn wv_d_the_preview_is_the_page_and_enter_hands_it_over() {
    let (mut fx, fake) = page_fixture(None, false);
    fx.select("index.html");
    fx.drv.idle(&mut fx.b, 12);
    loaded(&mut fx, &fake, 0, "index.html");
    assert_eq!(
        fake.calls(0)[0],
        Did::Open {
            page: true,
            preview: true,
            scripts: false
        }
    );
    assert!(!shows(&fx, "scripts off"), "a preview has no bars");
    // Its keys are the list's (Cmd/Ctrl+R reads the folder again, F2 renames): never the page's.
    fx.drv
        .key(&mut fx.b, Key::R, crate::gui::testkit::command(false));
    fx.settle();
    fx.key(Key::F2);
    fx.drv.idle(&mut fx.b, 2);
    fx.key(Key::Escape);
    fx.settle();
    let calls = fake.calls(0);
    assert!(
        !calls.iter().any(|d| matches!(d, Did::Scripts(_))),
        "{calls:?}"
    );
    assert_eq!(count(&calls, &Did::Reload), 0, "{calls:?}");
    assert_eq!(fake.views(), 1, "the same preview");
    fx.select("index.html");
    fx.key(Key::Enter);
    fx.settle();
    assert!(fx.b.viewer.is_some());
    assert_eq!(fake.views(), 1, "the same view");
    let calls = fake.calls(0);
    assert!(calls.contains(&Did::Preview(false)), "handed over");
    assert_eq!(loads(&calls).len(), 1, "no second load");
    assert_eq!(count(&calls, &Did::Reload), 0, "the budget cut nothing");
    assert!(
        fx.drv.find_text("scripts off").is_some(),
        "the viewer's rows"
    );
    fx.key(Key::Escape);
    // The bar moves on: the preview's view closes.
    fx.select("index.html");
    fx.drv.idle(&mut fx.b, 12);
    loaded(&mut fx, &fake, 1, "index.html");
    fx.select("notes.txt");
    fx.drv.idle(&mut fx.b, 14);
    fx.settle();
    assert_eq!(count(&fake.calls(1), &Did::Close), 1, "its view closed");
    assert!(!fake.calls(1).iter().any(|d| matches!(d, Did::Scripts(_))));
}

/// 09 §3.1's preview budget: a preview's page reads at most 32 MiB, a request past it failing
/// before its head; the hand-over lifts it and the page it cut reloads once.
#[test]
fn wv_d_a_preview_the_budget_cut_reloads_once_at_the_hand_over() {
    let (mut fx, fake) = page_fixture(None, false);
    fx.select("index.html");
    fx.drv.idle(&mut fx.b, 12);
    loaded(&mut fx, &fake, 0, "index.html");
    let big = fake.request(0, &fake.host(0).url("big.bin"), None);
    let (head, _, failed) = answer(&fake, 0, big);
    assert_eq!(head, None, "failed before its head");
    assert!(failed.is_some());
    fx.key(Key::Enter);
    fx.drv.idle(&mut fx.b, 2);
    let calls = fake.calls(0);
    assert_eq!(count(&calls, &Did::Reload), 1, "one reload: {calls:?}");
    let again = fake.request(0, &fake.host(0).url("big.bin"), Some("bytes=0-3"));
    let (head, body, _) = answer(&fake, 0, again);
    assert_eq!(head.map(|h| h.code()), Some(206), "no budget in the viewer");
    assert_eq!(body, [0, 0, 0, 0]);
}

/// 09 §3.1 "Transport", 06 §13: a remote page is served by its page server over the stream —
/// its files read where they are, nothing downloaded into the cache — and its rows name it
/// `host:/abs`; its preview reads on the window's preview session, which the viewer keeps at the
/// hand-over.
#[test]
fn wv_d_a_remote_page_is_served_through_its_page_server() {
    let (mut fx, fake) = dual(Some("box"));
    enter_page(&mut fx, &fake, "index.html", 0);
    assert!(shows(&fx, &format!("box:{}/index.html", fx.root)));
    let host = fake.host(0);
    let (head, body, _) = answer(&fake, 0, fake.request(0, &host.url("style.css"), None));
    assert_eq!(head.map(|h| h.code()), Some(200));
    assert_eq!(body, STYLE.as_bytes());
    let (head, body, _) = answer(&fake, 0, fake.request(0, &host.url("index.html"), None));
    assert_eq!(head.map(|h| h.length()), Some(INDEX.len() as u64));
    assert_eq!(body, INDEX.as_bytes());
    assert_eq!(
        files_in(&fx.cache),
        Vec::<PathBuf>::new(),
        "nothing downloaded"
    );
    fx.key(Key::Escape);
    // The preview: on the window's preview session, kept by the viewer.
    fx.b.mode = view::Mode::Preview;
    fx.select("styled.xml");
    fx.drv.idle(&mut fx.b, 12);
    loaded(&mut fx, &fake, 1, "styled.xml");
    assert!(matches!(fake.calls(1)[0], Did::Open { preview: true, .. }));
    fx.key(Key::Enter);
    fx.settle();
    let (head, _, _) = answer(
        &fake,
        1,
        fake.request(1, &fake.host(1).url("styled.xml"), None),
    );
    assert_eq!(
        head.map(|h| h.code()),
        Some(200),
        "served after the hand-over"
    );
    assert_eq!(files_in(&fx.cache), Vec::<PathBuf>::new());
}

/// 09 §3.1 "Channels", 06 §13, WV-S6's preview half: a remote page's preview reads on the
/// window's one preview session, and its server gives that session back when the bar moves on —
/// the next preview runs on it, no session more (a lost one would cost a wait and a new one).
#[test]
fn wv_d_a_remote_page_preview_gives_the_preview_session_back() {
    let connects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = connects.clone();
    let connect: Connect = Arc::new(move || {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>)
    });
    let (mut fx, fake) = page_fixture_with(Some("box"), false, connect);
    let opened = || connects.load(std::sync::atomic::Ordering::SeqCst);
    fx.select("index.html");
    fx.drv.idle(&mut fx.b, 12);
    loaded(&mut fx, &fake, 0, "index.html");
    assert_eq!(opened(), 1, "the preview session");
    let host = fake.host(0);
    let (head, _, _) = answer(&fake, 0, fake.request(0, &host.url("style.css"), None));
    assert_eq!(head.map(|h| h.code()), Some(200), "served on it");
    fx.select("notes.txt");
    fx.drv.idle(&mut fx.b, 12);
    fx.settle();
    assert_eq!(
        count(&fake.calls(0), &Did::Close),
        1,
        "the page's view closed"
    );
    assert!(shows(&fx, "plain notes"), "the next preview");
    assert_eq!(opened(), 1, "on the same session, given back");
}

/// 09 §8, 10 §11: in a terminal no view is made and no engine asked — the page's cells are the
/// terminal's hand-off box, its frame and rows cells — and the window the switch opens makes
/// it; a window that went took its view down, and the next one makes another.
#[test]
fn wv_d_in_a_terminal_the_page_is_a_hand_off_box() {
    let (mut fx, fake) = dual(None);
    crate::gui::mark_terminal(&fx.drv.ctx);
    fx.select("index.html");
    fx.key(Key::Enter);
    fx.settle();
    assert!(fx.b.viewer.is_some());
    assert_eq!(fake.views(), 0, "no view in a terminal");
    assert_eq!(strip(&fx), "1 Page · 2 Hex · 3 Text");
    let layers = crate::gui::cells::layers(&fx.drv.ctx);
    let screen = crate::gui::term::compose(&fx.drv.ctx, &layers, 125, 43);
    assert!(
        screen
            .hand_offs
            .iter()
            .any(|r| r.width > 100 && r.height > 30),
        "the page's cells: {:?}",
        screen.hand_offs
    );
    assert!(
        fx.drv.find_text("Page Viewer").is_some(),
        "its frame in cells"
    );
    // The switch: a window of its own (a new context) makes the view.
    let mut window = Driver::new(1000.0, 700.0);
    window.until(&mut fx.b, 20, |_| fake.views() == 1);
    window.idle(&mut fx.b, 5);
    assert_eq!(fake.views(), 1, "one view while its window stays");
    assert_eq!(loads(&fake.calls(0)), [fake.host(0).url("index.html")]);
    // That window goes (its views with it); in the terminal again, then another window.
    crate::gui::web::teardown(&window.ctx);
    assert_eq!(count(&fake.calls(0), &Did::Close), 1);
    fx.drv.idle(&mut fx.b, 2);
    let mut again = Driver::new(1000.0, 700.0);
    again.until(&mut fx.b, 20, |_| fake.views() == 2);
    assert_eq!(loads(&fake.calls(1)), [fake.host(1).url("index.html")]);
}

// ---------------------------------------------------------------------------------------------
// The fix-check of WV-D.

/// Review MF1 (a): a remote preview whose window went (the terminal's switch) is made again in
/// the next window on the window's preview session — the connection its server gave back — no
/// session more, and not the closed line.
#[test]
fn wv_d_a_remote_preview_is_made_again_in_the_next_window() {
    let (connect, connects) = counted();
    let (mut fx, fake) = page_fixture_with(Some("box"), false, connect);
    fx.select("index.html");
    fx.drv.idle(&mut fx.b, 12);
    loaded(&mut fx, &fake, 0, "index.html");
    assert_eq!(connects(), 1);
    crate::gui::web::teardown(&fx.drv.ctx);
    assert_eq!(count(&fake.calls(0), &Did::Close), 1);
    let mut t = terminal();
    t.idle(&mut fx.b, 3);
    let mut window = Driver::new(1000.0, 700.0);
    assert!(
        passes_until(&mut window, &mut fx, |_| fake.views() == 2),
        "made again: {:?}",
        window
            .texts()
            .iter()
            .map(|t| t.2.clone())
            .collect::<Vec<_>>()
    );
    assert!(matches!(fake.calls(1)[0], Did::Open { preview: true, .. }));
    let (head, _, _) = answer(
        &fake,
        1,
        fake.request(1, &fake.host(1).url("style.css"), None),
    );
    assert_eq!(head.map(|h| h.code()), Some(200), "served");
    assert_eq!(connects(), 1, "on the preview session");
}

/// Review MF1 (b), the reviewer's RED: preview, its window goes, Enter in the terminal, the next
/// window — the viewer's page is made (its connection went back to the preview session, so it
/// connects on its own, 06 §13) and comes ready; never stuck with no view and no line.
#[test]
fn wv_d_a_preview_handed_over_in_the_terminal_is_made_in_the_next_window() {
    let (connect, connects) = counted();
    let (mut fx, fake) = page_fixture_with(Some("box"), false, connect);
    fx.select("index.html");
    fx.drv.idle(&mut fx.b, 12);
    loaded(&mut fx, &fake, 0, "index.html");
    crate::gui::web::teardown(&fx.drv.ctx);
    // The old server gives its connection back on its own thread: the scenario is the one
    // where the preview session has it back before Enter (Enter first keeps it with the viewer,
    // the other right answer: one session).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !fx.b.preview.session_back() {
        assert!(
            std::time::Instant::now() < deadline,
            "the session never came back"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut t = terminal();
    t.idle(&mut fx.b, 3);
    t.key(&mut fx.b, Key::Enter, Modifiers::NONE);
    t.idle(&mut fx.b, 2);
    assert!(fx.b.viewer.is_some(), "handed over in the terminal");
    let mut window = Driver::new(1000.0, 700.0);
    assert!(
        passes_until(&mut window, &mut fx, |_| fake.views() == 2),
        "no view, no line"
    );
    assert!(matches!(fake.calls(1)[0], Did::Open { preview: false, .. }));
    let events = fake.events(1);
    events.send(WebEvent::Loading);
    events.send(WebEvent::Finished);
    assert!(
        passes_until(&mut window, &mut fx, |b| b.ready()),
        "comes ready"
    );
    let (head, _, _) = answer(
        &fake,
        1,
        fake.request(1, &fake.host(1).url("index.html"), None),
    );
    assert_eq!(head.map(|h| h.code()), Some(200));
    assert_eq!(connects(), 2, "the viewer's own session");
}

/// Review MF1, the dual-window run: a remote viewer's page, its window gone, a terminal's pass,
/// a second window: made again on the connection its server gave back, and answering.
#[test]
fn wv_d_a_remote_viewer_is_made_again_in_a_second_window() {
    let (connect, connects) = counted();
    let (mut fx, fake) = page_fixture_with(Some("box"), false, connect);
    fx.b.mode = view::Mode::Dual;
    enter_page(&mut fx, &fake, "index.html", 0);
    crate::gui::web::teardown(&fx.drv.ctx);
    let mut t = terminal();
    t.idle(&mut fx.b, 2);
    let mut window = Driver::new(1000.0, 700.0);
    assert!(passes_until(&mut window, &mut fx, |_| fake.views() == 2));
    let (head, body, _) = answer(
        &fake,
        1,
        fake.request(1, &fake.host(1).url("style.css"), None),
    );
    assert_eq!(head.map(|h| h.code()), Some(200));
    assert_eq!(body, STYLE.as_bytes());
    assert_eq!(connects(), 1, "the viewer's one connection, given back");
}

/// Review MF2 (one page decision): what a file's views are is decided from the file opened —
/// its resolved name and head — not the name the list shows: a link `x.html` to plain XML is
/// `1 Text · 2 Hex · 3 Page`, one `y.html` to text has no page, one `alias.xml` to an HTML is
/// its page; a link whose name asks no page (`alias.txt`) opens as text, no engine asked.
#[test]
fn wv_d_a_links_views_are_its_targets() {
    let (mut fx, fake) = dual(None);
    enter_other(&mut fx, "x.html");
    assert_eq!(strip(&fx), "1 Text · 2 Hex · 3 Page");
    assert!(shows(&fx, "<rss version=\"2.0\">"), "its source");
    fx.key(Key::Escape);
    enter_other(&mut fx, "y.html");
    assert_eq!(strip(&fx), "1 Text · 2 Hex · 3 Raw");
    assert!(shows(&fx, "plain notes"));
    fx.key(Key::Escape);
    enter_other(&mut fx, "alias.txt");
    assert_eq!(strip(&fx), "1 Text · 2 Hex · 3 Raw");
    assert_eq!(fake.views(), 0, "no page view");
    fx.key(Key::Escape);
    enter_page(&mut fx, &fake, "alias.xml", 0);
    assert_eq!(strip(&fx), "1 Page · 2 Hex · 3 Text");
    assert_eq!(loads(&fake.calls(0)), [fake.host(0).url("index.html")]);
}

/// Review MF2: the preview's decision goes with the view it hands over — a link `x.html` to
/// plain XML previews its source and Enter keeps `1 Text · 2 Hex · 3 Page`; `alias.xml` to an
/// HTML previews its page and keeps `1 Page · 2 Hex · 3 Text`.
#[test]
fn wv_d_the_preview_hands_its_page_decision_over() {
    let (mut fx, fake) = page_fixture(None, false);
    fx.select("x.html");
    fx.drv.idle(&mut fx.b, 12);
    fx.settle();
    assert_eq!(fake.views(), 0, "its source");
    fx.key(Key::Enter);
    fx.settle();
    assert_eq!(strip(&fx), "1 Text · 2 Hex · 3 Page");
    fx.key(Key::Escape);
    fx.select("alias.xml");
    fx.drv.idle(&mut fx.b, 12);
    loaded(&mut fx, &fake, 0, "index.html");
    fx.key(Key::Enter);
    fx.settle();
    assert_eq!(strip(&fx), "1 Page · 2 Hex · 3 Text");
    assert_eq!(fake.views(), 1, "the same view");
}

/// Review MF4: an engine that refuses the preview's view — the preview shows the file's source
/// at once, the bar where it was, the reason said; the preview session comes back for it (no
/// session more), and Enter opens the source too.
#[test]
fn wv_d_a_preview_the_engine_refuses_shows_the_source() {
    let (connect, connects) = counted();
    let (mut fx, fake) = page_fixture_with(Some("box"), false, connect);
    fake.refuse_open(Some("the content rule list did not compile: x"));
    fx.select("index.html");
    fx.drv.idle(&mut fx.b, 12);
    fx.settle();
    fx.drv.idle(&mut fx.b, 4);
    fx.settle();
    assert_eq!(fx.current(), "index.html", "the bar where it was");
    assert!(shows(&fx, "<h1>Index</h1>"), "the source in the preview");
    let line = "no web engine here (the content rule list did not compile: x): showing the source";
    assert!(shows(&fx, line), "{:?}", fx.b.status_line().text);
    assert_eq!(connects(), 1, "the preview session came back for it");
    fx.key(Key::Enter);
    fx.settle();
    assert_eq!(strip(&fx), "1 Text · 2 Hex · 3 Raw");
}

/// Review MF3 (the hand-over keeps the session): a remote page previewed, Enter — the viewer
/// keeps the preview session — Esc, the next remote preview connects again: 2 sessions. Were
/// the session given back to the preview at the hand-over, it would be 1.
#[test]
fn wv_d_the_viewer_keeps_the_preview_session_it_was_handed() {
    let (connect, connects) = counted();
    let (mut fx, fake) = page_fixture_with(Some("box"), false, connect);
    fx.select("index.html");
    fx.drv.idle(&mut fx.b, 12);
    loaded(&mut fx, &fake, 0, "index.html");
    fx.key(Key::Enter);
    fx.settle();
    assert_eq!(connects(), 1);
    fx.key(Key::Escape);
    // Back in the list, the bar on the page: its preview is made again, on a session of its
    // own — the one the viewer had is the viewer's.
    fx.drv.idle(&mut fx.b, 12);
    loaded(&mut fx, &fake, 1, "index.html");
    assert_eq!(connects(), 2, "the next preview's own session");
    let n = fake.views();
    fx.select("styled.xml");
    fx.drv.idle(&mut fx.b, 12);
    loaded(&mut fx, &fake, n, "styled.xml");
    assert_eq!(connects(), 2, "and the one after it on the same");
}

/// Enter while a page's preview is still being prepared (02 §4.4): that preparation becomes the
/// viewer's opening — one connection (a fresh open would make a second), one view, opened as
/// the viewer's (no shield), loaded once.
#[test]
fn wv_d_enter_while_a_page_preview_is_prepared_makes_one_view() {
    let gate = Gate::default();
    let held = gate.clone();
    let made = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = made.clone();
    let connect: Connect = Arc::new(move || {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        held.hold();
        Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>)
    });
    let (mut fx, fake) = page_fixture_with(Some("box"), false, connect);
    fx.select("index.html");
    fx.drv.idle(&mut fx.b, 12);
    gate.reached();
    fx.key(Key::Enter);
    gate.release();
    loaded(&mut fx, &fake, 0, "index.html");
    assert!(fx.b.viewer.is_some());
    assert_eq!(fake.views(), 1);
    let calls = fake.calls(0);
    assert_eq!(
        calls[0],
        Did::Open {
            page: true,
            preview: false,
            scripts: false
        }
    );
    assert_eq!(loads(&calls).len(), 1);
    // The preview's preparation became the opening: its one connection, no fresh open's.
    assert_eq!(
        made.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one connection"
    );
}

/// D25 with the page: Page, 2 Hex, an edit, 1 — the prompt's Save lands — the hidden page view
/// is stale: closed once, and 1 shows a page view made again from the file as saved.
#[test]
fn wv_d_a_save_in_hex_makes_the_page_view_again() {
    let (mut fx, fake) = dual(None);
    let file = format!("{}/index.html", fx.root);
    enter_page(&mut fx, &fake, "index.html", 0);
    digit(&mut fx, 2);
    fx.settle();
    fx.key(Key::Enter);
    digit(&mut fx, 4);
    digit(&mut fx, 1);
    fx.key(Key::F9);
    digit(&mut fx, 1);
    assert!(fx.drv.find_text("Unsaved changes").is_some(), "the prompt");
    fx.drv.click_text_on_top(&mut fx.b, "Save");
    fx.drv.until(&mut fx.b, 20, |_| {
        std::fs::read(&file).unwrap()[0] == 0x41 && fake.views() == 2
    });
    assert_eq!(
        count(&fake.calls(0), &Did::Close),
        1,
        "the stale view closed once"
    );
    loaded(&mut fx, &fake, 1, "index.html");
    assert_eq!(loads(&fake.calls(1)), [fake.host(1).url("index.html")]);
    assert_eq!(strip(&fx), "1 Page · 2 Hex · 3 Text");
}

/// WV-S6's viewer half: a remote page in the viewer holds one session, the one that opened it;
/// `.htm` is a page; a styled XML's stylesheet is fetched through the scheme, as its type.
#[test]
fn wv_d_one_session_an_htm_and_a_stylesheet_through_the_scheme() {
    let (connect, connects) = counted();
    let (mut fx, fake) = page_fixture_with(Some("box"), false, connect);
    fx.b.mode = view::Mode::Dual;
    fx.drv.idle(&mut fx.b, 2);
    enter_page(&mut fx, &fake, "index.html", 0);
    let (head, _, _) = answer(
        &fake,
        0,
        fake.request(0, &fake.host(0).url("style.css"), None),
    );
    assert_eq!(head.map(|h| h.code()), Some(200));
    assert_eq!(connects(), 1, "one session");
    fx.key(Key::Escape);
    enter_page(&mut fx, &fake, "page.htm", 1);
    assert_eq!(strip(&fx), "1 Page · 2 Hex · 3 Text");
    assert_eq!(loads(&fake.calls(1)), [fake.host(1).url("page.htm")]);
    fx.key(Key::Escape);
    enter_page(&mut fx, &fake, "styled.xml", 2);
    let (head, body, _) = answer(&fake, 2, fake.request(2, &fake.host(2).url("s.xsl"), None));
    let head = head.expect("a head");
    assert_eq!(head.code(), 200);
    assert!(head.mime().starts_with("text/xsl"), "{}", head.mime());
    assert_eq!(body, XSL.as_bytes());
}
