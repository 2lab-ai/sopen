//! The editor's keys (`.prd/12-editor.md` §5.1, §5.3–§5.5; ED-ops): every row of §5.1 as a
//! [`Command`], with the two armings — Ctrl+K's block prefix and, in a terminal until Shift is
//! known, Ctrl+S's `^S` — and E34's precedence while either waits. Screen-free (12 §15).
//!
//! - [`decide`] is the table over (armed state, press, Shift known, backend). Every Save a key
//!   starts comes out of it (12 §5.3), and only from: F2 or F5 pressed as a key or clicked on the
//!   F-key row, except while `^S` waits, where F5 cancels; Ctrl/⌘+S in the window, and in a
//!   terminal once Shift is known (E31); F2, its row slot or the prompt's `[Save]` while `^S`
//!   waits; Ctrl+K then S. A digit the shell's hook left a digit (E5: Ctrl/⌘ or, in the TUI,
//!   Alt with 2 or 5) saves nothing and says so.
//! - [`Keys`] runs it press by press, keeping the arming, Shift known — seen once a Ctrl/⌘+Shift
//!   chord on a letter key arrives with Shift, forgotten at every focus report and resize in a
//!   terminal, always known in the window (E31) — and the undo hint's once a session (E7).
//! - **Modifiers match exactly** (04 §3 rule 1). "Ctrl/⌘" is egui's `command` (⌘ in a Mac
//!   window, Ctrl elsewhere, 10 T26); the WordStar rows (Ctrl+K, Y, T, N, L) and the Ctrl rows of
//!   keys that type nothing (arrows, Home, End, PgUp, PgDn, Ins, Del) also match a Mac's ⌃,
//!   which arrives without `command` (§5.1).
//! - **The letter an arming waits for** comes with or without Ctrl, by the physical key where
//!   the window gives one (the input method off meanwhile, §5.5), and in the TUI as the 2-set
//!   (두벌식) jamo on its key ([`letter`]).
//!
//! The binding (ED-layout) gives each key press, typed text, clipboard event, row click and
//! focus report or resize as a [`Press`], in the order the source sends them. A key egui follows
//! with its typed text decides [`Command::Typing`] when that text is to be typed; any other
//! command takes the key, and [`Keys`] drops the text that follows it, never typed (08 §4's
//! rule).

use eframe::egui::{Key, Modifiers};

/// The backend the editor's keys arrive from (10 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// An xtools window: Shift always arrives (E31).
    Window,
    /// The TUI: Shift may be lost with Ctrl under the legacy encoding (§5.2).
    Terminal,
}

/// One input the editor's keys decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    /// A key pressed: egui's logical key, its physical key where the window gives one, the
    /// modifiers as egui reports them.
    Key {
        key: Key,
        physical: Option<Key>,
        modifiers: Modifiers,
    },
    /// Typed text that no key press came with: a terminal's character, committed IME text.
    Text(String),
    /// egui's clipboard events: Cmd/Ctrl+C, +X in every backend, the window's Cmd/Ctrl+V and the
    /// terminal's own paste (§5.2).
    Copy,
    Cut,
    Paste(String),
    /// A click on the F-key row's slot 1–10 (`1Help 2Save … 10Menu`, §4).
    Slot(u8),
    /// A click on the `^S` prompt's `[Save]` (§5.4).
    PromptSave,
    /// A focus report (`WindowFocused`, gained or lost).
    Focus,
    /// The terminal's size changed.
    Resize,
}

impl Press {
    /// A key pressed with `modifiers` and no physical key (a terminal's, the tests').
    pub fn key(key: Key, modifiers: Modifiers) -> Press {
        Press::Key {
            key,
            physical: None,
            modifiers,
        }
    }
}

/// What waits for the next key (§5.4). No timeout (08 D33).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Armed {
    #[default]
    None,
    /// Ctrl+K: the status row shows `^K`.
    Block,
    /// `^S` in a terminal until Shift is known (E31): the status row asks [`save_over`].
    Save,
}

/// A caret move (§5.1). With Shift it extends the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    WordLeft,
    WordRight,
    /// Ctrl+Home, Ctrl+PgUp: after a BOM (§7.2).
    FileStart,
    /// Ctrl+End, Ctrl+PgDn.
    FileEnd,
}

/// Ctrl+K's second key (§5.1), but S, which saves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockOp {
    Begin,
    End,
    Copy,
    Move,
    Delete,
    Hide,
    Indent,
    Unindent,
    MarkLine,
    MarkWord,
}

/// What a press does in the editor: every row of §5.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// A character key with no Ctrl/⌘: not a command — its typed text follows and is decided
    /// as [`Press::Text`].
    Typing,
    /// A key with no row in the editor (F8, F9 — the view keys', Ctrl+letters not taken), or an
    /// input that is no key (a focus report, a modifier's own key): nothing, and an arming
    /// keeps waiting.
    Nothing,
    /// Text typed at the caret.
    Type(String),
    /// The caret, by characters, lines, words, pages; `true`: Shift extends the block.
    Move(Motion, bool),
    /// Ins: Insert ↔ Overwrite.
    ToggleInsert,
    /// Enter: a new line, indented as the line above while `Indent` is on.
    NewLine,
    Tab,
    /// Shift+Tab: unindent the line or the block.
    Unindent,
    /// Del: delete right, or the block shown.
    DeleteRight,
    /// Backspace: delete left, or the block shown.
    DeleteLeft,
    /// Ctrl+Y.
    DeleteLine,
    /// Ctrl+T: the word right.
    DeleteWord,
    /// Ctrl+N.
    InsertLine,
    Block(BlockOp),
    /// Ctrl+Del.
    DeleteBlock,
    /// Ctrl+Ins, Ctrl/⌘+C.
    Copy,
    /// Shift+Del, Ctrl/⌘+X.
    Cut,
    /// The clipboard's text, pasted (E36, §7.2).
    Paste(String),
    /// Shift+Ins in the window, which egui sends as a key: the binding reads the clipboard and
    /// pastes it.
    PasteClipboard,
    /// Ctrl+V or Shift+Ins in a terminal, which brings no text (E9): the status row names the
    /// terminal's own paste.
    TerminalPaste,
    Save,
    /// Save copy (07 §6): a new file, never over this one.
    SaveCopy,
    /// Save As… (07 §7).
    SaveAs,
    /// Ctrl/⌘+Z, Alt+Backspace. `hint`: the first one of a terminal session before Shift is
    /// known says that Ctrl+Shift+Z may arrive as it (E7).
    Undo {
        hint: bool,
    },
    Redo,
    Find,
    /// Ctrl+L.
    FindAgain,
    /// Ctrl/⌘+G.
    GoTo,
    SelectAll,
    /// Ctrl/⌘+R: read the file again (asks first, §9.6).
    Reread,
    Help,
    /// F3, Esc: leave the editor (§6.4).
    Leave,
    /// F4: the Enter viewer on this file, at the caret.
    View,
    /// Shift+F4: VS Code / Zed (E3).
    External,
    /// F6: the keyboard to the column, and back.
    Panel,
    /// F10.
    Menu,
    /// F12; Alt+G in a terminal (10 §7).
    Switch,
    /// Ctrl/⌘+Q, +W: sopen's close (E8).
    Close,
    /// Ctrl+K: armed, `^K` on the status row.
    ArmBlock,
    /// Ctrl+S in a terminal before Shift is known: armed, the prompt [`save_over`] (E31).
    ArmSave,
    /// `^K` disarmed by Esc or any other key: dropped, never typed.
    Disarm,
    /// `^S` cancelled by any key but its two confirmations: nothing saved, nothing typed,
    /// [`NOT_SAVED`].
    Cancel,
    /// A digit the hook left as itself (E5): `held` names its modifiers. Nothing saved, the
    /// line [`Command::line`] gives.
    NoSave {
        held: &'static str,
        digit: u8,
    },
}

/// The line every `^S` cancel shows (E31).
pub const NOT_SAVED: &str = "not saved — F2, Ctrl+K S or a click saves";

/// The line Ctrl+V and Shift+Ins show in a terminal (E9).
pub const TERMINAL_PASTE: &str = "the terminal's paste: ⌘V / Ctrl+Shift+V";

/// The first Ctrl+Z of a terminal session before Shift is known (E7).
pub const UNDO_HINT: &str = "Undo — this terminal may send Ctrl+Shift+Z as Ctrl+Z; Redo: F10 E R";

/// `^S`'s prompt on the status row (E31).
pub fn save_over(name: &str) -> String {
    format!(
        "Save over {name}? F2 or a click on [Save] saves · C saves a copy · any other key cancels"
    )
}

impl Command {
    /// The status row's line for this command, about the file `name`; `None` for most.
    pub fn line(&self, name: &str) -> Option<String> {
        match self {
            Command::ArmSave => Some(save_over(name)),
            Command::Cancel => Some(NOT_SAVED.to_string()),
            Command::TerminalPaste => Some(TERMINAL_PASTE.to_string()),
            Command::Undo { hint: true } => Some(UNDO_HINT.to_string()),
            Command::NoSave { held, digit } => {
                let also = if *held == "Ctrl" && *digit == 5 {
                    " (Ctrl+] in some terminals)"
                } else {
                    ""
                };
                Some(format!(
                    "this key arrived as {held}+{digit}{also} and saved nothing — F2, F5, Ctrl+K S, a click on 2Save, or F10 F S saves"
                ))
            }
            _ => None,
        }
    }
}

/// The 2-set (두벌식) jamo on each letter key, Shift's too (§5.5).
const JAMO: [(char, char); 33] = [
    ('ㅂ', 'Q'),
    ('ㅃ', 'Q'),
    ('ㅈ', 'W'),
    ('ㅉ', 'W'),
    ('ㄷ', 'E'),
    ('ㄸ', 'E'),
    ('ㄱ', 'R'),
    ('ㄲ', 'R'),
    ('ㅅ', 'T'),
    ('ㅆ', 'T'),
    ('ㅛ', 'Y'),
    ('ㅕ', 'U'),
    ('ㅑ', 'I'),
    ('ㅐ', 'O'),
    ('ㅒ', 'O'),
    ('ㅔ', 'P'),
    ('ㅖ', 'P'),
    ('ㅁ', 'A'),
    ('ㄴ', 'S'),
    ('ㅇ', 'D'),
    ('ㄹ', 'F'),
    ('ㅎ', 'G'),
    ('ㅗ', 'H'),
    ('ㅓ', 'J'),
    ('ㅏ', 'K'),
    ('ㅣ', 'L'),
    ('ㅋ', 'Z'),
    ('ㅌ', 'X'),
    ('ㅊ', 'C'),
    ('ㅍ', 'V'),
    ('ㅠ', 'B'),
    ('ㅜ', 'N'),
    ('ㅡ', 'M'),
];

/// The letter key `c` is on, upper case: an ASCII letter, or a 2-set jamo (§5.5: ㄴ = S, ㅊ = C,
/// ㅕ = U). Armings, menus and Find's options read letters so.
pub fn letter_of(c: char) -> Option<char> {
    if c.is_ascii_alphabetic() {
        return Some(c.to_ascii_uppercase());
    }
    JAMO.iter().find(|(j, _)| *j == c).map(|&(_, l)| l)
}

/// [`letter_of`] for text that is exactly one character.
pub fn letter(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => letter_of(c),
        _ => None,
    }
}

/// egui's letter key's letter.
fn key_letter(key: Key) -> Option<char> {
    use Key::*;
    const KEYS: [Key; 26] = [
        A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    ];
    KEYS.iter()
        .position(|k| *k == key)
        .map(|i| char::from(b'A' + i as u8))
}

/// egui's digit key's digit.
fn key_digit(key: Key) -> Option<u8> {
    use Key::*;
    [Num0, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9]
        .iter()
        .position(|k| *k == key)
        .map(|i| i as u8)
}

/// A modifier's own key (egui 0.37 sends them as presses): part of a chord, no press of its own.
fn is_modifier(key: Key) -> bool {
    use Key::*;
    matches!(
        key,
        ShiftLeft
            | ShiftRight
            | ControlLeft
            | ControlRight
            | AltLeft
            | AltRight
            | SuperLeft
            | SuperRight
            | Key::Fn
    )
}

/// The modifiers as the rows read them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Chord {
    /// Ctrl/⌘: egui's `command` alone (⌘ in a Mac window, Ctrl elsewhere).
    command: bool,
    /// Ctrl as the WordStar rows read it: `command`, or a Mac's ⌃.
    ctrl: bool,
    shift: bool,
    alt: bool,
    /// The Ctrl/⌘ held is ⌘.
    mac: bool,
    /// ⌘ and ⌃ together: a chord no row has.
    both: bool,
}

impl Chord {
    fn of(m: Modifiers) -> Chord {
        let both = m.mac_cmd && m.ctrl;
        Chord {
            command: m.command && !both,
            ctrl: (m.command || m.ctrl) && !both,
            shift: m.shift,
            alt: m.alt,
            mac: m.mac_cmd,
            both,
        }
    }

    fn none(self) -> bool {
        !self.ctrl && !self.shift && !self.alt && !self.both
    }

    fn only_shift(self) -> bool {
        !self.ctrl && self.shift && !self.alt && !self.both
    }

    /// Ctrl (or ⌃) and nothing else; `shift`: with Shift.
    fn ctrl_only(self, shift: bool) -> bool {
        self.ctrl && self.shift == shift && !self.alt
    }
}

/// An input that is no key: an arming keeps waiting through it (no timeout, 12 §5.4).
fn is_quiet(press: &Press) -> bool {
    match press {
        Press::Focus | Press::Resize => true,
        Press::Key { key, .. } => is_modifier(*key),
        _ => false,
    }
}

/// The letter a press stands for while an arming waits: a letter key (its physical key first),
/// with no Alt, Shift and Ctrl as `ctrl` allows; or typed text that is one letter or jamo.
fn armed_letter(press: &Press, ctrl: bool) -> Option<char> {
    match press {
        Press::Key {
            key,
            physical,
            modifiers,
        } => {
            let chord = Chord::of(*modifiers);
            if chord.alt || chord.both || (chord.ctrl && !ctrl) {
                return None;
            }
            physical.and_then(key_letter).or_else(|| key_letter(*key))
        }
        Press::Text(text) => letter(text),
        _ => None,
    }
}

/// `true` for a plain F-key `f` pressed as itself.
fn is_fkey(press: &Press, f: Key) -> bool {
    matches!(press, Press::Key { key, modifiers, .. } if *key == f && Chord::of(*modifiers).none())
}

/// §5.1–§5.4's table: what `press` does with `armed` waiting, Shift known or not, from
/// `backend`, and what waits after it.
pub fn decide(
    armed: Armed,
    press: &Press,
    shift_known: bool,
    backend: Backend,
) -> (Command, Armed) {
    if is_quiet(press) {
        return (Command::Nothing, armed);
    }
    match armed {
        // E34: F2, F5 (never from a digit: the hook leaves those digits) and their row slots
        // save; then the letter, with or without Ctrl; any other key disarms, dropped.
        Armed::Block => {
            if is_fkey(press, Key::F2) || is_fkey(press, Key::F5) || is_slot(press, &[2, 5]) {
                return (Command::Save, Armed::None);
            }
            let command = match armed_letter(press, true) {
                Some('S') => Command::Save,
                Some(l) => block_op(l).map_or(Command::Disarm, Command::Block),
                None => Command::Disarm,
            };
            (command, Armed::None)
        }
        // E31: only F2 as itself, its row slot or the prompt's [Save] overwrite; C saves a copy;
        // every other key cancels, untyped.
        Armed::Save => {
            let command =
                if is_fkey(press, Key::F2) || is_slot(press, &[2]) || *press == Press::PromptSave {
                    Command::Save
                } else if armed_letter(press, false) == Some('C') {
                    Command::SaveCopy
                } else {
                    Command::Cancel
                };
            (command, Armed::None)
        }
        Armed::None => {
            let command = unarmed(press, shift_known, backend);
            let next = match command {
                Command::ArmBlock => Armed::Block,
                Command::ArmSave => Armed::Save,
                _ => Armed::None,
            };
            (command, next)
        }
    }
}

fn is_slot(press: &Press, slots: &[u8]) -> bool {
    matches!(press, Press::Slot(n) if slots.contains(n))
}

/// Ctrl+K's letters (§5.1), but S.
fn block_op(letter: char) -> Option<BlockOp> {
    Some(match letter {
        'B' => BlockOp::Begin,
        'K' => BlockOp::End,
        'C' => BlockOp::Copy,
        'V' => BlockOp::Move,
        'Y' => BlockOp::Delete,
        'H' => BlockOp::Hide,
        'I' => BlockOp::Indent,
        'U' => BlockOp::Unindent,
        'L' => BlockOp::MarkLine,
        'T' => BlockOp::MarkWord,
        _ => return None,
    })
}

/// §5.1 with nothing armed.
fn unarmed(press: &Press, shift_known: bool, backend: Backend) -> Command {
    let terminal = backend == Backend::Terminal;
    let (key, chord) = match press {
        Press::Key { key, modifiers, .. } => (*key, Chord::of(*modifiers)),
        Press::Text(text) => return Command::Type(text.clone()),
        Press::Copy => return Command::Copy,
        Press::Cut => return Command::Cut,
        Press::Paste(text) => return Command::Paste(text.clone()),
        Press::Slot(n) => return slot(*n),
        Press::PromptSave | Press::Focus | Press::Resize => return Command::Nothing,
    };
    if chord.both {
        return Command::Nothing;
    }
    if let Some(command) = motion(key, chord) {
        return command;
    }
    if let Some(digit) = key_digit(key) {
        // E5: the hook makes no F2 or F5 of these while the editor has the keyboard.
        let held = if chord.ctrl && chord.mac {
            "⌘"
        } else if chord.ctrl {
            "Ctrl"
        } else if chord.alt && terminal {
            "Alt"
        } else {
            return Command::Typing;
        };
        return match digit {
            2 | 5 => Command::NoSave { held, digit },
            _ => Command::Nothing,
        };
    }
    if let Some(l) = key_letter(key) {
        return letter_row(l, chord, shift_known, terminal);
    }
    use Command::*;
    let plain = chord.none();
    match key {
        Key::Insert if plain => ToggleInsert,
        Key::Insert if chord.ctrl_only(false) => Copy,
        Key::Insert if chord.only_shift() && terminal => TerminalPaste,
        Key::Insert if chord.only_shift() => PasteClipboard,
        Key::Delete if plain => DeleteRight,
        Key::Delete if chord.only_shift() => Cut,
        Key::Delete if chord.ctrl_only(false) => DeleteBlock,
        Key::Backspace if plain => DeleteLeft,
        Key::Backspace if !chord.ctrl && chord.alt => {
            if chord.shift {
                Redo
            } else {
                Undo { hint: false }
            }
        }
        Key::Enter if plain => NewLine,
        Key::Tab if plain => Tab,
        Key::Tab if chord.only_shift() => Unindent,
        Key::Escape if plain => Leave,
        Key::F4 if chord.only_shift() => External,
        _ if plain => match key {
            Key::F1 => Help,
            Key::F2 | Key::F5 => Save,
            Key::F3 => Leave,
            Key::F4 => View,
            Key::F6 => Panel,
            Key::F7 => SaveAs,
            Key::F10 => Menu,
            Key::F12 => Switch,
            // F8 is empty while editing (§7.2), F9 the view keys' (08 §4).
            _ if is_character_key(key) => Typing,
            _ => Nothing,
        },
        // A character with Shift, or with Alt in a Mac window (Option composes), types.
        _ if is_character_key(key) && !chord.ctrl && (!chord.alt || !terminal) => Typing,
        _ => Nothing,
    }
}

/// A key egui follows with its typed text: space, punctuation, digits, letters.
fn is_character_key(key: Key) -> bool {
    use Key::*;
    matches!(
        key,
        Space
            | Colon
            | Comma
            | Backslash
            | Slash
            | Pipe
            | Questionmark
            | Exclamationmark
            | OpenBracket
            | CloseBracket
            | OpenCurlyBracket
            | CloseCurlyBracket
            | Backtick
            | Minus
            | Period
            | Plus
            | Equals
            | Semicolon
            | Quote
            | IntlBackslash
    ) || key_digit(key).is_some()
        || key_letter(key).is_some()
}

/// The caret's rows: arrows, Home, End, PgUp, PgDn — with Ctrl by words and to the file's ends,
/// with Shift extending the block.
fn motion(key: Key, chord: Chord) -> Option<Command> {
    use Motion::*;
    if chord.alt {
        return None;
    }
    let m = match (key, chord.ctrl) {
        (Key::ArrowLeft, false) => Left,
        (Key::ArrowRight, false) => Right,
        (Key::ArrowUp, false) => Up,
        (Key::ArrowDown, false) => Down,
        (Key::Home, false) => Home,
        (Key::End, false) => End,
        (Key::PageUp, false) => PageUp,
        (Key::PageDown, false) => PageDown,
        (Key::ArrowLeft, true) => WordLeft,
        (Key::ArrowRight, true) => WordRight,
        (Key::Home | Key::PageUp, true) => FileStart,
        (Key::End | Key::PageDown, true) => FileEnd,
        _ => return None,
    };
    Some(Command::Move(m, chord.shift))
}

/// A letter key's row: the WordStar rows by Ctrl (⌃ too), the Ctrl/⌘ rows by `command`.
fn letter_row(l: char, chord: Chord, shift_known: bool, terminal: bool) -> Command {
    use Command::*;
    if !chord.ctrl {
        return match l {
            'G' if chord.alt && terminal => Switch,
            _ if chord.alt && terminal => Nothing,
            _ => Typing,
        };
    }
    if chord.ctrl_only(false) {
        match l {
            'K' => return ArmBlock,
            'Y' => return DeleteLine,
            'T' => return DeleteWord,
            'N' => return InsertLine,
            'L' => return FindAgain,
            _ => {}
        }
    }
    if !chord.command {
        return Nothing;
    }
    match (l, chord.shift, chord.alt) {
        ('S', false, false) if terminal && !shift_known => ArmSave,
        ('S', false, false) => Save,
        // The chord that arrives with Shift is its own proof that Shift is known.
        ('S', true, false) => SaveCopy,
        ('S', false, true) => SaveAs,
        ('Z', false, false) => Undo { hint: false },
        ('Z', true, false) => Redo,
        ('F', false, false) => Find,
        ('G', false, false) => GoTo,
        ('A', false, false) => SelectAll,
        ('R', false, false) => Reread,
        ('Q' | 'W', false, false) => Close,
        ('C', false, false) => Copy,
        ('X', false, false) => Cut,
        ('V', false, false) if terminal => TerminalPaste,
        ('V', false, false) => PasteClipboard,
        _ => Nothing,
    }
}

/// The F-key row's slots (§4: `1Help 2Save 3Exit 4View 5Save 6Panel 7SaveAs 8 9View 10Menu`).
/// Slot 9 is the view keys' F9 (08 §4).
fn slot(n: u8) -> Command {
    match n {
        1 => Command::Help,
        2 | 5 => Command::Save,
        3 => Command::Leave,
        4 => Command::View,
        6 => Command::Panel,
        7 => Command::SaveAs,
        10 => Command::Menu,
        _ => Command::Nothing,
    }
}

/// The editor's keys of one editor: the arming, Shift known (E31), the undo hint (E7), and the
/// typed text of a key that was taken, dropped when it arrives.
#[derive(Debug, Clone)]
pub struct Keys {
    backend: Backend,
    armed: Armed,
    shift_known: bool,
    hinted: bool,
    /// The last press was a character key whose text follows, and it was taken.
    drop_text: bool,
}

impl Keys {
    /// The keys of an editor in `backend`: in the window Shift is known from the start.
    pub fn new(backend: Backend) -> Keys {
        Keys {
            backend,
            armed: Armed::None,
            shift_known: backend == Backend::Window,
            hinted: false,
            drop_text: false,
        }
    }

    pub fn armed(&self) -> Armed {
        self.armed
    }

    pub fn shift_known(&self) -> bool {
        self.shift_known
    }

    /// The status row's arming: `^K`, or `^S`'s prompt about `name`.
    pub fn mark(&self, name: &str) -> Option<String> {
        match self.armed {
            Armed::None => None,
            Armed::Block => Some("^K".to_string()),
            Armed::Save => Some(save_over(name)),
        }
    }

    /// One press: Shift known as it shows, then [`decide`]. The text a taken character key is
    /// followed by is [`Command::Nothing`]: never typed (08 §4's rule; EB-6, EK-1).
    pub fn press(&mut self, press: &Press) -> Command {
        let drop_text = std::mem::take(&mut self.drop_text);
        if let Press::Text(_) = press {
            if drop_text {
                return Command::Nothing;
            }
        }
        if self.backend == Backend::Terminal {
            match press {
                Press::Focus | Press::Resize => self.shift_known = false,
                Press::Key { key, modifiers, .. } => {
                    let chord = Chord::of(*modifiers);
                    if chord.ctrl && chord.shift && key_letter(*key).is_some() {
                        self.shift_known = true;
                    }
                }
                _ => {}
            }
        }
        let (mut command, armed) = decide(self.armed, press, self.shift_known, self.backend);
        self.armed = armed;
        if let Command::Undo { hint } = &mut command {
            if self.backend == Backend::Terminal && !self.shift_known && !self.hinted {
                *hint = true;
                self.hinted = true;
            }
        }
        if let Press::Key { key, modifiers, .. } = press {
            self.drop_text = command != Command::Typing && self.text_follows(*key, *modifiers);
        }
        command
    }

    /// Whether a source sends typed text right after this key: a character key with no Ctrl or
    /// ⌘ held, and in a terminal no Alt either (egui-winit; `term_input.rs`'s translator).
    fn text_follows(&self, key: Key, m: Modifiers) -> bool {
        let held = m.ctrl || m.command || m.mac_cmd || (m.alt && self.backend == Backend::Terminal);
        is_character_key(key) && !held
    }
}

#[cfg(test)]
mod tests;
