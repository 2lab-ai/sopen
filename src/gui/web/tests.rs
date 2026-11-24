//! WV-0's tests (09 §11 "unit level, no engine"): the scheme's contract with a recording server,
//! and the component on the test kit with the recording [`FakeEngine`].

use super::fake::{Did, FakeEngine};
use super::*;
use crate::gui::testkit::Driver;
use crate::gui::{chrome, help, Window};
use crate::scheme::{
    Call, Head, Mode, Read, Refused, Reply, Request, RequestId, Server, Table, WINDOW,
};
use eframe::egui::{Event, Key, Modifiers, Pos2, Rect};
use std::sync::atomic::{AtomicUsize, Ordering};

// ---------------------------------------------------------------------------------------------
// The scheme (X-W1's result, change 4).

/// What the server was told, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Said {
    Start(Request),
    Stop(RequestId),
    Credit(RequestId),
}

/// A server that records what it is told and keeps each reply for the test to answer.
#[derive(Clone, Default)]
struct Asked(Arc<Mutex<(Vec<Said>, VecDeque<Reply>)>>);

use std::collections::VecDeque;

impl Asked {
    fn said(&self) -> Vec<Said> {
        self.0.lock().unwrap().0.clone()
    }

    /// The oldest reply not taken yet.
    fn reply(&self) -> Reply {
        self.0
            .lock()
            .unwrap()
            .1
            .pop_front()
            .expect("a request was asked")
    }

    fn server(&self) -> Box<dyn Server> {
        Box::new(Recorder(self.clone()))
    }
}

struct Recorder(Asked);

impl Server for Recorder {
    fn start(&mut self, request: Request, reply: Reply) {
        let mut asked = self.0 .0.lock().unwrap();
        asked.0.push(Said::Start(request));
        asked.1.push_back(reply);
    }

    fn stop(&mut self, id: RequestId) {
        self.0 .0.lock().unwrap().0.push(Said::Stop(id));
    }

    fn credit(&mut self, id: RequestId) {
        self.0 .0.lock().unwrap().0.push(Said::Credit(id));
    }
}

/// A table on this thread (the test's "engine thread") with a recording server; its wakes counted.
fn table(mode: Mode) -> (Table, Asked, Host, Arc<AtomicUsize>) {
    let asked = Asked::default();
    let host = Host::new();
    let woke = Arc::new(AtomicUsize::new(0));
    let count = woke.clone();
    let table = Table::new(host.clone(), asked.server(), mode, move || {
        count.fetch_add(1, Ordering::SeqCst);
    });
    (table, asked, host, woke)
}

/// What the glue would call now, in order.
fn drain(table: &mut Table) -> Vec<(RequestId, Call)> {
    std::iter::from_fn(|| table.next_call()).collect()
}

fn starts(said: &[Said]) -> usize {
    said.iter().filter(|s| matches!(s, Said::Start(_))).count()
}

fn stops(said: &[Said], id: RequestId) -> usize {
    said.iter().filter(|s| **s == Said::Stop(id)).count()
}

fn credits(said: &[Said], id: RequestId) -> usize {
    said.iter().filter(|s| **s == Said::Credit(id)).count()
}

/// X-W1 change 4: in, `start(id, path, range)`; out, the complete head, the pieces, the end —
/// called in that order on the engine's thread, after one wake for what arrived together; each
/// piece a push engine takes is credit back to the server.
#[test]
fn x_w1_a_response_is_its_head_then_its_pieces_then_its_end() {
    let (mut t, asked, host, woke) = table(Mode::Push);
    let id = t.start(&host.url("sub/a b.css"), Some("bytes=0-3".into()));
    assert_eq!(
        asked.said(),
        [Said::Start(Request {
            id,
            path: "/sub/a%20b.css".into(),
            range: Some("bytes=0-3".into()),
        })]
    );
    let reply = asked.reply();
    assert!(
        drain(&mut t).is_empty(),
        "nothing before the server answers"
    );
    let head = Head::part("text/css", 0, 3, 10).unwrap();
    let mut body = reply.head(head.clone());
    body.data(b"ab".to_vec()).unwrap();
    body.data(b"cd".to_vec()).unwrap();
    body.done().unwrap();
    assert_eq!(woke.load(Ordering::SeqCst), 1, "one wake for the batch");
    assert_eq!(
        drain(&mut t),
        [
            (id, Call::Head(head)),
            (id, Call::Data(b"ab".to_vec())),
            (id, Call::Data(b"cd".to_vec())),
            (id, Call::Done),
        ]
    );
    assert_eq!(t.live(), 0, "an ended request is gone");
    assert_eq!(
        credits(&asked.said(), id),
        2,
        "each piece taken is credit back"
    );
    // A later answer wakes again.
    let id2 = t.start(&host.url("b"), None);
    asked.reply().head(Head::not_found()).done().unwrap();
    assert_eq!(woke.load(Ordering::SeqCst), 2);
    assert_eq!(
        drain(&mut t),
        [(id2, Call::Head(Head::not_found())), (id2, Call::Done)]
    );
    assert!(id2 > id, "ids are never reused");
}

/// The head comes first, and a body is exactly its length: no body without a head (the types:
/// a `Body` comes only from `Reply::head`); a piece that reached the table before its head
/// anyway — a producer that broke the order — is refused there: the request fails and the
/// server is told to stop, and nothing else of it reaches the engine. A piece past the length is
/// refused; an end before it fails the request; 403, 404 and 416 have no body.
#[test]
fn x_w1_data_before_the_head_is_refused() {
    let (mut t, asked, host, _) = table(Mode::Push);
    let id = t.start(&host.url("a.html"), None);
    let reply = asked.reply();
    let raw = reply.raw();
    raw.data(b"x".to_vec());
    let mut body = reply.head(Head::whole("text/html", 1));
    let _ = body.data(b"y".to_vec());
    let _ = body.done();
    let calls = drain(&mut t);
    let failed = match calls.as_slice() {
        [(i, Call::Fail(why))] => *i == id && why.contains("a piece before its head"),
        _ => false,
    };
    assert!(failed, "{calls:?}");
    assert_eq!(stops(&asked.said(), id), 1, "the server is told to stop");
    assert_eq!(t.live(), 0);
    // The length.
    let id = t.start(&host.url("b.txt"), None);
    let mut body = asked.reply().head(Head::whole("text/plain", 3));
    assert_eq!(body.data(vec![0; 4]), Err(Refused::PastLength { left: 3 }));
    body.data(vec![1, 2]).unwrap();
    assert_eq!(body.left(), 1);
    assert_eq!(body.done(), Err(Refused::Short { left: 1 }));
    let calls = drain(&mut t);
    let short = match calls.as_slice() {
        [(_, Call::Head(_)), (_, Call::Data(d)), (i, Call::Fail(_))] => *i == id && d == &[1, 2],
        _ => false,
    };
    assert!(short, "{calls:?}");
    // No body after 403, 404, 416.
    for head in [Head::forbidden(), Head::not_found(), Head::unsatisfiable(9)] {
        let id = t.start(&host.url("c"), None);
        let mut body = asked.reply().head(head.clone());
        assert_eq!(body.left(), 0);
        assert_eq!(body.data(vec![0]), Err(Refused::PastLength { left: 0 }));
        body.done().unwrap();
        assert_eq!(drain(&mut t), [(id, Call::Head(head)), (id, Call::Done)]);
    }
}

/// A stop before the head (one already on its way included), mid-body, and after the last byte
/// but before the end (macOS `<video>`): nothing more of the request reaches the engine, the
/// server is told once, and the server's own handle says so and refuses what follows.
#[test]
fn x_w1_a_stop_at_any_point_leaves_no_later_call() {
    let (mut t, asked, host, _) = table(Mode::Push);
    let url = host.url("v.mp4");
    // Before the head.
    let id = t.start(&url, None);
    let reply = asked.reply();
    t.stop(id);
    assert!(reply.stopped());
    let mut body = reply.head(Head::whole("video/mp4", 4));
    assert_eq!(body.data(vec![0; 4]), Err(Refused::Stopped));
    assert_eq!(body.done(), Err(Refused::Stopped));
    assert!(drain(&mut t).is_empty());
    assert_eq!(stops(&asked.said(), id), 1);
    // The head crossed the stop: sent, not called yet.
    let id = t.start(&url, None);
    let body = asked.reply().head(Head::whole("video/mp4", 4));
    t.stop(id);
    drop(body);
    assert!(drain(&mut t).is_empty(), "a late head is dropped");
    // Mid-body.
    let id = t.start(&url, None);
    let mut body = asked.reply().head(Head::whole("video/mp4", 4));
    body.data(vec![1, 2]).unwrap();
    assert_eq!(drain(&mut t).len(), 2, "the head and a piece");
    t.stop(id);
    assert_eq!(body.data(vec![3, 4]), Err(Refused::Stopped));
    drop(body);
    assert!(drain(&mut t).is_empty());
    // After the last byte, before the end.
    let id = t.start(&url, None);
    let mut body = asked.reply().head(Head::whole("video/mp4", 2));
    body.data(vec![1, 2]).unwrap();
    assert_eq!(drain(&mut t).len(), 2);
    body.done().unwrap();
    t.stop(id);
    assert!(
        drain(&mut t).is_empty(),
        "the end after the stop is dropped"
    );
    assert_eq!(stops(&asked.said(), id), 1);
    t.stop(id);
    assert_eq!(stops(&asked.said(), id), 1, "a second stop is nothing");
}

/// The tombstone: an id the table no longer has is a request that ended or was stopped, and
/// whatever still arrives for it is dropped; stopping it then tells the server nothing.
#[test]
fn x_w1_the_tombstone_drops_whatever_arrives_for_an_ended_request() {
    let (mut t, asked, host, _) = table(Mode::Push);
    let id = t.start(&host.url("a"), None);
    let reply = asked.reply();
    let raw = reply.raw();
    let body = reply.head(Head::whole("text/plain", 0));
    body.done().unwrap();
    assert_eq!(drain(&mut t).len(), 2);
    raw.data(b"late".to_vec());
    raw.head(Head::whole("text/plain", 0));
    raw.done();
    raw.fail("late");
    assert!(drain(&mut t).is_empty());
    t.stop(id);
    assert_eq!(
        stops(&asked.said(), id),
        0,
        "an ended request is not stopped"
    );
}

/// A pull engine (GTK) reads the body from a buffer the server can fill only up to its credit:
/// the head is the call, then `Readable` for each arrival; each read gives the bytes read back as
/// credit; the end and a failure are read last. A push engine has nothing to read.
#[test]
fn x_w1_a_pull_engine_reads_through_a_bounded_buffer() {
    let (mut t, asked, host, _) = table(Mode::Pull);
    let id = t.start(&host.url("big.bin"), None);
    let head = Head::whole("application/octet-stream", 3 * WINDOW as u64);
    let mut body = asked.reply().head(head.clone());
    assert_eq!(drain(&mut t), [(id, Call::Head(head))]);
    assert_eq!(t.read(id, 8192), Read::Pending, "nothing yet");
    assert_eq!(body.room(), WINDOW);
    body.data(vec![7; WINDOW]).unwrap();
    assert_eq!(body.room(), 0);
    assert_eq!(body.data(vec![1]), Err(Refused::NoRoom { room: 0 }));
    assert_eq!(drain(&mut t), [(id, Call::Readable)]);
    assert_eq!(t.read(id, 8192), Read::Bytes(vec![7; 8192]));
    assert_eq!(credits(&asked.said(), id), 1);
    assert_eq!(body.room(), 8192, "the bytes read are room again");
    let _big = body;
    // The end and a failure, read after the bytes.
    let id = t.start(&host.url("small.txt"), None);
    let mut body = asked.reply().head(Head::whole("text/plain", 10));
    body.data(vec![1; 10]).unwrap();
    body.done().unwrap();
    assert_eq!(drain(&mut t).len(), 3, "the head and two readables");
    assert_eq!(t.read(id, 8), Read::Bytes(vec![1; 8]));
    assert_eq!(t.read(id, 8), Read::Bytes(vec![1; 2]));
    assert_eq!(t.read(id, 8), Read::End);
    assert_eq!(t.read(id, 8), Read::Gone);
    let id = t.start(&host.url("lost.txt"), None);
    let body = asked.reply().head(Head::whole("text/plain", 10));
    body.fail("the connection went");
    let _ = drain(&mut t);
    assert_eq!(t.read(id, 8), Read::Failed("the connection went".into()));
    assert_eq!(t.live(), 1, "the first request's body is still open");
    // Push: nothing to read.
    let (mut push, asked, host, _) = table(Mode::Push);
    let id = push.start(&host.url("a"), None);
    let _reply = asked.reply();
    assert_eq!(push.read(id, 8), Read::Gone);
}

/// Linux's stop before the head (X-W1): the request has no signal of its own, the view's
/// resource failing names its address; the oldest head-less request there is stopped, and its
/// late head never reaches the engine. A request with its head is stopped by its read's
/// cancellable instead (`stop`).
#[test]
fn x_w1_linux_stops_a_request_before_its_head_by_its_address() {
    let (mut t, asked, host, _) = table(Mode::Pull);
    let img = host.url("img.png");
    let a = t.start(&img, None);
    let b = t.start(&format!("{img}?v=2"), None);
    let c = t.start(&host.url("x.css"), None);
    let (ra, _rb, rc) = (asked.reply(), asked.reply(), asked.reply());
    let _css = rc.head(Head::whole("text/css", 1));
    assert_eq!(drain(&mut t).len(), 1);
    assert_eq!(t.stop_headless(&img), Some(a));
    assert_eq!(t.stop_headless(&img), Some(b), "the query is not the path");
    assert_eq!(t.stop_headless(&img), None);
    assert_eq!(t.stop_headless(&host.url("x.css")), None, "it has its head");
    let _late = ra.head(Head::whole("image/png", 1));
    assert!(drain(&mut t).is_empty());
    let said = asked.said();
    assert_eq!(
        (stops(&said, a), stops(&said, b), stops(&said, c)),
        (1, 1, 0)
    );
}

/// Each view's handler serves its own only (§3.1): another view's address, or another scheme's,
/// fails without reaching the server.
#[test]
fn x_w1_another_views_address_is_never_served() {
    let (mut t, asked, _, _) = table(Mode::Push);
    let other = Host::new();
    for url in [other.url("a.html"), "https://example.com/a".to_string()] {
        let id = t.start(&url, None);
        let calls = drain(&mut t);
        assert!(
            matches!(calls.as_slice(), [(i, Call::Fail(_))] if *i == id),
            "{url}: {calls:?}"
        );
    }
    assert_eq!(starts(&asked.said()), 0);
    assert_eq!(t.live(), 0);
}

/// A reply or body the server drops unanswered fails its request (the engine never waits on
/// it); dropping the table — the view closed — stops every request it still has.
#[test]
fn x_w1_a_dropped_answer_fails_and_a_closed_view_stops_all() {
    let (mut t, asked, host, _) = table(Mode::Push);
    let a = t.start(&host.url("a"), None);
    drop(asked.reply());
    let b = t.start(&host.url("b"), None);
    let head = Head::whole("text/plain", 5);
    drop(asked.reply().head(head.clone()));
    let calls = drain(&mut t);
    let dropped = match calls.as_slice() {
        [(i, Call::Fail(_)), (j, Call::Head(_)), (k, Call::Fail(_))] => (*i, *j, *k) == (a, b, b),
        _ => false,
    };
    assert!(dropped, "{calls:?}");
    let c = t.start(&host.url("c"), None);
    let d = t.start(&host.url("d"), None);
    let (rc, rd) = (asked.reply(), asked.reply());
    drop(t);
    assert!(rc.stopped() && rd.stopped());
    let said = asked.said();
    assert_eq!((stops(&said, c), stops(&said, d)), (1, 1));
}

/// The head's fields, the same for both engines.
#[test]
fn x_w1_the_heads_fields() {
    let h = Head::whole("text/html; charset=utf-8", 120);
    assert_eq!((h.code(), h.reason(), h.length()), (200, "OK", 120));
    assert_eq!(
        h.headers(),
        [
            ("Content-Type", "text/html; charset=utf-8".to_string()),
            ("Content-Length", "120".to_string()),
            ("Accept-Ranges", "bytes".to_string()),
        ]
    );
    let p = Head::part("video/mp4", 2809, 2812, 4 << 20).unwrap();
    assert_eq!((p.code(), p.length()), (206, 4));
    assert_eq!(
        p.headers()[2],
        ("Content-Range", "bytes 2809-2812/4194304".to_string())
    );
    assert!(Head::part("video/mp4", 5, 4, 10).is_none());
    assert!(Head::part("video/mp4", 0, 10, 10).is_none());
    let u = Head::unsatisfiable(4 << 20);
    assert_eq!((u.code(), u.length()), (416, 0));
    assert!(u
        .headers()
        .contains(&("Content-Range", "bytes */4194304".to_string())));
    assert_eq!(
        (Head::forbidden().code(), Head::not_found().code()),
        (403, 404)
    );
    assert_eq!(
        Head::not_found().headers()[0],
        ("Content-Length", "0".to_string())
    );
}

/// The view's scheme host and its addresses (§3.1): 128 random bits, never two alike; a path
/// percent-encoded into the address and back out of it, without its query or fragment.
#[test]
fn x_w1_the_view_host_and_its_addresses() {
    let h = Host::new();
    assert_eq!(h.as_str().len(), 32);
    assert!(h.as_str().bytes().all(|b| b.is_ascii_hexdigit()));
    assert_ne!(h, Host::new());
    let url = h.url("/sub/a b#?%.html");
    assert_eq!(
        url,
        format!("sopen-page://{}/sub/a%20b%23%3F%25.html", h.as_str())
    );
    assert_eq!(h.path_of(&url), Some("/sub/a%20b%23%3F%25.html"));
    assert_eq!(
        h.path_of(&format!("{url}?v=1#top")),
        Some("/sub/a%20b%23%3F%25.html")
    );
    assert_eq!(
        h.path_of(&format!("SOPEN-PAGE://{}", h.as_str())),
        Some("/")
    );
    assert_eq!(h.path_of(&format!("sopen-page://{}x/a", h.as_str())), None);
    assert_eq!(h.path_of(&Host::new().url("a")), None);
    assert_eq!(h.path_of(&format!("http://{}/a", h.as_str())), None);
}

// ---------------------------------------------------------------------------------------------
// The component.

/// A window hosting one page view at `rect`, with the things that cover it; what reached the
/// window's own pass after the page's is kept.
struct Hosting {
    webs: Vec<Web>,
    rect: Rect,
    draw: bool,
    dialog: bool,
    popup: Option<Rect>,
    keys: Vec<(Key, Modifiers)>,
    typed: String,
}

impl Hosting {
    fn new(webs: Vec<Web>) -> Self {
        Hosting {
            webs,
            rect: Rect::from_min_max(Pos2::new(50.0, 50.0), Pos2::new(250.0, 200.0)),
            draw: true,
            dialog: false,
            popup: None,
            keys: vec![],
            typed: String::new(),
        }
    }
}

impl Window for Hosting {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            if self.draw {
                for (n, web) in self.webs.iter_mut().enumerate() {
                    web.ui(ui, self.rect.translate(egui::vec2(0.0, 20.0 * n as f32)));
                }
            }
        });
        ctx.input(|i| {
            for e in &i.events {
                match e {
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => self.keys.push((*key, *modifiers)),
                    Event::Text(t) => self.typed.push_str(t),
                    _ => {}
                }
            }
        });
        if self.dialog {
            chrome::dialog(&ctx, egui::Id::unique("wv0-dialog"), "Close", |ui| {
                ui.label("Save the changes first?");
            });
        }
        if let Some(r) = self.popup {
            egui::Area::new(egui::Id::unique("wv0-popup"))
                .order(egui::Order::Foreground)
                .fixed_pos(r.min)
                .show(&ctx, |ui| {
                    ui.set_min_size(r.size());
                    ui.label("menu");
                });
        }
    }

    fn ready(&self) -> bool {
        true
    }
}

fn page(asked: &Asked) -> Web {
    Web::new(
        Content::Page {
            server: asked.server(),
        },
        Settings::default(),
    )
    .expect("the fake opens it")
}

fn address() -> Web {
    Web::new(
        Content::Address {
            origin: "http://127.0.0.1:8000".into(),
        },
        Settings::default(),
    )
    .expect("the fake opens it")
}

fn opened(page: bool, scripts: bool) -> Did {
    Did::Open {
        page,
        preview: false,
        scripts,
    }
}

/// §4: `SOPEN_WEB=off` is the reason on every system, the loader not even tried; unset, empty or
/// anything else leaves the pick to the loader, whose error is the reason. A thread that picked
/// no engine gets the reason from every `Web::new`.
#[test]
fn sopen_web_off_forces_the_reason() {
    let fake = FakeEngine::native();
    let tried = std::cell::Cell::new(0);
    let load = || {
        tried.set(tried.get() + 1);
        Ok(Rc::new(fake.clone()) as Rc<dyn Engine>)
    };
    let off = choose(Some("off"), load);
    assert_eq!(off.as_ref().err(), Some(&Unavailable::NoEngine(OFF.into())));
    assert_eq!(tried.get(), 0, "the loader is not tried");
    assert_eq!(
        off.err().unwrap().line(),
        "no web engine here (turned off (SOPEN_WEB=off))"
    );
    for value in [None, Some(""), Some("on"), Some("OFF")] {
        assert!(choose(value, load).is_ok(), "{value:?}");
    }
    assert_eq!(tried.get(), 4);
    let missing = choose(None, || Err("libwebkit2gtk-4.1.so.0: not found".into()));
    assert_eq!(
        missing.err().unwrap().line(),
        "no web engine here (libwebkit2gtk-4.1.so.0: not found)"
    );
    use_engine(choose(Some("off"), load));
    assert_eq!(
        Web::new(
            Content::Address {
                origin: "http://127.0.0.1:1".into()
            },
            Settings::default()
        )
        .err(),
        Some(Unavailable::NoEngine(OFF.into()))
    );
    let asked = Asked::default();
    let page = Web::new(
        Content::Page {
            server: asked.server(),
        },
        Settings::default(),
    );
    assert_eq!(page.err(), Some(Unavailable::NoEngine(OFF.into())));
}

/// §3.3 R10 and §4: a backend that does not contain a file view with scripts off shows file pages
/// as source, naming the channel; the address window is not affected. An engine that cannot open
/// a view gives its reason.
#[test]
fn an_uncontained_backend_shows_file_pages_as_source_not_address_windows() {
    let fake = FakeEngine::native();
    fake.install().set_uncontained(false, Some("DNS"));
    let asked = Asked::default();
    let page = Web::new(
        Content::Page {
            server: asked.server(),
        },
        Settings::default(),
    );
    let why = page.err().unwrap();
    assert_eq!(why, Unavailable::NotContained("DNS".into()));
    assert_eq!(why.line(), "the page view is not contained here (DNS)");
    assert!(address_ok());
    fake.refuse_open(Some("gtk_init_check failed"));
    assert_eq!(
        Web::new(
            Content::Address {
                origin: "http://127.0.0.1:1".into()
            },
            Settings::default()
        )
        .err(),
        Some(Unavailable::NoEngine("gtk_init_check failed".into()))
    );
}

fn address_ok() -> bool {
    Web::new(
        Content::Address {
            origin: "http://127.0.0.1:8000".into(),
        },
        Settings::default(),
    )
    .is_ok()
}

/// A file view opens hidden with scripts off, its own scheme registered; `load` opens a path
/// under its root in that scheme, and the engine's requests reach the server with the path. An
/// address window opens with scripts on and loads paths on its origin.
#[test]
fn a_file_view_loads_inside_its_own_scheme_and_an_address_on_its_origin() {
    let fake = FakeEngine::native();
    fake.install();
    let asked = Asked::default();
    let mut web = page(&asked);
    assert_eq!(fake.calls(0), [opened(true, false)]);
    assert!(!web.scripts());
    let host = fake.host(0);
    web.load("sub/a b.html");
    assert_eq!(
        fake.calls(0)[1],
        Did::Load(format!("sopen-page://{}/sub/a%20b.html", host.as_str()))
    );
    let id = fake.request(0, &host.url("sub/a b.html"), None);
    assert_eq!(
        asked.said(),
        [Said::Start(Request {
            id,
            path: "/sub/a%20b.html".into(),
            range: None,
        })]
    );
    asked.reply().head(Head::not_found()).done().unwrap();
    assert_eq!(fake.woke(0), 1);
    assert_eq!(fake.pump(0).len(), 2);
    web.reload();
    assert_eq!(fake.calls(0).last(), Some(&Did::Reload));
    // WebKit stops one; the view's close stops the rest.
    let (a, b) = (
        fake.request(0, &host.url("a.css"), None),
        fake.request(0, &host.url("b.css"), None),
    );
    fake.stop(0, a);
    web.close();
    assert!(!fake.has_table(0), "the requests go with the view");
    let said = asked.said();
    assert_eq!((stops(&said, a), stops(&said, b)), (1, 1));
    let mut web = address();
    assert_eq!(fake.calls(1), [opened(false, true)]);
    assert!(web.scripts());
    web.load("/x?y=1");
    assert_eq!(
        fake.calls(1)[1],
        Did::Load("http://127.0.0.1:8000/x?y=1".into())
    );
}

/// WV-1: a page's frame is the rect it is drawn at, told once per change of rect or scale, and
/// nothing is told while nothing changes. A view is put into its window the first frame it is
/// drawn, with the window's handle, then shown.
#[test]
fn wv_1_the_frame_is_told_only_when_it_changes() {
    let fake = FakeEngine::native();
    fake.install();
    let mut win = Hosting::new(vec![address()]);
    let mut drv = Driver::new(400.0, 300.0);
    let r1 = win.rect;
    drv.idle(&mut win, 3);
    assert_eq!(
        fake.take(0),
        [
            opened(false, true),
            Did::Attach(Parent::Window),
            Did::Place(r1, 1.0),
            Did::Hidden(false),
        ]
    );
    drv.idle(&mut win, 3);
    assert_eq!(fake.take(0), []);
    win.rect = Rect::from_min_max(Pos2::new(40.0, 50.0), Pos2::new(300.0, 260.0));
    drv.idle(&mut win, 3);
    assert_eq!(fake.take(0), [Did::Place(win.rect, 1.0)]);
    drv.resize(500.0, 400.0);
    drv.idle(&mut win, 2);
    assert_eq!(fake.take(0), [], "the window's size is not the page's");
}

/// Passes until `text` is painted (at most 3: a new egui area spends its first frame on an
/// invisible sizing pass); the view gets no call before that frame. Its calls in that frame.
fn until_painted(drv: &mut Driver, win: &mut Hosting, fake: &FakeEngine, text: &str) -> Vec<Did> {
    for _ in 0..3 {
        drv.frame(win, vec![], Modifiers::NONE);
        let calls = fake.take(0);
        if drv.find_text(text).is_some() {
            return calls;
        }
        assert_eq!(calls, [], "{text}: a call before it shows");
    }
    panic!("{text} is never painted");
}

const SAVE: &str = "Save the changes first?";

/// WV-2 (§2.2): a native view is hidden in the very frame anything egui paints above the page's
/// own layer meets its rect — a dialog (an `egui::Modal`: its backdrop covers the whole window,
/// so a view beside the box hides too), a popup area, F1's help (a painter of its own, after
/// the window's pass) — and in a frame that did not draw it; shown again at the first frame
/// drawn with none. Something above it elsewhere leaves it shown.
#[test]
fn wv_2_a_native_view_hides_under_anything_painted_above_it_in_the_same_frame() {
    let fake = FakeEngine::native();
    fake.install();
    let mut win = Hosting::new(vec![address()]);
    let mut drv = Driver::new(400.0, 300.0);
    drv.idle(&mut win, 2);
    fake.take(0);
    let pass = |drv: &mut Driver, win: &mut Hosting| drv.frame(win, vec![], Modifiers::NONE);
    win.dialog = true;
    let calls = until_painted(&mut drv, &mut win, &fake, SAVE);
    assert_eq!(calls, [Did::Hidden(true)], "the frame the dialog shows");
    pass(&mut drv, &mut win);
    assert_eq!(fake.take(0), []);
    win.dialog = false;
    pass(&mut drv, &mut win);
    assert_eq!(fake.take(0), [Did::Hidden(false)]);
    // Beside the dialog's box: under its backdrop all the same.
    win.rect = Rect::from_min_size(Pos2::new(10.0, 10.0), egui::vec2(50.0, 30.0));
    win.dialog = true;
    pass(&mut drv, &mut win);
    let dialog = drv.find_text(SAVE).expect("the dialog shows");
    assert!(!dialog.intersects(win.rect), "{dialog:?}");
    assert_eq!(fake.take(0), [Did::Place(win.rect, 1.0), Did::Hidden(true)]);
    win.dialog = false;
    win.rect = Rect::from_min_max(Pos2::new(50.0, 50.0), Pos2::new(250.0, 200.0));
    pass(&mut drv, &mut win);
    assert_eq!(
        fake.take(0),
        [Did::Place(win.rect, 1.0), Did::Hidden(false)]
    );
    win.popup = Some(Rect::from_min_size(
        Pos2::new(200.0, 150.0),
        egui::vec2(80.0, 60.0),
    ));
    let calls = until_painted(&mut drv, &mut win, &fake, "menu");
    assert_eq!(calls, [Did::Hidden(true)], "a popup over its corner");
    win.popup = None;
    pass(&mut drv, &mut win);
    assert_eq!(fake.take(0), [Did::Hidden(false)]);
    win.popup = Some(Rect::from_min_size(
        Pos2::new(300.0, 220.0),
        egui::vec2(60.0, 40.0),
    ));
    pass(&mut drv, &mut win);
    pass(&mut drv, &mut win);
    assert!(drv.find_text("menu").is_some());
    assert_eq!(fake.take(0), [], "a popup elsewhere");
    win.popup = None;
    help::open(&drv.ctx, help::for_window(&[]));
    pass(&mut drv, &mut win);
    assert!(help::is_open(&drv.ctx));
    assert_eq!(
        fake.take(0),
        [Did::Hidden(true)],
        "the key help, painted after the pass"
    );
    drv.key(&mut win, Key::Escape, Modifiers::NONE);
    assert!(!help::is_open(&drv.ctx));
    assert_eq!(fake.take(0), [Did::Hidden(false)]);
    win.draw = false;
    pass(&mut drv, &mut win);
    assert_eq!(fake.take(0), [Did::Hidden(true)], "not drawn");
    win.draw = true;
    pass(&mut drv, &mut win);
    assert_eq!(fake.take(0), [Did::Hidden(false)]);
}

/// The painted-above check itself: the page's own layer and what is under it never count;
/// everything painted later does — here a painter-only layer of a higher order, which egui
/// lists nowhere — and a shape painted invisibly does not.
#[test]
fn wv_2_what_is_painted_above_a_layer() {
    let ctx = egui::Context::default();
    let page = egui::LayerId::background();
    let mut above = vec![];
    let tip = Rect::from_min_size(Pos2::new(20.0, 20.0), egui::vec2(5.0, 5.0));
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        let own = Rect::from_min_size(Pos2::ZERO, egui::vec2(9.0, 9.0));
        ui.painter().rect_filled(own, 0.0, egui::Color32::RED);
        let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("tip"));
        let painter = ui.ctx().layer_painter(layer);
        painter.rect_filled(tip, 0.0, egui::Color32::BLUE);
        let mut faded = painter.clone();
        faded.set_invisible();
        faded.rect_filled(own, 0.0, egui::Color32::BLUE);
        above = painted_above(ui.ctx(), page);
    });
    out.textures_delta.clear();
    assert_eq!(
        above,
        [tip],
        "the painter-only layer above, not the page's own shape"
    );
}

/// Within one order egui's own sequence decides: of two areas of an order, the one egui paints
/// later is above the other, never below it.
#[test]
fn wv_2_a_later_area_of_the_same_order_is_above() {
    let ctx = egui::Context::default();
    let first = Rect::from_min_size(Pos2::new(10.0, 10.0), egui::vec2(30.0, 30.0));
    let second = Rect::from_min_size(Pos2::new(20.0, 20.0), egui::vec2(30.0, 30.0));
    let mut above = (vec![], vec![]);
    // The second pass: an area's first is its invisible sizing pass.
    for _ in 0..2 {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut layers = vec![];
            for (name, rect) in [("first", first), ("second", second)] {
                let shown = egui::Area::new(egui::Id::unique(name))
                    .order(egui::Order::Foreground)
                    .fixed_pos(rect.min)
                    .show(ui.ctx(), |ui| {
                        ui.painter().rect_filled(rect, 0.0, egui::Color32::RED);
                        ui.allocate_space(rect.size());
                    });
                layers.push(shown.response.layer_id);
            }
            above = (
                painted_above(ui.ctx(), layers[0]),
                painted_above(ui.ctx(), layers[1]),
            );
        });
        out.textures_delta.clear();
    }
    assert!(above.0.contains(&second), "{:?}", above.0);
    assert!(!above.1.contains(&first), "{:?}", above.1);
}

/// §2.3: a texture is egui content — painted at its rect each frame, drawn over by dialogs and
/// never hidden for them; a frame that does not draw it still hides it (nothing to keep fresh).
#[test]
fn wv_2_a_texture_is_drawn_over_not_hidden() {
    let fake = FakeEngine::texture();
    fake.install();
    let mut win = Hosting::new(vec![address()]);
    let mut drv = Driver::new(400.0, 300.0);
    drv.idle(&mut win, 2);
    assert!(fake.take(0).contains(&Did::Paint(win.rect)));
    win.dialog = true;
    drv.idle(&mut win, 2);
    let calls = fake.take(0);
    assert!(!calls.contains(&Did::Hidden(true)), "{calls:?}");
    assert!(calls.contains(&Did::Paint(win.rect)));
    win.draw = false;
    drv.idle(&mut win, 1);
    assert_eq!(fake.take(0), [Did::Hidden(true)]);
}

/// §2.1, §2.5: every view of a window closes before the window goes, those of another window
/// stay; a closed view gets no later call. The key monitor comes with the first view put into a
/// window and goes with the last one closed.
#[test]
fn every_view_of_a_window_closes_before_it_goes_and_the_monitor_with_the_last() {
    let fake = FakeEngine::native();
    fake.install();
    let mut win = Hosting::new(vec![address(), address()]);
    let mut other = Hosting::new(vec![address()]);
    let never_drawn = address();
    let (mut drv, mut drv2) = (Driver::new(400.0, 300.0), Driver::new(400.0, 300.0));
    assert_eq!(fake.monitor(), Vec::<bool>::new());
    drv.idle(&mut win, 2);
    assert_eq!(fake.monitor(), [true], "with the first view in a window");
    drv2.idle(&mut other, 2);
    assert_eq!(fake.monitor(), [true]);
    teardown(&drv.ctx);
    assert_eq!(fake.calls(0).last(), Some(&Did::Close));
    assert_eq!(fake.calls(1).last(), Some(&Did::Close));
    assert_ne!(fake.calls(2).last(), Some(&Did::Close), "another window's");
    assert_eq!(fake.monitor(), [true], "a view is still in a window");
    // Closed: no later call, whatever the host does.
    let before = (fake.calls(0).len(), fake.calls(1).len());
    drv.idle(&mut win, 2);
    for web in &mut win.webs {
        web.load("/x");
        web.reload();
        let _ = web.set_scripts(false);
        web.set_preview(true);
        web.set_shown(false);
        web.close();
    }
    teardown(&drv.ctx);
    assert_eq!((fake.calls(0).len(), fake.calls(1).len()), before);
    drop(win);
    drop(never_drawn);
    assert_eq!(
        fake.calls(3).last(),
        Some(&Did::Close),
        "a dropped view closes"
    );
    assert_eq!(
        fake.monitor(),
        [true],
        "a view never in a window is not counted"
    );
    drop(other);
    assert_eq!(fake.monitor(), [true, false], "with the last");
}

/// The shell's part (§2.1): the window's handle reaches the view at its first frame (eframe's
/// test frame has none to give: the plain window); the close that goes through tears every
/// view down in that same pass; the shell's end does it too if nothing else did.
#[test]
fn the_shells_close_tears_every_view_down_in_its_pass() {
    use crate::gui::{Press, Raise, Shell};
    let fake = FakeEngine::native();
    fake.install();
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let mut shell = Shell {
        inner: Hosting::new(vec![address()]),
        scripted: false,
        shot: None,
        frames: 0,
        started: std::time::Instant::now(),
        focus_asked: None,
        raise: Raise::Done,
        press: Press::Done,
        fkeys: None,
        closed: Arc::default(),
        web: None,
    };
    let pass = |shell: &mut Shell<Hosting>, close: bool| {
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 300.0))),
            ..Default::default()
        };
        if close {
            raw.viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .events
                .push(egui::ViewportEvent::Close);
        }
        let mut frame = eframe::Frame::_new_kittest();
        let mut out = ctx.run_ui(raw, |ui| eframe::App::ui(shell, ui, &mut frame));
        out.textures_delta.clear();
    };
    pass(&mut shell, false);
    assert!(fake.calls(0).contains(&Did::Attach(Parent::Window)));
    assert!(!fake.calls(0).contains(&Did::Close));
    pass(&mut shell, true);
    assert!(shell.closed.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        fake.calls(0).last(),
        Some(&Did::Close),
        "in the close's pass"
    );
    // The shell's end, a view still open (drawn after the close, as a hosted viewer might be).
    let mut shell2 = Shell {
        inner: Hosting::new(vec![address()]),
        scripted: false,
        shot: None,
        frames: 0,
        started: std::time::Instant::now(),
        focus_asked: None,
        raise: Raise::Done,
        press: Press::Done,
        fkeys: None,
        closed: Arc::default(),
        web: None,
    };
    pass(&mut shell2, false);
    assert!(!fake.calls(1).contains(&Did::Close));
    // The window's `Web`s outlive the shell here, as a host's own field would not.
    let webs = std::mem::take(&mut shell2.inner.webs);
    drop(shell2);
    assert_eq!(fake.calls(1).last(), Some(&Did::Close), "the shell's end");
    drop(webs);
}

/// §2.4: where a key goes. While sopen has the keyboard everything is sopen's; while the page has
/// it, Esc takes it back and does nothing else, the other always-sopen keys take it back and act,
/// and every other key is the page's.
#[test]
fn keys_go_by_who_has_the_keyboard() {
    let cmd = Modifiers::COMMAND;
    let always = [
        (Key::F1, Modifiers::NONE),
        (Key::F3, Modifiers::NONE),
        (Key::F12, Modifiers::NONE),
        (Key::F5, Modifiers::SHIFT),
        (Key::W, cmd),
        (Key::Q, cmd),
        (Key::Num1, cmd),
        (Key::Num0, cmd),
    ];
    let pages = [
        (Key::A, Modifiers::NONE),
        (Key::Tab, Modifiers::NONE),
        (Key::Enter, Modifiers::NONE),
        (Key::C, cmd),
        (Key::V, cmd),
        (Key::A, cmd),
        (Key::R, cmd),
        (Key::W, Modifiers::NONE),
        (Key::W, Modifiers::SHIFT | cmd),
        (Key::Num1, Modifiers::NONE),
        (Key::Num1, Modifiers::ALT),
    ];
    for (key, m) in always
        .iter()
        .chain(&pages)
        .chain(&[(Key::Escape, Modifiers::NONE)])
    {
        assert_eq!(
            route(Keyboard::Sopen, *key, *m),
            Route::Sopen,
            "{key:?} {m:?}"
        );
    }
    assert_eq!(
        route(Keyboard::Page, Key::Escape, Modifiers::NONE),
        Route::TakeBack
    );
    assert_eq!(
        route(Keyboard::Page, Key::Escape, Modifiers::SHIFT),
        Route::TakeBack
    );
    for (key, m) in always {
        assert_eq!(
            route(Keyboard::Page, key, m),
            Route::TakeBackAndAct,
            "{key:?} {m:?}"
        );
    }
    for (key, m) in pages {
        assert_eq!(route(Keyboard::Page, key, m), Route::Page, "{key:?} {m:?}");
    }
}

/// §2.4 on Linux (a texture): a view opens with sopen holding the keyboard; a click in the page
/// gives it to the page, whose keys and typing are forwarded and never reach the window; Esc
/// takes it back and nothing more, a second Esc is the window's; F3 and Cmd/Ctrl+1 (F1) take it
/// back and act; a click elsewhere takes it back.
#[test]
fn a_texture_page_takes_the_keyboard_by_a_click_and_esc_gives_it_back() {
    let fake = FakeEngine::texture();
    fake.install();
    let mut win = Hosting::new(vec![address()]);
    let mut drv = Driver::new(400.0, 300.0);
    drv.idle(&mut win, 2);
    assert_eq!(win.webs[0].keyboard(), Keyboard::Sopen);
    let inside = win.rect.center();
    drv.click(&mut win, inside);
    assert_eq!(win.webs[0].keyboard(), Keyboard::Page);
    let forwarded = |fake: &FakeEngine| -> Vec<Event> {
        fake.take(0)
            .into_iter()
            .filter_map(|d| match d {
                Did::Input(e) => Some(e),
                _ => None,
            })
            .collect()
    };
    let clicks = forwarded(&fake);
    let at = inside - win.rect.min.to_vec2();
    assert!(
        clicks
            .iter()
            .any(|e| matches!(e, Event::PointerButton { pos, pressed: true, .. } if *pos == at)),
        "the press, in the page's own points: {clicks:?}"
    );
    win.keys.clear();
    drv.frame(&mut win, vec![Event::Text("abc".into())], Modifiers::NONE);
    drv.key(&mut win, Key::A, Modifiers::NONE);
    drv.key(&mut win, Key::Tab, Modifiers::NONE);
    assert_eq!((win.typed.as_str(), win.keys.as_slice()), ("", &[][..]));
    let keys = forwarded(&fake);
    assert!(keys.contains(&Event::Text("abc".into())), "{keys:?}");
    assert!(keys.iter().any(|e| matches!(
        e,
        Event::Key {
            key: Key::Tab,
            pressed: true,
            ..
        }
    )));
    drv.key(&mut win, Key::Escape, Modifiers::NONE);
    assert_eq!(win.webs[0].keyboard(), Keyboard::Sopen);
    assert_eq!(win.keys, [], "the first Esc only takes the keyboard back");
    drv.key(&mut win, Key::Escape, Modifiers::NONE);
    assert_eq!(
        win.keys,
        [(Key::Escape, Modifiers::NONE)],
        "the second is the window's"
    );
    for (key, mods, seen) in [
        (Key::F3, Modifiers::NONE, Key::F3),
        (Key::Num1, Modifiers::COMMAND, Key::F1),
    ] {
        drv.click(&mut win, inside);
        assert_eq!(win.webs[0].keyboard(), Keyboard::Page);
        win.keys.clear();
        drv.key(&mut win, key, mods);
        assert_eq!(win.webs[0].keyboard(), Keyboard::Sopen, "{key:?}");
        assert!(
            win.keys.iter().any(|(k, _)| *k == seen),
            "{key:?}: {:?}",
            win.keys
        );
    }
    drv.click(&mut win, inside);
    drv.click(&mut win, Pos2::new(350.0, 280.0));
    assert_eq!(win.webs[0].keyboard(), Keyboard::Sopen, "a click elsewhere");
}

/// §2.4 on macOS (native): who has the keyboard is the view's word each frame (AppKit's first
/// responder), never tracked from clicks; nothing is forwarded. In a preview sopen always has it.
#[test]
fn a_native_pages_keyboard_is_the_views_word() {
    let fake = FakeEngine::native();
    fake.install();
    let mut win = Hosting::new(vec![address()]);
    let mut drv = Driver::new(400.0, 300.0);
    drv.idle(&mut win, 2);
    fake.set_page_keyboard(0, true);
    assert_eq!(win.webs[0].keyboard(), Keyboard::Page);
    fake.set_page_keyboard(0, false);
    assert_eq!(win.webs[0].keyboard(), Keyboard::Sopen);
    let centre = win.rect.center();
    drv.click(&mut win, centre);
    assert_eq!(
        win.webs[0].keyboard(),
        Keyboard::Sopen,
        "a click is AppKit's business"
    );
    assert!(!fake.calls(0).iter().any(|d| matches!(d, Did::Input(_))));
    let asked = Asked::default();
    let preview = Web::new(
        Content::Page {
            server: asked.server(),
        },
        Settings { preview: true },
    )
    .unwrap();
    fake.set_page_keyboard(1, true);
    assert_eq!(preview.keyboard(), Keyboard::Sopen);
}

/// A dialog over a page that has the keyboard gets the keys: a native view gives the keyboard
/// back before it hides (a hidden first responder would swallow them); a texture stops
/// forwarding and sopen has the keyboard; so the dialog's Enter is the window's.
#[test]
fn a_dialog_over_the_page_takes_the_keyboard_back() {
    let fake = FakeEngine::native();
    fake.install();
    let mut win = Hosting::new(vec![address()]);
    let mut drv = Driver::new(400.0, 300.0);
    drv.idle(&mut win, 2);
    fake.set_page_keyboard(0, true);
    fake.take(0);
    win.dialog = true;
    let calls = until_painted(&mut drv, &mut win, &fake, SAVE);
    assert_eq!(calls, [Did::TakeKeyboard, Did::Hidden(true)]);
    assert_eq!(win.webs[0].keyboard(), Keyboard::Sopen);
    let texture = FakeEngine::texture();
    texture.install();
    let mut win = Hosting::new(vec![address()]);
    let mut drv = Driver::new(400.0, 300.0);
    drv.idle(&mut win, 2);
    let centre = win.rect.center();
    drv.click(&mut win, centre);
    assert_eq!(win.webs[0].keyboard(), Keyboard::Page);
    win.dialog = true;
    drv.idle(&mut win, 3);
    texture.take(0);
    win.keys.clear();
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    assert_eq!(win.webs[0].keyboard(), Keyboard::Sopen);
    assert!(
        win.keys.contains(&(Key::Enter, Modifiers::NONE)),
        "{:?}",
        win.keys
    );
    let calls = texture.take(0);
    assert!(
        !calls
            .iter()
            .any(|d| matches!(d, Did::Input(Event::Key { .. }))),
        "{calls:?}"
    );
}

/// §6: in the preview nothing reaches the page — no click gives it the keyboard, nothing is
/// forwarded — and F2 does nothing; the hand-over makes it a viewer.
#[test]
fn the_preview_takes_no_input_and_f2_does_nothing_there() {
    let fake = FakeEngine::texture();
    fake.install();
    let asked = Asked::default();
    let preview = Web::new(
        Content::Page {
            server: asked.server(),
        },
        Settings { preview: true },
    )
    .unwrap();
    assert_eq!(
        fake.calls(0),
        [Did::Open {
            page: true,
            preview: true,
            scripts: false
        }]
    );
    let mut win = Hosting::new(vec![preview]);
    let mut drv = Driver::new(400.0, 300.0);
    drv.idle(&mut win, 2);
    let centre = win.rect.center();
    drv.click(&mut win, centre);
    drv.key(&mut win, Key::A, Modifiers::NONE);
    assert_eq!(win.webs[0].keyboard(), Keyboard::Sopen);
    assert!(!fake.calls(0).iter().any(|d| matches!(d, Did::Input(_))));
    assert!(
        win.keys.contains(&(Key::A, Modifiers::NONE)),
        "the key is the window's"
    );
    assert_eq!(win.webs[0].set_scripts(true), Ok(false));
    win.webs[0].set_preview(false);
    assert_eq!(fake.calls(0).last(), Some(&Did::Preview(false)));
    let centre = win.rect.center();
    drv.click(&mut win, centre);
    assert_eq!(win.webs[0].keyboard(), Keyboard::Page, "a viewer now");
}

/// §3.3: a file view starts with scripts off; F2 turns them on only where the backend contains
/// every channel (R1) and only on a WebKit version that passed (R11) — none has yet, so F2 says
/// why and does nothing. An address window starts with them on and F2 toggles, reloading.
#[test]
fn f2_turns_a_file_views_scripts_on_only_where_they_are_available() {
    let fake = FakeEngine::native();
    fake.install();
    let asked = Asked::default();
    let mut web = page(&asked);
    assert!(!web.scripts());
    assert_eq!(
        web.set_scripts(true),
        Err("scripts are not available with this WebKit (2.52.6): not yet checked".into())
    );
    fake.set_uncontained(true, Some("WebRTC"));
    assert_eq!(
        web.set_scripts(true),
        Err("scripts are not available here: WebRTC is not contained".into())
    );
    assert_eq!(web.set_scripts(false), Ok(false), "already off");
    fake.set_uncontained(true, None);
    fake.set_version("2.50.0");
    assert_eq!(
        web.set_scripts(true),
        Err("scripts are not available with this WebKit (2.50.0): not yet checked".into()),
        "the version read when the engine loaded"
    );
    fake.set_uncontained(true, Some("WebRTC"));
    assert!(!fake
        .calls(0)
        .iter()
        .any(|d| matches!(d, Did::Scripts(_) | Did::Reload)));
    assert_eq!(scripts_check("2.52.6", &["2.52.6"], None), Ok(()));
    assert_eq!(
        scripts_check("2.52.7", &["2.52.6"], None),
        Err("scripts are not available with this WebKit (2.52.7): not yet checked".into())
    );
    assert!(scripts_check("2.52.6", &["2.52.6"], Some("DNS")).is_err());
    assert_eq!(
        scripts_available(&fake),
        Err("scripts are not available here: WebRTC is not contained".into())
    );
    let mut web = address();
    assert!(web.scripts());
    assert_eq!(web.set_scripts(false), Ok(true));
    assert_eq!(fake.calls(1)[1..], [Did::Scripts(false), Did::Reload]);
    assert!(!web.scripts());
    assert_eq!(
        web.set_scripts(true),
        Ok(true),
        "not a file view: no version check"
    );
}

/// The view's events come back in order, once each; shown or not passes on.
#[test]
fn the_views_events_come_back_in_order() {
    let fake = FakeEngine::native();
    fake.install();
    let mut web = address();
    let said = fake.events(0);
    let events = [
        Event1::Loading,
        Event1::Document("/index.html".into()),
        Event1::Blocked(3),
        Event1::Refused {
            target: "https://example.com/".into(),
            clicked: true,
        },
        Event1::Finished,
        Event1::Failed("cannot load".into()),
        Event1::ProcessEnded,
    ];
    for e in events.clone() {
        said.send(e);
    }
    assert_eq!(web.events(), events);
    assert_eq!(web.events(), []);
    web.set_shown(false);
    assert_eq!(fake.calls(0).last(), Some(&Did::Shown(false)));
}

use super::Event as Event1;

/// §2.2 receipts: the screenshot asks every native view the window shows for its picture — not a
/// hidden one, not a texture's (the read-back has it) — and is ready once each has answered.
#[test]
fn the_screenshot_waits_for_every_shown_native_views_picture() {
    let fake = FakeEngine::native();
    fake.install();
    fake.set_picture(None);
    let mut win = Hosting::new(vec![address(), address()]);
    let mut drv = Driver::new(400.0, 300.0);
    drv.idle(&mut win, 2);
    win.dialog = true;
    drv.idle(&mut win, 2);
    let shots = snapshots(&drv.ctx);
    assert!(shots.ready(), "every view hidden: none asked");
    win.dialog = false;
    drv.idle(&mut win, 1);
    let shots = snapshots(&drv.ctx);
    assert!(!shots.ready());
    let picture = egui::ColorImage::filled([4, 3], egui::Color32::GREEN);
    fake.answer(0, Some(picture.clone()));
    assert!(!shots.ready());
    fake.answer(1, None);
    assert!(shots.ready());
    let got = shots.take();
    assert_eq!(got.len(), 1, "the view that had a picture");
    assert_eq!((got[0].rect, got[0].image.size), (win.rect, [4, 3]));
    let texture = FakeEngine::texture();
    texture.install();
    let mut win = Hosting::new(vec![address()]);
    let mut drv = Driver::new(400.0, 300.0);
    drv.idle(&mut win, 2);
    assert!(snapshots(&drv.ctx).ready());
    assert!(!texture.calls(0).contains(&Did::Snapshot));
}

/// The compose call (§2.2): each picture over the read-back at its frame, in pixels, scaled to
/// fit it and clipped to the image.
#[test]
fn compose_draws_each_picture_at_its_frame() {
    use egui::Color32;
    let back = Color32::from_rgb(9, 9, 9);
    let mut image = egui::ColorImage::filled([40, 30], back);
    let mut picture = egui::ColorImage::filled([2, 2], Color32::RED);
    picture[(1, 1)] = Color32::BLUE;
    let shot = Snapshot {
        rect: Rect::from_min_size(Pos2::new(5.0, 5.0), egui::vec2(5.0, 5.0)),
        image: picture.clone(),
    };
    let clipped = Snapshot {
        rect: Rect::from_min_size(Pos2::new(18.0, 12.0), egui::vec2(10.0, 10.0)),
        image: picture,
    };
    compose(&mut image, 2.0, &[shot, clipped]);
    assert_eq!(image[(10, 10)], Color32::RED);
    assert_eq!(image[(14, 14)], Color32::RED);
    assert_eq!(image[(15, 15)], Color32::BLUE);
    assert_eq!(image[(19, 19)], Color32::BLUE);
    assert_eq!(image[(9, 9)], back);
    assert_eq!(image[(20, 20)], back);
    assert_eq!(image[(39, 29)], Color32::RED, "clipped at the image's edge");
    assert_eq!(image[(36, 24)], Color32::RED);
}
