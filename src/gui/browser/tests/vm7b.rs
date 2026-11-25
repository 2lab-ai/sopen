//! VM-7b (08 §5–§6, §13): the views of one remote file share its stream and one save session
//! (D13, D14) — counted below the `Source` by the metered connection, never by the product's own
//! statistics; a view's reload makes the hidden views stale (D25); empty media and a video none
//! of whose frames decode open on Hex with the reason (D10); F9 closes the text view's Go to
//! (D29); VA-5 and VA-6.

use super::*;
use crate::gui::testkit::command;
use crate::remote::SftpRemote;
use std::sync::atomic::Ordering;

/// `n` pressed as the keyboard sends it (the key, then its text), the window settled after it.
fn digit(fx: &mut Fx, n: u8) {
    let key = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4][usize::from(n)];
    fx.drv.press_typed(&mut fx.b, key, &n.to_string());
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
}

/// The strip's entry on screen, in the bar's colours.
fn on_strip(fx: &Fx, entry: &str) -> bool {
    let p = chrome::PALETTE;
    (fx.drv.colors_of(entry)).is_some_and(|c| c.iter().all(|&c| c == p.bar_text))
}

fn hex_row(fx: &Fx, bytes: &str) -> bool {
    fx.drv.text_starting_with(bytes).is_some()
}

/// The browser posing as host `box` in dual mode — Enter opens each file from the start, no
/// preview handed over — with every connection of its viewers metered: the local disk posing
/// as a remote.
fn metered() -> (Fx, Arc<Meter>) {
    metered_over(false)
}

/// [`metered`], each connection OpenSSH's `sftp-server` on this machine with `sftp`.
fn metered_over(sftp: bool) -> (Fx, Arc<Meter>) {
    let meter = Meter::new(u64::MAX, u64::MAX);
    let connect = meter.connect(move || {
        Ok(match sftp {
            true => Box::new(SftpRemote::local_server().expect("sftp-server")),
            false => Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>,
        })
    });
    let mut fx = fixture_with(Some("box"), connect, None);
    fx.b.mode = view::Mode::Dual;
    fx.drv.idle(&mut fx.b, 2);
    (fx, meter)
}

/// Sessions alive now, and the most there were since [`from_here`].
fn live(meter: &Meter) -> usize {
    meter.live.load(Ordering::SeqCst)
}

fn peak(meter: &Meter) -> usize {
    meter.peak.load(Ordering::SeqCst)
}

/// The peak counts from now.
fn from_here(meter: &Meter) {
    meter.peak.store(live(meter), Ordering::SeqCst);
}

fn transports(meter: &Meter) -> usize {
    meter.opened.lock().unwrap().len()
}

/// Enter on the hex view, `byte`'s two digits typed over the byte under the cursor (the first,
/// here), Save (F5) until the file starts with `byte`, Esc: the hex view on screen, viewing.
fn save_byte_at_the_start(fx: &mut Fx, file: &str, byte: u8) {
    fx.key(Key::Enter);
    let [hi, lo] = [byte >> 4, byte & 0xf];
    digit(fx, hi);
    digit(fx, lo);
    fx.key(Key::F5);
    let file = file.to_string();
    fx.drv.until(&mut fx.b, 20, |b| {
        b.ready() && std::fs::read(&file).unwrap()[0] == byte
    });
    fx.settle();
    fx.key(Key::Escape);
    fx.drv.idle(&mut fx.b, 2);
}

/// D13, D14 and 06 §6: a remote text file, 2 (Hex), 3 (Raw), 2, a Save in Hex — one stream for
/// all of them and one save session: at most 2 sessions at every point, never the 3–4 of a
/// session per view. Hidden, the hex view gives its save session back (1 Text: 1 session); its
/// next Save connects again. Over the local disk and over OpenSSH's `sftp-server` (the hex
/// Save by handle through the views' save session; required under `SOPEN_REQUIRE_SFTP_SERVER`).
#[test]
fn d13_d14_a_remote_texts_views_hold_at_most_two_sessions() {
    for sftp in [false, true] {
        if sftp && SftpRemote::local_server().is_none() {
            continue;
        }
        d13_d14_over(sftp);
    }
}

fn d13_d14_over(sftp: bool) {
    let (mut fx, meter) = metered_over(sftp);
    let file = format!("{}/main.rs", fx.root);
    from_here(&meter);
    fx.open("main.rs");
    assert_eq!(live(&meter), 1, "the opening's stream");
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "2F 2F 20 61"), "Hex");
    digit(&mut fx, 3);
    assert!(on_strip(&fx, "3 Raw"));
    digit(&mut fx, 2);
    assert_eq!(
        (
            live(&meter),
            transports(&meter),
            meter.connects.load(Ordering::SeqCst)
        ),
        (1, 1, 1),
        "Text, Hex and Raw read the opening's stream: one session, one transport"
    );
    save_byte_at_the_start(&mut fx, &file, 0x41);
    assert_eq!(live(&meter), 2, "the stream and the save session");
    assert_eq!(meter.connects.load(Ordering::SeqCst), 2);
    digit(&mut fx, 1);
    assert!(
        shows(&fx, "A/ a comment"),
        "Text made again: the file saved"
    );
    assert_eq!(
        live(&meter),
        1,
        "hidden, Hex gave its save session back (D14)"
    );
    digit(&mut fx, 2);
    save_byte_at_the_start(&mut fx, &file, 0x42);
    assert_eq!(live(&meter), 2, "its next Save connected again");
    assert_eq!(
        meter.connects.load(Ordering::SeqCst),
        3,
        "the opening's and two save sessions, one after the other: no stream of its own"
    );
    assert!(
        peak(&meter) <= 2,
        "at most 2 at any point: {}",
        peak(&meter)
    );
}

/// D14 across two views that save: a remote image cropped and saved, then 2 and a Save in Hex,
/// then 1 — each hidden view gives the session back, so the viewer never holds more than its
/// stream and one save session (2), where a session per view that saved held 3.
#[test]
fn d14_an_image_and_its_hex_view_both_saved_hold_at_most_two_sessions() {
    let (mut fx, meter) = metered();
    let file = format!("{}/photo.png", fx.root);
    from_here(&meter);
    fx.open("photo.png");
    pass_until(&mut fx, "the picture", |b| b.ready());
    edit_image(&mut fx);
    fx.drv.key(&mut fx.b, Key::S, command(false));
    let before = std::fs::read(&file).unwrap().len();
    pass_until(&mut fx, "the image's Save", |b| {
        b.ready() && std::fs::read(&file).unwrap().len() != before
    });
    fx.settle();
    assert_eq!(
        live(&meter),
        2,
        "the stream kept beside the views, the save session"
    );
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "89 50 4E 47"), "Hex: the file as saved");
    assert_eq!(live(&meter), 1, "the hidden image gave its session back");
    save_byte_at_the_start(&mut fx, &file, 0x41);
    assert_eq!(live(&meter), 2);
    digit(&mut fx, 1);
    assert_eq!(live(&meter), 1, "the hidden hex view gave it back");
    assert!(
        peak(&meter) <= 2,
        "at most 2 at any point: {}",
        peak(&meter)
    );
}

/// 06 §6 for a page file (09 §6, WV-D): its Page view serves the page on a connection of its
/// own, which it keeps hidden (until WV-F); Hex opens the views' stream and Text reads it — 2
/// sessions once all three are made, where a session per view held 3. A Save in Hex adds the
/// save session (3 while it runs); once it lands the hidden Page is stale and closes with its
/// connection (D25), and hidden, Hex gives the save session back.
#[test]
fn d13_a_remote_page_files_page_hex_and_text_hold_two_sessions() {
    let fake = crate::gui::web::fake::FakeEngine::native();
    fake.install();
    let (mut fx, meter) = metered();
    let file = format!("{}/index.html", fx.root);
    std::fs::write(&file, "<p>a page</p>\n").unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    from_here(&meter);
    fx.select("index.html");
    fx.key(Key::Enter);
    fx.drv.until(&mut fx.b, 20, |_| fake.views() > 0);
    let events = fake.events(0);
    events.send(crate::gui::web::Event::Loading);
    events.send(crate::gui::web::Event::Document("index.html".to_string()));
    events.send(crate::gui::web::Event::Finished);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    assert!(on_strip(&fx, "1 Page"), "the page");
    assert_eq!(live(&meter), 1, "its page server's connection");
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "3C 70 3E 61"), "Hex: `<p>a`");
    digit(&mut fx, 3);
    assert!(on_strip(&fx, "3 Text"));
    assert!(shows(&fx, "<p>a page</p>"), "Text: the source");
    assert_eq!(
        (live(&meter), transports(&meter)),
        (2, 1),
        "Page's connection and the one stream Hex opened and Text reads"
    );
    digit(&mut fx, 2);
    save_byte_at_the_start(&mut fx, &file, 0x41);
    assert_eq!(
        meter.connects.load(Ordering::SeqCst),
        3,
        "the Save connected"
    );
    assert!(
        !fx.b.viewer.as_ref().unwrap().views.set.is_made(View::Page),
        "the Save made the hidden Page stale"
    );
    assert_eq!(
        live(&meter),
        2,
        "the stream and the save session: Page closed"
    );
    digit(&mut fx, 3);
    assert_eq!(live(&meter), 1, "hidden, Hex gave the save session back");
    assert_eq!(
        peak(&meter),
        3,
        "Page's, the stream and the save session while it ran"
    );
}

/// D25's reload half: Cmd/Ctrl+R in the text view after the file changed on the remote makes
/// the hidden Hex stale — 2 makes it again, and it shows the file as reloaded.
#[test]
fn d25_a_views_reload_makes_the_hidden_views_stale() {
    let (mut fx, _meter) = metered();
    let file = format!("{}/main.rs", fx.root);
    fx.open("main.rs");
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "2F 2F 20 61"));
    digit(&mut fx, 1);
    let makes = |fx: &Fx| fx.b.viewer.as_ref().unwrap().views.makes;
    assert_eq!(makes(&fx), 1, "Hex made once, hidden");
    std::fs::write(&file, "ZZ changed on the remote\n").unwrap();
    fx.drv.key(&mut fx.b, Key::R, command(false));
    pass_until(&mut fx, "the reload", |b| {
        b.ready()
            && b.viewer
                .as_ref()
                .is_some_and(|v| !v.views.set.is_made(View::Hex))
    });
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(&fx, "ZZ changed on the remote"), "the text reloaded");
    digit(&mut fx, 2);
    assert_eq!(makes(&fx), 2, "the stale Hex was made again");
    assert!(hex_row(&fx, "5A 5A 20 63"), "the file as reloaded: `ZZ c`");
}

/// D10: empty media (an `.mp3`, an `.mp4`) opens on Hex, its first row the reason, locally and
/// through the stream; 1 says it again and stays on Hex.
#[test]
fn d10_empty_media_opens_on_hex_with_the_reason() {
    for label in [None, Some("box")] {
        for name in ["empty.mp3", "empty.mp4"] {
            let mut fx = fixture_with(label, local_connect(), None);
            fx.b.mode = view::Mode::Dual;
            std::fs::write(format!("{}/{name}", fx.root), b"").unwrap();
            let root = fx.root.clone();
            fx.b.list(Some(0), root, None, None);
            fx.settle();
            fx.open(name);
            fx.drv.idle(&mut fx.b, 2);
            let why = "the file is empty (0 bytes); it is shown as bytes";
            assert!(shows(&fx, why), "{label:?} {name}: the reason first");
            assert!(on_strip(&fx, "2 Hex"), "{label:?} {name}: on Hex");
            let one = if name.ends_with(".mp3") {
                "1 Audio"
            } else {
                "1 Video"
            };
            let strip = fx.b.viewer.as_ref().unwrap().views.strip().text();
            assert!(strip.starts_with(one), "{label:?} {name}: {strip}");
            digit(&mut fx, 1);
            assert!(on_strip(&fx, "2 Hex"), "{label:?} {name}: 1 stays on Hex");
            assert_eq!(fx.b.status.text, why, "{label:?} {name}: 1 says why");
        }
    }
}

/// D10 (VA-6): a video none of whose frames decode — its index whole, its pictures cut off —
/// gives way to Hex with the reason, locally and through the stream; 1 says it again.
#[test]
fn d10_a_video_none_of_whose_frames_decode_gives_way_to_hex() {
    let Some(tools) = crate::gui::media::fixtures::tools() else {
        return;
    };
    for label in [None, Some("box")] {
        let mut fx = fixture_with(label, local_connect(), None);
        fx.b.mode = view::Mode::Dual;
        let cut = PathBuf::from(format!("{}/cut.mp4", fx.root));
        crate::gui::media::fixtures::index_only(&tools, &cut);
        let root = fx.root.clone();
        fx.b.list(Some(0), root, None, None);
        fx.settle();
        fx.select("cut.mp4");
        fx.key(Key::Enter);
        pass_until(&mut fx, "Hex with the reason", |b| {
            b.viewer
                .as_ref()
                .is_some_and(|v| v.views.set.on_screen() == Some(View::Hex))
                && b.ready()
        });
        fx.drv.idle(&mut fx.b, 2);
        assert!(shows(&fx, "no frame"), "{label:?}: the reason on Hex");
        assert!(shows(&fx, "the file is shown as bytes"), "{label:?}");
        assert!(on_strip(&fx, "2 Hex"), "{label:?}");
        digit(&mut fx, 1);
        assert!(on_strip(&fx, "2 Hex"), "{label:?}: 1 stays on Hex");
        assert!(
            fx.b.status.text.contains("no frame"),
            "{:?}",
            fx.b.status.text
        );
    }
}

/// A view 1 that finds on its first pass that it cannot show its file after all: it says why
/// and asks to be closed — as the media window does when no frame decodes, and the image window
/// when its stream changed or failed while it was read.
struct Failing(&'static str);

impl Window for Failing {
    fn ui(&mut self, ui: &mut egui::Ui) {
        crate::gui::request_close(ui.ctx());
    }
    fn ready(&self) -> bool {
        true
    }
    fn failure(&self) -> Option<String> {
        Some(self.0.to_string())
    }
}

/// The viewer on the local `name` as `kind`, its view 1 a [`Failing`] window saying `why`.
fn failing(fx: &mut Fx, name: &str, kind: crate::kind::Kind, why: &'static str) {
    let abs = format!("{}/{name}", fx.root);
    let means = view_set::Means {
        path: abs.clone(),
        name: name.to_string(),
        title: abs.clone(),
        connect: local_connect(),
        label: None,
        in_place: false,
    };
    let opened = view_set::OpenedAs {
        kind,
        stat: LocalFs::new().stat(&abs).unwrap(),
        undecodable: None,
    };
    let ctx = fx.drv.ctx.clone();
    let views = view_set::open(&ctx, &opened, means, None);
    let made = view_set::Made {
        embedded: Embedded {
            window: Box::new(Failing(why)),
            settle: Box::new(|_| Some(Outcome::default())),
        },
        hold: None,
        switch: None,
    };
    fx.b.view(&ctx, made, abs, false, views);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
}

/// D10 is a video's (06 §7.3's rule for the image, 08 §8): a video that cannot show its file
/// gives way to Hex, the reason first; an image's failure — one that does not decode gives way
/// by its switch, so it is its stream's — goes back to the list, said as before.
#[test]
fn d10_a_failed_video_gives_way_to_hex_and_a_failed_image_goes_back_to_the_list() {
    let mut fx = fixture();
    let why = "cannot show data.bin: no frame at 0:00.00";
    failing(&mut fx, "data.bin", crate::kind::Kind::Video, why);
    pass_until(&mut fx, "Hex", |b| {
        b.ready()
            && b.viewer
                .as_ref()
                .is_some_and(|v| v.views.set.on_screen() == Some(View::Hex))
    });
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(&fx, &format!("{why}; the file is shown as bytes")));
    let mut fx = fixture();
    let why = "the stream failed while the picture was read";
    failing(&mut fx, "photo.png", crate::kind::Kind::Image, why);
    assert!(fx.b.viewer.is_none(), "back in the list");
    assert!(
        fx.b.status.error && fx.b.status.text == why,
        "{:?}",
        fx.b.status
    );
}

/// D29 in the text view: Go to open, `1` goes into the field; F9 closes it, as Esc would, and
/// arms — the next 2 is Hex.
#[test]
fn d29_f9_closes_the_text_views_go_to_and_arms() {
    let mut fx = fixture();
    fx.open("main.rs");
    fx.drv.key(&mut fx.b, Key::G, command(false));
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(&fx, "Go to"), "the field");
    fx.drv.press_typed(&mut fx.b, Key::Num1, "1");
    fx.drv.idle(&mut fx.b, 2);
    assert!(on_strip(&fx, "1 Text"), "1 went into the field");
    fx.key(Key::F9);
    fx.drv.idle(&mut fx.b, 2);
    assert!(!shows(&fx, "Go to"), "F9 closed the field");
    assert!(on_strip(&fx, "1 Text"));
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "2F 2F 20 61"), "armed: 2 chose Hex");
}

/// VA-5: a remote image's 1 → 2 → 1 → 2 reads the file once — the Hex view reads the opening's
/// stream, its cache, no transport of its own.
#[test]
fn va5_a_remote_images_1_2_1_2_reads_the_file_once() {
    let (mut fx, meter) = metered();
    let size = std::fs::metadata(format!("{}/photo.png", fx.root))
        .unwrap()
        .len();
    fx.open("photo.png");
    pass_until(&mut fx, "the picture", |b| b.ready());
    assert!(!fx.drv.pictures().is_empty());
    let read = meter.stream_bytes.load(Ordering::SeqCst);
    assert!(read >= size, "the picture read whole: {read} of {size}");
    for n in [2, 1, 2] {
        digit(&mut fx, n);
    }
    assert!(hex_row(&fx, "89 50 4E 47"), "the bytes");
    assert_eq!(
        meter.stream_bytes.load(Ordering::SeqCst),
        read,
        "nothing read again"
    );
    assert_eq!(transports(&meter), 1, "the one stream");
}

/// VA-5: a handed-over image preview has no stream (it was read whole and gave the preview's
/// session back); its 2 opens one stream, kept — 1, 2 again open none — and the viewer's
/// channels never pass 2.
#[test]
fn va5_a_handed_over_image_previews_2_opens_one_stream() {
    let meter = Meter::new(u64::MAX, u64::MAX);
    let connect = meter.connect(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>));
    let mut fx = fixture_with(Some("box"), connect, None);
    fx.select("photo.png");
    // The bar rests: the preview is made (past the 150 ms commit).
    fx.drv.idle(&mut fx.b, 12);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    assert!(!fx.drv.pictures().is_empty(), "the preview's picture");
    from_here(&meter);
    let before = transports(&meter);
    fx.key(Key::Enter);
    fx.settle();
    assert!(fx.b.viewer.is_some(), "handed over");
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "89 50 4E 47"));
    assert_eq!(transports(&meter), before + 1, "2 opened one stream");
    for n in [1, 2] {
        digit(&mut fx, n);
    }
    assert_eq!(transports(&meter), before + 1, "and kept it");
    assert!(peak(&meter) <= 2, "channels: {}", peak(&meter));
}

/// The browser posing as host `box` in preview mode, its connections metered, the bar rested
/// on `name`: its preview made, to be handed over by Enter (02 §4.4, the default route).
fn previewed(name: &str) -> (Fx, Arc<Meter>) {
    let meter = Meter::new(u64::MAX, u64::MAX);
    let connect = meter.connect(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>));
    let mut fx = fixture_with(Some("box"), connect, None);
    fx.select(name);
    fx.drv.idle(&mut fx.b, 12);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.preview.shown_window().is_some(), "{name}: its preview");
    (fx, meter)
}

/// D13, D14 on the default route (preview mode, Enter: 02 §4.4): a handed-over text preview
/// brings its stream and save session to the views — 2 (Hex) reads the preview's stream, and a
/// Save in Hex adds the one save session: at most 2 at every point, as from the list; Hex
/// hidden gives it back.
#[test]
fn d13_d14_on_the_default_route_a_handed_over_texts_views_hold_at_most_two_sessions() {
    let (mut fx, meter) = previewed("main.rs");
    let file = format!("{}/main.rs", fx.root);
    from_here(&meter);
    fx.key(Key::Enter);
    fx.settle();
    assert!(fx.b.viewer.is_some(), "handed over");
    assert_eq!(live(&meter), 1, "the preview's stream, the viewer's now");
    let streams = transports(&meter);
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "2F 2F 20 61"), "Hex");
    assert_eq!(
        (live(&meter), transports(&meter)),
        (1, streams),
        "Hex reads the handed-over stream"
    );
    save_byte_at_the_start(&mut fx, &file, 0x41);
    assert_eq!(live(&meter), 2, "the stream and the save session");
    digit(&mut fx, 1);
    assert!(
        shows(&fx, "A/ a comment"),
        "Text made again: the file saved"
    );
    assert_eq!(live(&meter), 1, "Hex hidden gave the save session back");
    assert!(
        peak(&meter) <= 2,
        "at most 2 at any point: {}",
        peak(&meter)
    );
}

/// D14 on the default route with two views that save: a handed-over image (read whole, its
/// stream closed, the window's preview session idle again) saves through the views' session,
/// which it gives back when 2 hides it; Hex opens the views' stream and saves through that one
/// session — the viewer never holds more than 2 (3 with the window's idle preview session).
#[test]
fn d14_on_the_default_route_a_handed_over_image_and_its_hex_view_saved_hold_at_most_two() {
    let (mut fx, meter) = previewed("photo.png");
    let file = format!("{}/photo.png", fx.root);
    from_here(&meter);
    let window = live(&meter);
    assert_eq!(window, 1, "the window's preview session, idle");
    fx.key(Key::Enter);
    fx.settle();
    edit_image(&mut fx);
    fx.drv.key(&mut fx.b, Key::S, command(false));
    let before = std::fs::read(&file).unwrap().len();
    pass_until(&mut fx, "the image's Save", |b| {
        b.ready() && std::fs::read(&file).unwrap().len() != before
    });
    fx.settle();
    assert_eq!(live(&meter) - window, 1, "the views' save session");
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "89 50 4E 47"));
    assert_eq!(
        live(&meter) - window,
        1,
        "Hex's stream; the hidden image gave the save session back"
    );
    save_byte_at_the_start(&mut fx, &file, 0x41);
    assert_eq!(live(&meter) - window, 2, "the stream and the save session");
    digit(&mut fx, 1);
    assert_eq!(live(&meter) - window, 1, "Hex hidden gave it back");
    assert!(
        peak(&meter) - window <= 2,
        "the viewer's at most: {}",
        peak(&meter)
    );
}

/// VA-5: a revision's 2 shows its bytes — the `.revisions` copy's — read-only.
#[test]
fn va5_a_revisions_2_shows_its_bytes_read_only() {
    let mut fx = fixture();
    let copy = fx.cache.join(".revisions-test").join("photo.png");
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::write(&copy, b"\x89PNG a revision's bytes").unwrap();
    let ctx = fx.drv.ctx.clone();
    let read_only = super::git_host::ReadOnly {
        title: "photo.png @ 1234567".to_string(),
        edit: format!("{}/photo.png", fx.root),
    };
    let abs = copy.to_str().unwrap().to_string();
    fx.b.start_open_with(
        &ctx,
        abs,
        "photo.png".to_string(),
        local_connect(),
        None,
        Some(read_only),
    );
    pass_until(&mut fx, "the viewer", |b| b.viewer.is_some() && b.ready());
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.viewer.as_ref().unwrap().views.read_only());
    // The bytes do not decode: the set opened on Hex (D10). 2 is that view, read-only.
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "89 50 4E 47 20 61"), "the revision's bytes");
    fx.key(Key::Enter);
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(&fx, "cannot edit"), "read-only: Enter says so");
}

/// VA-6: F1 in the viewer lists the view keys 1, 2, 3 and F9 (one table, 04 §3 rule 4).
#[test]
fn va6_f1_lists_the_view_keys() {
    let mut fx = fixture();
    fx.open("main.rs");
    fx.key(Key::F1);
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(&fx, "Keys in the viewer"));
    for (key, what) in [
        ("1, 2, 3", "View 1 (the file's own), 2 (Hex) or 3"),
        (
            "F9",
            "Then 1, 2 or 3: that view, also where digits are typed",
        ),
    ] {
        let rows = fx.drv.texts_starting_with(key);
        let at = rows
            .iter()
            .find(|(_, t)| t == key)
            .map(|(r, _)| r.center().y);
        let at = at.unwrap_or_else(|| panic!("{key} in the key help"));
        let said = fx.drv.texts_starting_with(what);
        assert!(
            said.iter().any(|(r, _)| (r.center().y - at).abs() < 2.0),
            "{key}: {what} on its row"
        );
    }
}

use crate::views::View;
