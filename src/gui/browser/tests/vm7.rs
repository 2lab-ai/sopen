//! VM-7 (08 §8, §11's VA items end to end through the window): the viewer's views as the
//! keyboard sends their keys — the key with its physical key, then its typed text, in one pass
//! (`Driver::press_typed`, 08 §12) — on a text, an image, a binary and a media file; the hex view
//! viewing until Enter (D35); F8's list of encodings; the strip and the key rows.

use super::*;

/// `n` pressed as the keyboard sends it (the key printed `n`, then its text), the window
/// settled after it.
fn digit(fx: &mut Fx, n: u8) {
    let key = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4][usize::from(n)];
    fx.drv.press_typed(&mut fx.b, key, &n.to_string());
    fx.settle();
    // A text view says it is ready in the pass its page or index lands, drawn before it.
    fx.drv.idle(&mut fx.b, 2);
}

/// Enter on F8's list: the lines are read again in the encoding chosen. The text view says
/// it is ready in the pass a new index lands, whose bar was drawn before it: one pass more.
fn choose(fx: &mut Fx) {
    fx.key(Key::Enter);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
}

/// The strip's entry on screen, in the bar's colours: a run of its own on the title row.
fn on_strip(fx: &Fx, entry: &str) -> bool {
    let p = chrome::PALETTE;
    (fx.drv.colors_of(entry)).is_some_and(|c| c.iter().all(|&c| c == p.bar_text))
}

/// The first row of the hex view starts with `bytes` (its offset column before it).
fn hex_row(fx: &Fx, bytes: &str) -> bool {
    fx.drv.text_starting_with(bytes).is_some()
}

/// VA-1 for a text file: Enter shows 1 (Text, highlighted), 2 Hex, 3 Raw (the same lines, not
/// highlighted), 1 Text again — nothing typed anywhere, the file as it was.
#[test]
fn va1_a_text_files_views_by_1_2_3() {
    let mut fx = fixture();
    let file = format!("{}/main.rs", fx.root);
    fx.open("main.rs");
    assert!(on_strip(&fx, "1 Text"), "the strip: 1 Text on screen");
    let colors = fx.drv.colors_of("fn main() {").expect("the code");
    assert!(
        colors.iter().collect::<HashSet<_>>().len() >= 2,
        "highlighted"
    );
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "2F 2F 20 61"), "Hex: `// a` as bytes");
    assert!(on_strip(&fx, "2 Hex"));
    assert!(
        shows(&fx, "viewing — Enter edits"),
        "the hex view opens viewing"
    );
    assert!(
        fx.drv.find_text("fn main() {").is_none(),
        "the text is hidden"
    );
    digit(&mut fx, 3);
    assert!(on_strip(&fx, "3 Raw"));
    let colors = fx.drv.colors_of("fn main() {").expect("Raw shows the code");
    assert_eq!(
        colors.iter().collect::<HashSet<_>>().len(),
        1,
        "Raw is not highlighted: {colors:?}"
    );
    digit(&mut fx, 1);
    assert!(on_strip(&fx, "1 Text"));
    let colors = fx.drv.colors_of("fn main() {").expect("Text again");
    assert!(
        colors.iter().collect::<HashSet<_>>().len() >= 2,
        "highlighted again"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        RUST,
        "nothing typed"
    );
    fx.key(Key::Escape);
    assert!(
        fx.b.viewer.is_none(),
        "Esc leaves, the hidden views with it"
    );
}

/// VA-1 and VA-2 for an image: 2 shows its bytes viewing, 1 the picture again with no byte
/// written, and 3 does nothing (image has none, D7): the strip has no 3.
#[test]
fn va1_an_images_views_by_1_2_and_no_3() {
    let mut fx = fixture();
    let file = format!("{}/photo.png", fx.root);
    let before = std::fs::read(&file).unwrap();
    fx.open("photo.png");
    assert!(!fx.drv.pictures().is_empty(), "view 1: the picture");
    assert!(on_strip(&fx, "1 Image"));
    let strip = fx.b.viewer.as_ref().unwrap().views.strip();
    assert_eq!(strip.text(), "1 Image · 2 Hex", "no 3 on the strip");
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "89 50 4E 47"), "view 2: the PNG's bytes");
    assert!(fx.drv.pictures().is_empty(), "the picture hidden");
    digit(&mut fx, 1);
    assert!(!fx.drv.pictures().is_empty(), "the picture again");
    assert!(!hex_row(&fx, "89 50 4E 47"));
    digit(&mut fx, 3);
    assert!(!fx.drv.pictures().is_empty(), "3 does nothing on an image");
    assert_eq!(std::fs::read(&file).unwrap(), before, "no byte written");
}

/// VA-1 for a binary: it opens on Hex (1 and 2 are the one view: `1·2 Hex`), viewing — the
/// digits never change a byte — and 3 is its text by §7.2's default; 2 is Hex again.
#[test]
fn va1_a_binarys_views_1_2_hex_and_3_text() {
    let mut fx = fixture();
    let file = format!("{}/data.bin", fx.root);
    let before = std::fs::read(&file).unwrap();
    fx.open("data.bin");
    assert!(hex_row(&fx, "00 01 02 03"));
    assert!(on_strip(&fx, "1·2 Hex"), "1 and 2 the one Hex view");
    for n in [1, 2, 4] {
        digit(&mut fx, n);
        assert!(hex_row(&fx, "00 01 02 03"), "{n}: still Hex, no byte typed");
    }
    digit(&mut fx, 3);
    assert!(on_strip(&fx, "3 Text"));
    assert!(!hex_row(&fx, "00 01 02 03"), "the text view");
    assert!(shows(&fx, "UTF-8"), "its bar names the encoding");
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "00 01 02 03"), "Hex again");
    assert_eq!(std::fs::read(&file).unwrap(), before, "no byte written");
}

/// VA-1 for media: a sound's view 1, 2 its bytes, 1 the sound again — nothing plays while it is
/// hidden, and nothing is written.
#[test]
fn va1_a_sounds_views_by_1_and_2() {
    let tools = match crate::ffmpeg::resolve() {
        Ok(t) => t,
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping: {e}");
            return;
        }
    };
    let mut fx = fixture();
    let wav = format!("{}/tone.wav", fx.root);
    let ok = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
        .arg(&wav)
        .status()
        .unwrap()
        .success();
    assert!(ok, "fixture");
    let before = std::fs::read(&wav).unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.open("tone.wav");
    assert!(on_strip(&fx, "1 Audio"));
    // The transport's clock: the playhead.
    let playhead = |fx: &Fx| {
        let line = fx.drv.text_starting_with("0:0").expect("the clock line");
        line.split(" / ").next().unwrap().trim().to_string()
    };
    // Playing, then hidden: it stops where it was (D1), and nothing plays while hidden.
    fx.key(Key::Space);
    for _ in 0..10 {
        fx.drv.idle(&mut fx.b, 1);
        std::thread::sleep(Duration::from_millis(20));
    }
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "52 49 46 46"), "the WAV's bytes: RIFF");
    assert!(on_strip(&fx, "2 Hex"));
    for _ in 0..20 {
        fx.drv.idle(&mut fx.b, 1);
        std::thread::sleep(Duration::from_millis(20));
    }
    digit(&mut fx, 1);
    assert!(on_strip(&fx, "1 Audio"), "the sound again");
    assert!(!hex_row(&fx, "52 49 46 46"));
    let back = playhead(&fx);
    for _ in 0..10 {
        fx.drv.idle(&mut fx.b, 1);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        playhead(&fx),
        back,
        "paused where it was hidden: nothing plays"
    );
    assert_eq!(std::fs::read(&wav).unwrap(), before, "nothing written");
}

/// D35 (VA-2): in the hex view viewing, digits and letters are never bytes; Enter edits, and
/// then `4`, `1` type 0x41 with no switch; Esc is viewing again, its edit kept; `1` with that
/// edit asks the close prompt, whose Discard shows the picture, the file untouched.
#[test]
fn va2_hex_viewing_types_nothing_and_editing_asks_before_a_switch() {
    let mut fx = fixture();
    let file = format!("{}/photo.png", fx.root);
    let before = std::fs::read(&file).unwrap();
    fx.open("photo.png");
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "89 50 4E 47"));
    for n in [4, 4] {
        digit(&mut fx, n);
    }
    fx.typed("ab");
    assert!(hex_row(&fx, "89 50 4E 47"), "viewing: nothing typed");
    fx.key(Key::Enter);
    assert!(shows(&fx, "editing — Esc ends"));
    digit(&mut fx, 4);
    digit(&mut fx, 1);
    assert!(hex_row(&fx, "41 50 4E 47"), "editing: 4, 1 typed 0x41");
    assert!(on_strip(&fx, "2 Hex"), "and no switch");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_some(), "Esc ends editing, it does not leave");
    assert!(shows(&fx, "viewing — Enter edits"));
    digit(&mut fx, 1);
    assert!(
        fx.drv.find_text("Unsaved changes").is_some(),
        "the edit asks first"
    );
    fx.drv.click_text(&mut fx.b, "Cancel");
    fx.settle();
    assert!(hex_row(&fx, "41 50 4E 47"), "Cancel: Hex, the edit kept");
    digit(&mut fx, 1);
    fx.drv.click_text(&mut fx.b, "Discard");
    fx.settle();
    assert!(!fx.drv.pictures().is_empty(), "Discard: the picture");
    assert_eq!(std::fs::read(&file).unwrap(), before, "nothing written");
    // The hex view made again shows the file as stored.
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "89 50 4E 47"));
}

/// F9 then a digit chooses from where digits are typed (D4): from the hex view being edited.
#[test]
fn va3_f9_then_a_digit_chooses_while_editing() {
    let mut fx = fixture();
    fx.open("main.rs");
    digit(&mut fx, 2);
    fx.key(Key::Enter);
    fx.key(Key::F9);
    digit(&mut fx, 3);
    assert!(on_strip(&fx, "3 Raw"), "F9, 3: Raw");
    assert!(fx.drv.colors_of("fn main() {").is_some());
}

/// A file whose bytes are BOM-less UTF-16LE Korean, which §7.2 cannot tell (no ASCII pattern):
/// it opens as UTF-8, its rows mojibake. F8 lists every encoding, the current one marked; ↓ and
/// Enter choose UTF-16LE; the rows are the Korean text and the bar says `UTF-16LE`.
#[test]
fn f8_reads_a_misread_file_again_in_the_encoding_chosen() {
    let mut fx = fixture();
    let text = "가나다라 마바사\n아자차카 타파하\n";
    let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    std::fs::write(format!("{}/hangul.txt", fx.root), bytes).unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.open("hangul.txt");
    assert!(shows(&fx, "UTF-8"), "the default: UTF-8");
    assert!(!shows(&fx, "가나다라"), "misread");
    assert!(fx.drv.find_text("Encode ").is_some(), "8Encode in Text");
    fx.key(Key::F8);
    for label in [
        "ASCII", "UTF-16BE", "UCS-2LE", "UTF-32LE", "Latin-1", "CP437", "CP949",
    ] {
        assert!(shows(&fx, label), "{label} in the list");
    }
    assert!(
        shows(&fx, "• UTF-8 — the default"),
        "the current one marked, the default named"
    );
    fx.key(Key::ArrowDown);
    choose(&mut fx);
    assert!(shows(&fx, "가나다라 마바사"), "read again in UTF-16LE");
    assert!(shows(&fx, "아자차카 타파하"));
    assert!(shows(&fx, "UTF-16LE"), "the bar says it");
    assert!(!shows(&fx, "(guessed)"));
    // E opens it too; Esc leaves the encoding as it was.
    fx.key(Key::E);
    assert!(shows(&fx, "• UTF-16LE"));
    fx.key(Key::Escape);
    fx.settle();
    assert!(fx.b.viewer.is_some(), "Esc closed the list, not the viewer");
    assert!(shows(&fx, "가나다라 마바사"));
}

/// A guess is shown `(guessed)` (§7.2); another encoding chosen is said without it, and Raw
/// keeps the one encoding (D21): 3 shows the lines in the encoding chosen in Text.
#[test]
fn f8_a_choice_drops_the_guess_and_raw_keeps_it() {
    let mut fx = fixture();
    let bytes: Vec<u8> = "hello there\nsecond line\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    std::fs::write(format!("{}/wide.txt", fx.root), bytes).unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.open("wide.txt");
    assert!(shows(&fx, "UTF-16LE (guessed)"), "step 2's guess");
    assert!(shows(&fx, "hello there"));
    fx.key(Key::F8);
    assert!(
        shows(&fx, "• UTF-16LE — the default, guessed"),
        "the guess named"
    );
    // UTF-16LE → Latin-1: six rows down the list.
    for _ in 0..6 {
        fx.key(Key::ArrowDown);
    }
    choose(&mut fx);
    assert!(shows(&fx, "Latin-1"));
    assert!(!shows(&fx, "(guessed)"), "a choice is no guess");
    digit(&mut fx, 3);
    assert!(on_strip(&fx, "3 Raw"));
    assert!(shows(&fx, "Latin-1"), "Raw reads in the same encoding");
}

/// The key rows (08 §8, 04 K-R1): `9View` in every view, `8Encode` in Text and Raw only — the
/// hex view's 8 is Asc/hx; the list's row is as it was.
#[test]
fn the_rows_name_9view_everywhere_and_8encode_in_text() {
    let mut fx = fixture();
    let row = |fx: &Fx| -> Vec<(u8, &'static str)> {
        let viewer = fx.b.viewer.as_ref().expect("a viewer");
        let keys = chrome::merged(
            &keys::row(Context::Viewer, fx.b.mode),
            viewer.embedded.window.keys(),
        );
        keys.iter().map(|k| (k.n, k.label)).collect()
    };
    fx.open("main.rs");
    let text = row(&fx);
    assert!(
        text.contains(&(9, "View")) && text.contains(&(8, "Encode")),
        "{text:?}"
    );
    digit(&mut fx, 3);
    assert!(row(&fx).contains(&(8, "Encode")), "Raw too");
    digit(&mut fx, 2);
    let hex = row(&fx);
    assert!(
        hex.contains(&(9, "View")) && hex.contains(&(8, "Asc/hx")),
        "{hex:?}"
    );
    fx.key(Key::Escape);
    fx.open("photo.png");
    let image = row(&fx);
    assert!(
        image.contains(&(9, "View")) && !image.iter().any(|k| k.0 == 8),
        "{image:?}"
    );
    fx.key(Key::Escape);
    assert_eq!(
        keys::row(Context::List, fx.b.mode)
            .iter()
            .find(|k| k.n == 9)
            .map(|k| k.label),
        Some("Dual"),
        "the list's F9 is as it was"
    );
}

/// `SOPEN_KEYS`'s `typed:2` is the keyboard's press (08 §12): played into a viewer it chooses
/// view 2 and types nothing; `view=2` opens there.
#[test]
fn typed_steps_and_view_state_choose_views() {
    assert_eq!(
        parse_keys("typed:2,F8").unwrap(),
        [
            Play::Typed(Key::Num2, "2".to_string()),
            Play::Key(Key::F8, Modifiers::NONE)
        ]
    );
    assert!(parse_keys("typed:22").is_err());
    let mut fx = fixture();
    fx.open("main.rs");
    fx.b.script = parse_keys("typed:2").unwrap().into();
    fx.settle();
    assert!(hex_row(&fx, "2F 2F 20 61"), "typed:2 chose Hex");
    fx.key(Key::Escape);
    let state = crate::gui::State::parse("view=3").unwrap();
    assert_eq!(state.view, Some(3));
    assert!(crate::gui::State::parse("view=4").is_err());
    fx.b.scripted(&state);
    fx.open("notes.txt");
    fx.settle();
    assert!(on_strip(&fx, "3 Raw"), "view=3 opened Raw");
}

/// The prompt's Save (D2, VA-2): an edit in the hex view, `1`, Save → the Save lands, then
/// view 1, made again from the file as saved (D25: the hidden text view went stale) — `A/ a`.
#[test]
fn va2_the_prompts_save_lands_then_view_1_shows_the_file_saved() {
    let mut fx = fixture();
    let file = format!("{}/main.rs", fx.root);
    fx.open("main.rs");
    digit(&mut fx, 2);
    fx.key(Key::Enter);
    digit(&mut fx, 4);
    digit(&mut fx, 1);
    assert!(hex_row(&fx, "41 2F 20 61"), "0x41 over the first byte");
    // Editing, a digit is a nibble: F9 then 1 chooses (D4).
    fx.key(Key::F9);
    digit(&mut fx, 1);
    assert!(fx.drv.find_text("Unsaved changes").is_some(), "the prompt");
    fx.drv.click_text_on_top(&mut fx.b, "Save");
    fx.drv.until(&mut fx.b, 20, |b| {
        b.ready() && std::fs::read(&file).unwrap()[0] == 0x41
    });
    fx.settle();
    assert!(on_strip(&fx, "1 Text"), "view 1 once the Save landed");
    assert!(shows(&fx, "A/ a comment"), "the file as saved");
}

/// A Save of the view's own (F5) that lands makes the hidden views stale (D25): the image
/// shown again is made again from the file as saved — here bytes that no longer decode, so 1
/// says why and Hex stays (D10).
#[test]
fn va2_a_save_of_its_own_makes_the_hidden_views_stale() {
    let mut fx = fixture();
    let file = format!("{}/photo.png", fx.root);
    fx.open("photo.png");
    digit(&mut fx, 2);
    fx.key(Key::Enter);
    digit(&mut fx, 4);
    digit(&mut fx, 1);
    fx.key(Key::F5);
    fx.drv.until(&mut fx.b, 20, |b| {
        b.ready() && std::fs::read(&file).unwrap()[0] == 0x41
    });
    fx.settle();
    fx.key(Key::Escape);
    digit(&mut fx, 1);
    fx.settle();
    assert!(fx.drv.pictures().is_empty(), "the old picture is not shown");
    assert!(hex_row(&fx, "41 50 4E 47"), "Hex, the file as saved");
}

/// The memory line in the viewer names Hex view's key 2 (08 §8); 2 shows the bytes, 1 the
/// memory line again — the window that gave its file to the hex view is spent, made again.
#[test]
fn the_memory_lines_hex_view_is_2_and_1_asks_again() {
    let mut fx = fixture();
    std::fs::write(
        format!("{}/huge.png", fx.root),
        png_claiming(100_000, 100_000),
    )
    .unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.key(Key::F9);
    fx.select("huge.png");
    fx.key(Key::Enter);
    pass_until(&mut fx, "the memory line", |b| {
        b.viewer.is_some() && b.ready()
    });
    fx.drv.idle(&mut fx.b, 1);
    assert!(shows(&fx, "Hex view (2)"), "the memory line");
    digit(&mut fx, 2);
    pass_until(&mut fx, "the hex view", |b| b.ready());
    fx.drv.idle(&mut fx.b, 2);
    assert!(hex_row(&fx, "89 50 4E 47"), "the bytes");
    digit(&mut fx, 1);
    pass_until(&mut fx, "the memory line again", |b| b.ready());
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(&fx, "Hex view (2)"), "the memory line again");
    // H, the alias, gives the image window's own file to the hex view: that window is spent,
    // and 1 makes view 1 again — the memory line once more.
    fx.key(Key::H);
    pass_until(&mut fx, "the hex view by H", |b| b.ready());
    fx.drv.idle(&mut fx.b, 2);
    assert!(hex_row(&fx, "89 50 4E 47"), "H: the bytes");
    digit(&mut fx, 1);
    pass_until(&mut fx, "the memory line after H", |b| b.ready());
    fx.drv.idle(&mut fx.b, 2);
    assert!(shows(&fx, "Hex view (2)"), "the memory line after H");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
}

/// F8 to an encoding with the same LF unit reads the lines again in place (D22: no new index):
/// Latin-1 bytes misread as UTF-8 become `café`; from Raw too.
#[test]
fn f8_in_raw_reads_again_in_place() {
    let mut fx = fixture();
    std::fs::write(format!("{}/latin.txt", fx.root), b"caf\xe9 au lait\n").unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.open("latin.txt");
    assert!(shows(&fx, "caf\u{fffd} au lait"), "misread as UTF-8");
    digit(&mut fx, 3);
    assert!(on_strip(&fx, "3 Raw"));
    fx.key(Key::F8);
    assert!(shows(&fx, "• UTF-8 — the default"));
    // UTF-8 → Latin-1: seven rows down.
    for _ in 0..7 {
        fx.key(Key::ArrowDown);
    }
    choose(&mut fx);
    assert!(shows(&fx, "café au lait"), "Latin-1, read in place");
    assert!(shows(&fx, "Latin-1"));
    digit(&mut fx, 1);
    assert!(
        shows(&fx, "café au lait"),
        "Text keeps the one encoding (D21)"
    );
}

/// VA-1 through a stream (06): a remote text file's 2 and 3 are made over a stream of their own
/// (08 §8's binding; D13 not built), 1 the text again.
#[test]
fn va1_a_remote_text_files_views_by_1_2_3() {
    let mut fx = fixture_with(Some("box"), local_connect(), None);
    fx.open("main.rs");
    assert!(on_strip(&fx, "1 Text"));
    digit(&mut fx, 2);
    assert!(hex_row(&fx, "2F 2F 20 61"), "Hex through the stream");
    digit(&mut fx, 3);
    assert!(on_strip(&fx, "3 Raw"));
    assert!(
        fx.drv.colors_of("fn main() {").is_some(),
        "Raw through the stream"
    );
    digit(&mut fx, 1);
    assert!(on_strip(&fx, "1 Text"));
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
}

/// D11: Text → Hex puts the cursor on the first byte of the text's top row.
#[test]
fn va4_text_to_hex_keeps_the_place() {
    let mut fx = fixture();
    let lines: String = (0..300).map(|i| format!("line {i:03}\n")).collect();
    std::fs::write(format!("{}/long.txt", fx.root), lines).unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.open("long.txt");
    fx.key(Key::PageDown);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    let top =
        fx.b.viewer
            .as_ref()
            .and_then(|v| v.embedded.window.byte_place())
            .expect("a place");
    assert!(top > 0 && top.is_multiple_of(9), "a row's start: {top}");
    digit(&mut fx, 2);
    let cursor = format!("0x{top:X} ({top})");
    assert!(
        fx.drv.text_starting_with(&cursor).is_some(),
        "the hex cursor on byte {top}"
    );
}

/// D2 for the image window: a crop, then 2 asks its close prompt; Cancel stays on the picture.
#[test]
fn va2_an_edited_image_asks_before_a_switch() {
    let mut fx = fixture();
    fx.open("photo.png");
    edit_image(&mut fx);
    digit(&mut fx, 2);
    assert!(fx.drv.find_text("Unsaved changes").is_some(), "the prompt");
    fx.drv.click_text(&mut fx.b, "Cancel");
    fx.settle();
    assert!(on_strip(&fx, "1 Image"), "Cancel stays");
    assert!(!fx.drv.pictures().is_empty());
}

/// What a [`Holding`] window says, as the test sets it: what it holds, the Saves of it that
/// wrote the file, whether its close prompt is up.
#[derive(Debug, Clone, Copy)]
struct Told {
    hold: Hold,
    saves: u64,
    question: bool,
}

/// A window that says what it holds, as the test sets it ([`Told`]); asked to close with
/// something unsaved, it puts up its prompt.
struct Holding(Arc<Mutex<Told>>);

impl Window for Holding {
    fn ui(&mut self, _ui: &mut egui::Ui) {}
    fn ready(&self) -> bool {
        true
    }
    fn hold(&mut self) -> Hold {
        self.0.lock().unwrap().hold
    }
    fn saves_landed(&self) -> u64 {
        self.0.lock().unwrap().saves
    }
    fn view_state(&mut self) -> crate::view_keys::State {
        match self.0.lock().unwrap().question {
            true => crate::view_keys::State::Question,
            false => crate::view_keys::State::Free,
        }
    }
    fn close_requested(&mut self) -> bool {
        let mut told = self.0.lock().unwrap();
        told.question = told.hold != Hold::Clean;
        !told.question
    }
}

/// The viewer on `photo.png` with a [`Holding`] window as its view 1 (the image).
fn holding(fx: &mut Fx) -> Arc<Mutex<Told>> {
    let told = Arc::new(Mutex::new(Told {
        hold: Hold::Clean,
        saves: 0,
        question: false,
    }));
    let abs = format!("{}/photo.png", fx.root);
    let means = view_set::Means {
        path: abs.clone(),
        name: "photo.png".to_string(),
        title: abs.clone(),
        connect: local_connect(),
        label: None,
        in_place: false,
    };
    let opened = view_set::OpenedAs {
        kind: crate::kind::Kind::Image,
        stat: LocalFs::new().stat(&abs).unwrap(),
        undecodable: None,
    };
    let ctx = fx.drv.ctx.clone();
    let views = view_set::open(&ctx, &opened, means, None);
    let made = view_set::Made {
        embedded: Embedded {
            window: Box::new(Holding(told.clone())),
            settle: Box::new(|_| Some(Outcome::default())),
        },
        hold: None,
        switch: None,
    };
    fx.b.view(&ctx, made, abs, false, views);
    fx.drv.idle(&mut fx.b, 2);
    told
}

fn set(fx: &Fx) -> &crate::views::ViewSet {
    &fx.b.viewer.as_ref().expect("a viewer").views.set
}

/// D2: a Save running makes a switch wait (`saving…`); once it lands the switch goes on; one
/// that does not land keeps the view, and `saving…` is not said any more.
#[test]
fn va2_a_running_save_makes_the_switch_wait() {
    for landed in [true, false] {
        let mut fx = fixture();
        let told = holding(&mut fx);
        told.lock().unwrap().hold = Hold::Saving;
        digit(&mut fx, 2);
        assert!(shows(&fx, "saving…"), "{landed}: the switch waits");
        assert!(on_strip(&fx, "1 Image"), "{landed}: still view 1");
        {
            let mut told = told.lock().unwrap();
            told.hold = if landed { Hold::Clean } else { Hold::Unsaved };
            told.saves += u64::from(landed);
        }
        // The pass that sees it first (the browser was ready before it: the Save is the
        // window's), then the view made.
        fx.drv.idle(&mut fx.b, 1);
        fx.settle();
        fx.drv.idle(&mut fx.b, 2);
        assert!(!shows(&fx, "saving…"), "{landed}: said no more");
        if landed {
            assert!(hex_row(&fx, "89 50 4E 47"), "landed: Hex");
        } else {
            assert!(on_strip(&fx, "1 Image"), "not landed: the view stays");
        }
    }
}

/// D25 by the receipt, not by sampling what the window holds: a Save that lands in the pass
/// before a view key (Unsaved → Clean, and the key in the very next input) makes the hidden Hex
/// stale — it is made again, never shown as it was.
#[test]
fn d25_a_save_landing_right_before_a_view_key_makes_the_hidden_view_stale() {
    let mut fx = fixture();
    let told = holding(&mut fx);
    digit(&mut fx, 2);
    digit(&mut fx, 1);
    let views = &fx.b.viewer.as_ref().unwrap().views;
    assert!(
        views.set.is_made(View::Hex) && views.makes == 1,
        "Hex made, hidden"
    );
    told.lock().unwrap().hold = Hold::Unsaved;
    fx.drv.idle(&mut fx.b, 1);
    {
        let mut told = told.lock().unwrap();
        (told.hold, told.saves) = (Hold::Clean, 1);
    }
    digit(&mut fx, 2);
    assert_eq!(
        fx.b.viewer.as_ref().unwrap().views.makes,
        2,
        "the stale Hex was closed and made again"
    );
    assert!(hex_row(&fx, "89 50 4E 47"));
}

/// A Save from the prompt that lands while the window stays open (its notes on screen; an
/// image's earlier Save found landed) is a landed Save: the hidden views are stale.
#[test]
fn d25_a_prompts_save_that_lands_and_keeps_the_window_open_makes_the_hidden_views_stale() {
    let mut fx = fixture();
    let told = holding(&mut fx);
    digit(&mut fx, 2);
    digit(&mut fx, 1);
    assert!(set(&fx).is_made(View::Hex));
    told.lock().unwrap().hold = Hold::Unsaved;
    digit(&mut fx, 2);
    assert!(set(&fx).asking(), "the prompt");
    // Its Save runs, the prompt gone…
    {
        let mut told = told.lock().unwrap();
        (told.hold, told.question) = (Hold::Saving, false);
    }
    fx.drv.idle(&mut fx.b, 2);
    assert!(set(&fx).asking(), "the switch waits for the prompt's Save");
    // …and lands; the window stays open.
    {
        let mut told = told.lock().unwrap();
        (told.hold, told.saves) = (Hold::Clean, 1);
    }
    fx.drv.idle(&mut fx.b, 2);
    assert!(!set(&fx).is_made(View::Hex), "the hidden Hex is stale");
    assert!(!set(&fx).asking());
    assert_eq!(set(&fx).on_screen(), Some(View::Image), "the view stays");
}

/// `enc=` (and `at=`) are the first text file's: an image opened first leaves them to it.
#[test]
fn enc_is_kept_past_an_image_for_the_first_text_file() {
    let mut fx = fixture();
    fx.b.scripted(&crate::gui::State::parse("enc=UTF-16LE").unwrap());
    fx.open("photo.png");
    fx.key(Key::Escape);
    fx.open("notes.txt");
    assert!(shows(&fx, "UTF-16LE"), "the text reads in enc=");
}

/// VA-4: the image's zoom survives 1 → 2 → 1 (D1: the view kept hidden, as it was) — the
/// zoom `A` gives, 100 % (D12: `1` is view 1 here).
#[test]
fn va4_the_images_zoom_survives_a_switch() {
    let mut fx = fixture();
    fx.open("photo.png");
    let fitted = fx.drv.pictures()[0];
    fx.key(Key::A);
    fx.settle();
    let zoomed = fx.drv.pictures()[0];
    assert_ne!(zoomed, fitted, "A: 100 %");
    digit(&mut fx, 2);
    digit(&mut fx, 1);
    assert_eq!(fx.drv.pictures()[0], zoomed, "the zoom kept");
}

/// A hidden media view reads nothing (06 §5, 08 §5): a remote sound's stream sends no poll
/// while Hex is on screen, and polls again once the sound is shown.
#[test]
fn a_hidden_remote_sounds_stream_does_not_poll() {
    let tools = match crate::ffmpeg::resolve() {
        Ok(t) => t,
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping: {e}");
            return;
        }
    };
    let meter = Meter::new(u64::MAX, u64::MAX);
    let connect = meter.connect(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>));
    let mut fx = fixture_with(Some("box"), connect, None);
    let wav = format!("{}/tone.wav", fx.root);
    let ok = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
        .arg(&wav)
        .status()
        .unwrap()
        .success();
    assert!(ok, "fixture");
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.key(Key::F9);
    fx.open("tone.wav");
    let sound = meter.opened.lock().unwrap()[0].0;
    let polls = |meter: &Meter| {
        meter
            .polls
            .lock()
            .unwrap()
            .iter()
            .filter(|&&id| id == sound)
            .count()
    };
    // Polls every 2 s (06 §5): over 5 s on screen, some.
    let watch = |fx: &mut Fx| {
        let deadline = std::time::Instant::now() + Duration::from_millis(5_000);
        while std::time::Instant::now() < deadline {
            fx.drv.idle(&mut fx.b, 1);
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let before = polls(&meter);
    watch(&mut fx);
    assert!(polls(&meter) > before, "the control: on screen it polls");
    digit(&mut fx, 2);
    let hidden = polls(&meter);
    watch(&mut fx);
    assert_eq!(polls(&meter), hidden, "hidden, it polls not");
    digit(&mut fx, 1);
    watch(&mut fx);
    assert!(polls(&meter) > hidden, "shown again, it polls");
}

/// D22 with D21: a rebuild in another LF unit that Esc stops keeps the encoding the text had —
/// and the set hears of the one the text reads in, never of the one only asked for.
#[test]
fn f8_a_rebuild_esc_stops_leaves_the_sets_encoding_as_it_reads() {
    let mut fx = fixture();
    // Large enough that its index pass in UTF-16LE still runs a pass later.
    let line = "abcdefghijklmnopqrstuvwxyz 0123456789\n";
    std::fs::write(
        format!("{}/big.txt", fx.root),
        line.repeat((64 << 20) / line.len()),
    )
    .unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.open("big.txt");
    fx.key(Key::F8);
    fx.key(Key::ArrowDown);
    let press = |key| Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    fx.drv
        .frame(&mut fx.b, vec![press(Key::Enter)], Modifiers::NONE);
    fx.drv
        .frame(&mut fx.b, vec![press(Key::Escape)], Modifiers::NONE);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    assert!(
        fx.b.viewer.is_some(),
        "Esc stopped the rebuild, it did not leave"
    );
    let viewer = fx.b.viewer.as_ref().unwrap();
    let reads = viewer.embedded.window.encoding().map(|(e, _)| e.label());
    assert_eq!(reads, Some("UTF-8"), "kept UTF-8");
    assert_ne!(
        viewer.views.set.encoding(),
        Some("UTF-16LE"),
        "never told UTF-16LE"
    );
}

use crate::views::{Hold, View};
use std::collections::HashSet;
