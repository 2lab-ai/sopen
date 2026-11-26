use super::*;
use crate::kind;
use std::time::{Duration, SystemTime};
use View::{Audio, Hex, Image, Page, Raw, Text, Video};

const NONE: Option<View> = None;
const PAGES: Screen = Screen::GUI;
const CLEAN: Now = Now::CLEAN;
const UNSAVED: Now = Now {
    hold: Hold::Unsaved,
    place: None,
};
const SAVING: Now = Now {
    hold: Hold::Saving,
    place: None,
};
const HOLDS: [Hold; 3] = [Hold::Clean, Hold::Unsaved, Hold::Saving];

fn stat(size: u64) -> Stat {
    Stat {
        is_dir: false,
        is_file: true,
        is_symlink: false,
        size,
        mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        mode: 0o644,
        uid: None,
        gid: None,
        missing: None,
        sha256: None,
    }
}

/// An opening of `name`, whose kind its extension decides (kind.rs's tables).
fn named(name: &str) -> Opening {
    let kind = kind::by_extension(name).unwrap_or_else(|| panic!("{name}: no kind by extension"));
    Opening::new(kind, format!("/srv/{name}"), stat(4096), Reach::Stream)
}

/// An opening of `name` routed as kind.rs routes it, `head` being the whole file.
fn routed(name: &str, head: &[u8]) -> Opening {
    let kind = kind::classify(name, || head.to_vec(), head.len() as u64);
    let mut opening = Opening::new(
        kind,
        format!("/srv/{name}"),
        stat(head.len() as u64),
        Reach::Stream,
    );
    opening.head = head.to_vec();
    opening
}

fn of_kind(kind: Kind, size: u64) -> Opening {
    Opening::new(kind, "/srv/f", stat(size), Reach::Stream)
}

fn empty(mut opening: Opening) -> Opening {
    opening.stat.size = 0;
    opening
}

fn undecodable(mut opening: Opening, words: &str) -> Opening {
    opening.undecodable = Some(words.to_string());
    opening
}

/// A file at a revision: read-only, its bytes the `.revisions` copy (D13).
fn revision(mut opening: Opening) -> Opening {
    opening.read_only = true;
    opening.reach = Reach::Local;
    opening.path = "/cache/.revisions/abc123/f".to_string();
    opening
}

fn paged(mut opening: Opening, styled: bool) -> Opening {
    opening.page = PageClass::of(&opening.path, styled);
    opening
}

fn reached(mut opening: Opening, reach: Reach) -> Opening {
    opening.reach = reach;
    opening
}

fn mpeg_ts() -> Vec<u8> {
    let mut bytes = vec![0u8; 188 * 4];
    for packet in bytes.chunks_mut(188) {
        packet[0] = 0x47;
        packet[1] = 0x40;
    }
    bytes
}

fn open(opening: Opening) -> ViewSet {
    ViewSet::open(opening, Screen::GUI).expect("opened")
}

fn choose(n: u8) -> Input {
    Input::Choose(Slot::from_number(n).unwrap())
}

fn show(view: View) -> Effect {
    Effect::Show {
        view,
        at: None,
        viewing: view == Hex,
    }
}

fn show_at(view: View, at: u64) -> Effect {
    Effect::Show {
        view,
        at: Some(at),
        viewing: view == Hex,
    }
}

/// A make with the view's place 0 (a byte view's first time).
fn make(ticket: u64, view: View) -> Effect {
    Effect::Make {
        ticket: Ticket(ticket),
        view,
        at: view.is_bytes().then_some(0),
        notice: None,
        encoding: None,
        viewing: view == Hex,
    }
}

fn make_at(ticket: u64, view: View, at: u64) -> Effect {
    Effect::Make {
        ticket: Ticket(ticket),
        view,
        at: Some(at),
        notice: None,
        encoding: None,
        viewing: view == Hex,
    }
}

fn hide(view: View) -> Effect {
    Effect::Hide(view)
}

fn release(view: View) -> Effect {
    Effect::ReleaseSaveConnection(view)
}

fn close(view: View) -> Effect {
    Effect::Close(view)
}

fn say(why: &Why) -> Effect {
    Effect::Say(why.to_string())
}

type Step = (Input, Now, Vec<Effect>);

/// Applies each step's input and checks its effects.
#[track_caller]
fn run(set: &mut ViewSet, steps: Vec<Step>) {
    for (i, (input, now, want)) in steps.into_iter().enumerate() {
        let got = set.apply(input.clone(), now);
        assert_eq!(got, want, "step {i}: {input:?} with {now:?}");
    }
}

/// `on-screen pending made=docs`, or `left`.
fn summary(set: &ViewSet) -> String {
    if set.is_left() {
        return "left".to_string();
    }
    let then = |then: Then| match then {
        Then::Show(view) => view.name().to_string(),
        Then::Leave => "leave".to_string(),
    };
    let pending = match set.pending {
        Pending::Idle => "idle".to_string(),
        Pending::Making { view, .. } => format!("making {}", view.name()),
        Pending::Asking(t) => format!("asking {}", then(t)),
        Pending::Waiting(t) => format!("waiting {}", then(t)),
    };
    let made: Vec<&str> = Doc::ALL
        .into_iter()
        .filter(|&d| set.docs[d as usize].made)
        .map(|d| d.view().name())
        .collect();
    let on = set.on_screen().map_or("-", View::name);
    format!("{on} {pending} made={}", made.join(","))
}

// ---- §3: the views of a file (VA-1) ----

struct Row {
    what: &'static str,
    opening: Opening,
    screen: Screen,
    slots: [Option<View>; 3],
    opens: View,
    unshown: Option<Why>,
}

fn row(
    what: &'static str,
    opening: Opening,
    screen: Screen,
    slots: [Option<View>; 3],
    opens: View,
    unshown: Option<Why>,
) -> Row {
    Row {
        what,
        opening,
        screen,
        slots,
        opens,
        unshown,
    }
}

/// Every row of §3, its kinds as kind.rs routes them (the UTF-16/32 row by D18 since VM-3).
fn rows() -> Vec<Row> {
    let (i, v, a, h) = (Some(Image), Some(Video), Some(Audio), Some(Hex));
    let (t, r, p) = (Some(Text), Some(Raw), Some(Page));
    let gui = Screen::GUI;
    let bad = Why::Undecodable("unknown image format".to_string());
    let typescript = b"export const x: number = 1;\n";
    let mut out = Vec::new();
    for name in [
        "a.png", "a.jpg", "a.jpeg", "a.webp", "a.bmp", "a.gif", "a.tif", "a.tiff",
    ] {
        out.push(row(name, named(name), gui, [i, h, NONE], Image, None));
    }
    for name in ["a.mp4", "a.mov", "a.mkv", "a.webm"] {
        out.push(row(name, named(name), gui, [v, h, NONE], Video, None));
    }
    for name in ["a.mp3", "a.wav", "a.flac", "a.opus"] {
        out.push(row(name, named(name), gui, [a, h, NONE], Audio, None));
    }
    // D10: what view 1 cannot show opens on 2.
    let shot = undecodable(named("shot.png"), "unknown image format");
    out.push(row(
        "an image that does not decode",
        shot,
        gui,
        [i, h, NONE],
        Hex,
        Some(bad),
    ));
    out.push(row(
        "empty png",
        empty(named("e.png")),
        gui,
        [i, h, NONE],
        Hex,
        Some(Why::Empty),
    ));
    out.push(row(
        "empty mp4",
        empty(named("e.mp4")),
        gui,
        [v, h, NONE],
        Hex,
        Some(Why::Empty),
    ));
    out.push(row(
        "empty mp3",
        empty(named("e.mp3")),
        gui,
        [a, h, NONE],
        Hex,
        Some(Why::Empty),
    ));
    for name in ["main.rs", "data.json", "a.yaml", "t.csv", "pic.svg"] {
        out.push(row(name, named(name), gui, [t, h, r], Text, None));
    }
    let makefile = routed("Makefile", b"all:\n\ttrue\n");
    out.push(row("text by sniff", makefile, gui, [t, h, r], Text, None));
    let utf16 = routed("notes.dat", &[0xFF, 0xFE, b'h', 0, b'i', 0, b'\n', 0]);
    out.push(row(
        "UTF-16, no known extension (D18)",
        utf16,
        gui,
        [t, h, r],
        Text,
        None,
    ));
    // D40, with 09's page view.
    let html = paged(named("index.html"), false);
    out.push(row("html", html.clone(), PAGES, [p, h, t], Page, None));
    let htm = paged(named("index.htm"), false);
    out.push(row("htm", htm.clone(), PAGES, [p, h, t], Page, None));
    let styled = paged(named("feed.xml"), true);
    out.push(row(
        "styled xml",
        styled.clone(),
        PAGES,
        [p, h, t],
        Page,
        None,
    ));
    let plain = paged(named("feed.xml"), false);
    out.push(row(
        "plain xml, RSS among it",
        plain.clone(),
        PAGES,
        [t, h, p],
        Text,
        None,
    ));
    // D30: with no engine (09 §4) all of them are 1 Text, 2 Hex, 3 Raw.
    for (what, opening) in [
        ("html, no engine", html),
        ("htm, no engine", htm),
        ("styled xml, no engine", styled),
        ("plain xml, no engine", plain),
    ] {
        out.push(row(what, opening, Screen::NO_ENGINE, [t, h, r], Text, None));
    }
    let blob = routed("blob.bin", &[0x7f, 0, 1, 2]);
    out.push(row("binary", blob, gui, [h, h, t], Hex, None));
    // An empty file that is not media: as its kind (kind.rs routes an empty one as text).
    let none = routed("empty.bin", b"");
    out.push(row("empty, routed", none, gui, [t, h, r], Text, None));
    let other = of_kind(Kind::Other, 0);
    out.push(row("empty, other", other, gui, [h, h, t], Hex, None));
    // A revision or a diff: as its kind, read-only.
    out.push(row(
        "png at a revision",
        revision(named("a.png")),
        gui,
        [i, h, NONE],
        Image,
        None,
    ));
    out.push(row(
        "rs at a revision",
        revision(named("a.rs")),
        gui,
        [t, h, r],
        Text,
        None,
    ));
    let bin = revision(routed("b.bin", &[0, 1]));
    out.push(row("binary at a revision", bin, gui, [h, h, t], Hex, None));
    // `.ts`: as its refined kind.
    let ts = routed("a.ts", typescript);
    out.push(row(".ts TypeScript", ts, gui, [t, h, r], Text, None));
    let ts = routed("clip.ts", &mpeg_ts());
    out.push(row(
        ".ts transport stream",
        ts,
        gui,
        [v, h, NONE],
        Video,
        None,
    ));
    let ts = of_kind(
        kind::refine_after_fetch("clip.ts", Kind::Video, Some((false, true))),
        9,
    );
    out.push(row(
        ".ts with audio only",
        ts,
        gui,
        [a, h, NONE],
        Audio,
        None,
    ));
    let ts = of_kind(kind::refine_after_fetch("clip.ts", Kind::Video, None), 9);
    out.push(row(".ts that is not media", ts, gui, [h, h, t], Hex, None));
    // 10 T21 (TP-5): the terminal keeps every slot; an image and a video open on 1, a page
    // file on its source, 3 (its view 1 the page's hand-off), plain XML and RSS on 1.
    let term = Screen::TERMINAL;
    out.push(row(
        "png, terminal",
        named("a.png"),
        term,
        [i, h, NONE],
        Image,
        None,
    ));
    out.push(row(
        "mp4, terminal",
        named("a.mp4"),
        term,
        [v, h, NONE],
        Video,
        None,
    ));
    out.push(row(
        "mp3, terminal",
        named("a.mp3"),
        term,
        [a, h, NONE],
        Audio,
        None,
    ));
    for (what, opening) in [
        ("html, terminal", paged(named("index.html"), false)),
        ("htm, terminal", paged(named("index.htm"), false)),
        ("styled xml, terminal", paged(named("feed.xml"), true)),
    ] {
        out.push(row(what, opening, term, [p, h, t], Text, None));
    }
    let plain = paged(named("feed.xml"), false);
    out.push(row(
        "plain xml, terminal",
        plain,
        term,
        [t, h, p],
        Text,
        None,
    ));
    let e = empty(named("e.png"));
    out.push(row(
        "empty png, terminal",
        e,
        term,
        [i, h, NONE],
        Hex,
        Some(Why::Empty),
    ));
    out
}

#[test]
fn every_row_of_section_3_locally_and_through_a_stream() {
    for row in rows() {
        for reach in [Reach::Local, Reach::Stream] {
            let what = format!("{} ({reach:?})", row.what);
            let opening = reached(row.opening.clone(), reach);
            let set = ViewSet::open(opening.clone(), row.screen).expect(&what);
            assert_eq!(set.slots(), row.slots, "{what}");
            assert_eq!(set.on_screen(), Some(row.opens), "{what}");
            assert_eq!(set.unshown(), row.unshown.as_ref(), "{what}");
            assert_eq!(set.read_only(), opening.read_only, "{what}");
            assert!(set.is_made(row.opens), "{what}");
            assert!(set.hidden().is_empty(), "{what}");
            assert_eq!(set.making(), None, "{what}");
            assert_eq!(set.reach(), reach, "{what}");
            assert_eq!(set.opening(), &opening, "{what}");
        }
    }
}

/// 10 T21: the same keys mean the same views in both backends — every row's slots in the
/// terminal are the window's, and so is D10; only where a page file opens differs: on its
/// source, 3, where the window opens its page, 1.
#[test]
fn t21_the_terminal_keeps_the_windows_slots_and_opens_a_page_on_its_source() {
    for row in rows().into_iter().filter(|r| r.screen == Screen::GUI) {
        let what = row.what;
        let window = ViewSet::open(row.opening.clone(), Screen::GUI).unwrap();
        let term = ViewSet::open(row.opening.clone(), Screen::TERMINAL).unwrap();
        assert_eq!(term.slots(), window.slots(), "{what}");
        assert_eq!(term.unshown(), window.unshown(), "{what}");
        let opens = match window.on_screen() {
            Some(Page) => term.slot(Slot::Three),
            on => on,
        };
        assert_eq!(term.on_screen(), opens, "{what}");
    }
}

#[test]
fn a_folder_is_not_opened() {
    let folder = of_kind(Kind::Directory, 0);
    assert!(ViewSet::open(folder.clone(), Screen::GUI).is_none());
    assert!(ViewSet::open(folder, Screen::TERMINAL).is_none());
}

/// VA-1: Enter shows 1; 2 shows Hex; 3 shows the third view, or does nothing (no 3).
#[test]
fn va1_each_key_from_the_opening_of_every_row() {
    for row in rows() {
        for reach in [Reach::Local, Reach::Stream] {
            for slot in Slot::ALL {
                let what = format!("{} ({reach:?}), key {}", row.what, slot.number());
                let opening = reached(row.opening.clone(), reach);
                let mut set = ViewSet::open(opening, row.screen).unwrap();
                let fx = set.apply(Input::Choose(slot), CLEAN);
                let want = match row.slots[slot.index()] {
                    None => {
                        assert_eq!(fx, [], "{what}: none does nothing");
                        row.opens
                    }
                    Some(_) if slot == Slot::One && row.unshown.is_some() => {
                        assert_eq!(fx, [say(row.unshown.as_ref().unwrap())], "{what}");
                        row.opens
                    }
                    Some(view) if view == row.opens => {
                        assert_eq!(fx, [], "{what}: the view on screen");
                        view
                    }
                    Some(view) if view.doc() == row.opens.doc() => {
                        assert_eq!(fx, [show(view)], "{what}: one document");
                        view
                    }
                    Some(view) => {
                        assert_eq!(fx, [make(0, view)], "{what}");
                        let fx = set.apply(Input::Made(Ticket(0)), CLEAN);
                        assert_eq!(fx.first(), Some(&hide(row.opens)), "{what}");
                        assert!(
                            matches!(fx.last(), Some(Effect::Show { view: v, .. }) if *v == view),
                            "{what}: {fx:?}"
                        );
                        view
                    }
                };
                assert_eq!(set.on_screen(), Some(want), "{what}");
            }
        }
    }
}

#[test]
fn page_class_by_name() {
    let cases = [
        ("index.html", false, PageClass::PageFirst),
        ("INDEX.HTM", false, PageClass::PageFirst),
        ("feed.xml", true, PageClass::PageFirst),
        ("feed.xml", false, PageClass::PageThird),
        ("/srv/rss.XML", false, PageClass::PageThird),
        // 09 serves only .html, .htm and .xml as pages.
        ("feed.rss", false, PageClass::Plain),
        ("feed.RSS", true, PageClass::Plain),
        ("pic.svg", true, PageClass::Plain),
        ("a.json", false, PageClass::Plain),
        ("Makefile", false, PageClass::Plain),
    ];
    for (name, styled, want) in cases {
        assert_eq!(PageClass::of(name, styled), want, "{name} {styled}");
    }
}

#[test]
fn slots_by_number() {
    for slot in Slot::ALL {
        assert_eq!(Slot::from_number(slot.number()), Some(slot));
    }
    assert_eq!(Slot::from_number(0), None);
    assert_eq!(Slot::from_number(4), None);
}

#[test]
fn the_reasons_say_the_file_is_shown_as_bytes() {
    let bad = Why::Undecodable("unknown image format".to_string());
    assert_eq!(
        bad.to_string(),
        "unknown image format; the file is shown as bytes"
    );
    assert!(Why::Empty.to_string().contains("0 bytes"));
}

// ---- §5: a switch ----

/// D1, VA-4, VA-5: a clean view switched away from is kept hidden and comes back as it was —
/// the image's zoom and rectangle, the media playhead — and is made only once.
#[test]
fn d1_clean_views_are_kept_hidden_and_made_once() {
    let mut set = open(named("shot.png"));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (
                Input::Made(Ticket(0)),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
            (choose(1), CLEAN, vec![hide(Hex), release(Hex), show(Image)]),
            (
                choose(2),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
            (choose(1), CLEAN, vec![hide(Hex), release(Hex), show(Image)]),
        ],
    );
    assert_eq!(set.hidden(), [Hex]);
    // A hidden video plays nothing (Hide) and comes back at its playhead (Show, not Make).
    let mut set = open(named("talk.mkv"));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (
                Input::Made(Ticket(0)),
                CLEAN,
                vec![hide(Video), release(Video), show(Hex)],
            ),
            (choose(1), CLEAN, vec![hide(Hex), release(Hex), show(Video)]),
        ],
    );
}

/// VA-2 and D35, step by step at the model: the hex view comes up viewing, so 1→2→1 on an
/// image writes nothing; the edits made while editing ask the close prompt on a switch.
#[test]
fn va2_hex_viewing_editing_and_a_switch() {
    let mut set = open(named("shot.png"));
    // An image, 2, 1 → the image again, no byte written: the hex view came up viewing.
    let viewing_hex = Effect::Make {
        ticket: Ticket(0),
        view: Hex,
        at: Some(0),
        notice: None,
        encoding: None,
        viewing: true,
    };
    let shown_viewing = Effect::Show {
        view: Hex,
        at: None,
        viewing: true,
    };
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![viewing_hex]),
            (
                Input::Made(Ticket(0)),
                CLEAN,
                vec![hide(Image), release(Image), shown_viewing.clone()],
            ),
            (choose(1), CLEAN, vec![hide(Hex), release(Hex), show(Image)]),
            (
                choose(2),
                CLEAN,
                vec![hide(Image), release(Image), shown_viewing],
            ),
        ],
    );
    // Enter → editing; 4, 1 type 0x41 (VM-6: no view key). F9 then 1 → the close prompt;
    // Cancel → Hex, the edit kept; plain 1 → the prompt again; Discard → view 1.
    run(
        &mut set,
        vec![
            (choose(1), UNSAVED, vec![Effect::AskClose]),
            (Input::Cancelled, UNSAVED, vec![]),
        ],
    );
    assert_eq!((set.on_screen(), set.asking()), (Some(Hex), false));
    run(
        &mut set,
        vec![
            (choose(1), UNSAVED, vec![Effect::AskClose]),
            (Input::Closed, UNSAVED, vec![close(Hex), show(Image)]),
        ],
    );
    assert!(
        !set.is_made(Hex),
        "Discard: the hex view and its journal go"
    );
    // Again with Save → view 1 once the Save lands; the image, stale, is made again.
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(1, Hex)]),
            (
                Input::Made(Ticket(1)),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
            (choose(1), UNSAVED, vec![Effect::AskClose]),
            (Input::SaveEnded { landed: true }, CLEAN, vec![close(Image)]),
            (Input::Closed, CLEAN, vec![close(Hex), make(2, Image)]),
            (Input::Made(Ticket(2)), CLEAN, vec![show(Image)]),
        ],
    );
    assert_eq!(summary(&set), "Image idle made=Image");
    // Viewing, during a Save, 1 → saving…, then view 1; the hex view, clean now, is kept.
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(3, Hex)]),
            (
                Input::Made(Ticket(3)),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
            (choose(1), SAVING, vec![Effect::WaitForSave]),
            (
                Input::SaveEnded { landed: true },
                CLEAN,
                vec![close(Image), make(4, Image)],
            ),
            (
                Input::Made(Ticket(4)),
                CLEAN,
                vec![hide(Hex), release(Hex), show(Image)],
            ),
        ],
    );
    assert_eq!(set.hidden(), [Hex]);
    // Editing, Esc → viewing (VM-6); a second Esc → the close prompt.
    run(
        &mut set,
        vec![
            (
                choose(2),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
            (Input::Leave, UNSAVED, vec![Effect::AskClose]),
        ],
    );
    // An image crop, then 2 → the prompt.
    let mut set = open(named("shot.png"));
    run(&mut set, vec![(choose(2), UNSAVED, vec![Effect::AskClose])]);
}

/// 07 §10: a Save that did not land keeps the view, as closing does; the switch is dropped.
#[test]
fn d2_a_save_that_did_not_land_drops_the_switch() {
    let mut set = open(named("shot.png"));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (
                Input::Made(Ticket(0)),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
            (choose(1), SAVING, vec![Effect::WaitForSave]),
            (Input::SaveEnded { landed: false }, UNSAVED, vec![]),
            (choose(1), UNSAVED, vec![Effect::AskClose]),
            (Input::SaveEnded { landed: false }, UNSAVED, vec![]),
        ],
    );
    assert_eq!(summary(&set), "Hex idle made=Image,Hex");
}

/// §5: the view being made comes up only once made, and only if the view on screen still
/// lets it go — the owner may have cropped or saved meanwhile.
#[test]
fn made_asks_the_view_on_screen_again() {
    let mut set = open(named("shot.png"));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (Input::Made(Ticket(0)), UNSAVED, vec![Effect::AskClose]),
            (Input::Closed, UNSAVED, vec![close(Image), show(Hex)]),
        ],
    );
    let mut set = open(named("shot.png"));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (Input::Made(Ticket(0)), SAVING, vec![Effect::WaitForSave]),
            // The image's Save changed the file: the hex view just made is stale already.
            (
                Input::SaveEnded { landed: true },
                CLEAN,
                vec![close(Hex), make(1, Hex)],
            ),
            (
                Input::Made(Ticket(1)),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
        ],
    );
}

/// D25: a Save or a reload that lands in one view makes every hidden view stale; it is made
/// again when next shown, at its place.
#[test]
fn d25_hidden_views_go_stale() {
    let mut set = open(reached(named("notes.txt"), Reach::Local));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN.at(64), vec![make_at(0, Hex, 64)]),
            (
                Input::Made(Ticket(0)),
                CLEAN.at(64),
                vec![hide(Text), show_at(Hex, 64)],
            ),
            (Input::Reloaded, CLEAN.at(64), vec![close(Text)]),
            (choose(1), CLEAN.at(80), vec![make_at(1, Text, 80)]),
            // Raw is the same document, being made already: it comes up as Raw.
            (choose(3), CLEAN.at(80), vec![]),
            (
                Input::Made(Ticket(1)),
                CLEAN.at(80),
                vec![hide(Hex), release(Hex), show_at(Raw, 80)],
            ),
            // Raw saves nothing; the text document reads the file again after it changed.
            (Input::Reloaded, CLEAN, vec![close(Hex)]),
        ],
    );
    assert_eq!(set.place(Hex), Some(80), "kept for its next make");
    run(
        &mut set,
        vec![(choose(2), CLEAN.at(96), vec![make_at(2, Hex, 96)])],
    );
}

/// D25 with D23: a binary's 3 runs its local whole-file pass while the owner saves an edit in
/// Hex; the text read before the Save is not shown — the make starts again.
#[test]
fn d25_a_save_during_a_make_makes_it_again() {
    let mut set = open(reached(routed("blob.bin", &[0x7f, 0, 1]), Reach::Local));
    run(
        &mut set,
        vec![
            (choose(3), CLEAN, vec![make(0, Text)]),
            (
                Input::SaveEnded { landed: true },
                CLEAN,
                vec![Effect::Abandon(Ticket(0)), make(1, Text)],
            ),
            (Input::Made(Ticket(0)), CLEAN, vec![]),
            (
                Input::Made(Ticket(1)),
                CLEAN,
                vec![hide(Hex), release(Hex), show_at(Text, 0)],
            ),
        ],
    );
}

/// The prompt's Save, a view key during it: the switch waits for the Save, and the window's own
/// close after it changes nothing, in either order (07 §10's close_when_done).
#[test]
fn a_view_key_during_the_prompts_save_waits_for_it() {
    for closed_first in [false, true] {
        let mut set = open(routed("blob.bin", &[0x7f, 0, 1]));
        run(
            &mut set,
            vec![
                (choose(3), CLEAN, vec![make(0, Text)]),
                (
                    Input::Made(Ticket(0)),
                    CLEAN,
                    vec![hide(Hex), release(Hex), show_at(Text, 0)],
                ),
                (choose(2), CLEAN, vec![hide(Text), show_at(Hex, 0)]),
                (choose(3), UNSAVED, vec![Effect::AskClose]),
                // Save chosen in the prompt; the owner presses 3 again while it runs.
                (choose(3), SAVING, vec![Effect::WaitForSave]),
            ],
        );
        let mut ends = vec![
            (
                Input::SaveEnded { landed: true },
                CLEAN,
                vec![close(Text), make(1, Text)],
            ),
            (Input::Closed, CLEAN, vec![]),
        ];
        if closed_first {
            ends.reverse();
        }
        run(&mut set, ends);
        assert_eq!(
            summary(&set),
            "Hex making Text made=Hex",
            "closed first: {closed_first}"
        );
    }
}

/// D14: a hidden view gives back its save connection; the read-only ones hold none.
#[test]
fn d14_a_hidden_view_gives_back_its_save_connection() {
    let cases: [(&str, Opening, Screen, u8, bool); 9] = [
        ("image → hex", named("a.png"), Screen::GUI, 2, true),
        ("video → hex", named("a.mp4"), Screen::GUI, 2, true),
        ("audio → hex", named("a.mp3"), Screen::GUI, 2, true),
        ("text → hex", named("a.rs"), Screen::GUI, 2, false),
        ("hex → text", routed("b.bin", &[0, 1]), Screen::GUI, 3, true),
        ("page → hex", paged(named("a.html"), false), PAGES, 2, false),
        (
            "revision image → hex",
            revision(named("a.png")),
            Screen::GUI,
            2,
            false,
        ),
        (
            "revision hex → text",
            revision(routed("b.bin", &[0])),
            Screen::GUI,
            3,
            false,
        ),
        (
            "revision text → hex",
            revision(named("a.rs")),
            Screen::GUI,
            2,
            false,
        ),
    ];
    for (what, opening, screen, key, gives_back) in cases {
        let mut set = ViewSet::open(opening, screen).unwrap();
        let first = set.on_screen().unwrap();
        assert!(
            matches!(set.apply(choose(key), CLEAN)[..], [Effect::Make { .. }]),
            "{what}"
        );
        let fx = set.apply(Input::Made(Ticket(0)), CLEAN);
        assert_eq!(fx.contains(&release(first)), gives_back, "{what}: {fx:?}");
        let released = fx
            .iter()
            .filter(|e| matches!(e, Effect::ReleaseSaveConnection(_)))
            .count();
        assert_eq!(
            released,
            usize::from(gives_back),
            "{what}: only the hidden view's"
        );
    }
}

/// D11, VA-4: Text → Hex puts the cursor on the top row's first byte; Hex → Text opens at the
/// cursor's byte; Text ↔ Raw is one view state; from Image a byte view keeps its own place.
#[test]
fn d11_the_place_follows_the_owner_between_byte_views() {
    let mut set = open(reached(named("main.rs"), Reach::Local));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN.at(4096), vec![make_at(0, Hex, 4096)]),
            (
                Input::Made(Ticket(0)),
                CLEAN.at(4096),
                vec![hide(Text), show_at(Hex, 4096)],
            ),
            (
                choose(1),
                CLEAN.at(100),
                vec![hide(Hex), release(Hex), show_at(Text, 100)],
            ),
            (choose(3), CLEAN.at(200), vec![show(Raw)]),
            (choose(2), CLEAN.at(200), vec![hide(Raw), show_at(Hex, 200)]),
        ],
    );
    assert_eq!(set.place(Text), Some(200));
    assert_eq!(set.place(Raw), Some(200));
    assert_eq!(set.place(Image), None);

    let mut set = open(named("shot.png"));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make_at(0, Hex, 0)]),
            (
                Input::Made(Ticket(0)),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
            (
                choose(1),
                CLEAN.at(50),
                vec![hide(Hex), release(Hex), show(Image)],
            ),
            (
                choose(2),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
        ],
    );
    assert_eq!(set.place(Hex), Some(50), "its own place, kept");

    // A binary's 3 opens at the hex cursor.
    let mut set = open(routed("blob.bin", &[0x7f, 0, 1]));
    run(
        &mut set,
        vec![(choose(3), CLEAN.at(777), vec![make_at(0, Text, 777)])],
    );
}

/// D8: Raw is the Text view with highlighting off — nothing is made, hidden or asked.
#[test]
fn d8_text_and_raw_are_one_document() {
    let mut set = open(named("data.json"));
    run(
        &mut set,
        vec![
            (choose(3), CLEAN, vec![show(Raw)]),
            (choose(1), CLEAN, vec![show(Text)]),
        ],
    );
    assert!(set.is_made(Raw) && set.is_made(Text));
    assert!(set.hidden().is_empty());
}

/// D7 and §4: none does nothing; the view on screen chosen again does nothing — a binary's
/// 1 and 2 are one Hex view — and leaves whatever waits as it was.
#[test]
fn d7_none_and_the_view_on_screen_do_nothing() {
    for name in ["a.png", "a.mp4", "a.mp3"] {
        let mut set = open(named(name));
        for now in [CLEAN, UNSAVED, SAVING] {
            assert_eq!(set.apply(choose(3), now), [], "{name}");
            assert_eq!(set.apply(choose(1), now), [], "{name}");
        }
    }
    let mut set = open(routed("blob.bin", &[0x7f, 0]));
    for now in [CLEAN, UNSAVED, SAVING] {
        assert_eq!(set.apply(choose(1), now), []);
        assert_eq!(set.apply(choose(2), now), []);
    }
    let mut set = open(named("shot.png"));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (choose(1), CLEAN, vec![]),
            (choose(2), CLEAN, vec![]),
        ],
    );
    assert_eq!(set.making(), Some(Hex), "the make goes on");
    run(
        &mut set,
        vec![
            (
                Input::Made(Ticket(0)),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
            (choose(1), SAVING, vec![Effect::WaitForSave]),
            (choose(2), SAVING, vec![]),
        ],
    );
    assert!(set.waiting(), "still waiting for the Save");
}

/// D10, VA-6: what view 1 cannot show opens on Hex with the reason; 1 says it again and stays,
/// even over unsaved hex edits (no switch, so no prompt).
#[test]
fn d10_opens_on_hex_and_1_says_why() {
    let bad = Why::Undecodable("unknown image format".to_string());
    let cases = [
        (undecodable(named("shot.png"), "unknown image format"), bad),
        (empty(named("e.png")), Why::Empty),
        (empty(named("e.mp4")), Why::Empty),
        (empty(named("e.mp3")), Why::Empty),
    ];
    for (opening, why) in cases {
        let mut set = open(opening);
        assert_eq!(set.on_screen(), Some(Hex));
        assert_eq!(set.unshown(), Some(&why));
        run(
            &mut set,
            vec![
                (choose(1), CLEAN, vec![say(&why)]),
                (choose(1), UNSAVED, vec![say(&why)]),
                (choose(2), UNSAVED, vec![]),
                (choose(3), UNSAVED, vec![]),
            ],
        );
        assert_eq!(set.on_screen(), Some(Hex));
    }
}

/// D10, VA-6: a video none of whose frames decode, or a remote image that does not, finds out
/// on screen and gives way to Hex, its first row the reason.
#[test]
fn d10_a_media_view_that_fails_on_screen_gives_way_to_hex() {
    let why = Why::Undecodable("no frame of it decodes".to_string());
    let notice = why.to_string();
    let first_row = Effect::Make {
        ticket: Ticket(0),
        view: Hex,
        at: Some(0),
        notice: Some(notice.clone()),
        encoding: None,
        viewing: true,
    };
    for name in ["talk.mp4", "shot.png"] {
        let mut set = open(named(name));
        let first = set.on_screen().unwrap();
        run(
            &mut set,
            vec![
                (
                    Input::Failed(why.clone()),
                    CLEAN,
                    vec![close(first), first_row.clone()],
                ),
                (Input::Made(Ticket(0)), CLEAN, vec![show(Hex)]),
                (choose(1), CLEAN, vec![say(&why)]),
            ],
        );
        assert_eq!(set.unshown(), Some(&why), "{name}");
        assert_eq!(summary(&set), "Hex idle made=Hex", "{name}");
    }
    // Hex made already: shown at once, the reason said.
    let mut set = open(named("talk.mp4"));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (
                Input::Made(Ticket(0)),
                CLEAN,
                vec![hide(Video), release(Video), show(Hex)],
            ),
            (choose(1), CLEAN, vec![hide(Hex), release(Hex), show(Video)]),
            (
                Input::Failed(why.clone()),
                CLEAN,
                vec![close(Video), show(Hex), Effect::Say(notice.clone())],
            ),
        ],
    );
    // Hex being made: made again, the reason its first row (and again if the file changes).
    let mut set = open(named("talk.mp4"));
    let first_row_at = |ticket: u64| Effect::Make {
        ticket: Ticket(ticket),
        view: Hex,
        at: Some(0),
        notice: Some(notice.clone()),
        encoding: None,
        viewing: true,
    };
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (
                Input::Failed(why.clone()),
                CLEAN,
                vec![close(Video), Effect::Abandon(Ticket(0)), first_row_at(1)],
            ),
            (
                Input::Reloaded,
                CLEAN,
                vec![Effect::Abandon(Ticket(1)), first_row_at(2)],
            ),
            (Input::Made(Ticket(1)), CLEAN, vec![]),
            (Input::Made(Ticket(2)), CLEAN, vec![show(Hex)]),
        ],
    );
    // Only a media view on screen can fail.
    let mut set = open(named("a.rs"));
    assert_eq!(set.apply(Input::Failed(why), CLEAN), []);
}

/// §4 "Leave wins" (VA-3, PR #61's race): a view key and a leave together leave; no view is
/// made, in either order with a give-way.
#[test]
fn leave_wins() {
    let mut set = open(named("shot.png"));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (
                Input::Leave,
                CLEAN,
                vec![Effect::Abandon(Ticket(0)), Effect::Leave],
            ),
            (Input::Made(Ticket(0)), CLEAN, vec![]),
            (choose(2), CLEAN, vec![]),
        ],
    );
    assert!(set.is_left());
    let why = Why::Undecodable("truncated".to_string());
    let mut set = open(named("shot.png"));
    let fx = set.apply(Input::Failed(why.clone()), CLEAN);
    assert!(
        matches!(fx[..], [Effect::Close(Image), Effect::Make { .. }]),
        "{fx:?}"
    );
    run(
        &mut set,
        vec![(
            Input::Leave,
            CLEAN,
            vec![Effect::Abandon(Ticket(0)), Effect::Leave],
        )],
    );
    let mut set = open(named("shot.png"));
    run(
        &mut set,
        vec![
            (Input::Leave, CLEAN, vec![Effect::Leave]),
            (Input::Failed(why), CLEAN, vec![]),
        ],
    );
}

/// §5: leaving asks the view on screen; hidden views close with it.
#[test]
fn leaving_asks_the_view_on_screen() {
    let mut set = open(named("shot.png"));
    run(
        &mut set,
        vec![
            (Input::Leave, UNSAVED, vec![Effect::AskClose]),
            (Input::Cancelled, UNSAVED, vec![]),
            (Input::Leave, UNSAVED, vec![Effect::AskClose]),
            (Input::Closed, UNSAVED, vec![Effect::Leave]),
        ],
    );
    let mut set = open(named("shot.png"));
    run(
        &mut set,
        vec![
            (Input::Leave, SAVING, vec![Effect::WaitForSave]),
            (Input::SaveEnded { landed: false }, UNSAVED, vec![]),
            (Input::Leave, SAVING, vec![Effect::WaitForSave]),
            (
                Input::SaveEnded { landed: true },
                CLEAN,
                vec![Effect::Leave],
            ),
        ],
    );
    assert!(set.is_left());
}

/// §5: Esc during a make stays on the view on screen (D23's local 3 of a binary); with none
/// on screen — the one there closed after its prompt — it leaves.
#[test]
fn esc_during_a_make() {
    let mut set = open(reached(routed("blob.bin", &[0x7f, 0]), Reach::Local));
    run(
        &mut set,
        vec![
            (choose(3), CLEAN, vec![make(0, Text)]),
            (Input::CancelMake, CLEAN, vec![Effect::Abandon(Ticket(0))]),
            (Input::Made(Ticket(0)), CLEAN, vec![]),
        ],
    );
    assert_eq!(summary(&set), "Hex idle made=Hex");
    run(&mut set, vec![(choose(3), CLEAN, vec![make(1, Text)])]);

    let mut set = blank();
    run(
        &mut set,
        vec![(
            Input::CancelMake,
            CLEAN,
            vec![Effect::Abandon(Ticket(1)), Effect::Leave],
        )],
    );
}

#[test]
fn a_make_that_fails_says_why() {
    let mut set = open(routed("blob.bin", &[0x7f, 0]));
    run(
        &mut set,
        vec![
            (choose(3), CLEAN, vec![make(0, Text)]),
            (
                Input::MakeFailed(Ticket(0), "read failed".to_string()),
                CLEAN,
                vec![Effect::Say("read failed".to_string())],
            ),
        ],
    );
    assert_eq!(summary(&set), "Hex idle made=Hex");
}

/// D10 for a make: an Image (or Video, Audio) view that cannot be made cannot show the file. With
/// Hex on screen, 1 says why from then on and is not tried again; with nothing on screen (VA-2's
/// Save: the hex view closed, the stale image made again), Hex shows the file, the reason its
/// first row (§1: every file has a hex view). Only a Hex view that fails too leaves.
#[test]
fn d10_a_media_view_that_cannot_be_made_gives_way_to_hex() {
    let why = Why::Undecodable("unknown image format".to_string());
    let mut set = open(named("shot.png"));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (
                Input::Made(Ticket(0)),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
            (Input::Reloaded, CLEAN, vec![close(Image)]),
            (choose(1), CLEAN, vec![make(1, Image)]),
            (
                Input::MakeFailed(Ticket(1), "unknown image format".to_string()),
                CLEAN,
                vec![say(&why)],
            ),
            (choose(1), CLEAN, vec![say(&why)]),
        ],
    );
    assert_eq!(set.unshown(), Some(&why));
    assert_eq!(summary(&set), "Hex idle made=Hex");

    let mut set = blank();
    let first_row = Effect::Make {
        ticket: Ticket(2),
        view: Hex,
        at: Some(0),
        notice: Some(why.to_string()),
        encoding: None,
        viewing: true,
    };
    run(
        &mut set,
        vec![
            (
                Input::MakeFailed(Ticket(1), "unknown image format".to_string()),
                CLEAN,
                vec![first_row],
            ),
            (choose(1), CLEAN, vec![say(&why)]),
            (
                Input::MakeFailed(Ticket(2), "read failed".to_string()),
                CLEAN,
                vec![Effect::Say("read failed".to_string()), Effect::Leave],
            ),
        ],
    );
    assert!(set.is_left());
}

/// A different view chosen during a make replaces it; an abandoned make's answer is ignored.
#[test]
fn another_view_chosen_during_a_make_replaces_it() {
    let mut set = ViewSet::open(paged(named("index.html"), false), Screen::NO_ENGINE).unwrap();
    let mut set_pages = ViewSet::open(paged(named("index.html"), false), PAGES).unwrap();
    run(
        &mut set_pages,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (
                choose(3),
                CLEAN,
                vec![Effect::Abandon(Ticket(0)), make(1, Text)],
            ),
            (Input::Made(Ticket(0)), CLEAN, vec![]),
            (Input::Made(Ticket(1)), CLEAN, vec![hide(Page), show(Text)]),
        ],
    );
    // With no engine: Text on screen, Hex being made; Raw is the document on screen.
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (
                choose(3),
                CLEAN,
                vec![Effect::Abandon(Ticket(0)), show(Raw)],
            ),
        ],
    );
}

/// §6, VA-5: a handed-over image preview has no stream; the first view made after it opens one,
/// and the file is read once whatever the switches.
#[test]
fn a_handed_over_image_opens_one_stream() {
    let mut set = open(reached(named("shot.png"), Reach::NoStream));
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![Effect::OpenStream, make(0, Hex)]),
            (
                Input::Made(Ticket(0)),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
            (choose(1), CLEAN, vec![hide(Hex), release(Hex), show(Image)]),
            (
                choose(2),
                CLEAN,
                vec![hide(Image), release(Image), show(Hex)],
            ),
            // A hex Save: the image, stale, is made again from the kept stream.
            (Input::SaveEnded { landed: true }, CLEAN, vec![close(Image)]),
            (choose(1), CLEAN, vec![make(1, Image)]),
        ],
    );
    assert_eq!(set.reach(), Reach::Stream);
    assert_eq!(
        set.opening().reach,
        Reach::NoStream,
        "the opening as it was"
    );
}

/// VA-5: a revision's views are read-only, read from its `.revisions` copy, and hold no save
/// connection.
#[test]
fn a_revision_is_read_only_in_every_view() {
    let mut set = open(revision(named("a.png")));
    assert!(set.read_only());
    assert_eq!(set.opening().path, "/cache/.revisions/abc123/f");
    run(
        &mut set,
        vec![
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (Input::Made(Ticket(0)), CLEAN, vec![hide(Image), show(Hex)]),
            (choose(1), CLEAN, vec![hide(Hex), show(Image)]),
        ],
    );
}

/// D21: one opening, one encoding, for a text document made again too.
#[test]
fn d21_one_encoding_per_opening() {
    let mut set = open(named("notes.txt"));
    assert_eq!(set.encoding(), None);
    run(
        &mut set,
        vec![
            (Input::Encoding("CP949".to_string()), CLEAN, vec![]),
            (choose(2), CLEAN, vec![make(0, Hex)]),
            (
                Input::Made(Ticket(0)),
                CLEAN,
                vec![hide(Text), show_at(Hex, 0)],
            ),
            (Input::Reloaded, CLEAN, vec![close(Text)]),
            (choose(3), CLEAN, vec![in_cp949(1, Raw)]),
            (Input::CancelMake, CLEAN, vec![Effect::Abandon(Ticket(1))]),
            (choose(1), CLEAN, vec![in_cp949(2, Text)]),
        ],
    );
    assert_eq!(set.encoding(), Some("CP949"));
    // A binary's 3, made after the encoding was said, decodes by it too (the Hex make above
    // carried none).
    let mut set = open(routed("blob.bin", &[0x7f, 0]));
    let _ = set.apply(Input::Encoding("CP949".to_string()), CLEAN);
    run(&mut set, vec![(choose(3), CLEAN, vec![in_cp949(0, Text)])]);
    // Before the text document has said one, a make carries none: §7.2's default.
    let mut set = open(routed("blob.bin", &[0x7f, 0]));
    run(&mut set, vec![(choose(3), CLEAN, vec![make(0, Text)])]);
}

/// A text document made in the opening's CP949 (D21), at byte 0.
fn in_cp949(ticket: u64, view: View) -> Effect {
    Effect::Make {
        ticket: Ticket(ticket),
        view,
        at: Some(0),
        notice: None,
        encoding: Some("CP949".to_string()),
        viewing: false,
    }
}

// ---- every pending state × every hold × every input ----

/// An image on screen, Hex made and hidden.
fn idle() -> ViewSet {
    let mut set = open(named("shot.png"));
    for input in [choose(2), Input::Made(Ticket(0)), choose(1)] {
        let _ = set.apply(input, CLEAN);
    }
    set
}

/// Hex on screen with edits, the prompt up for a switch to Image.
fn asking() -> ViewSet {
    let mut set = idle();
    let _ = set.apply(choose(2), CLEAN);
    let _ = set.apply(choose(1), UNSAVED);
    set
}

/// Hex on screen saving, the switch to Image waiting.
fn waiting() -> ViewSet {
    let mut set = idle();
    let _ = set.apply(choose(2), CLEAN);
    let _ = set.apply(choose(1), SAVING);
    set
}

/// The prompt's Save landed and the hex view closed: nothing on screen, Image being made.
fn blank() -> ViewSet {
    let mut set = asking();
    let _ = set.apply(Input::SaveEnded { landed: true }, CLEAN);
    let _ = set.apply(Input::Closed, CLEAN);
    set
}

fn making() -> ViewSet {
    let mut set = open(named("shot.png"));
    let _ = set.apply(choose(2), CLEAN);
    set
}

#[test]
fn every_pending_state_by_every_hold_and_input() {
    let why = Why::Undecodable("x".to_string());
    let failed = Input::Failed(why.clone());
    let any: &[Hold] = &HOLDS;
    let (c, u, s): (&[Hold], &[Hold], &[Hold]) =
        (&[Hold::Clean], &[Hold::Unsaved], &[Hold::Saving]);
    let fail = |t: u64| Input::MakeFailed(Ticket(t), "x".to_string());
    let x = || Effect::Say("x".to_string());
    let landed = Input::SaveEnded { landed: true };
    let not_landed = Input::SaveEnded { landed: false };
    let enc = Input::Encoding("UTF-8".to_string());
    let abandon = |t: u64| Effect::Abandon(Ticket(t));
    let first_row = |t: u64| Effect::Make {
        ticket: Ticket(t),
        view: Hex,
        at: Some(0),
        notice: Some(why.to_string()),
        encoding: None,
        viewing: true,
    };
    /// An input, the holds it is tried with, its effects, the summary after (`=`: unchanged).
    type Cell = (Input, &'static [Hold], Vec<Effect>, &'static str);
    /// A situation's name, how it is reached, its summary, its cells.
    type Situation = (&'static str, fn() -> ViewSet, &'static str, Vec<Cell>);
    #[rustfmt::skip]
    let grid: Vec<Situation> = vec![
        ("idle", idle, "Image idle made=Image,Hex", vec![
            (choose(1), any, vec![], "="),
            (choose(2), c, vec![hide(Image), release(Image), show(Hex)], "Hex idle made=Image,Hex"),
            (choose(2), u, vec![Effect::AskClose], "Image asking Hex made=Image,Hex"),
            (choose(2), s, vec![Effect::WaitForSave], "Image waiting Hex made=Image,Hex"),
            (choose(3), any, vec![], "="),
            (Input::Leave, c, vec![Effect::Leave], "left"),
            (Input::Leave, u, vec![Effect::AskClose], "Image asking leave made=Image,Hex"),
            (Input::Leave, s, vec![Effect::WaitForSave], "Image waiting leave made=Image,Hex"),
            (Input::Made(Ticket(0)), any, vec![], "="),
            (Input::Made(Ticket(99)), any, vec![], "="),
            (fail(0), any, vec![], "="),
            (Input::CancelMake, any, vec![], "="),
            (Input::Cancelled, any, vec![], "="),
            (Input::Closed, any, vec![], "="),
            (landed.clone(), any, vec![close(Hex)], "Image idle made=Image"),
            (not_landed.clone(), any, vec![], "="),
            (Input::Reloaded, any, vec![close(Hex)], "Image idle made=Image"),
            (failed.clone(), any, vec![close(Image), show(Hex), say(&why)], "Hex idle made=Hex"),
            (enc.clone(), any, vec![], "="),
        ]),
        ("making", making, "Image making Hex made=Image", vec![
            (choose(1), any, vec![], "="),
            (choose(2), any, vec![], "="),
            (choose(3), any, vec![], "="),
            (Input::Leave, c, vec![abandon(0), Effect::Leave], "left"),
            (Input::Leave, u, vec![abandon(0), Effect::AskClose], "Image asking leave made=Image"),
            (Input::Leave, s, vec![abandon(0), Effect::WaitForSave], "Image waiting leave made=Image"),
            (Input::Made(Ticket(0)), c, vec![hide(Image), release(Image), show(Hex)], "Hex idle made=Image,Hex"),
            (Input::Made(Ticket(0)), u, vec![Effect::AskClose], "Image asking Hex made=Image,Hex"),
            (Input::Made(Ticket(0)), s, vec![Effect::WaitForSave], "Image waiting Hex made=Image,Hex"),
            (Input::Made(Ticket(99)), any, vec![], "="),
            (fail(0), any, vec![x()], "Image idle made=Image"),
            (fail(99), any, vec![], "="),
            (Input::CancelMake, any, vec![abandon(0)], "Image idle made=Image"),
            (Input::Cancelled, any, vec![], "="),
            (Input::Closed, any, vec![], "="),
            // D25: the view being made read the file before it changed.
            (landed.clone(), any, vec![abandon(0), make(1, Hex)], "="),
            (not_landed.clone(), any, vec![], "="),
            (Input::Reloaded, any, vec![abandon(0), make(1, Hex)], "="),
            (failed.clone(), any, vec![close(Image), abandon(0), first_row(1)], "- making Hex made="),
            (enc.clone(), any, vec![], "="),
        ]),
        ("asking", asking, "Hex asking Image made=Image,Hex", vec![
            (choose(1), c, vec![hide(Hex), release(Hex), show(Image)], "Image idle made=Image,Hex"),
            (choose(1), u, vec![Effect::AskClose], "="),
            (choose(1), s, vec![Effect::WaitForSave], "Hex waiting Image made=Image,Hex"),
            (choose(2), any, vec![], "="),
            (choose(3), any, vec![], "="),
            (Input::Leave, c, vec![Effect::Leave], "left"),
            (Input::Leave, u, vec![Effect::AskClose], "Hex asking leave made=Image,Hex"),
            (Input::Leave, s, vec![Effect::WaitForSave], "Hex waiting leave made=Image,Hex"),
            (Input::Made(Ticket(0)), any, vec![], "="),
            (Input::Made(Ticket(99)), any, vec![], "="),
            (fail(0), any, vec![], "="),
            (Input::CancelMake, any, vec![], "="),
            (Input::Cancelled, any, vec![], "Hex idle made=Image,Hex"),
            (Input::Closed, any, vec![close(Hex), show(Image)], "Image idle made=Image"),
            (landed.clone(), any, vec![close(Image)], "Hex asking Image made=Hex"),
            (not_landed.clone(), any, vec![], "Hex idle made=Image,Hex"),
            (Input::Reloaded, any, vec![close(Image)], "Hex asking Image made=Hex"),
            (failed.clone(), any, vec![], "="),
            (enc.clone(), any, vec![], "="),
        ]),
        ("waiting", waiting, "Hex waiting Image made=Image,Hex", vec![
            (choose(1), c, vec![hide(Hex), release(Hex), show(Image)], "Image idle made=Image,Hex"),
            (choose(1), u, vec![Effect::AskClose], "Hex asking Image made=Image,Hex"),
            (choose(1), s, vec![Effect::WaitForSave], "="),
            (choose(2), any, vec![], "="),
            (choose(3), any, vec![], "="),
            (Input::Leave, c, vec![Effect::Leave], "left"),
            (Input::Leave, u, vec![Effect::AskClose], "Hex asking leave made=Image,Hex"),
            (Input::Leave, s, vec![Effect::WaitForSave], "Hex waiting leave made=Image,Hex"),
            (Input::Made(Ticket(0)), any, vec![], "="),
            (Input::Made(Ticket(99)), any, vec![], "="),
            (fail(0), any, vec![], "="),
            (Input::CancelMake, any, vec![], "="),
            (Input::Cancelled, any, vec![], "="),
            // Nothing was asked: the switch goes on by the Save's end, whichever comes first.
            (Input::Closed, any, vec![], "="),
            (landed.clone(), c, vec![close(Image), make(1, Image)], "Hex making Image made=Hex"),
            (landed.clone(), u, vec![close(Image), Effect::AskClose], "Hex asking Image made=Hex"),
            (landed.clone(), s, vec![close(Image), Effect::WaitForSave], "Hex waiting Image made=Hex"),
            (not_landed.clone(), any, vec![], "Hex idle made=Image,Hex"),
            (Input::Reloaded, any, vec![close(Image)], "Hex waiting Image made=Hex"),
            (failed.clone(), any, vec![], "="),
            (enc.clone(), any, vec![], "="),
        ]),
        ("blank", blank, "- making Image made=", vec![
            (choose(1), any, vec![], "="),
            (choose(2), any, vec![abandon(1), make(2, Hex)], "- making Hex made="),
            (choose(3), any, vec![], "="),
            (Input::Leave, any, vec![abandon(1), Effect::Leave], "left"),
            (Input::Made(Ticket(1)), any, vec![show(Image)], "Image idle made=Image"),
            (Input::Made(Ticket(99)), any, vec![], "="),
            // D10: Image cannot be made, so Hex shows the file, the reason its first row.
            (fail(1), any, vec![first_row(2)], "- making Hex made="),
            (Input::CancelMake, any, vec![abandon(1), Effect::Leave], "left"),
            (Input::Cancelled, any, vec![], "="),
            (Input::Closed, any, vec![], "="),
            (landed.clone(), any, vec![abandon(1), make(2, Image)], "="),
            (not_landed.clone(), any, vec![], "="),
            (Input::Reloaded, any, vec![abandon(1), make(2, Image)], "="),
            (failed.clone(), any, vec![], "="),
            (enc.clone(), any, vec![], "="),
        ]),
    ];
    for (name, situation, start, cells) in grid {
        assert_eq!(summary(&situation()), start, "{name}");
        for (input, holds, want, after) in cells {
            let after = if after == "=" { start } else { after };
            for &hold in holds {
                let mut set = situation();
                let what = format!("{name}: {input:?} holding {hold:?}");
                assert_eq!(set.apply(input.clone(), Now::holding(hold)), want, "{what}");
                assert_eq!(summary(&set), after, "{what}");
            }
        }
    }
}

// ---- a long random walk: the rules hold whatever comes ----

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// What every step keeps: hide only what is on screen and clean, the hidden view's connection
/// given back, at most one view shown and it made, Hex shown viewing, one stream opened.
#[track_caller]
fn check(before: &ViewSet, input: &Input, now: Now, fx: &[Effect], after: &ViewSet) {
    let what = format!("{input:?} holding {now:?} → {fx:?}");
    if before.is_left() {
        assert!(fx.is_empty(), "{what}");
        return;
    }
    let shown = before.on_screen();
    let mut hides = 0;
    let mut shows = 0;
    for (i, effect) in fx.iter().enumerate() {
        let next = fx.get(i + 1);
        match effect {
            Effect::Hide(view) => {
                hides += 1;
                assert_eq!(Some(*view), shown, "{what}: only the view on screen hides");
                assert_eq!(now.hold, Hold::Clean, "{what}: only a clean view hides");
                assert!(
                    fx[i..].iter().any(|e| matches!(e, Effect::Show { .. })),
                    "{what}: a view is shown after it"
                );
                let gives_back = view.saves() && !before.read_only();
                assert_eq!(
                    next == Some(&release(*view)),
                    gives_back,
                    "{what}: its save connection (D14)"
                );
            }
            Effect::ReleaseSaveConnection(view) => {
                assert_eq!(fx.get(i.wrapping_sub(1)), Some(&hide(*view)), "{what}");
            }
            Effect::Show { view, viewing, .. } => {
                shows += 1;
                assert_eq!(*viewing, *view == Hex, "{what}: D35");
                assert_eq!(after.on_screen(), Some(*view), "{what}");
                assert!(after.is_made(*view), "{what}");
            }
            Effect::Make {
                view,
                viewing,
                encoding,
                ..
            } => {
                assert_eq!(*viewing, *view == Hex, "{what}: D35");
                let text = matches!(view, Text | Raw);
                let want = after.encoding().filter(|_| text);
                assert_eq!(encoding.as_deref(), want, "{what}: D21");
                assert_eq!(after.making(), Some(*view), "{what}");
                assert_eq!(i + 1, fx.len(), "{what}: a make comes last");
            }
            Effect::OpenStream => {
                assert_eq!(before.reach(), Reach::NoStream, "{what}");
                assert!(matches!(next, Some(Effect::Make { .. })), "{what}");
            }
            Effect::Close(view) => {
                if shown.map(View::doc) == Some(view.doc()) {
                    assert!(
                        matches!(input, Input::Closed | Input::Failed(_)),
                        "{what}: the view on screen closes only once it agreed or failed"
                    );
                }
            }
            Effect::AskClose => {
                assert!(shown.is_some() && now.hold == Hold::Unsaved, "{what}");
            }
            Effect::WaitForSave => {
                assert!(shown.is_some() && now.hold == Hold::Saving, "{what}");
            }
            Effect::Leave => {
                assert_eq!(i + 1, fx.len(), "{what}: leaving comes last");
                assert!(after.is_left(), "{what}");
            }
            Effect::Abandon(_) | Effect::Say(_) => {}
        }
    }
    assert!(hides <= 1 && shows <= 1, "{what}");
    assert_eq!(after.slots(), before.slots(), "{what}");
    if after.is_left() {
        return;
    }
    if let Some(view) = after.on_screen() {
        assert!(after.is_made(view), "{what}: the view on screen is made");
    } else {
        assert!(
            after.making().is_some(),
            "{what}: nothing on screen and nothing coming"
        );
    }
    if let Some(view) = after.making() {
        assert!(!after.is_made(view), "{what}: made once");
    }
}

#[test]
fn a_long_random_walk_keeps_the_rules() {
    let openings = [
        (named("shot.png"), Screen::GUI),
        (reached(named("shot.png"), Reach::NoStream), Screen::GUI),
        (named("talk.mp4"), Screen::GUI),
        (reached(named("main.rs"), Reach::Local), Screen::GUI),
        (routed("blob.bin", &[0x7f, 0, 1]), Screen::GUI),
        (paged(named("index.html"), false), PAGES),
        (paged(named("feed.xml"), false), PAGES),
        (revision(named("a.png")), Screen::GUI),
        (undecodable(named("shot.png"), "bad"), Screen::GUI),
        (named("a.png"), Screen::TERMINAL),
    ];
    for (k, (opening, screen)) in openings.into_iter().enumerate() {
        let mut rng = Lcg(0x5eed + k as u64);
        let mut set = ViewSet::open(opening.clone(), screen).unwrap();
        let mut streams = 0;
        let mut last_ticket = None;
        for _ in 0..20_000 {
            let hold = HOLDS[rng.pick(3)];
            let place = (rng.pick(2) == 0).then(|| rng.next() % (1 << 30));
            let now = Now { hold, place };
            let making = match set.pending {
                Pending::Making { ticket, .. } => Some(ticket),
                _ => None,
            };
            let ticket = match (making, rng.pick(4)) {
                (Some(t), 0..=2) => t,
                _ => Ticket(rng.next() % 8),
            };
            let input = match rng.pick(16) {
                0..=4 => Input::Choose(Slot::ALL[rng.pick(3)]),
                5 if rng.pick(6) == 0 => Input::Leave,
                5 | 6 => Input::Made(ticket),
                7 => Input::MakeFailed(ticket, "x".to_string()),
                8 => Input::CancelMake,
                9 => Input::Cancelled,
                10 => Input::Closed,
                11 => Input::SaveEnded {
                    landed: rng.pick(2) == 0,
                },
                12 => Input::Reloaded,
                13 if rng.pick(4) == 0 => Input::Failed(Why::Undecodable("x".to_string())),
                _ => Input::Encoding("UTF-8".to_string()),
            };
            let before = set.clone();
            let fx = set.apply(input.clone(), now);
            check(&before, &input, now, &fx, &set);
            for effect in &fx {
                match effect {
                    Effect::OpenStream => streams += 1,
                    Effect::Make { ticket, .. } => {
                        assert!(
                            last_ticket.is_none_or(|t: u64| ticket.0 > t),
                            "fresh tickets"
                        );
                        last_ticket = Some(ticket.0);
                    }
                    _ => {}
                }
            }
            assert!(streams <= 1, "one stream per opening (§6)");
            if set.is_left() {
                set = ViewSet::open(opening.clone(), screen).unwrap();
                streams = 0;
                last_ticket = None;
            }
        }
    }
}
