//! The editor's keys (12 §14 EK-1, EB-6's arming half; §5.5's letters): [`decide`] as a table
//! over (armed state, press, Shift known, backend), [`Keys`] press by press, and a sweep proving
//! that no press outside §5.3's list saves. The lines are 12 §14's, as written there.

use super::*;
use Command::*;

const NONE: Modifiers = Modifiers::NONE;

/// Ctrl as a terminal and a Linux window send it: `ctrl` and `command` (10 T26).
fn ctrl() -> Modifiers {
    Modifiers {
        ctrl: true,
        command: true,
        ..NONE
    }
}

fn ctrl_shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..ctrl()
    }
}

fn ctrl_alt() -> Modifiers {
    Modifiers {
        alt: true,
        ..ctrl()
    }
}

/// ⌘ in a Mac window.
fn mac_cmd() -> Modifiers {
    Modifiers {
        mac_cmd: true,
        command: true,
        ..NONE
    }
}

/// ⌃ in a Mac window: no `command`.
fn mac_ctrl() -> Modifiers {
    Modifiers { ctrl: true, ..NONE }
}

fn shift() -> Modifiers {
    Modifiers::SHIFT
}

fn alt() -> Modifiers {
    Modifiers::ALT
}

fn k(key: Key) -> Press {
    Press::key(key, NONE)
}

fn km(key: Key, modifiers: Modifiers) -> Press {
    Press::key(key, modifiers)
}

fn text(t: &str) -> Press {
    Press::Text(t.to_string())
}

/// 12 §14 EK-1's lines, as written there.
const SAVE_OVER: &str =
    "Save over notes.txt? F2 or a click on [Save] saves · C saves a copy · any other key cancels";
const CANCELLED: &str = "not saved — F2, Ctrl+K S or a click saves";
const CTRL_5: &str = "this key arrived as Ctrl+5 (Ctrl+] in some terminals) and saved nothing — F2, F5, Ctrl+K S, a click on 2Save, or F10 F S saves";

const BACKENDS: [Backend; 2] = [Backend::Window, Backend::Terminal];
const ARMINGS: [Armed; 3] = [Armed::None, Armed::Block, Armed::Save];

/// A terminal's keys with `^S` armed by Ctrl+S, its line checked.
fn armed_save() -> Keys {
    let mut keys = Keys::new(Backend::Terminal);
    let command = keys.press(&km(Key::S, ctrl()));
    assert_eq!(command, ArmSave);
    assert_eq!(command.line("notes.txt").as_deref(), Some(SAVE_OVER));
    assert_eq!(keys.mark("notes.txt").as_deref(), Some(SAVE_OVER));
    assert_eq!(keys.armed(), Armed::Save);
    keys
}

fn armed_block(backend: Backend) -> Keys {
    let mut keys = Keys::new(backend);
    assert_eq!(keys.press(&km(Key::K, ctrl())), ArmBlock);
    assert_eq!(keys.mark("x").as_deref(), Some("^K"));
    keys
}

#[test]
fn ek1_f2_and_f5_save_from_a_key_and_from_a_row_click() {
    for backend in BACKENDS {
        for shift_known in [false, true] {
            for armed in [Armed::None, Armed::Block] {
                for press in [k(Key::F2), k(Key::F5), Press::Slot(2), Press::Slot(5)] {
                    assert_eq!(
                        decide(armed, &press, shift_known, backend),
                        (Save, Armed::None),
                        "{backend:?} {shift_known} {armed:?} {press:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn ek1_window_ctrl_s_saves_and_shift_ctrl_s_saves_a_copy() {
    for command in [ctrl(), mac_cmd()] {
        let with = |m: fn(&mut Modifiers)| {
            let mut mods = command;
            m(&mut mods);
            km(Key::S, mods)
        };
        let decided = |press: &Press| decide(Armed::None, press, true, Backend::Window).0;
        assert_eq!(decided(&km(Key::S, command)), Save, "{command:?}");
        assert_eq!(decided(&with(|m| m.shift = true)), SaveCopy);
        assert_eq!(decided(&with(|m| m.alt = true)), SaveAs);
    }
    // A Mac's ⌃+S is no Ctrl/⌘ row: nothing.
    assert_eq!(
        decide(Armed::None, &km(Key::S, mac_ctrl()), true, Backend::Window).0,
        Nothing
    );
    // Window: Shift known from the start, never forgotten.
    let mut keys = Keys::new(Backend::Window);
    assert!(keys.shift_known());
    for press in [Press::Focus, Press::Resize, Press::Focus] {
        assert_eq!(keys.press(&press), Nothing);
        assert!(keys.shift_known());
    }
    assert_eq!(keys.press(&km(Key::S, ctrl())), Save);
}

#[test]
fn ek1_terminal_before_shift_is_known_ctrl_s_arms() {
    // F2, the row's 2Save and the prompt's [Save] confirm; C saves a copy.
    for confirm in [k(Key::F2), Press::Slot(2), Press::PromptSave] {
        let mut keys = armed_save();
        assert_eq!(keys.press(&confirm), Save, "{confirm:?}");
        assert_eq!(keys.armed(), Armed::None);
    }
    for copy in [k(Key::C), km(Key::C, shift()), text("c"), text("ㅊ")] {
        let mut keys = armed_save();
        assert_eq!(keys.press(&copy), SaveCopy, "{copy:?}");
        assert_eq!(keys.armed(), Armed::None);
    }
    // Every other key cancels: nothing saved, nothing typed, the line.
    let others = [
        (k(Key::S), Some("s")),
        (km(Key::S, shift()), Some("S")),
        (k(Key::X), Some("x")),
        (k(Key::Enter), None),
        (k(Key::Space), Some(" ")),
        (k(Key::Escape), None),
        (k(Key::F5), None),
        (km(Key::S, ctrl()), None),
        (Press::Slot(5), None),
        (k(Key::F9), None),
        (Press::Paste("save".to_string()), None),
    ];
    for (press, typed) in others {
        let mut keys = armed_save();
        let command = keys.press(&press);
        assert_eq!(command, Cancel, "{press:?}");
        assert_eq!(command.line("notes.txt").as_deref(), Some(CANCELLED));
        assert_eq!(keys.armed(), Armed::None);
        // The text a source sends after the key is dropped with it: never typed. Sent alone, a
        // letter cancels too.
        if let Some(t) = typed {
            assert_eq!(
                keys.press(&text(t)),
                Nothing,
                "{press:?}: its text is not typed"
            );
            let mut keys = armed_save();
            assert_eq!(keys.press(&text(t)), Cancel, "{t:?}");
        }
    }
    // The modifiers' own keys and a quiet input are no key: still armed.
    let mut keys = armed_save();
    for press in [k(Key::ShiftLeft), k(Key::ControlLeft), k(Key::Fn)] {
        assert_eq!(keys.press(&press), Nothing);
        assert_eq!(keys.armed(), Armed::Save);
    }
}

#[test]
fn ek1_terminal_shift_known_saves_at_once_until_a_focus_report_or_a_resize() {
    for forget in [Press::Focus, Press::Resize] {
        let mut keys = Keys::new(Backend::Terminal);
        assert!(!keys.shift_known());
        // Shift with Ctrl on an arrow proves nothing (a legacy terminal sends `CSI 1;6A`).
        keys.press(&km(Key::ArrowUp, ctrl_shift()));
        assert!(!keys.shift_known());
        // Kitty's `CSI 122;6u`: Ctrl+Shift+Z with Shift — Redo, and Shift is known.
        assert_eq!(keys.press(&km(Key::Z, ctrl_shift())), Redo);
        assert!(keys.shift_known());
        assert_eq!(keys.press(&km(Key::S, ctrl())), Save);
        assert_eq!(keys.press(&km(Key::S, ctrl_shift())), SaveCopy);
        assert_eq!(keys.press(&forget), Nothing);
        assert!(!keys.shift_known(), "{forget:?}");
        assert_eq!(keys.press(&km(Key::S, ctrl())), ArmSave, "{forget:?}");
    }
    // The chord that proves Shift is its own proof: Shift+Ctrl+S with Shift saves a copy.
    let mut keys = Keys::new(Backend::Terminal);
    assert_eq!(keys.press(&km(Key::S, ctrl_shift())), SaveCopy);
    assert!(keys.shift_known());
}

#[test]
fn ek1_a_digit_the_hook_left_saves_nothing_and_says_so() {
    for backend in BACKENDS {
        let command = decide(Armed::None, &km(Key::Num5, ctrl()), false, backend).0;
        assert_eq!(
            command,
            NoSave {
                held: "Ctrl",
                digit: 5
            }
        );
        assert_eq!(command.line("x").as_deref(), Some(CTRL_5), "{backend:?}");
        let two = decide(Armed::None, &km(Key::Num2, ctrl()), false, backend).0;
        assert_eq!(
            two.line("x").as_deref(),
            Some("this key arrived as Ctrl+2 and saved nothing — F2, F5, Ctrl+K S, a click on 2Save, or F10 F S saves")
        );
    }
    let cmd5 = decide(
        Armed::None,
        &km(Key::Num5, mac_cmd()),
        true,
        Backend::Window,
    )
    .0;
    assert_eq!(
        cmd5,
        NoSave {
            held: "⌘",
            digit: 5
        }
    );
    // The TUI's Alt+digits are F-keys (10 T7); Alt+2 and +5 stay digits. In a Mac window
    // Option+5 types a character.
    for digit in [Key::Num2, Key::Num5] {
        assert!(matches!(
            decide(Armed::None, &km(digit, alt()), false, Backend::Terminal).0,
            NoSave { held: "Alt", .. }
        ));
        assert_eq!(
            decide(Armed::None, &km(digit, alt()), true, Backend::Window).0,
            Typing
        );
    }
    // While ^K waits a digit is any other key; while ^S waits it cancels.
    assert_eq!(
        decide(
            Armed::Block,
            &km(Key::Num5, ctrl()),
            false,
            Backend::Terminal
        ),
        (Disarm, Armed::None)
    );
    assert_eq!(
        decide(
            Armed::Save,
            &km(Key::Num5, ctrl()),
            false,
            Backend::Terminal
        ),
        (Cancel, Armed::None)
    );
}

#[test]
fn ek1_precedence_while_armed() {
    // ^K: F2, F5, a click on 2Save or 5Save save, and disarm (E34).
    for backend in BACKENDS {
        for press in [k(Key::F2), k(Key::F5), Press::Slot(2), Press::Slot(5)] {
            let mut keys = armed_block(backend);
            assert_eq!(keys.press(&press), Save, "{backend:?} {press:?}");
            assert_eq!(keys.armed(), Armed::None);
        }
    }
    // ^S: F5 and 5Save cancel; only F2, 2Save and [Save] save.
    for press in [k(Key::F5), Press::Slot(5)] {
        let mut keys = armed_save();
        assert_eq!(keys.press(&press), Cancel, "{press:?}");
    }
    // A Shift+F2 or Ctrl+F2 is not F2 pressed as itself.
    for press in [km(Key::F2, shift()), km(Key::F2, ctrl())] {
        let mut keys = armed_save();
        assert_eq!(keys.press(&press), Cancel, "{press:?}");
    }
}

/// EB-6's arming half: Ctrl+K, then each of B K C V Y H I U L T S, with and without Ctrl — by
/// key, by the physical key, as a terminal's text, and as the 2-set jamo (§5.5).
#[test]
fn eb6_ctrl_k_then_each_letter_with_and_without_ctrl() {
    let letters = [
        ('B', Key::B, 'ㅠ', Block(BlockOp::Begin)),
        ('K', Key::K, 'ㅏ', Block(BlockOp::End)),
        ('C', Key::C, 'ㅊ', Block(BlockOp::Copy)),
        ('V', Key::V, 'ㅍ', Block(BlockOp::Move)),
        ('Y', Key::Y, 'ㅛ', Block(BlockOp::Delete)),
        ('H', Key::H, 'ㅗ', Block(BlockOp::Hide)),
        ('I', Key::I, 'ㅑ', Block(BlockOp::Indent)),
        ('U', Key::U, 'ㅕ', Block(BlockOp::Unindent)),
        ('L', Key::L, 'ㅣ', Block(BlockOp::MarkLine)),
        ('T', Key::T, 'ㅅ', Block(BlockOp::MarkWord)),
        ('S', Key::S, 'ㄴ', Save),
    ];
    for backend in BACKENDS {
        for (l, key, jamo, want) in letters.clone() {
            let lower = l.to_ascii_lowercase().to_string();
            let presses = [
                k(key),
                km(key, shift()),
                km(key, ctrl()),
                km(key, mac_cmd()),
                km(key, mac_ctrl()),
                text(&lower),
                text(&jamo.to_string()),
                // The window's physical key decides (the input method is off): on another
                // layout the logical key is another letter.
                Press::Key {
                    key: Key::Q,
                    physical: Some(key),
                    modifiers: NONE,
                },
            ];
            for press in presses {
                let mut keys = armed_block(backend);
                let command = keys.press(&press);
                assert_eq!(command, want, "{backend:?} {l} {press:?}");
                assert_eq!(keys.armed(), Armed::None);
                // The letter's text, which follows a key pressed without Ctrl/⌘: not typed.
                if matches!(press, Press::Key { modifiers, .. } if !modifiers.command && !modifiers.ctrl)
                {
                    assert_eq!(
                        keys.press(&text(&lower)),
                        Nothing,
                        "{press:?}: nothing typed"
                    );
                }
            }
        }
        // Esc or any other key disarms: dropped, nothing typed.
        for (press, typed) in [
            (k(Key::Escape), None),
            (k(Key::X), Some("x")),
            (text("x"), None),
            (k(Key::Enter), None),
            (k(Key::F9), None),
            (k(Key::Space), Some(" ")),
        ] {
            let mut keys = armed_block(backend);
            let command = keys.press(&press);
            assert_eq!(command, Disarm, "{press:?}");
            assert_eq!(keys.armed(), Armed::None);
            if let Some(t) = typed {
                assert_eq!(keys.press(&text(t)), Nothing, "{press:?}: its text dropped");
            }
        }
        // No timeout: a thousand inputs that are no key leave it armed.
        let mut keys = armed_block(backend);
        for i in 0..1000 {
            let quiet = [Press::Focus, Press::Resize, k(Key::ShiftLeft)][i % 3].clone();
            assert_eq!(keys.press(&quiet), Nothing);
        }
        assert_eq!(keys.armed(), Armed::Block);
        assert_eq!(keys.press(&k(Key::B)), Block(BlockOp::Begin));
    }
}

#[test]
fn the_2_set_jamo_are_their_keys_letters() {
    assert_eq!(letter("ㄴ"), Some('S'));
    assert_eq!(letter("ㅊ"), Some('C'));
    assert_eq!(letter("ㅕ"), Some('U'));
    assert_eq!(letter("s"), Some('S'));
    assert_eq!(letter("한"), None);
    assert_eq!(letter("ss"), None);
    // Every letter has one unshifted jamo, and no jamo two letters.
    let mut seen: Vec<char> = JAMO.iter().map(|&(_, l)| l).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 26);
}

#[test]
fn e7_the_first_undo_of_a_terminal_session_says_redo_is_in_the_menu() {
    let mut keys = Keys::new(Backend::Terminal);
    let first = keys.press(&km(Key::Z, ctrl()));
    assert_eq!(first, Undo { hint: true });
    assert_eq!(
        first.line("x").as_deref(),
        Some("Undo — this terminal may send Ctrl+Shift+Z as Ctrl+Z; Redo: F10 E R")
    );
    assert_eq!(keys.press(&km(Key::Z, ctrl())), Undo { hint: false });
    assert_eq!(keys.press(&km(Key::Backspace, alt())), Undo { hint: false });
    let mut window = Keys::new(Backend::Window);
    assert_eq!(window.press(&km(Key::Z, ctrl())), Undo { hint: false });
}

/// A character key's text follows it: typed when the key types, dropped when it was taken —
/// one text only, and none after a key that sends none (Ctrl held; Alt in a terminal).
#[test]
fn a_key_and_its_typed_text_go_together() {
    let mut keys = Keys::new(Backend::Terminal);
    assert_eq!(keys.press(&k(Key::A)), Typing);
    assert_eq!(keys.press(&text("a")), Type("a".to_string()));
    assert_eq!(keys.press(&text("한")), Type("한".to_string()));
    assert_eq!(keys.press(&k(Key::Num5)), Typing, "a plain digit is typed");
    assert_eq!(keys.press(&text("5")), Type("5".to_string()));
    for backend in BACKENDS {
        let mut keys = armed_block(backend);
        assert_eq!(keys.press(&k(Key::X)), Disarm);
        assert_eq!(
            keys.press(&text("x")),
            Nothing,
            "{backend:?}: x is not typed"
        );
        assert_eq!(
            keys.press(&text("y")),
            Type("y".to_string()),
            "only its own text"
        );
        // Ctrl+Y sends no text: the next text is typed.
        assert_eq!(keys.press(&km(Key::Y, ctrl())), DeleteLine);
        assert_eq!(keys.press(&text("z")), Type("z".to_string()));
    }
    // The TUI's Alt+G sends no text either; a Mac window's Option+G types its own.
    let mut keys = Keys::new(Backend::Terminal);
    assert_eq!(keys.press(&km(Key::G, alt())), Switch);
    assert_eq!(keys.press(&text("g")), Type("g".to_string()));
    let mut keys = Keys::new(Backend::Window);
    assert_eq!(keys.press(&km(Key::G, alt())), Typing);
    assert_eq!(keys.press(&text("©")), Type("©".to_string()));
}

/// §5.1, row by row, unarmed, as each arrives in the window (Shift known) and in a terminal.
#[test]
fn ek1_every_row_of_5_1_is_its_command() {
    use Motion::*;
    let rows: Vec<(Press, Command)> = vec![
        (k(Key::ArrowLeft), Move(Left, false)),
        (k(Key::ArrowRight), Move(Right, false)),
        (k(Key::ArrowUp), Move(Up, false)),
        (k(Key::ArrowDown), Move(Down, false)),
        (k(Key::Home), Move(Home, false)),
        (k(Key::End), Move(End, false)),
        (k(Key::PageUp), Move(PageUp, false)),
        (k(Key::PageDown), Move(PageDown, false)),
        (km(Key::ArrowLeft, ctrl()), Move(WordLeft, false)),
        (km(Key::ArrowRight, ctrl()), Move(WordRight, false)),
        (km(Key::Home, ctrl()), Move(FileStart, false)),
        (km(Key::End, ctrl()), Move(FileEnd, false)),
        (km(Key::PageUp, ctrl()), Move(FileStart, false)),
        (km(Key::PageDown, ctrl()), Move(FileEnd, false)),
        (km(Key::ArrowLeft, shift()), Move(Left, true)),
        (km(Key::ArrowDown, shift()), Move(Down, true)),
        (km(Key::End, shift()), Move(End, true)),
        (km(Key::PageDown, shift()), Move(PageDown, true)),
        (km(Key::ArrowRight, ctrl_shift()), Move(WordRight, true)),
        (km(Key::Home, ctrl_shift()), Move(FileStart, true)),
        (k(Key::Insert), ToggleInsert),
        (k(Key::Enter), NewLine),
        (k(Key::Tab), Tab),
        (km(Key::Tab, shift()), Unindent),
        (k(Key::Delete), DeleteRight),
        (k(Key::Backspace), DeleteLeft),
        (km(Key::Y, ctrl()), DeleteLine),
        (km(Key::T, ctrl()), DeleteWord),
        (km(Key::N, ctrl()), InsertLine),
        (km(Key::Y, mac_ctrl()), DeleteLine),
        (km(Key::K, ctrl()), ArmBlock),
        (km(Key::Insert, ctrl()), Copy),
        (km(Key::Delete, shift()), Cut),
        (km(Key::Delete, ctrl()), DeleteBlock),
        (Press::Copy, Copy),
        (Press::Cut, Cut),
        (Press::Paste("p".to_string()), Paste("p".to_string())),
        (km(Key::S, ctrl_alt()), SaveAs),
        (km(Key::Z, ctrl()), Undo { hint: false }),
        (km(Key::Backspace, alt()), Undo { hint: false }),
        (km(Key::Z, ctrl_shift()), Redo),
        (
            km(
                Key::Backspace,
                Modifiers {
                    shift: true,
                    alt: true,
                    ..NONE
                },
            ),
            Redo,
        ),
        (km(Key::F, ctrl()), Find),
        (km(Key::L, ctrl()), FindAgain),
        (km(Key::L, mac_ctrl()), FindAgain),
        (km(Key::G, ctrl()), GoTo),
        (km(Key::A, ctrl()), SelectAll),
        (km(Key::R, ctrl()), Reread),
        (k(Key::F1), Help),
        (k(Key::F2), Save),
        (k(Key::F3), Leave),
        (k(Key::Escape), Leave),
        (k(Key::F4), View),
        (km(Key::F4, shift()), External),
        (k(Key::F5), Save),
        (k(Key::F6), Panel),
        (k(Key::F7), SaveAs),
        (k(Key::F8), Nothing),
        (k(Key::F9), Nothing),
        (k(Key::F10), Menu),
        (k(Key::F12), Switch),
        (km(Key::Q, ctrl()), Close),
        (km(Key::W, ctrl()), Close),
        (Press::Slot(1), Help),
        (Press::Slot(3), Leave),
        (Press::Slot(4), View),
        (Press::Slot(6), Panel),
        (Press::Slot(7), SaveAs),
        (Press::Slot(8), Nothing),
        (Press::Slot(10), Menu),
        (k(Key::A), Typing),
        (km(Key::A, shift()), Typing),
        (text("가"), Type("가".to_string())),
        (km(Key::E, ctrl()), Nothing),
        (km(Key::H, ctrl()), Nothing),
        (Press::PromptSave, Nothing),
    ];
    for backend in BACKENDS {
        for (press, want) in &rows {
            let armed = if *want == ArmBlock {
                Armed::Block
            } else {
                Armed::None
            };
            assert_eq!(
                decide(Armed::None, press, true, backend),
                (want.clone(), armed),
                "{backend:?} {press:?}"
            );
        }
    }
    // The window's clipboard by key, and the terminal's (E9).
    let window = |p: &Press| decide(Armed::None, p, true, Backend::Window).0;
    let terminal = |p: &Press| decide(Armed::None, p, true, Backend::Terminal).0;
    assert_eq!(window(&km(Key::C, ctrl())), Copy);
    assert_eq!(window(&km(Key::X, ctrl())), Cut);
    assert_eq!(window(&km(Key::V, ctrl())), PasteClipboard);
    assert_eq!(window(&km(Key::Insert, shift())), PasteClipboard);
    assert_eq!(terminal(&km(Key::V, ctrl())), TerminalPaste);
    assert_eq!(terminal(&km(Key::Insert, shift())), TerminalPaste);
    assert_eq!(
        TerminalPaste.line("x").as_deref(),
        Some("the terminal's paste: ⌘V / Ctrl+Shift+V")
    );
    // Alt+G is the TUI's switch (10 T33); in a Mac window Option+G types.
    assert_eq!(terminal(&km(Key::G, alt())), Switch);
    assert_eq!(window(&km(Key::G, alt())), Typing);
}

/// EK-1's last clause: no press outside §5.3's list saves — every key egui names, under every
/// set of modifiers, every arming, Shift known or not, in both backends; and every other input.
#[test]
fn ek1_no_press_outside_the_list_saves() {
    let mods = [
        NONE,
        shift(),
        alt(),
        ctrl(),
        ctrl_shift(),
        ctrl_alt(),
        mac_cmd(),
        mac_ctrl(),
        Modifiers {
            shift: true,
            alt: true,
            ..NONE
        },
        Modifiers {
            shift: true,
            ..mac_cmd()
        },
        Modifiers {
            ctrl: true,
            ..mac_cmd()
        },
    ];
    let mut presses: Vec<Press> = Vec::new();
    for &key in Key::ALL {
        for m in mods {
            presses.push(km(key, m));
        }
    }
    presses.extend((0..=11).map(Press::Slot));
    presses.extend([
        Press::PromptSave,
        Press::Focus,
        Press::Resize,
        Press::Copy,
        Press::Cut,
        Press::Paste("s".to_string()),
        text("s"),
        text("ㄴ"),
        text("S"),
    ]);
    // §5.3's list, spelled as the presses themselves (not through the code under test).
    let f2 = km(Key::F2, NONE);
    let f5 = km(Key::F5, NONE);
    // Ctrl+K's S comes with or without Ctrl/⌘/⌃ and Shift; never with Alt, nor with ⌘ and ⌃
    // together, which no row has.
    let block_s = [
        NONE,
        shift(),
        ctrl(),
        ctrl_shift(),
        mac_cmd(),
        mac_ctrl(),
        Modifiers {
            shift: true,
            ..mac_cmd()
        },
    ]
    .map(|m| km(Key::S, m));
    let mut saves = 0;
    for backend in BACKENDS {
        for shift_known in [false, true] {
            for armed in ARMINGS {
                for press in &presses {
                    let (command, _) = decide(armed, press, shift_known, backend);
                    if command != Save {
                        continue;
                    }
                    saves += 1;
                    let ok = match armed {
                        Armed::Save => [&f2, &Press::Slot(2), &Press::PromptSave].contains(&press),
                        Armed::Block => {
                            [&f2, &f5, &Press::Slot(2), &Press::Slot(5)].contains(&press)
                                || block_s.contains(press)
                                || [text("s"), text("S"), text("ㄴ")].contains(press)
                        }
                        Armed::None => {
                            [&f2, &f5, &Press::Slot(2), &Press::Slot(5)].contains(&press)
                                || ((*press == km(Key::S, ctrl())
                                    || *press == km(Key::S, mac_cmd()))
                                    && (backend == Backend::Window || shift_known))
                        }
                    };
                    assert!(
                        ok,
                        "{backend:?} shift_known={shift_known} {armed:?} {press:?} saved"
                    );
                }
            }
        }
    }
    assert!(saves > 0, "the positive controls saved");
}
