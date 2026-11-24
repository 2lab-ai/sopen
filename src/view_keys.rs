//! The viewer's view keys (`.prd/08-viewer.md` §4): which press chooses view 1, 2 or 3, arms F9,
//! or starts or ends a hex view's editing (D35) — and which is the view's own, passed on as today.
//! One press, one action: a press is taken by the viewer (its key event, its typed text and its
//! release with it) or passed on to the view, never both.
//!
//! Screen-free (D31). [`decide`] is §4's table, over the keys a binding tells apart ([`Press`], §9:
//! a digit, F9, Esc, Enter, any other). [`ViewKeys`] runs it over one input of egui events — egui's
//! input side is the input of both backends (`research/tui-gui.md` §11, option A) — and adds what
//! only a whole input or a held key decides: a key's typed text goes where its key went, Leave wins,
//! auto-repeats. The egui binding (VM-7) calls [`ViewKeys::take`] in the shell's input hook, after
//! the Cmd/Ctrl+digit aliases (`fkey_events`, `gui/mod.rs:815`): Cmd/Ctrl+1…3 reach it as F1…F3,
//! Cmd/Ctrl+9 and a click on slot 9 as F9. A terminal binding (VM-8) sends the character `1` alone,
//! which is a press of its own here. A window on its own (`--hex`, the binary dialog, the image and
//! media windows) has no views and no view keys (D24).

use eframe::egui::{Event, ImeEvent, Key, Modifiers};

/// What the view on screen declares, as of its last pass (§4: one state per view).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// A view in no other state that is not a hex view: image, video, audio, text, raw, the
    /// image's memory line (06 §7.3), whose Enter and Esc stay its own.
    Free,
    /// The hex view in the browser's viewer (D35): Free while viewing, DigitsAreData while editing.
    Hex(Hex),
    /// A status-row field has the typing: Go to (hex, text), the hex window's read-only path field.
    Field,
    /// A question is up: the close prompt, Save As, a confirmation, the encoding list.
    Question,
}

/// What the hex view in the browser's viewer declares (D35).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hex {
    /// Editing, since Enter or Insert; else viewing, as it opens and as it is shown again.
    pub editing: bool,
    /// A revision's bytes (`hex.rs:2270-2275`): it never edits.
    pub read_only: bool,
    /// A Save that Esc stops is running (a streamed file's, `hex.rs:3549-3554`): Esc is the
    /// window's. Typing is held back meanwhile, which the window says, as today.
    pub saving: bool,
}

/// §4's four rows, which a [`State`] belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// A plain digit chooses a view; F9 arms.
    Free,
    /// A plain digit is typed (a nibble or an ASCII byte, `hex.rs:1002-1025`); F9 ends editing
    /// and arms.
    DigitsAreData,
    /// A plain digit goes into the field (04 §3 rule 4); F9 closes the field, as Esc would, and
    /// arms (D29).
    Field,
    /// Every key belongs to the question; F9 does nothing (D5).
    Question,
}

impl State {
    /// The row of §4's table this state is.
    pub fn row(self) -> Row {
        match self {
            State::Free => Row::Free,
            State::Hex(hex) if hex.editing => Row::DigitsAreData,
            State::Hex(_) => Row::Free,
            State::Field => Row::Field,
            State::Question => Row::Question,
        }
    }
}

/// A press, as the view keys tell them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// 1, 2 or 3 with no modifier (D3): in the GUI by the physical key first, so the key printed 1
    /// is 1 on any layout; in a terminal, the character.
    Digit(u8),
    F9,
    Esc,
    /// Leaves the viewer, as Esc does (`keys.rs:501-502`).
    F3,
    Enter,
    Insert,
    /// Any other press: another key, a digit with Shift, Alt or Cmd/Ctrl, a clipboard shortcut
    /// (egui-winit sends `Copy`, `Cut` or `Paste` in place of its key press), typed text on its own
    /// that is not 1, 2 or 3.
    Other,
}

impl Press {
    /// A key event's press. Only a key pressed with no modifier is a view key, and a digit is
    /// its physical key's first, else its logical key's — as the Cmd/Ctrl+digit aliases read it
    /// (`gui/mod.rs:866`), so a key is one digit to both.
    pub fn of_key(key: Key, physical_key: Option<Key>, modifiers: Modifiers) -> Press {
        if !modifiers.is_none() {
            return Press::Other;
        }
        let number = |key: Key| {
            use Key::*;
            [Num0, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9]
                .iter()
                .position(|k| *k == key)
        };
        if let Some(n) = physical_key.and_then(number).or_else(|| number(key)) {
            return match n {
                1..=3 => Press::Digit(n as u8),
                _ => Press::Other,
            };
        }
        match key {
            Key::F9 => Press::F9,
            Key::Escape => Press::Esc,
            Key::F3 => Press::F3,
            Key::Enter => Press::Enter,
            Key::Insert => Press::Insert,
            _ => Press::Other,
        }
    }

    /// Typed text that no key press came with — a terminal's character (§9), committed IME text:
    /// exactly `1`, `2` or `3` is that digit.
    pub fn of_text(text: &str) -> Press {
        match text {
            "1" => Press::Digit(1),
            "2" => Press::Digit(2),
            "3" => Press::Digit(3),
            _ => Press::Other,
        }
    }
}

/// What becomes of one event of an input: passed on to the view ([`Command::Pass`],
/// [`Command::Leave`]) or taken from it (every other command; [`Command::taken`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Passed on, as today: not a view key here (a digit typed as data or into a field, a key of
    /// the view or of the question, a release, the pointer).
    Pass,
    /// Passed on, and it leaves the viewer: Esc or F3 goes its way — the view's own Esc first
    /// (stopping a Save, clearing the image's rectangle), then the browser's row asking the view on
    /// screen (`keys.rs:501-502`, §5). No view is chosen in the same input (Leave wins, §4).
    Leave,
    /// Show the view in slot 1, 2 or 3. The view on screen chosen again, or a slot with no view,
    /// does nothing; unsaved edits ask first (the view set's rules, §3, §5).
    Choose(u8),
    /// F9: armed, after closing what it closes. The next key decides (D4); no timeout (D33).
    Arm(Ending),
    /// Esc or F9 while armed.
    Disarm,
    /// Enter or Insert in a hex view viewing: it edits now (`editing — Esc ends`).
    StartEditing,
    /// Esc in a hex view editing: viewing again; its edits stay unsaved.
    EndEditing,
    /// Enter or Insert in a read-only hex view, which never edits: the status row says so.
    ReadOnly,
    /// Dropped, never typed: armed's "any other key", a key held down while armed, the rest of an
    /// input once a view is chosen, a field closed or the viewer left, a view key Leave overrides,
    /// the auto-repeat or the release of a press that was taken.
    Drop,
    /// The typed text of a press that was taken (egui-winit sends it right after the key).
    DropText,
}

/// What F9 closes before it arms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Nothing,
    /// The hex view's editing (D35).
    Editing,
    /// The status-row field, as Esc would (D29).
    Field,
}

impl Command {
    /// Whether the event is taken from the view: every command but [`Command::Pass`] and
    /// [`Command::Leave`].
    pub fn taken(self) -> bool {
        !matches!(self, Command::Pass | Command::Leave)
    }

    /// Whether the binding carries it out (a view action) or must know of it (Leave): every
    /// command but [`Command::Pass`], [`Command::Drop`] and [`Command::DropText`].
    pub fn acts(self) -> bool {
        !matches!(self, Command::Pass | Command::Drop | Command::DropText)
    }
}

/// §4's table: what `press` does in `state`, with F9 `armed` or not, and whether F9 is armed
/// after it. Armed comes before the state — F9 then a digit chooses from a field or an editing hex
/// view in one input — but for a question, which keeps its keyboard and disarms (D5, VA-3).
pub fn decide(state: State, armed: bool, press: Press) -> (Command, bool) {
    if state.row() == Row::Question {
        return (Command::Pass, false);
    }
    if armed {
        let command = match press {
            Press::Digit(n) => Command::Choose(n),
            Press::Esc | Press::F9 => Command::Disarm,
            _ => Command::Drop,
        };
        return (command, false);
    }
    let hex = match state {
        State::Hex(hex) => Some(hex),
        _ => None,
    };
    let command = match press {
        Press::Digit(n) if state.row() == Row::Free => Command::Choose(n),
        // Typed as data (held back, and said why, while a Save or a reload runs), or into the field.
        Press::Digit(_) => Command::Pass,
        Press::F9 => Command::Arm(match state.row() {
            Row::DigitsAreData => Ending::Editing,
            Row::Field => Ending::Field,
            _ => Ending::Nothing,
        }),
        // A field's own Esc closes it; a running Save's stops it first (D35).
        Press::Esc if state == State::Field || hex.is_some_and(|h| h.saving) => Command::Pass,
        Press::Esc if state.row() == Row::DigitsAreData => Command::EndEditing,
        Press::Esc => Command::Leave,
        // A field withholds every key but Esc from the viewer's rows (`browser.rs:2352-2363`).
        Press::F3 if state == State::Field => Command::Pass,
        Press::F3 => Command::Leave,
        Press::Enter | Press::Insert => match hex {
            Some(h) if h.editing => Command::Pass,
            Some(h) if h.read_only => Command::ReadOnly,
            Some(_) => Command::StartEditing,
            None => Command::Pass,
        },
        Press::Other => Command::Pass,
    };
    (command, matches!(command, Command::Arm(_)))
}

/// The view keys of one viewer: F9 armed or not, and the keys held down. Made when the viewer
/// opens ([`ViewKeys::new`]) and fed every input until it closes.
#[derive(Clone, Debug, Default)]
pub struct ViewKeys {
    armed: bool,
    /// The keys down, by the physical key first (as the digits are read) and matched by either
    /// key — Shift changes a digit's logical key between its press and its release — and whether
    /// their press was taken. A press of a key already down is its auto-repeat — egui-winit 0.37
    /// sends every press with `repeat: false` and egui marks repeats only after the input hook
    /// (egui-winit `lib.rs:1159`) — and a key's repeats and release go where its press went: the
    /// media window pastes at a V release whose press it never saw (`media.rs:3851-3864`).
    held: Vec<(Key, bool)>,
}

/// How far one input has gone: the view's declared state, as the view keys' own commands change
/// it, or past what the model can know until the view's next pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Now {
    Known(State),
    /// A view was chosen or a field closed: which view, in which state, the rest of the input
    /// meets is not known (made, shown again, or the close prompt). Esc and F3 leave; any other
    /// press is dropped — never typed into a view it was not meant for, never a second switch.
    Unknown,
    /// The viewer is being left: the rest is dropped.
    Leaving,
}

impl ViewKeys {
    /// The view keys of a viewer opened while `down` were held (egui's `keys_down`, the Enter
    /// that opened it among them): their repeats and releases are passed on, never decided, so a
    /// held Enter does not start editing the hex view it opened.
    pub fn new(down: impl IntoIterator<Item = Key>) -> ViewKeys {
        ViewKeys {
            armed: false,
            held: down.into_iter().map(|key| (key, false)).collect(),
        }
    }

    /// F9 armed: the strip is warn-coloured (§8).
    pub fn armed(&self) -> bool {
        self.armed
    }

    /// One command per event of one input, in order. A key's typed text follows the key; typed
    /// text with no key before it is a press of its own (a terminal's character; committed IME
    /// text too, a guess: how the Korean input source sends digits is LR-2's to see). If the
    /// input leaves the viewer, no view key in it chooses (Leave wins, §4).
    pub fn route(&mut self, state: State, events: &[Event]) -> Vec<Command> {
        if state.row() == Row::Question {
            self.armed = false;
        }
        let mut now = Now::Known(state);
        let mut out = Vec::with_capacity(events.len());
        // Whether the press the next typed text belongs to was taken.
        let mut text_of: Option<bool> = None;
        for event in events {
            let typed = match event {
                Event::Text(text) | Event::Ime(ImeEvent::Commit(text)) => Some(text),
                _ => None,
            };
            let command = match (event, typed) {
                (_, Some(text)) => match text_of {
                    Some(true) => Command::DropText,
                    Some(false) => Command::Pass,
                    None => self.step(&mut now, Press::of_text(text)),
                },
                (
                    Event::Key {
                        key,
                        physical_key,
                        pressed: true,
                        repeat,
                        modifiers,
                    },
                    None,
                ) => {
                    let press = Press::of_key(*key, *physical_key, *modifiers);
                    let at = self.held_at(*key, *physical_key);
                    let command = match at.map(|at| self.held[at].1) {
                        Some(taken) => self.repeat(now, taken),
                        None if *repeat => self.repeat(now, false),
                        None => self.step(&mut now, press),
                    };
                    if at.is_none() {
                        self.held
                            .push((physical_key.unwrap_or(*key), command.taken()));
                    }
                    // F9, Esc, F3, Enter and Insert type nothing (egui-winit sends no text for
                    // them): text right after one is a press of its own, as a terminal's F9
                    // then `2` is.
                    let types = matches!(press, Press::Digit(_) | Press::Other);
                    text_of = types.then_some(command.taken());
                    out.push(command);
                    continue;
                }
                (
                    Event::Key {
                        key, physical_key, ..
                    },
                    None,
                ) => {
                    let at = self.held_at(*key, *physical_key);
                    match at.map(|at| self.held.remove(at)) {
                        Some((_, true)) => Command::Drop,
                        _ => Command::Pass,
                    }
                }
                (Event::Copy | Event::Cut | Event::Paste(_) | Event::PasteImage(_), None) => {
                    // The shortcut stands for its key's press, and egui-winit sends it again for
                    // each repeat; the release arrives as the key's.
                    let key = match event {
                        Event::Copy => Key::C,
                        Event::Cut => Key::X,
                        _ => Key::V,
                    };
                    let at = self.held_at(key, None);
                    let command = match at.map(|at| self.held[at].1) {
                        Some(taken) => self.repeat(now, taken),
                        None => self.step(&mut now, Press::Other),
                    };
                    if at.is_none() {
                        self.held.push((key, command.taken()));
                    }
                    command
                }
                (Event::WindowFocused(false), None) => {
                    // winit may never send the releases of keys held then; egui forgets them too.
                    self.held.clear();
                    Command::Pass
                }
                _ => Command::Pass,
            };
            if typed.is_none() {
                text_of = None;
            }
            out.push(command);
        }
        if out.contains(&Command::Leave) {
            for command in &mut out {
                if matches!(command, Command::Choose(_)) {
                    *command = Command::Drop;
                }
            }
            self.armed = false;
        }
        out
    }

    /// [`ViewKeys::route`], then removes the taken events from `events`: what is left goes on to
    /// the view. Returns what the binding does, in order (every command that [`Command::acts`]).
    pub fn take(&mut self, state: State, events: &mut Vec<Event>) -> Vec<Command> {
        let commands = self.route(state, events);
        let mut each = commands.iter();
        events.retain(|_| !each.next().is_some_and(|c| c.taken()));
        commands.into_iter().filter(|c| c.acts()).collect()
    }

    /// Where the key of an event is held, matched by its logical or its physical key.
    fn held_at(&self, key: Key, physical_key: Option<Key>) -> Option<usize> {
        self.held
            .iter()
            .position(|(k, _)| *k == key || Some(*k) == physical_key)
    }

    /// A press of a key already down, its auto-repeat: dropped when its press was taken; else
    /// passed on as today, but never while armed (it is not the next key, and it is never typed)
    /// or past what the input can know — and never a view key.
    fn repeat(&self, now: Now, taken: bool) -> Command {
        if taken || self.armed || !matches!(now, Now::Known(_)) {
            Command::Drop
        } else {
            Command::Pass
        }
    }

    /// One press, decided where the input has got to.
    fn step(&mut self, now: &mut Now, press: Press) -> Command {
        let state = match *now {
            Now::Leaving => return Command::Drop,
            // Armed decides alike in every state but a question, which is not reached so.
            Now::Unknown if self.armed => State::Free,
            Now::Unknown => {
                return match press {
                    Press::Esc | Press::F3 => {
                        *now = Now::Leaving;
                        Command::Leave
                    }
                    _ => Command::Drop,
                };
            }
            Now::Known(state) => state,
        };
        let (command, armed) = decide(state, self.armed, press);
        self.armed = armed;
        *now = match (command, *now, state) {
            (Command::Leave, ..) => Now::Leaving,
            (Command::Choose(_) | Command::Arm(Ending::Field), ..) => Now::Unknown,
            (_, Now::Unknown, _) => Now::Unknown,
            (Command::StartEditing, _, State::Hex(hex)) => Now::Known(State::Hex(Hex {
                editing: true,
                ..hex
            })),
            (Command::EndEditing | Command::Arm(Ending::Editing), _, State::Hex(hex)) => {
                Now::Known(State::Hex(Hex {
                    editing: false,
                    ..hex
                }))
            }
            (_, now, _) => now,
        };
        command
    }
}

#[cfg(test)]
mod tests;
