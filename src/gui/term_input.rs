//! The terminal's input as the window's (10 §4, §9): crossterm's events become `egui::Event`s in
//! egui-winit 0.37's shape — a named key with its modifiers, Ctrl as egui's `command` on every OS,
//! ⌘ only under the kitty protocol, a printable ASCII key as `Key` and `Text`, other text as `Text`
//! alone, mouse at the cell's centre, wheel by lines, focus as `WindowFocused` — so the `FKeyInput`
//! hook and the key table see them as from the window.
//!
//! - **A key** (egui-winit's `State::on_keyboard_input`): its `Event::Key`, its typed text, then
//!   its release, made at once. A terminal sends presses only — releases only under the kitty
//!   protocol's REPORT_EVENT_TYPES, and for a key that types text not even then unless every key is
//!   an escape code (crossterm's `KeyboardEnhancementFlags` docs) — while the window's code counts
//!   on every key down coming up: `FKeyInput` keeps a digit as its F-key, or as the digit, until
//!   its release (`key_aliases` in `gui/mod.rs`), and egui calls every later press of a key still down a
//!   repeat. A release the terminal does report would be a second one, so it is not passed on; a
//!   repeat is a press of its own; a modifier key's own press or release changes only the
//!   modifiers.
//! - **The clipboard keys** (egui-winit's shape: the shortcut's event alone, then its letter's
//!   key-up, which the image and media windows and the view keys wait for before the next).
//!   Cmd/Ctrl+X and +C are `Cut` / `Copy`, then X's or C's release. The terminal's own paste
//!   (bracketed) is `Paste`, its CRLFs and lone CRs as LFs, then V's release; Cmd/Ctrl+V itself
//!   gives nothing, not even a bare V release (a terminal program cannot read the clipboard).
//!   Residuals, by design: a held Cmd/Ctrl+X auto-repeats as the terminal's repeated presses, so
//!   as repeated `Cut`s each with its release, where a window gives repeated `Cut`s and one
//!   release; and a kitty Repeat (flag 2, not pushed) is flattened into a press with its release.
//! - **Command** (T26). `keys::Mods` matches on egui's `command`. A terminal keeps ⌘ for itself and
//!   the legacy encoding cannot carry it, so `command` is **Ctrl on every OS**, `ctrl` set too, as
//!   egui reports Ctrl on Linux. ⌘ — the kitty protocol's SUPER, where the terminal passes it — is
//!   `mac_cmd` + `command` on macOS and nothing elsewhere, as egui-winit makes it.
//! - **Text** (T25). Every printable ASCII character egui names a key for (letters, digits, Space,
//!   the punctuation keys) is `Key` and `Text`, as egui-winit sends it: the key table and the
//!   viewers match keys, a field inserts the text. Other ASCII (`# $ % & ( ) * < > @ ^ _ ~ "`) and
//!   every other character is `Text` alone — an input method's commit too: a terminal receives the
//!   committed syllables as UTF-8, one `Char` each, and draws the composition itself. With Ctrl,
//!   Alt or ⌘ held, the key and no text, so Alt+G and Alt+digits never type (§9).
//! - **Pointer.** A cell (column, row) is the point at its centre on the 8×16 grid, so a click
//!   lands where the cell grid drew the row; the wheel moves by lines. A mouse report carries the
//!   modifiers, so it updates them as a key does.
//! - **Focus** reports are `WindowFocused`; losing it forgets the modifiers, as egui-winit does.

use super::chrome::CELL;
use crossterm::event::{
    Event as CEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode, MouseButton,
    MouseEvent, MouseEventKind,
};
use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2, Vec2};

/// Whether this source's events carry key releases: they do — every press is followed by its
/// release, made at once (see the module) — so a model that asks it of its input source (the view
/// keys' `ViewKeys::new`, 08 VM-6) is given this, whatever the terminal itself reports.
pub const RELEASES: bool = true;

/// The translator's state across events.
#[derive(Debug, Clone)]
pub struct TermInput {
    /// macOS: SUPER is ⌘ (`mac_cmd`).
    mac: bool,
    /// The modifiers as last reported, with a key or a mouse event.
    modifiers: Modifiers,
    /// The terminal's size in cells, once a resize said it.
    size: Option<(u16, u16)>,
    /// Whether the terminal has the keyboard (focus reports).
    focused: bool,
}

impl TermInput {
    /// The translator of one terminal; `mac`: whether SUPER is ⌘ (`cfg!(target_os = "macos")`; a
    /// parameter, so the tests play either machine).
    pub fn new(mac: bool) -> Self {
        TermInput {
            mac,
            modifiers: Modifiers::NONE,
            size: None,
            focused: true,
        }
    }

    /// The modifiers as last reported (the pass's `RawInput::modifiers`).
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// Whether the terminal has the keyboard (the pass's `RawInput::focused`).
    pub fn focused(&self) -> bool {
        self.focused
    }

    /// The terminal's size in cells, once a `Resize` said it.
    pub fn size(&self) -> Option<(u16, u16)> {
        self.size
    }

    /// The egui events of one crossterm event.
    pub fn translate(&mut self, ev: &CEvent) -> Vec<Event> {
        let mut out = vec![];
        match ev {
            CEvent::Key(k) => {
                let mods = self.key_modifiers(k);
                self.report(mods, &mut out);
                if k.kind != KeyEventKind::Release {
                    press(k.code, mods, &mut out);
                }
            }
            // Nothing for an empty paste, as egui-winit gives no `Paste` for an empty clipboard
            // (and no bare V key-up: Ctrl+V's own is not made either).
            CEvent::Paste(text) if text.is_empty() => {}
            CEvent::Paste(text) => {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                // V's key-up after it, as the window's Cmd/Ctrl+V gives: the windows and the view
                // keys hold a paste until then.
                out.extend([
                    Event::Paste(text),
                    Event::Key {
                        key: Key::V,
                        physical_key: None,
                        pressed: false,
                        repeat: false,
                        modifiers: self.modifiers,
                    },
                ]);
            }
            CEvent::Mouse(m) => self.mouse(m, &mut out),
            CEvent::FocusGained | CEvent::FocusLost => {
                self.focused = matches!(ev, CEvent::FocusGained);
                if !self.focused {
                    // egui forgets them too on `WindowFocused(false)`.
                    self.modifiers = Modifiers::NONE;
                }
                out.push(Event::WindowFocused(self.focused));
            }
            CEvent::Resize(cols, rows) => self.size = Some((*cols, *rows)),
        }
        out
    }

    /// crossterm's modifiers as egui's (the module's Command rule).
    fn modifiers_of(&self, m: KeyModifiers) -> Modifiers {
        let ctrl = m.contains(KeyModifiers::CONTROL);
        let cmd = self.mac && m.contains(KeyModifiers::SUPER);
        Modifiers {
            alt: m.contains(KeyModifiers::ALT),
            ctrl,
            shift: m.contains(KeyModifiers::SHIFT),
            mac_cmd: cmd,
            command: ctrl || cmd,
        }
    }

    /// The modifiers after key event `k`: crossterm still sets a modifier key's own flag on its
    /// release (kitty, every key as an escape code).
    fn key_modifiers(&self, k: &KeyEvent) -> Modifiers {
        let mut mods = self.modifiers_of(k.modifiers);
        if let (KeyCode::Modifier(m), KeyEventKind::Release) = (k.code, k.kind) {
            use ModifierKeyCode as M;
            match m {
                M::LeftShift | M::RightShift => mods.shift = false,
                M::LeftAlt | M::RightAlt => mods.alt = false,
                M::LeftControl | M::RightControl => {
                    mods.ctrl = false;
                    mods.command = mods.mac_cmd;
                }
                M::LeftSuper | M::RightSuper => {
                    mods.mac_cmd = false;
                    mods.command = mods.ctrl;
                }
                _ => {}
            }
        }
        mods
    }

    /// Takes `mods` as the modifiers now, and tells egui if they changed.
    fn report(&mut self, mods: Modifiers, out: &mut Vec<Event>) {
        if mods != self.modifiers {
            self.modifiers = mods;
            out.push(Event::ModifiersChanged(mods));
        }
    }

    fn mouse(&mut self, m: &MouseEvent, out: &mut Vec<Event>) {
        let modifiers = self.modifiers_of(m.modifiers);
        self.report(modifiers, out);
        let pos = cell_centre(m.column, m.row);
        out.push(Event::PointerMoved(pos));
        let button = |b: MouseButton| match b {
            MouseButton::Left => PointerButton::Primary,
            MouseButton::Right => PointerButton::Secondary,
            MouseButton::Middle => PointerButton::Middle,
        };
        let wheel = |x: f32, y: f32| Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(x, y),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers,
        };
        out.push(match m.kind {
            MouseEventKind::Down(b) | MouseEventKind::Up(b) => Event::PointerButton {
                pos,
                button: button(b),
                pressed: matches!(m.kind, MouseEventKind::Down(_)),
                modifiers,
            },
            MouseEventKind::Drag(_) | MouseEventKind::Moved => return,
            // winit's signs: the content moves down for a wheel turned up, right for left.
            MouseEventKind::ScrollDown => wheel(0.0, -1.0),
            MouseEventKind::ScrollUp => wheel(0.0, 1.0),
            MouseEventKind::ScrollLeft => wheel(1.0, 0.0),
            MouseEventKind::ScrollRight => wheel(-1.0, 0.0),
        });
    }
}

/// The events of one press of `code` with `mods`: its key, its text, its release.
fn press(code: KeyCode, mods: Modifiers, out: &mut Vec<Event>) {
    let key = key_of(code);
    let event = |key: Key, pressed: bool| Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: mods,
    };
    // egui-winit: Cmd/Ctrl+X and C are the clipboard's, alone (the letter's release follows, as it
    // comes later in a window); Cmd/Ctrl+V reads the clipboard there — here the terminal's own
    // paste arrives as `Event::Paste` (bracketed paste, with V's release), so Ctrl+V gives nothing.
    if let Some(key) = key.filter(|_| mods.command_only()) {
        match key {
            Key::X | Key::C => {
                out.push(if key == Key::X {
                    Event::Cut
                } else {
                    Event::Copy
                });
                out.push(event(key, false));
                return;
            }
            Key::V => return,
            _ => {}
        }
    }
    if let Some(key) = key {
        out.push(event(key, true));
    }
    if let KeyCode::Char(c) = code {
        let held = mods.ctrl || mods.command || mods.mac_cmd || mods.alt;
        if !held && typed(c) {
            out.push(Event::Text(c.to_string()));
        }
    }
    if let Some(key) = key {
        out.push(event(key, false));
    }
}

/// Whether `c` is typed text: not a control character, not in a private use area (egui-winit's
/// `is_printable_char`: macOS sends function keys there).
fn typed(c: char) -> bool {
    let private = ('\u{e000}'..='\u{f8ff}').contains(&c)
        || ('\u{f0000}'..='\u{ffffd}').contains(&c)
        || ('\u{100000}'..='\u{10fffd}').contains(&c);
    !c.is_control() && !private
}

/// The point at the centre of cell (`col`, `row`).
pub fn cell_centre(col: u16, row: u16) -> Pos2 {
    Pos2::new(
        col as f32 * CELL.x + CELL.x / 2.0,
        row as f32 * CELL.y + CELL.y / 2.0,
    )
}

/// The egui key of a crossterm key code (`None`: egui has no such key — a typed `*`, a Hangul
/// syllable, a media key, a lone modifier). Only an ASCII character is a key: egui's names also
/// take `⏷` and `−`, which a terminal can only have typed.
pub fn key_of(code: KeyCode) -> Option<Key> {
    Some(match code {
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Enter => Key::Enter,
        KeyCode::Left => Key::ArrowLeft,
        KeyCode::Right => Key::ArrowRight,
        KeyCode::Up => Key::ArrowUp,
        KeyCode::Down => Key::ArrowDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Tab | KeyCode::BackTab => Key::Tab,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Esc => Key::Escape,
        KeyCode::F(n) => return Key::from_name(&format!("F{n}")),
        KeyCode::Char(c) if c.is_ascii() => return Key::from_name(c.encode_utf8(&mut [0; 4])),
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
