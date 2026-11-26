//! VM-8 (08 §9, 10 T21, TP-5, TK-3): 08's views in the terminal backend — the view set opened
//! for it with the window's slots (a page file on its source, 3, where the window opens its
//! page; an image on 1 in both; plain XML on 1), and the view keys as the translator
//! (`term_input`) gives them: a terminal's key is its `Key`, its `Text` and its release made at
//! once, all in one pass, as the backend's `serve` passes them.

use super::*;
use crate::gui::term_input::TermInput;
use crate::gui::web::fake::FakeEngine;
use crate::gui::web::Event as WebEvent;
use crate::views::{Hold, View};
use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyModifiers};

const INDEX: &str = "<!doctype html>\n<html><body><h1>Index</h1></body></html>\n";
const STYLED: &str =
    "<?xml version=\"1.0\"?>\n<?xml-stylesheet type=\"text/xsl\" href=\"s.xsl\"?>\n<doc/>\n";
const FEED: &str =
    "<?xml version=\"1.0\"?>\n<rss version=\"2.0\"><channel><title>feed</title></channel></rss>\n";

/// The tree with a page, a styled XML and an RSS feed besides (`photo.png` is the tree's), this
/// thread's engine the fake (no WebKit); in dual mode, so Enter opens each file from the start.
/// `terminal`: the context is the terminal backend's, as `Backend::new` marks it.
fn pages(terminal: bool) -> (Fx, FakeEngine) {
    pages_on(terminal, None, local_connect())
}

/// [`pages`] posing as host `label` (every file read as from a remote), the viewers'
/// connections made by `connect`.
fn pages_on(terminal: bool, label: Option<&str>, connect: Connect) -> (Fx, FakeEngine) {
    let fake = FakeEngine::native();
    fake.install();
    let mut fx = fixture_with(label, connect, None);
    for (name, text) in [
        ("index.html", INDEX),
        ("styled.xml", STYLED),
        ("feed.xml", FEED),
    ] {
        std::fs::write(format!("{}/{name}", fx.root), text).unwrap();
    }
    if terminal {
        crate::gui::mark_terminal(&fx.drv.ctx);
    }
    fx.b.mode = view::Mode::Dual;
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    (fx, fake)
}

fn strip(fx: &Fx) -> String {
    fx.b.viewer.as_ref().expect("a viewer").views.strip().text()
}

fn armed(fx: &Fx) -> bool {
    fx.b.viewer.as_ref().expect("a viewer").views.strip().armed
}

fn on(fx: &Fx) -> Option<View> {
    fx.b.viewer
        .as_ref()
        .expect("a viewer")
        .views
        .set
        .on_screen()
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

/// One key as a terminal sends it, through the translator, in one pass; then settled.
fn term_key(fx: &mut Fx, input: &mut TermInput, code: KeyCode) {
    term_chord(fx, input, code, KeyModifiers::NONE);
}

fn term_chord(fx: &mut Fx, input: &mut TermInput, code: KeyCode, held: KeyModifiers) {
    let events = input.translate(&CEvent::Key(KeyEvent::new(code, held)));
    fx.drv.frame(&mut fx.b, events, input.modifiers());
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
}

/// TP-5 in the terminal: `index.html` and a styled XML open on their source, 3, with the page
/// as view 1 (no engine asked); `feed.xml` on 1, its source; an image on 1. The slots are the
/// window's (T21).
#[test]
fn tp5_in_the_terminal_a_page_opens_on_its_source_an_image_and_rss_on_1() {
    let (mut fx, fake) = pages(true);
    for name in ["index.html", "styled.xml"] {
        fx.open(name);
        assert_eq!(
            strip(&fx),
            "1 Page · 2 Hex · 3 Text",
            "{name}: the window's slots"
        );
        assert_eq!(on(&fx), Some(View::Text), "{name}: on its source, 3");
        assert!(
            fx.drv.find_text("Page Viewer").is_none(),
            "{name}: no page on screen"
        );
        fx.key(Key::Escape);
        assert!(fx.b.viewer.is_none(), "{name}: Esc leaves");
    }
    assert_eq!(fake.views(), 0, "no page view asked in a terminal");
    fx.open("index.html");
    assert!(
        fx.drv.find_text("<!doctype html>").is_some(),
        "the source's rows"
    );
    fx.key(Key::Escape);
    fx.open("feed.xml");
    assert_eq!(strip(&fx), "1 Text · 2 Hex · 3 Page");
    assert_eq!(on(&fx), Some(View::Text), "RSS: on 1, its source");
    fx.key(Key::Escape);
    fx.open("photo.png");
    assert_eq!(strip(&fx), "1 Image · 2 Hex");
    assert_eq!(on(&fx), Some(View::Image), "an image on 1");
    fx.key(Key::Escape);
    // `view=1` is the owner's: it wins over the terminal's 3.
    fx.b.scripted(&crate::gui::State::parse("mode=dual;view=1").unwrap());
    fx.open("index.html");
    assert_eq!(on(&fx), Some(View::Page), "view=1: the page");
    assert!(fx.drv.find_text("Page Viewer").is_some(), "its frame");
}

/// The window opens the same files on 1: the page (made by the engine), RSS's source, the
/// picture — the slots as in the terminal.
#[test]
fn tp5_in_the_window_a_page_an_image_and_rss_open_on_1() {
    let (mut fx, fake) = pages(false);
    fx.select("index.html");
    fx.key(Key::Enter);
    fx.drv.until(&mut fx.b, 20, |_| fake.views() == 1);
    let events = fake.events(0);
    events.send(WebEvent::Loading);
    events.send(WebEvent::Finished);
    fx.settle();
    assert_eq!(strip(&fx), "1 Page · 2 Hex · 3 Text");
    assert_eq!(on(&fx), Some(View::Page), "the page, 1");
    fx.key(Key::Escape);
    fx.open("feed.xml");
    assert_eq!(on(&fx), Some(View::Text));
    fx.key(Key::Escape);
    fx.open("photo.png");
    assert_eq!(on(&fx), Some(View::Image));
}

/// In the default mode the terminal's preview of a page is its source — nothing of the page made
/// — and Enter hands it over (02 §4.4) on 3, the page's slots kept.
#[test]
fn tp5_a_page_previewed_in_the_terminal_is_handed_over_on_its_source() {
    let (mut fx, fake) = pages(true);
    fx.b.mode = view::Mode::Preview;
    fx.drv.idle(&mut fx.b, 2);
    fx.select("index.html");
    fx.settle();
    // The preview's viewer is taken over in the pass of the press: no worker opens the file.
    let enter = |pressed| Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    fx.drv.frame(&mut fx.b, vec![enter(true)], Modifiers::NONE);
    assert!(fx.b.viewer.is_some(), "handed over in the pass of Enter");
    fx.drv.frame(&mut fx.b, vec![enter(false)], Modifiers::NONE);
    fx.settle();
    assert_eq!(strip(&fx), "1 Page · 2 Hex · 3 Text");
    assert_eq!(on(&fx), Some(View::Text), "on its source, 3");
    assert!(
        !fx.b.viewer.as_ref().unwrap().views.set.is_made(View::Page),
        "no page made"
    );
    assert_eq!(fake.views(), 0, "no page view asked in a terminal");
}

/// A remote page in the terminal makes nothing of the page: the opening reads its source
/// through one stream, on one connection, and no page server is made — until `1` asks for the
/// page (the hand-off box), whose server connects on its own: the viewer's 2.
#[test]
fn tp5_a_remote_page_in_the_terminal_makes_no_page_server() {
    let (connect, connects) = counted();
    let (mut fx, fake) = pages_on(true, Some("box"), connect);
    fx.open("index.html");
    assert_eq!(on(&fx), Some(View::Text), "on its source, 3");
    assert!(fx.drv.find_text("<!doctype html>").is_some());
    let set = &fx.b.viewer.as_ref().unwrap().views.set;
    assert!(!set.is_made(View::Page), "no page made");
    assert!(set.hidden().is_empty(), "nothing hidden");
    assert_eq!(connects(), 1, "the source's stream only");
    digit(&mut fx, 1);
    fx.settle();
    assert_eq!(on(&fx), Some(View::Page), "`1`: the page's hand-off");
    assert_eq!(fake.views(), 0, "no engine asked in a terminal");
    assert_eq!(
        connects(),
        2,
        "the source's stream and the page's server: the viewer's 2"
    );
}

/// `n` pressed as the keyboard sends it, settled.
fn digit(fx: &mut Fx, n: u8) {
    let key = [Key::Num0, Key::Num1, Key::Num2, Key::Num3][usize::from(n)];
    fx.drv.press_typed(&mut fx.b, key, &n.to_string());
    fx.drv.idle(&mut fx.b, 2);
}

/// TK-3 through the translator: in the terminal `1` → view 1 (the page's hand-off); Alt+9 (the
/// TUI's F9, 10 T7) then `3` → its source again; F9 then `2` → Hex (viewing); Enter edits and
/// `1` is then a typed nibble — no switch, the byte changed in memory, nothing written.
#[test]
fn tk3_the_view_keys_through_the_translator() {
    let (mut fx, _fake) = pages(true);
    let file = format!("{}/index.html", fx.root);
    let mut input = TermInput::new(cfg!(target_os = "macos"));
    fx.open("index.html");
    assert_eq!(on(&fx), Some(View::Text));
    term_key(&mut fx, &mut input, KeyCode::Char('1'));
    assert_eq!(on(&fx), Some(View::Page), "`1` → view 1");
    term_chord(&mut fx, &mut input, KeyCode::Char('9'), KeyModifiers::ALT);
    assert!(armed(&fx), "Alt+9 is F9: armed");
    term_key(&mut fx, &mut input, KeyCode::Char('3'));
    assert_eq!(on(&fx), Some(View::Text), "Alt+9 then `3` → view 3");
    assert!(!armed(&fx));
    term_key(&mut fx, &mut input, KeyCode::F(9));
    assert!(armed(&fx), "F9: armed");
    term_key(&mut fx, &mut input, KeyCode::Char('2'));
    assert_eq!(on(&fx), Some(View::Hex), "F9 then `2` → Hex");
    assert!(
        fx.drv.text_starting_with("3C 21 64 6F").is_some(),
        "`<!do` as bytes"
    );
    term_key(&mut fx, &mut input, KeyCode::Enter);
    term_key(&mut fx, &mut input, KeyCode::Char('1'));
    assert_eq!(on(&fx), Some(View::Hex), "`1` while editing: no switch");
    assert!(
        fx.drv.text_starting_with("1C 21 64 6F").is_some(),
        "`1` typed as the first byte's high nibble"
    );
    let viewer = fx.b.viewer.as_mut().unwrap();
    assert_eq!(viewer.embedded.window.hold(), Hold::Unsaved);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        INDEX,
        "nothing written"
    );
}
