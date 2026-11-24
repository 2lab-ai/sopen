//! The view keys' tests (08 §11 VA-2, VA-3, §12): §4's table as state × armed × press → command,
//! and inputs as egui-winit 0.37 sends them — a key event with its physical key, then its typed
//! text, in one input (`lib.rs:1128-1180`) — since a key sent alone (`testkit::Driver::key`) hides
//! the press that switches and types at once.

use super::*;
use Command::{
    Arm, Choose, Disarm, Drop, DropText, EndEditing, Leave, Pass, ReadOnly, StartEditing,
};

const NONE: Modifiers = Modifiers::NONE;

const VIEWING: State = State::Hex(Hex {
    editing: false,
    read_only: false,
    saving: false,
});
const EDITING: State = State::Hex(Hex {
    editing: true,
    read_only: false,
    saving: false,
});
const READ_ONLY: State = State::Hex(Hex {
    editing: false,
    read_only: true,
    saving: false,
});
const VIEWING_SAVING: State = State::Hex(Hex {
    editing: false,
    read_only: false,
    saving: true,
});
const EDITING_SAVING: State = State::Hex(Hex {
    editing: true,
    read_only: false,
    saving: true,
});

const STATES: [State; 8] = [
    State::Free,
    VIEWING,
    READ_ONLY,
    VIEWING_SAVING,
    EDITING,
    EDITING_SAVING,
    State::Field,
    State::Question,
];

const PRESSES: [Press; 9] = [
    Press::Digit(1),
    Press::Digit(2),
    Press::Digit(3),
    Press::F9,
    Press::Esc,
    Press::F3,
    Press::Enter,
    Press::Insert,
    Press::Other,
];

/// One press as egui-winit 0.37 sends it: the key (logical, else physical), then its typed text
/// unless Cmd/Ctrl is held.
fn down(key: Key, physical: Option<Key>, modifiers: Modifiers, text: Option<&str>) -> Vec<Event> {
    let mut events = vec![Event::Key {
        key,
        physical_key: physical,
        pressed: true,
        repeat: false,
        modifiers,
    }];
    let command = modifiers.ctrl || modifiers.command || modifiers.mac_cmd;
    if let Some(text) = text.filter(|_| !command) {
        events.push(Event::Text(text.to_string()));
    }
    events
}

/// A plain press of `key`, its physical key the same.
fn tap(key: Key, text: Option<&str>) -> Vec<Event> {
    down(key, Some(key), NONE, text)
}

fn num(n: u8) -> Key {
    [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4][usize::from(n)]
}

/// The plain digit `n`, with the text it types.
fn digit(n: u8) -> Vec<Event> {
    tap(num(n), Some(&n.to_string()))
}

fn up(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: Some(key),
        pressed: false,
        repeat: false,
        modifiers: NONE,
    }
}

/// The events of one press of `press`: a key and, for a printable key, its text.
fn events_of(press: Press) -> (Key, Vec<Event>) {
    let key = match press {
        Press::Digit(n) => num(n),
        Press::F9 => Key::F9,
        Press::Esc => Key::Escape,
        Press::F3 => Key::F3,
        Press::Enter => Key::Enter,
        Press::Insert => Key::Insert,
        Press::Other => Key::A,
    };
    let text = match press {
        Press::Digit(n) => Some(n.to_string()),
        Press::Other => Some("a".to_string()),
        _ => None,
    };
    (key, tap(key, text.as_deref()))
}

fn cat(parts: &[&[Event]]) -> Vec<Event> {
    parts.concat()
}

/// The GUI's view keys: egui-winit sends releases; nothing held at open.
fn gui() -> ViewKeys {
    ViewKeys::new(true, [])
}

/// A terminal's: no releases, so nothing is ever held.
fn terminal() -> ViewKeys {
    ViewKeys::new(false, [])
}

/// A terminal's key: no physical key, never a release.
fn term(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: NONE,
    }
}

/// View keys with F9 armed and let go.
fn armed() -> ViewKeys {
    let mut keys = gui();
    assert_eq!(
        keys.route(State::Free, &tap(Key::F9, None)),
        [Arm(Ending::Nothing)]
    );
    assert_eq!(keys.route(State::Free, &[up(Key::F9)]), [Drop]);
    assert!(keys.armed());
    keys
}

#[test]
fn each_state_is_one_of_the_four_rows() {
    let rows = STATES.map(State::row);
    assert_eq!(
        rows,
        [
            Row::Free,
            Row::Free,
            Row::Free,
            Row::Free,
            Row::DigitsAreData,
            Row::DigitsAreData,
            Row::Field,
            Row::Question,
        ]
    );
}

/// §4's table, unarmed: each state × `PRESSES` (1, 2, 3, F9, Esc, F3, Enter, Insert, any other).
#[rustfmt::skip]
const UNARMED: [(State, [Command; 9]); 8] = [
    (State::Free,     [Choose(1), Choose(2), Choose(3), Arm(Ending::Nothing), Leave,      Leave, Pass,         Pass,         Pass]),
    (VIEWING,         [Choose(1), Choose(2), Choose(3), Arm(Ending::Nothing), Leave,      Leave, StartEditing, StartEditing, Pass]),
    (READ_ONLY,       [Choose(1), Choose(2), Choose(3), Arm(Ending::Nothing), Leave,      Leave, ReadOnly,     ReadOnly,     Pass]),
    // Esc stops the running Save first (D35).
    (VIEWING_SAVING,  [Choose(1), Choose(2), Choose(3), Arm(Ending::Nothing), Pass,       Leave, StartEditing, StartEditing, Pass]),
    (EDITING,         [Pass,      Pass,      Pass,      Arm(Ending::Editing), EndEditing, Leave, Pass,         Pass,         Pass]),
    (EDITING_SAVING,  [Pass,      Pass,      Pass,      Arm(Ending::Editing), Pass,       Leave, Pass,         Pass,         Pass]),
    // A field withholds F3 from the viewer's rows; its own Esc closes it.
    (State::Field,    [Pass,      Pass,      Pass,      Arm(Ending::Field),   Pass,       Pass,  Pass,         Pass,         Pass]),
    (State::Question, [Pass; 9]),
];

/// §4's armed row (D4): 1, 2, 3 choose; Esc or F9 disarm; any other key disarms and is dropped —
/// in every state but a question, which keeps its keyboard and disarms (D5).
#[rustfmt::skip]
const ARMED: [Command; 9] = [Choose(1), Choose(2), Choose(3), Disarm, Disarm, Drop, Drop, Drop, Drop];

#[test]
fn the_table_unarmed() {
    assert_eq!(UNARMED.map(|(state, _)| state), STATES);
    for (state, row) in UNARMED {
        for (press, want) in PRESSES.into_iter().zip(row) {
            let armed_after = matches!(want, Arm(_));
            assert_eq!(
                decide(state, false, press),
                (want, armed_after),
                "{state:?} {press:?}"
            );
        }
    }
}

#[test]
fn the_table_armed() {
    for state in STATES {
        let row = if state == State::Question {
            [Pass; 9]
        } else {
            ARMED
        };
        for (press, want) in PRESSES.into_iter().zip(row) {
            assert_eq!(
                decide(state, true, press),
                (want, false),
                "{state:?} {press:?}"
            );
        }
    }
}

/// One press, one action (§4): every press, in every state, armed or not, sent as its key, its
/// typed text and its release in one input — the text and the release go where the key went, a
/// taken press leaves nothing for the view, a passed one leaves all of it.
#[test]
fn a_press_is_passed_or_taken_with_its_text_and_release() {
    for state in STATES {
        for is_armed in [false, true] {
            for press in PRESSES {
                let mut keys = if is_armed { armed() } else { gui() };
                let (key, pressed) = events_of(press);
                let input = cat(&[&pressed, &[up(key)]]);
                let label = format!("{state:?} armed={is_armed} {press:?}");
                let commands = keys.clone().route(state, &input);
                let first = commands[0];
                assert_eq!(
                    first,
                    decide(state, is_armed, press).0,
                    "{label}: the table decides"
                );
                let (text, release) = if first.taken() {
                    (DropText, Drop)
                } else {
                    (Pass, Pass)
                };
                let mut want = vec![first];
                if pressed.len() == 2 {
                    want.push(text);
                }
                want.push(release);
                assert_eq!(commands, want, "{label}");
                let mut left = input.clone();
                let acts = keys.take(state, &mut left);
                assert!(acts.len() <= 1, "{label}: {acts:?}");
                if first.taken() {
                    assert!(left.is_empty(), "{label}: {left:?}");
                } else {
                    assert_eq!(left, input, "{label}");
                }
            }
        }
    }
}

/// D3: the plain digit, by physical key; its typed text is not the digit on every layout.
#[test]
fn a_view_key_is_the_plain_digit_by_physical_key() {
    let choose = |events: &[Event]| gui().route(State::Free, events);
    // AZERTY: the key printed 1 types `&`; egui has no `&` key, so the logical key is the
    // physical one.
    assert_eq!(
        choose(&down(Key::Num1, Some(Key::Num1), NONE, Some("&"))),
        [Choose(1), DropText]
    );
    // The physical key first: what the Cmd/Ctrl+digit aliases read (`gui/mod.rs:866`).
    assert_eq!(
        choose(&down(Key::Num2, Some(Key::Num1), NONE, Some("2"))),
        [Choose(1), DropText]
    );
    assert_eq!(
        choose(&down(Key::Num1, Some(Key::Num4), NONE, Some("1"))),
        [Pass, Pass],
        "the key at 4 is 4"
    );
    // A physical key that is no digit: the logical key.
    assert_eq!(
        choose(&down(Key::Num3, Some(Key::Q), NONE, Some("3"))),
        [Choose(3), DropText]
    );
    // No physical key: the test kit's `Driver::key`, a terminal binding's key events.
    assert_eq!(
        choose(&down(Key::Num2, None, NONE, Some("2"))),
        [Choose(2), DropText]
    );
    // 4…9 and 0 are no view keys.
    for n in [0, 4] {
        assert_eq!(choose(&digit(n)), [Pass, Pass], "{n}");
    }
}

/// D3, VA-3: Shift, Alt and Cmd/Ctrl with a digit are not view keys; their key and text pass.
#[test]
fn a_digit_with_a_modifier_is_no_view_key() {
    let m = |f: fn(&mut Modifiers)| {
        let mut mods = NONE;
        f(&mut mods);
        mods
    };
    let shapes = [
        // US Shift+1 types `!`; AZERTY's Shift+& types `1`.
        (Key::Exclamationmark, m(|m| m.shift = true), "!"),
        (Key::Num1, m(|m| m.shift = true), "1"),
        // macOS Option+1 composes `¡`.
        (Key::Num1, m(|m| m.alt = true), "¡"),
        // Cmd/Ctrl as the test kit sends it, ⌘ on macOS, Ctrl elsewhere, a Mac's ⌃.
        (Key::Num1, m(|m| m.command = true), "1"),
        (
            Key::Num1,
            m(|m| {
                m.mac_cmd = true;
                m.command = true
            }),
            "1",
        ),
        (
            Key::Num1,
            m(|m| {
                m.ctrl = true;
                m.command = true
            }),
            "1",
        ),
        (Key::Num1, m(|m| m.ctrl = true), "1"),
    ];
    for state in [State::Free, VIEWING] {
        for (key, mods, text) in shapes {
            let input = down(key, Some(Key::Num1), mods, Some(text));
            let mut left = input.clone();
            let acts = gui().take(state, &mut left);
            assert!(acts.is_empty(), "{state:?} {mods:?}: {acts:?}");
            assert_eq!(left, input, "{state:?} {mods:?}");
        }
    }
    // After the shell's aliases Cmd/Ctrl+1, +2, +3 arrive as F1, F2, F3 (04 §6.1): F1 and F2 are
    // the view's, F3 leaves; none chooses a view.
    let alias = |n: Key| Event::Key {
        key: n,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: NONE,
    };
    let mut keys = gui();
    assert_eq!(keys.route(State::Free, &[alias(Key::F1)]), [Pass]);
    assert_eq!(keys.route(State::Free, &[alias(Key::F2)]), [Pass]);
    assert_eq!(keys.route(State::Free, &[alias(Key::F3)]), [Leave]);
    // Cmd/Ctrl+9 and a click on slot 9 are F9.
    assert_eq!(
        keys.route(State::Free, &[alias(Key::F9)]),
        [Arm(Ending::Nothing)]
    );
}

/// VA-2 at the model: hex viewing, editing and a switch, each press with its typed text.
#[test]
fn hex_viewing_editing_and_a_switch() {
    let mut keys = gui();
    let mut run = |state: State, input: Vec<Event>| {
        let mut left = input;
        let acts = keys.take(state, &mut left);
        (acts, left)
    };
    // An image, 2, 1: the image again, no byte written (the 1 never reaches the hex grid).
    assert_eq!(run(State::Free, digit(2)), (vec![Choose(2)], vec![]));
    assert_eq!(run(VIEWING, vec![up(Key::Num2)]), (vec![], vec![]));
    assert_eq!(run(VIEWING, digit(1)), (vec![Choose(1)], vec![]));
    assert_eq!(run(State::Free, vec![up(Key::Num1)]), (vec![], vec![]));
    // In Hex, Enter: editing. 4, 1 type 0x41: both reach the grid, no switch.
    let enter = cat(&[&tap(Key::Enter, None), &[up(Key::Enter)]]);
    assert_eq!(run(VIEWING, enter), (vec![StartEditing], vec![]));
    let typed = cat(&[&digit(4), &[up(Key::Num4)], &digit(1), &[up(Key::Num1)]]);
    assert_eq!(run(EDITING, typed.clone()), (vec![], typed));
    // F9 then 1: the close prompt (the view set's), from the view F9 left viewing.
    assert_eq!(run(EDITING, tap(Key::F9, None)).0, [Arm(Ending::Editing)]);
    assert_eq!(run(VIEWING, vec![up(Key::F9)]), (vec![], vec![]));
    assert_eq!(run(VIEWING, digit(1)), (vec![Choose(1)], vec![]));
    // Cancel: Hex, viewing, the edit kept; plain 1 asks again.
    assert_eq!(run(State::Question, vec![up(Key::Num1)]), (vec![], vec![]));
    assert_eq!(run(VIEWING, digit(1)), (vec![Choose(1)], vec![]));
    assert_eq!(run(State::Question, vec![up(Key::Num1)]), (vec![], vec![]));
    // Viewing during a Save, 1: chosen (the set says `saving…`); editing, held back as today.
    assert_eq!(run(VIEWING_SAVING, digit(1)).0, [Choose(1)]);
    assert!(run(VIEWING_SAVING, vec![up(Key::Num1)]).1.is_empty());
    assert_eq!(run(EDITING_SAVING, digit(1)), (vec![], digit(1)));
    assert_eq!(run(EDITING_SAVING, vec![up(Key::Num1)]).1, [up(Key::Num1)]);
    // Editing, Esc: viewing; a second Esc leaves (the browser's row asks the close prompt).
    let esc = || cat(&[&tap(Key::Escape, None), &[up(Key::Escape)]]);
    assert_eq!(run(EDITING, esc()), (vec![EndEditing], vec![]));
    assert_eq!(run(VIEWING, esc()), (vec![Leave], esc()));
    // A revision's Hex: Enter and Insert say it is read-only.
    assert_eq!(
        run(READ_ONLY, tap(Key::Enter, None)),
        (vec![ReadOnly], vec![])
    );
    assert_eq!(
        run(READ_ONLY, tap(Key::Insert, None)),
        (vec![ReadOnly], vec![])
    );
    // An image with a crop, 2: chosen (the view set asks the prompt).
    assert_eq!(run(State::Free, digit(2)).0, [Choose(2)]);
}

/// VA-3 at the model.
#[test]
fn f9_fields_questions_and_leave_wins() {
    // F9 then x while editing hex: nothing typed, editing ended, no switch.
    let mut keys = gui();
    assert_eq!(
        keys.route(EDITING, &tap(Key::F9, None)),
        [Arm(Ending::Editing)]
    );
    let x = tap(Key::X, Some("x"));
    let mut left = x.clone();
    assert!(keys.take(VIEWING, &mut left).is_empty());
    assert!(left.is_empty(), "{left:?}");
    assert!(!keys.armed());
    // Go to open: 1 goes into the field; F9 closes it and arms; then 2 chooses.
    let mut keys = gui();
    assert_eq!(keys.route(State::Field, &digit(1)), [Pass, Pass]);
    assert_eq!(
        keys.route(State::Field, &cat(&[&tap(Key::F9, None), &digit(2)])),
        [Arm(Ending::Field), Choose(2), DropText]
    );
    // The close prompt, Save As or the encoding list up: 1, 2, 3 and F9 belong to it.
    let mut keys = armed();
    assert!(keys.route(State::Question, &[]).is_empty());
    assert!(!keys.armed(), "a question disarms");
    for input in [digit(1), digit(2), digit(3), tap(Key::F9, None)] {
        let mut left = input.clone();
        assert!(keys.take(State::Question, &mut left).is_empty());
        assert_eq!(left, input);
        assert!(!keys.armed());
    }
    // 2 and Esc in one input: the viewer leaves, no view is chosen — in either order, F3 too.
    let esc = tap(Key::Escape, None);
    let f3 = tap(Key::F3, None);
    for (input, want) in [
        (cat(&[&digit(2), &esc]), vec![Drop, DropText, Leave]),
        (cat(&[&esc, &digit(2)]), vec![Leave, Drop, DropText]),
        (cat(&[&digit(2), &f3]), vec![Drop, DropText, Leave]),
    ] {
        for state in [State::Free, VIEWING] {
            let mut left = input.clone();
            let mut keys = gui();
            assert_eq!(keys.clone().route(state, &input), want, "{state:?}");
            assert_eq!(keys.take(state, &mut left), [Leave], "{state:?}");
            assert_eq!(left.len(), 1, "only the leaving key is left: {left:?}");
        }
    }
    // Armed, the same input chooses and then leaves: Leave still wins, and disarms.
    let mut keys = armed();
    assert_eq!(
        keys.route(EDITING, &cat(&[&digit(1), &esc])),
        [Drop, DropText, Leave]
    );
    assert!(!keys.armed());
}

/// D4: Esc and F9 disarm, taken — an Esc that disarms does not leave.
#[test]
fn esc_or_f9_disarms_and_is_taken() {
    for (key, state) in [(Key::Escape, State::Free), (Key::F9, VIEWING)] {
        let mut keys = armed();
        let mut left = tap(key, None);
        assert_eq!(keys.take(state, &mut left), [Disarm]);
        assert!(left.is_empty());
        assert!(!keys.armed());
        assert_eq!(keys.route(state, &[up(key)]), [Drop]);
        // Disarmed, a digit chooses as plain.
        assert_eq!(keys.route(state, &digit(3)), [Choose(3), DropText]);
    }
}

/// D33: an armed F9 lasts one key, with no timeout: inputs with no key — none at all, the
/// pointer, the modifiers, the window's focus — leave it armed.
#[test]
fn armed_has_no_timeout() {
    let mut keys = armed();
    for _ in 0..1000 {
        assert!(keys.route(State::Free, &[]).is_empty());
    }
    let quiet = [
        Event::PointerMoved(eframe::egui::Pos2::new(3.0, 4.0)),
        Event::ModifiersChanged(Modifiers::SHIFT),
        Event::WindowFocused(false),
        Event::WindowFocused(true),
    ];
    assert_eq!(keys.route(State::Free, &quiet), [Pass; 4]);
    assert!(keys.armed());
    assert_eq!(keys.route(State::Free, &digit(3)), [Choose(3), DropText]);
    assert!(!keys.armed());
}

/// egui-winit sends every press `repeat: false` (`lib.rs:1159`); in the input hook a held key's
/// auto-repeat is a further press of a key already down. It goes where its press went.
#[test]
fn a_held_key_repeats_where_its_press_went() {
    // F9 held: armed once, not toggled.
    let mut keys = gui();
    assert_eq!(
        keys.route(State::Free, &tap(Key::F9, None)),
        [Arm(Ending::Nothing)]
    );
    for _ in 0..3 {
        assert_eq!(keys.route(State::Free, &tap(Key::F9, None)), [Drop]);
        assert!(keys.armed());
    }
    assert_eq!(keys.route(State::Free, &[up(Key::F9)]), [Drop]);
    assert_eq!(keys.route(State::Free, &tap(Key::F9, None)), [Disarm]);
    // Esc held in an editing hex view: editing ends, and the repeat does not leave.
    let mut keys = gui();
    assert_eq!(keys.route(EDITING, &tap(Key::Escape, None)), [EndEditing]);
    assert_eq!(keys.route(VIEWING, &tap(Key::Escape, None)), [Drop]);
    assert_eq!(keys.route(VIEWING, &[up(Key::Escape)]), [Drop]);
    // 2 held: one switch, its repeats and their text dropped in the view it chose.
    let mut keys = gui();
    assert_eq!(keys.route(State::Free, &digit(2)), [Choose(2), DropText]);
    assert_eq!(keys.route(VIEWING, &digit(2)), [Drop, DropText]);
    // 4 held while editing: typed, as today; once F9 arms, its repeats are never typed.
    let mut keys = gui();
    assert_eq!(keys.route(EDITING, &digit(4)), [Pass, Pass]);
    assert_eq!(keys.route(EDITING, &digit(4)), [Pass, Pass]);
    assert_eq!(
        keys.route(EDITING, &tap(Key::F9, None)),
        [Arm(Ending::Editing)]
    );
    assert_eq!(keys.route(VIEWING, &digit(4)), [Drop, DropText]);
    assert!(keys.armed(), "a repeat is not the next key");
    assert_eq!(keys.route(VIEWING, &[up(Key::Num4)]), [Pass]);
    assert_eq!(keys.route(VIEWING, &digit(1)), [Choose(1), DropText]);
    // A press an integration marks as a repeat decides nothing.
    let repeat = Event::Key {
        key: Key::Enter,
        physical_key: Some(Key::Enter),
        pressed: true,
        repeat: true,
        modifiers: NONE,
    };
    assert_eq!(gui().route(VIEWING, &[repeat]), [Pass]);
}

/// The Enter that opened the viewer, held: its repeats do not start editing the hex view.
#[test]
fn keys_down_at_open_are_passed() {
    let mut keys = ViewKeys::new(true, [Key::Enter]);
    assert_eq!(keys.route(VIEWING, &tap(Key::Enter, None)), [Pass]);
    assert_eq!(keys.route(VIEWING, &[up(Key::Enter)]), [Pass]);
    assert_eq!(keys.route(VIEWING, &tap(Key::Enter, None)), [StartEditing]);
}

/// A taken press's release is taken too: the media window pastes at a V release whose press it
/// never saw (`media.rs:3851-3864`), and egui-winit sends Cmd/Ctrl+V as `Paste`, with only its
/// release as a key.
#[test]
fn a_taken_press_takes_its_release() {
    let mut keys = armed();
    assert_eq!(
        keys.route(State::Free, &tap(Key::V, Some("v"))),
        [Drop, DropText]
    );
    assert_eq!(keys.route(State::Free, &[up(Key::V)]), [Drop]);
    let mut keys = armed();
    let paste = Event::Paste("p".to_string());
    assert_eq!(
        keys.route(State::Free, std::slice::from_ref(&paste)),
        [Drop]
    );
    assert!(!keys.armed(), "a clipboard shortcut is any other key");
    assert_eq!(keys.route(State::Free, &[up(Key::V)]), [Drop]);
    // Unarmed, the shortcut and its release are the view's.
    let mut keys = gui();
    assert_eq!(keys.route(State::Free, &[paste, up(Key::V)]), [Pass, Pass]);
    for event in [Event::Copy, Event::Cut] {
        let mut keys = armed();
        assert_eq!(keys.route(State::Free, &[event]), [Drop]);
    }
    // Cmd/Ctrl+V held sends `Paste` again for each repeat (`testkit.rs`, `Driver::shortcut`): a
    // repeat is not a fresh press, so the shortcut armed's key dropped is not pasted after all.
    let mut keys = armed();
    let paste = || Event::Paste("p".to_string());
    assert_eq!(keys.route(State::Free, &[paste()]), [Drop]);
    assert_eq!(keys.route(State::Free, &[paste(), paste()]), [Drop, Drop]);
    assert_eq!(keys.route(State::Free, &[up(Key::V)]), [Drop]);
    assert_eq!(
        keys.route(State::Free, &[paste(), up(Key::V)]),
        [Pass, Pass]
    );
}

/// A key is held by its physical key first, as the digits are read: a release whose logical key
/// differs from its press's (Shift let go or pressed in between) still lets it go.
#[test]
fn a_release_matches_its_press_by_physical_key() {
    let shifted_up = Event::Key {
        key: Key::Exclamationmark,
        physical_key: Some(Key::Num1),
        pressed: false,
        repeat: false,
        modifiers: Modifiers::SHIFT,
    };
    let mut keys = gui();
    assert_eq!(keys.route(State::Free, &digit(1)), [Choose(1), DropText]);
    assert_eq!(keys.route(State::Free, &[shifted_up]), [Drop]);
    assert_eq!(keys.route(State::Free, &digit(1)), [Choose(1), DropText]);
    // Held by the physical key, a press under Shift is the same key's repeat.
    let mut keys = gui();
    assert_eq!(keys.route(State::Free, &digit(1)), [Choose(1), DropText]);
    let shifted = down(
        Key::Exclamationmark,
        Some(Key::Num1),
        Modifiers::SHIFT,
        Some("!"),
    );
    assert_eq!(keys.route(VIEWING, &shifted), [Drop, DropText]);
}

/// Losing the window's focus forgets the keys held (winit may never send their releases).
#[test]
fn focus_loss_forgets_held_keys() {
    let mut keys = gui();
    assert_eq!(keys.route(State::Free, &digit(2)), [Choose(2), DropText]);
    assert_eq!(keys.route(VIEWING, &[Event::WindowFocused(false)]), [Pass]);
    assert_eq!(keys.route(VIEWING, &digit(2)), [Choose(2), DropText]);
}

/// §9: a terminal sends the character alone — a press of its own; committed IME text too.
#[test]
fn typed_text_alone_is_a_press() {
    let text = |t: &str| Event::Text(t.to_string());
    let mut keys = gui();
    assert_eq!(keys.route(State::Free, &[text("1")]), [Choose(1)]);
    assert_eq!(keys.route(EDITING, &[text("1")]), [Pass]);
    assert_eq!(keys.route(State::Field, &[text("2")]), [Pass]);
    assert_eq!(keys.route(State::Question, &[text("3")]), [Pass]);
    assert_eq!(keys.route(State::Free, &[text("12")]), [Pass]);
    assert_eq!(keys.route(State::Free, &[text("x")]), [Pass]);
    // F9 then 2, as a terminal sends them.
    assert_eq!(
        keys.route(EDITING, &[tap(Key::F9, None)[0].clone(), text("2")]),
        [Arm(Ending::Editing), Choose(2)]
    );
    let mut keys = armed();
    assert_eq!(keys.route(State::Free, &[text("x")]), [Drop]);
    assert!(!keys.armed());
    let commit = Event::Ime(ImeEvent::Commit("3".to_string()));
    assert_eq!(gui().route(State::Free, &[commit]), [Choose(3)]);
}

/// Inside one input the hex view's own mode changes take effect at once; once a view is chosen
/// or a field closed, the rest is dropped (but Esc and F3, which leave); once leaving, all of it.
#[test]
fn one_input_after_each_command() {
    let route = |state: State, input: &[Event]| gui().route(state, input);
    let enter = tap(Key::Enter, None);
    let esc = tap(Key::Escape, None);
    let f9 = tap(Key::F9, None);
    let x = tap(Key::X, Some("x"));
    // Enter, then 4: typed into the view now editing.
    assert_eq!(
        route(VIEWING, &cat(&[&enter, &digit(4)])),
        [StartEditing, Pass, Pass]
    );
    // Esc, then 1: viewing now, so 1 chooses.
    assert_eq!(
        route(EDITING, &cat(&[&esc, &digit(1)])),
        [EndEditing, Choose(1), DropText]
    );
    // F9 in an editing view, Esc, then 1: viewing, unarmed — 1 chooses.
    assert_eq!(
        route(EDITING, &cat(&[&f9, &esc, &digit(1)])),
        [Arm(Ending::Editing), Disarm, Choose(1), DropText]
    );
    // After a switch: never typed into a view it was not meant for, never a second switch.
    assert_eq!(
        route(State::Free, &cat(&[&digit(2), &x, &digit(1), &f9])),
        [Choose(2), DropText, Drop, DropText, Drop, DropText, Drop]
    );
    // A field closed by F9 and disarmed: the view under it is not known yet.
    assert_eq!(
        route(State::Field, &cat(&[&f9, &esc, &digit(1), &x])),
        [Arm(Ending::Field), Disarm, Drop, DropText, Drop, DropText]
    );
    // …but Esc, once let go and pressed again, and F3 leave.
    assert_eq!(
        route(State::Field, &cat(&[&f9, &esc, &[up(Key::Escape)], &esc])),
        [Arm(Ending::Field), Disarm, Drop, Leave]
    );
    assert_eq!(
        route(State::Field, &cat(&[&f9, &esc, &tap(Key::F3, None)])),
        [Arm(Ending::Field), Disarm, Leave]
    );
    // Esc pressed again without being let go is its repeat: dropped, as the disarm was taken.
    assert_eq!(
        route(State::Field, &cat(&[&f9, &esc, &esc])),
        [Arm(Ending::Field), Disarm, Drop]
    );
    // Leaving: the rest is dropped.
    assert_eq!(
        route(State::Free, &cat(&[&esc, &x, &esc])),
        [Leave, Drop, DropText, Drop]
    );
}

/// A terminal sends no releases (§9): with `releases` false nothing is held, so every press is
/// decided, each in its own input. From a source that does send releases, the same presses with
/// none between them are repeats — the GUI's contract, which a terminal would break.
#[test]
fn a_terminal_without_releases_decides_every_press() {
    let text = |t: &str| Event::Text(t.to_string());
    let mut keys = terminal();
    assert_eq!(
        keys.route(State::Free, &[term(Key::F9)]),
        [Arm(Ending::Nothing)]
    );
    assert_eq!(keys.route(State::Free, &[text("2")]), [Choose(2)]);
    assert_eq!(
        keys.route(VIEWING, &[term(Key::F9)]),
        [Arm(Ending::Nothing)]
    );
    assert_eq!(keys.route(VIEWING, &[term(Key::Escape)]), [Disarm]);
    assert_eq!(keys.route(VIEWING, &[term(Key::Escape)]), [Leave]);
    let mut keys = terminal();
    assert_eq!(keys.route(VIEWING, &[term(Key::Enter)]), [StartEditing]);
    assert_eq!(keys.route(EDITING, &[term(Key::Escape)]), [EndEditing]);
    assert_eq!(keys.route(VIEWING, &[term(Key::Enter)]), [StartEditing]);
    let mut keys = gui();
    assert_eq!(keys.route(VIEWING, &[term(Key::Enter)]), [StartEditing]);
    assert_eq!(keys.route(EDITING, &[term(Key::Escape)]), [EndEditing]);
    assert_eq!(keys.route(VIEWING, &[term(Key::Enter)]), [Drop]);
    // A key that types nothing, then a character, in one batch: the character is its own press.
    assert_eq!(
        terminal().route(State::Free, &[term(Key::ArrowDown), text("2")]),
        [Pass, Choose(2)]
    );
    assert_eq!(
        terminal().route(State::Free, &[term(Key::F9), text("2")]),
        [Arm(Ending::Nothing), Choose(2)]
    );
}

/// The numpad with NumLock off: its logical key names a key (winit's xkb keymap) while
/// egui-winit gives it the physical key Num0…Num9 (`lib.rs:1720-1722`) — never a view key; its 0
/// is Insert. With NumLock on it is the digit, as the top row's.
#[test]
fn the_numpad_with_numlock_off_is_its_named_key() {
    for (key, physical) in [
        (Key::End, Key::Num1),
        (Key::ArrowDown, Key::Num2),
        (Key::PageDown, Key::Num3),
    ] {
        let press = down(key, Some(physical), NONE, None);
        assert_eq!(gui().route(State::Free, &press), [Pass], "{key:?}");
        assert_eq!(armed().route(State::Free, &press), [Drop], "{key:?}");
    }
    assert_eq!(
        gui().route(VIEWING, &down(Key::Insert, Some(Key::Num0), NONE, None)),
        [StartEditing]
    );
    assert_eq!(
        gui().route(
            State::Free,
            &down(Key::Num2, Some(Key::Num2), NONE, Some("2"))
        ),
        [Choose(2), DropText]
    );
}

/// The keys that stand for a character: space, egui's 18 punctuation keys, the ISO key, 10
/// digits, 26 letters — egui's groups, so a key egui adds shows here.
#[test]
fn character_keys_are_space_punctuation_digits_and_letters() {
    let characters: Vec<Key> = Key::ALL
        .iter()
        .copied()
        .filter(|k| is_character(*k))
        .collect();
    assert_eq!(characters.len(), 1 + 18 + 1 + 10 + 26, "{characters:?}");
    for key in [
        Key::End,
        Key::Insert,
        Key::Enter,
        Key::Tab,
        Key::F9,
        Key::ShiftLeft,
    ] {
        assert!(!is_character(key), "{key:?}");
    }
}

/// egui 0.37 sends a modifier's own key as a press (`Key::ShiftLeft` …): part of a chord, it
/// decides nothing — a Mac's fn held for F9 does not disarm before the F9 arrives.
#[test]
fn a_modifier_key_is_no_press() {
    let mut keys = armed();
    for key in [
        Key::ShiftLeft,
        Key::ControlRight,
        Key::AltLeft,
        Key::SuperLeft,
        Key::Fn,
    ] {
        let input = cat(&[&tap(key, None), &[up(key)]]);
        assert_eq!(keys.route(State::Free, &input), [Pass, Pass], "{key:?}");
        assert!(keys.armed(), "{key:?}");
    }
    assert_eq!(
        keys.route(
            State::Free,
            &cat(&[&tap(Key::Fn, None), &tap(Key::F9, None)])
        ),
        [Pass, Disarm]
    );
}

/// A hardware Cut, Copy or Paste key sends the shortcut's event and lets go as itself: its
/// release lets go of the letter the shortcut is held as, so that letter's next press is fresh.
#[test]
fn a_hardware_clipboard_key_lets_its_letter_go() {
    for (event, key, letter) in [
        (Event::Paste("p".to_string()), Key::Paste, Key::V),
        (Event::Cut, Key::Cut, Key::X),
        (Event::Copy, Key::Copy, Key::C),
    ] {
        let mut keys = armed();
        assert_eq!(keys.route(State::Free, &[event]), [Drop], "{key:?}");
        assert_eq!(keys.route(State::Free, &[up(key)]), [Drop], "{key:?}");
        assert_eq!(
            keys.route(State::Free, &tap(letter, Some("v"))),
            [Pass, Pass],
            "{key:?}"
        );
    }
}

/// A field's own Esc closes it, as F9 does: the rest of that input meets a view not known yet.
#[test]
fn a_fields_own_esc_ends_the_input() {
    let esc = tap(Key::Escape, None);
    let x = tap(Key::X, Some("x"));
    assert_eq!(
        gui().route(State::Field, &cat(&[&esc, &digit(1), &x])),
        [Pass, Drop, DropText, Drop, DropText]
    );
    assert_eq!(
        gui().route(State::Field, &cat(&[&esc, &[up(Key::Escape)], &esc])),
        [Pass, Pass, Leave]
    );
}

/// A view chosen otherwise — a click on the strip — disarms: the next key is the view's again.
#[test]
fn disarm_ends_an_armed_f9() {
    let mut keys = armed();
    keys.disarm();
    assert!(!keys.armed());
    assert_eq!(keys.route(VIEWING, &tap(Key::X, Some("x"))), [Pass, Pass]);
    assert_eq!(keys.route(VIEWING, &digit(3)), [Choose(3), DropText]);
}
