//! The translator against crossterm's real parser (10 §15's terminal driver; TK-1, T7, T25, T26,
//! T33): the bytes a terminal sends — in the legacy encoding and under the kitty keyboard
//! protocol's flag 1 (DISAMBIGUATE_ESCAPE_CODES, the one pushed by default) — are written into a
//! pty this test owns and decoded there by crossterm, in a child process (this test binary again,
//! its stdin the pty, in crossterm's raw mode, as the terminal backend reads); the decoded events
//! come back to be translated here and looked up in the key table as a window's press is.
//!
//! Both encoders are this file's: no terminal runs. The legacy bytes are xterm's, as the spike's
//! player (`keyrows.py`) sent them; the kitty bytes follow the protocol's spec for flag 1, as the
//! spike's did, except that Esc is `CSI 27u` (§9's table; the spike sent a lone ESC) and F1, F2,
//! F4 are `CSI P`, `CSI Q`, `CSI S`. What kitty, herdr-app and herdr send is the owner's (TK-2).

use super::*;
use crate::gui::browser::keys::{self, Command, Context, Mods, Trigger, ALL_SLICES};
use crate::gui::{fkey_events, Digit};
use crossterm::event::{KeyEventState, MouseButton as MB};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Child, Command as Process, Stdio};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// The decoder: crossterm in a child process on a pty.

/// Set in the child [`Decoder::start`] runs.
const CHILD: &str = "SOPEN_TEST_CROSSTERM_CHILD";
const CHILD_TEST: &str = "gui::term_input::tests::crossterm_child";
/// The child's lines: ready, a decoded event, a mark reached.
const READY: &str = "@@ready";
const EV: &str = "@@ev ";
const DONE: &str = "@@done ";
/// A bracketed paste starting so is a mark of the driver's, not input.
const MARK: &str = "@@mark ";
/// How long one answer of the child may take (a loaded CI runner starting a test binary).
const WAIT: Duration = Duration::from_secs(30);

/// crossterm's parser, in a pty of this test's own.
struct Decoder {
    master: File,
    child: Child,
    lines: VecDeque<String>,
    partial: Vec<u8>,
    /// Everything the child wrote, for a failure's message.
    seen: String,
    marks: usize,
}

impl Decoder {
    fn start() -> Decoder {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: two out-pointers to live ints; no name, termios or size asked for.
        let made = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(made, 0, "openpty: {}", std::io::Error::last_os_error());
        for fd in [master, slave] {
            // Close-on-exec: a child another test thread spawns does not inherit it.
            // SAFETY: fcntl on a descriptor openpty just returned.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        // SAFETY: each descriptor is owned by one `File` from here on.
        let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
        let io = || Stdio::from(slave.try_clone().unwrap());
        let child = Process::new(std::env::current_exe().unwrap())
            .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .stdin(io())
            .stdout(io())
            .stderr(io())
            .spawn()
            .unwrap();
        // The pty's slave is the child's alone now: its end is the master's end.
        drop(slave);
        let mut d = Decoder {
            master,
            child,
            lines: VecDeque::new(),
            partial: vec![],
            seen: String::new(),
            marks: 0,
        };
        d.expect(&[READY]);
        d
    }

    fn write(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }

    /// The next line of the child's from one of `prefixes` on (other lines are the test
    /// harness's own; its `test … ` may start the line the first one ends), within [`WAIT`].
    fn expect(&mut self, prefixes: &[&str]) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            while let Some(line) = self.lines.pop_front() {
                if let Some(at) = prefixes.iter().find_map(|p| line.find(p)) {
                    return line[at..].to_string();
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "no {prefixes:?} from the child within {WAIT:?}; it wrote:\n{}",
                self.seen
            );
            let mut poll = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one live pollfd.
            let ready = unsafe { libc::poll(&mut poll, 1, left.as_millis().min(1000) as i32) };
            if ready < 0 {
                let e = std::io::Error::last_os_error();
                assert_eq!(e.kind(), ErrorKind::Interrupted, "poll: {e}");
                continue;
            }
            if ready == 0 {
                continue;
            }
            assert_eq!(
                poll.revents & libc::POLLNVAL,
                0,
                "poll: the pty is not open"
            );
            // Something to read (POLLIN), or the child's end (POLLHUP, POLLERR): read says which.
            let mut buf = [0u8; 4096];
            let n = loop {
                match self.master.read(&mut buf) {
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    // Linux reads EIO, macOS 0, once the child has gone.
                    read => break read.unwrap_or(0),
                }
            };
            assert!(n > 0, "the child is gone; it wrote:\n{}", self.seen);
            self.seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            self.partial.extend_from_slice(&buf[..n]);
            while let Some(at) = self.partial.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.partial.drain(..=at).collect();
                let line = String::from_utf8_lossy(&line);
                self.lines
                    .push_back(line.trim_end_matches(['\r', '\n']).to_string());
            }
        }
    }

    /// The events crossterm decodes from `bytes`, written at once as a terminal sends one key.
    fn decode(&mut self, bytes: &[u8]) -> Vec<CEvent> {
        let mut events = vec![];
        self.write(bytes);
        if bytes.ends_with(b"\x1b") {
            // A lone ESC is Esc only at the end of what one read returns (crossterm reads ESC ESC
            // as one Esc): the mark must not follow it into the same read.
            events.push(unwire(&self.expect(&[EV])[EV.len()..]));
        }
        self.marks += 1;
        let mark = self.marks.to_string();
        self.write(format!("\x1b[200~{MARK}{mark}\x1b[201~").as_bytes());
        loop {
            let line = self.expect(&[EV, DONE]);
            match line.strip_prefix(DONE) {
                Some(done) => {
                    assert_eq!(done, mark, "marks out of order");
                    return events;
                }
                None => events.push(unwire(&line[EV.len()..])),
            }
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self
            .master
            .write_all(format!("\x1b[200~{MARK}quit\x1b[201~").as_bytes());
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Not a test of its own (it returns at once): the decoder's child. crossterm's raw mode on its
/// stdin, the pty; every event crossterm reads written back, a mark as reached.
#[test]
fn crossterm_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    crossterm::terminal::enable_raw_mode().unwrap();
    let mut out = std::io::stdout();
    let mut say = |line: String| {
        out.write_all(format!("{line}\n").as_bytes()).unwrap();
        out.flush().unwrap();
    };
    say(READY.to_string());
    loop {
        let ev = crossterm::event::read().unwrap();
        match &ev {
            CEvent::Paste(p) if p.starts_with(MARK) => {
                let mark = &p[MARK.len()..];
                if mark == "quit" {
                    break;
                }
                say(format!("{DONE}{mark}"));
            }
            ev => say(format!("{EV}{}", wire(ev))),
        }
    }
    crossterm::terminal::disable_raw_mode().unwrap();
}

// The child's events as lines: `key CODE MODS KIND STATE`, `mouse KIND COL ROW MODS`, `paste
// HEX`, `focus 0|1`, `resize COLS ROWS`.

const NAMED: [KeyCode; 23] = [
    KeyCode::Backspace,
    KeyCode::Enter,
    KeyCode::Left,
    KeyCode::Right,
    KeyCode::Up,
    KeyCode::Down,
    KeyCode::Home,
    KeyCode::End,
    KeyCode::PageUp,
    KeyCode::PageDown,
    KeyCode::Tab,
    KeyCode::BackTab,
    KeyCode::Delete,
    KeyCode::Insert,
    KeyCode::Null,
    KeyCode::Esc,
    KeyCode::CapsLock,
    KeyCode::ScrollLock,
    KeyCode::NumLock,
    KeyCode::PrintScreen,
    KeyCode::Pause,
    KeyCode::Menu,
    KeyCode::KeypadBegin,
];

const MODIFIER_KEYS: [ModifierKeyCode; 14] = [
    ModifierKeyCode::LeftShift,
    ModifierKeyCode::LeftControl,
    ModifierKeyCode::LeftAlt,
    ModifierKeyCode::LeftSuper,
    ModifierKeyCode::LeftHyper,
    ModifierKeyCode::LeftMeta,
    ModifierKeyCode::RightShift,
    ModifierKeyCode::RightControl,
    ModifierKeyCode::RightAlt,
    ModifierKeyCode::RightSuper,
    ModifierKeyCode::RightHyper,
    ModifierKeyCode::RightMeta,
    ModifierKeyCode::IsoLevel3Shift,
    ModifierKeyCode::IsoLevel5Shift,
];

const KINDS: [KeyEventKind; 3] = [
    KeyEventKind::Press,
    KeyEventKind::Repeat,
    KeyEventKind::Release,
];

const MOUSE: [MouseEventKind; 14] = [
    MouseEventKind::Down(MB::Left),
    MouseEventKind::Down(MB::Right),
    MouseEventKind::Down(MB::Middle),
    MouseEventKind::Up(MB::Left),
    MouseEventKind::Up(MB::Right),
    MouseEventKind::Up(MB::Middle),
    MouseEventKind::Drag(MB::Left),
    MouseEventKind::Drag(MB::Right),
    MouseEventKind::Drag(MB::Middle),
    MouseEventKind::Moved,
    MouseEventKind::ScrollDown,
    MouseEventKind::ScrollUp,
    MouseEventKind::ScrollLeft,
    MouseEventKind::ScrollRight,
];

fn index<T: PartialEq + std::fmt::Debug>(all: &[T], x: &T) -> usize {
    all.iter()
        .position(|a| a == x)
        .unwrap_or_else(|| panic!("{x:?} has no line form here"))
}

fn wire(ev: &CEvent) -> String {
    match ev {
        CEvent::Key(k) => {
            let code = match k.code {
                KeyCode::Char(c) => format!("c{}", c as u32),
                KeyCode::F(n) => format!("f{n}"),
                KeyCode::Modifier(m) => format!("m{}", index(&MODIFIER_KEYS, &m)),
                other => format!("n{}", index(&NAMED, &other)),
            };
            format!(
                "key {code} {} {} {}",
                k.modifiers.bits(),
                index(&KINDS, &k.kind),
                k.state.bits()
            )
        }
        CEvent::Mouse(m) => format!(
            "mouse {} {} {} {}",
            index(&MOUSE, &m.kind),
            m.column,
            m.row,
            m.modifiers.bits()
        ),
        CEvent::Paste(p) => {
            let hex: String = p.bytes().map(|b| format!("{b:02x}")).collect();
            format!("paste {hex}")
        }
        CEvent::FocusGained => "focus 1".to_string(),
        CEvent::FocusLost => "focus 0".to_string(),
        CEvent::Resize(cols, rows) => format!("resize {cols} {rows}"),
    }
}

fn unwire(line: &str) -> CEvent {
    let mut w = line.split(' ');
    let mut next = || w.next().unwrap_or("");
    let num = |s: &str| -> u32 { s.parse().unwrap_or_else(|_| panic!("{s:?} in {line:?}")) };
    match next() {
        "key" => {
            let code = next();
            let n = num(&code[1..]);
            let code = match &code[..1] {
                "c" => KeyCode::Char(char::from_u32(n).unwrap()),
                "f" => KeyCode::F(n as u8),
                "m" => KeyCode::Modifier(MODIFIER_KEYS[n as usize]),
                _ => NAMED[n as usize],
            };
            let mods = KeyModifiers::from_bits_truncate(num(next()) as u8);
            let kind = KINDS[num(next()) as usize];
            let state = KeyEventState::from_bits_truncate(num(next()) as u8);
            CEvent::Key(KeyEvent::new_with_kind_and_state(code, mods, kind, state))
        }
        "mouse" => {
            let kind = MOUSE[num(next()) as usize];
            let (column, row) = (num(next()) as u16, num(next()) as u16);
            let modifiers = KeyModifiers::from_bits_truncate(num(next()) as u8);
            CEvent::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers,
            })
        }
        "paste" => {
            let hex = next();
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
                .collect();
            CEvent::Paste(String::from_utf8(bytes).unwrap())
        }
        "focus" if next() == "1" => CEvent::FocusGained,
        "focus" => CEvent::FocusLost,
        "resize" => CEvent::Resize(num(next()) as u16, num(next()) as u16),
        other => panic!("not an event line: {other:?} in {line:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// What a terminal sends, and what a window would have given.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Enc {
    Legacy,
    /// The kitty keyboard protocol, flag 1.
    Kitty,
}

const ENCODINGS: [Enc; 2] = [Enc::Legacy, Enc::Kitty];

/// The modifier keys held with a key; `cmd` is ⌘ (the kitty protocol's SUPER).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    shift: bool,
    alt: bool,
    ctrl: bool,
    cmd: bool,
}

const PLAIN: Held = Held {
    shift: false,
    alt: false,
    ctrl: false,
    cmd: false,
};
const SHIFT: Held = Held {
    shift: true,
    ..PLAIN
};
const ALT: Held = Held { alt: true, ..PLAIN };
const CTRL: Held = Held {
    ctrl: true,
    ..PLAIN
};
const CMD: Held = Held { cmd: true, ..PLAIN };

impl Held {
    /// What a terminal holds for a row's modifiers: Cmd/Ctrl as Ctrl, or as ⌘ (`cmd`).
    fn of(mods: Mods, cmd: bool) -> Held {
        let command = if cmd { CMD } else { CTRL };
        match mods {
            Mods::None => PLAIN,
            Mods::Shift => SHIFT,
            Mods::Alt => ALT,
            Mods::Command => command,
            Mods::CommandShift => Held {
                shift: true,
                ..command
            },
            Mods::MacCmd => CMD,
        }
    }
}

/// The character `key` types unshifted, for a key egui names by one (`a`, `1`, ` `, `-`).
fn char_of(key: Key) -> Option<char> {
    (' '..='~')
        .filter(|c| !c.is_ascii_uppercase())
        .find(|c| Key::from_name(&c.to_string()) == Some(key))
}

/// The bytes a terminal sends for `key` with `held` in `enc`; `None`: it cannot send it (⌘ in the
/// legacy encoding).
fn sent(key: Key, held: Held, enc: Enc) -> Option<Vec<u8>> {
    use Key as K;
    let kitty = enc == Enc::Kitty;
    if held.cmd && !kitty {
        return None;
    }
    let m = 1
        + u8::from(held.shift)
        + 2 * u8::from(held.alt)
        + 4 * u8::from(held.ctrl)
        + 8 * u8::from(held.cmd);
    let modded = m > 1;
    // `CSI number ; modifier ~`.
    let tilde = match key {
        K::Insert => 2,
        K::Delete => 3,
        K::PageUp => 5,
        K::PageDown => 6,
        K::F3 if kitty => 13,
        K::F5 => 15,
        K::F6 => 17,
        K::F7 => 18,
        K::F8 => 19,
        K::F9 => 20,
        K::F10 => 21,
        K::F11 => 23,
        K::F12 => 24,
        _ => 0,
    };
    if tilde > 0 {
        let s = if modded {
            format!("\x1b[{tilde};{m}~")
        } else {
            format!("\x1b[{tilde}~")
        };
        return Some(s.into_bytes());
    }
    // `CSI 1 ; modifier X`, unmodified `CSI X` — F1–F4 as `SS3 X` in the legacy encoding.
    let letter = match key {
        K::ArrowUp => Some('A'),
        K::ArrowDown => Some('B'),
        K::ArrowRight => Some('C'),
        K::ArrowLeft => Some('D'),
        K::End => Some('F'),
        K::Home => Some('H'),
        K::F1 => Some('P'),
        K::F2 => Some('Q'),
        K::F3 => Some('R'),
        K::F4 => Some('S'),
        _ => None,
    };
    if let Some(x) = letter {
        let ss3 = !kitty && matches!(key, K::F1 | K::F2 | K::F3 | K::F4);
        let s = match (modded, ss3) {
            (true, _) => format!("\x1b[1;{m}{x}"),
            (false, true) => format!("\x1bO{x}"),
            (false, false) => format!("\x1b[{x}"),
        };
        return Some(s.into_bytes());
    }
    let alt = if held.alt { "\x1b" } else { "" };
    let special = match key {
        K::Escape => Some((27, "\x1b")),
        K::Enter => Some((13, "\r")),
        K::Tab => Some((9, "\t")),
        K::Backspace => Some((127, "\x7f")),
        _ => None,
    };
    if let Some((code, plain)) = special {
        // kitty: Esc always `CSI 27 u`; Enter, Tab and Backspace unmodified stay legacy.
        let s = match (kitty, modded) {
            (true, true) => format!("\x1b[{code};{m}u"),
            (true, false) if key == K::Escape => format!("\x1b[{code}u"),
            (true, false) => plain.to_string(),
            _ if key == K::Tab && held == SHIFT => "\x1b[Z".to_string(),
            _ if key == K::Backspace && held.ctrl => format!("{alt}\x08"),
            _ => format!("{alt}{plain}"),
        };
        return Some(s.into_bytes());
    }
    let c = char_of(key).unwrap_or_else(|| panic!("no bytes for {key:?} in this encoder"));
    if kitty && (held.ctrl || held.alt || held.cmd) {
        return Some(format!("\x1b[{};{m}u", c as u32).into_bytes());
    }
    let typed = if held.ctrl {
        // xterm: Ctrl+letter is its control code (Shift is lost), Ctrl+Space and Ctrl+2 NUL,
        // Ctrl+3…8 the codes below; Ctrl+1, 9 and 0 the digit itself.
        match c {
            'a'..='z' => char::from(c as u8 - b'a' + 1),
            ' ' | '2' => '\0',
            '3' => '\x1b',
            '4' => '\x1c',
            '5' => '\x1d',
            '6' => '\x1e',
            '7' => '\x1f',
            '8' => '\x7f',
            other => other,
        }
    } else if held.shift {
        let (from, to) = (r"1234567890-=[]\;',./`", "!@#$%^&*()_+{}|:\"<>?~");
        match from.chars().position(|f| f == c) {
            Some(i) => to.chars().nth(i).unwrap(),
            None => c.to_ascii_uppercase(),
        }
    } else {
        c
    };
    Some(format!("{alt}{typed}").into_bytes())
}

/// A window's events for one press of `key` with `mods` (on a Mac when `mac`), as egui-winit
/// sends them: the key, its text when it types one, its release.
fn window(key: Key, mods: Mods, mac: bool) -> Vec<Event> {
    let command = if mac {
        Modifiers::MAC_CMD | Modifiers::COMMAND
    } else {
        Modifiers::CTRL | Modifiers::COMMAND
    };
    let modifiers = match mods {
        Mods::None => Modifiers::NONE,
        Mods::Shift => Modifiers::SHIFT,
        Mods::Alt => Modifiers::ALT,
        Mods::Command => command,
        Mods::CommandShift => command | Modifiers::SHIFT,
        Mods::MacCmd => Modifiers::MAC_CMD | Modifiers::COMMAND,
    };
    let event = |pressed| Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    };
    let mut out = vec![event(true)];
    if let Some(c) = char_of(key).filter(|_| !modifiers.command && !modifiers.ctrl) {
        out.push(Event::Text(c.to_string()));
    }
    out.push(event(false));
    out
}

/// A window's events for typed `text`: per character its key and release where egui names one.
fn window_text(text: &str) -> Vec<Event> {
    text.chars()
        .flat_map(|c| {
            let key = c
                .is_ascii()
                .then(|| Key::from_name(&c.to_string()))
                .flatten();
            let modifiers = if c.is_uppercase() {
                Modifiers::SHIFT
            } else {
                Modifiers::NONE
            };
            let event = |key, pressed| Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            };
            let mut out: Vec<Event> = key.map(|k| event(k, true)).into_iter().collect();
            out.push(Event::Text(c.to_string()));
            out.extend(key.map(|k| event(k, false)));
            out
        })
        .collect()
}

/// What one pass makes of `events` with the keyboard in `ctx`: the shell's F-key aliases first
/// (`FKeyInput`'s hook; a field has the typing in a dialog), then the key table until it gives no
/// more. The commands, and the events left for the widgets.
fn pass(
    mut events: Vec<Event>,
    ctx: Context,
    digits: &mut [Digit; 10],
) -> (Vec<Command>, Vec<Event>) {
    fkey_events(&mut events, digits, ctx == Context::Dialog, true, None);
    let commands = std::iter::from_fn(|| keys::find(&mut events, ctx, ALL_SLICES)).collect();
    (commands, events)
}

fn commands(events: Vec<Event>, ctx: Context, digits: &mut [Digit; 10]) -> Vec<Command> {
    pass(events, ctx, digits).0
}

fn translated(input: &mut TermInput, decoded: &[CEvent]) -> Vec<Event> {
    decoded.iter().flat_map(|e| input.translate(e)).collect()
}

/// `events` without the modifiers' changes: the keys, text and pointer.
fn plain(events: Vec<Event>) -> Vec<Event> {
    events
        .into_iter()
        .filter(|e| !matches!(e, Event::ModifiersChanged(_)))
        .collect()
}

/// The text of `events`, joined.
fn text(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

fn key(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    }
}

const HERE: bool = cfg!(target_os = "macos");
const CTRL_COMMAND: Modifiers = Modifiers {
    ctrl: true,
    command: true,
    ..Modifiers::NONE
};

// ---------------------------------------------------------------------------------------------
// The key table, row by row (TK-1).

/// One key of the replay: its bytes (`None`: the terminal cannot send it), where the keyboard is,
/// whether it is played as on a Mac, and the window's press it must equal.
struct Play {
    label: String,
    ctx: Context,
    bytes: Option<Vec<u8>>,
    mac: bool,
    window: Vec<Event>,
}

/// Every key row of this build's table (all slices); under kitty each Cmd/Ctrl row again with ⌘;
/// the shell's Ctrl+1…0 aliases (with ⌘ under kitty) in the list; typed `+ - *`, letters and
/// Hangul — as the spike's player sent them, and as many: a replay that shrinks fails here.
fn plays(enc: Enc) -> Vec<Play> {
    let mut plays = vec![];
    let mut rows = 0;
    for ctx in Context::ALL {
        for row in keys::rows(ctx, ALL_SLICES) {
            let Trigger::Key(k, mods) = row.trigger else {
                continue;
            };
            rows += 1;
            let label = format!("{ctx:?} {mods:?}+{}", k.name());
            if enc == Enc::Kitty && matches!(mods, Mods::Command | Mods::CommandShift) {
                plays.push(Play {
                    label: format!("{label} (⌘)"),
                    ctx,
                    bytes: sent(k, Held::of(mods, true), enc),
                    mac: true,
                    window: window(k, mods, true),
                });
            }
            plays.push(Play {
                label,
                ctx,
                bytes: sent(k, Held::of(mods, false), enc),
                mac: HERE,
                window: window(k, mods, HERE),
            });
        }
    }
    for n in 1..=10 {
        let d = n % 10;
        let digit = Key::from_name(&d.to_string()).unwrap();
        plays.push(Play {
            label: format!("List Ctrl+{d}"),
            ctx: Context::List,
            bytes: sent(digit, CTRL, enc),
            mac: HERE,
            window: window(digit, Mods::Command, HERE),
        });
        if enc == Enc::Kitty {
            plays.push(Play {
                label: format!("List ⌘+{d}"),
                ctx: Context::List,
                bytes: sent(digit, CMD, enc),
                mac: true,
                window: window(digit, Mods::Command, true),
            });
        }
    }
    for typed in ["+", "-", "*", "n", "Z", "한", "새"] {
        plays.push(Play {
            label: format!("List typed {typed}"),
            ctx: Context::List,
            bytes: Some(typed.as_bytes().to_vec()),
            mac: HERE,
            window: window_text(typed),
        });
    }
    // The spike's counts (macOS): 101 rows — the ⌘⌫ row is a Mac's — and the viewer's 1, 2, 3
    // and F9 since VM-7 (08 §8), 26 ⌘ variants of them and 20 aliases under kitty (10 in the
    // legacy encoding, which cannot send ⌘), 7 typed.
    let count = |pick: fn(&Play) -> bool| plays.iter().filter(|p| pick(p)).count();
    let cmd_rows = count(|p| p.label.ends_with("(⌘)"));
    let aliases = count(|p| p.label.starts_with("List Ctrl+") || p.label.starts_with("List ⌘+"));
    let kitty = enc == Enc::Kitty;
    assert_eq!(
        (
            rows,
            cmd_rows,
            aliases,
            count(|p| p.label.contains("typed"))
        ),
        (
            if HERE { 105 } else { 104 },
            if kitty { 26 } else { 0 },
            if kitty { 20 } else { 10 },
            7
        ),
        "{enc:?}: rows, ⌘ variants, aliases, typed"
    );
    assert_eq!(plays.len(), rows + cmd_rows + aliases + 7);
    plays
}

/// The plays of `enc` whose commands are not the window press's, with what they gave instead.
fn losses(enc: Enc) -> Vec<(String, Vec<Command>)> {
    let mut decoder = Decoder::start();
    let (mut ours, mut theirs) = (Default::default(), Default::default());
    let mut lost = vec![];
    for play in plays(enc) {
        let want = commands(play.window, play.ctx, &mut theirs);
        assert!(!want.is_empty(), "{}: the window's press acts", play.label);
        let got = match &play.bytes {
            None => vec![],
            Some(bytes) => {
                let mut input = TermInput::new(play.mac);
                let events = translated(&mut input, &decoder.decode(bytes));
                commands(events, play.ctx, &mut ours)
            }
        };
        if got != want {
            lost.push((play.label, got));
        }
    }
    lost
}

/// Under the kitty protocol every key row, every ⌘ variant and every Ctrl/⌘+digit alias resolves
/// the window press's command, and typed text acts as typed in a window: nothing is lost.
#[test]
fn tk1_under_kitty_every_row_alias_and_cmd_variant_reaches_the_windows_command() {
    assert_eq!(losses(Enc::Kitty), []);
}

/// The legacy encoding loses exactly §9's 17 (16 off macOS, whose table has no ⌘⌫ row), all in
/// what the terminal sends and crossterm decodes — each asserted with what it does instead, so a
/// crossterm or terminal change that brings one back shows here.
#[test]
fn tk1_the_legacy_encoding_loses_exactly_the_seventeen() {
    use Command as C;
    let lost = |label: &str, fired: &[C]| (label.to_string(), fired.to_vec());
    let mut want = vec![];
    if HERE {
        // A legacy terminal cannot send ⌘ (Del and F8 are its routes).
        want.push(lost("List MacCmd+Backspace", &[]));
    }
    want.extend([
        // `CSI 1;2R` reads as a cursor report.
        lost("Git Shift+F3", &[]),
        // Ctrl+Shift+letter: Shift is lost, the Ctrl chord arrives.
        lost("Git CommandShift+F", &[C::Find]),
        lost("Git CommandShift+R", &[C::Reread]),
        lost("Git CommandShift+G", &[C::Pin]),
        lost("Git CommandShift+C", &[]),
        lost("Git CommandShift+S", &[]),
        lost("Git CommandShift+A", &[]),
        lost("Git CommandShift+B", &[C::Backups]),
        // Ctrl+M is CR: Enter.
        lost("Git CommandShift+M", &[C::Details]),
        lost("Git CommandShift+P", &[]),
        // Ctrl+1, 9, 0 are the digits; Ctrl+2 is Ctrl+Space; Ctrl+3 ESC; Ctrl+8 DEL.
        lost("List Ctrl+1", &[C::Jump('1')]),
        lost("List Ctrl+2", &[]),
        lost("List Ctrl+3", &[C::Stop]),
        lost("List Ctrl+8", &[C::Up]),
        lost("List Ctrl+9", &[C::Jump('9')]),
        lost("List Ctrl+0", &[C::Jump('0')]),
    ]);
    assert_eq!(losses(Enc::Legacy), want);
}

// ---------------------------------------------------------------------------------------------
// Text (T25, TK-3).

/// T25: every printable ASCII character egui names a key for arrives as `Key` and `Text` (then its
/// release), Shift with an uppercase letter as a window has it; the other fourteen as `Text`
/// alone, as do characters egui names keys for outside ASCII. A typed character is the same bytes
/// in both encodings (kitty's flag 1 leaves text as text), so this plays them once.
#[test]
fn t25_a_printable_ascii_key_is_key_and_text_other_characters_text_alone() {
    let mut decoder = Decoder::start();
    let mut input = TermInput::new(HERE);
    let (mut keyed, mut alone) = (0, String::new());
    for c in ' '..='~' {
        let decoded = decoder.decode(c.to_string().as_bytes());
        let shift = if c.is_ascii_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        assert_eq!(
            decoded,
            [CEvent::Key(KeyEvent::new(KeyCode::Char(c), shift))],
            "{c:?} as crossterm reads it"
        );
        let events = plain(translated(&mut input, &decoded));
        let typed = Event::Text(c.to_string());
        match Key::from_name(&c.to_string()) {
            Some(k) => {
                keyed += 1;
                let m = if c.is_ascii_uppercase() {
                    Modifiers::SHIFT
                } else {
                    Modifiers::NONE
                };
                assert_eq!(events, [key(k, true, m), typed, key(k, false, m)], "{c:?}");
            }
            None => {
                assert_eq!(events, [typed], "{c:?}");
                alone.push(c);
            }
        }
    }
    let mut spec: Vec<char> = "#$%&()*<>@^_~\"".chars().collect();
    spec.sort();
    assert_eq!(alone, spec.into_iter().collect::<String>(), "§4's list");
    assert_eq!(keyed, 95 - 14);
    // egui names keys for these too; a terminal can only have typed them.
    for c in ['⏷', '−'] {
        let events = translated(&mut input, &decoder.decode(c.to_string().as_bytes()));
        assert_eq!(events, [Event::Text(c.to_string())], "{c:?}");
    }
}

/// Space selects in the list, marks in the git panel and sizes in the tree, in both encodings
/// (TK-1); a plain letter is a key for the viewers (`W`, `P`, `F`, `H`) and a jump in the list.
#[test]
fn t25_space_acts_as_a_key_and_a_letter_as_a_key_and_a_jump() {
    use Command as C;
    let mut decoder = Decoder::start();
    let mut digits = Default::default();
    for enc in ENCODINGS {
        let space = decoder.decode(&sent(Key::Space, PLAIN, enc).unwrap());
        for (ctx, want) in [
            (Context::List, C::Select),
            (Context::Git, C::Mark),
            (Context::Tree, C::SizeNow),
        ] {
            let events = translated(&mut TermInput::new(HERE), &space);
            assert_eq!(
                commands(events, ctx, &mut digits),
                [want],
                "{enc:?} {ctx:?}"
            );
        }
        let w = decoder.decode(&sent(Key::W, PLAIN, enc).unwrap());
        let events = translated(&mut TermInput::new(HERE), &w);
        assert!(
            events.contains(&key(Key::W, true, Modifiers::NONE)),
            "{enc:?}"
        );
        assert_eq!(commands(events, Context::List, &mut digits), [C::Jump('w')]);
    }
}

/// TK-3: a Hangul name typed into the F7 field arrives whole, its syllables as `Text` alone (a
/// terminal receives an input method's commit as UTF-8; it draws the composition itself); a
/// Hangul letter jumps in the list.
#[test]
fn tk3_hangul_typed_into_a_field_arrives_whole_as_text_alone() {
    let mut decoder = Decoder::start();
    let mut input = TermInput::new(HERE);
    let mut digits = Default::default();
    let name = "새 폴더 2026";
    let mut events = vec![];
    for c in name.chars() {
        events.extend(translated(
            &mut input,
            &decoder.decode(c.to_string().as_bytes()),
        ));
    }
    let (commands_, left) = pass(events.clone(), Context::Dialog, &mut digits);
    assert_eq!(commands_, []);
    assert_eq!(text(&left), name);
    let keyed: Vec<Key> = events
        .iter()
        .filter_map(|e| match e {
            Event::Key {
                key, pressed: true, ..
            } => Some(*key),
            _ => None,
        })
        .collect();
    assert_eq!(
        keyed,
        [
            Key::Space,
            Key::Space,
            Key::Num2,
            Key::Num0,
            Key::Num2,
            Key::Num6
        ],
        "no key for a syllable"
    );
    let jump = translated(&mut input, &decoder.decode("새".as_bytes()));
    assert_eq!(jump, [Event::Text("새".to_string())]);
    assert_eq!(
        commands(jump, Context::List, &mut digits),
        [Command::Jump('새')]
    );
}

// ---------------------------------------------------------------------------------------------
// Ctrl, ⌘ and the clipboard keys (T26).

/// T26: Ctrl+R arrives as `ctrl` + `command` whether the build is a Mac's or not and rereads in the
/// list, the git panel and the tree — with egui-winit's Mac rule (⌃ as `ctrl` alone) no Cmd/Ctrl
/// row would match; Ctrl+J, sent as LF, reaches `ToHead` under legacy too (raw mode); ⌘ passes
/// only under kitty and is `mac_cmd` + `command` on a Mac, nothing elsewhere. Raw mode clears ISIG
/// and IXON: Ctrl+C, Z, S and Q arrive as keys.
#[test]
fn t26_ctrl_is_command_on_every_os_and_cmd_only_under_kitty() {
    use Command as C;
    let mut decoder = Decoder::start();
    let mut digits = Default::default();
    for enc in ENCODINGS {
        let r = decoder.decode(&sent(Key::R, CTRL, enc).unwrap());
        for mac in [true, false] {
            let events = plain(translated(&mut TermInput::new(mac), &r));
            assert_eq!(
                events[0],
                key(Key::R, true, CTRL_COMMAND),
                "{enc:?} mac {mac}"
            );
            for ctx in [Context::List, Context::Git, Context::Tree] {
                assert_eq!(
                    commands(events.clone(), ctx, &mut digits),
                    [C::Reread],
                    "{enc:?} mac {mac} {ctx:?}"
                );
            }
        }
        let bytes = sent(Key::J, CTRL, enc).unwrap();
        if enc == Enc::Legacy {
            assert_eq!(bytes, b"\n");
        }
        let j = translated(&mut TermInput::new(HERE), &decoder.decode(&bytes));
        assert_eq!(
            commands(j, Context::Git, &mut digits),
            [C::ToHead],
            "{enc:?}"
        );
        for c in ['c', 'z', 's', 'q'] {
            let bytes = sent(Key::from_name(&c.to_string()).unwrap(), CTRL, enc).unwrap();
            assert_eq!(
                decoder.decode(&bytes),
                [CEvent::Key(KeyEvent::new(
                    KeyCode::Char(c),
                    KeyModifiers::CONTROL
                ))],
                "{enc:?} Ctrl+{c}"
            );
        }
    }
    let row = [key(Key::R, true, Modifiers::CTRL)];
    assert_eq!(commands(row.to_vec(), Context::List, &mut digits), []);
    // A field's select-all (egui's COMMAND shortcut) sees Ctrl+A.
    assert!(CTRL_COMMAND.matches_logically(Modifiers::COMMAND));
    // ⌘: never in the legacy encoding; under kitty a Mac's Cmd, elsewhere no modifier at all.
    assert_eq!(sent(Key::R, CMD, Enc::Legacy), None);
    let cmd_r = decoder.decode(&sent(Key::R, CMD, Enc::Kitty).unwrap());
    let on_mac = plain(translated(&mut TermInput::new(true), &cmd_r));
    let mac_command = Modifiers::MAC_CMD | Modifiers::COMMAND;
    assert_eq!(
        on_mac,
        [
            key(Key::R, true, mac_command),
            key(Key::R, false, mac_command)
        ]
    );
    assert_eq!(commands(on_mac, Context::List, &mut digits), [C::Reread]);
    let elsewhere = translated(&mut TermInput::new(false), &cmd_r);
    assert_eq!(elsewhere[0], key(Key::R, true, Modifiers::NONE));
}

/// Cmd/Ctrl+X and +C are `Cut` and `Copy` alone, then the letter's release, as egui-winit hands
/// them over; Cmd/Ctrl+V gives nothing — the terminal's own paste is bracketed `Paste`, its CRLFs
/// and lone CRs as LFs, then V's release; an empty paste is nothing.
#[test]
fn ctrl_x_and_c_are_cut_and_copy_ctrl_v_nothing_the_terminals_paste_a_paste() {
    let mut decoder = Decoder::start();
    let mut held = vec![(CTRL, HERE, CTRL_COMMAND)];
    held.push((CMD, true, Modifiers::MAC_CMD | Modifiers::COMMAND));
    for enc in ENCODINGS {
        for (with, mac, modifiers) in held.clone() {
            let Some(x) = sent(Key::X, with, enc) else {
                continue;
            };
            let mut input = TermInput::new(mac);
            let mut play = |bytes: &[u8]| plain(translated(&mut input, &decoder.decode(bytes)));
            assert_eq!(play(&x), [Event::Cut, key(Key::X, false, modifiers)]);
            let c = sent(Key::C, with, enc).unwrap();
            assert_eq!(play(&c), [Event::Copy, key(Key::C, false, modifiers)]);
            let v = sent(Key::V, with, enc).unwrap();
            assert_eq!(play(&v), [], "{enc:?}");
        }
    }
    let pasted = decoder.decode(b"\x1b[200~a\r\nb\rc \xed\x95\x9c\x1b[201~");
    assert_eq!(
        translated(&mut TermInput::new(HERE), &pasted),
        [
            Event::Paste("a\nb\nc 한".to_string()),
            key(Key::V, false, Modifiers::NONE)
        ]
    );
    assert_eq!(
        translated(
            &mut TermInput::new(HERE),
            &decoder.decode(b"\x1b[200~\x1b[201~")
        ),
        [],
        "an empty paste"
    );
}

/// A window's paste is Cmd/Ctrl+V's `Paste`, then V's key-up, and the image and media windows and
/// the view keys hold the paste until that release: a terminal's paste ends with it too, so a
/// second paste is a paste, and F9 then `v` is a key after F9 — it ends the armed F9 — not a
/// repeat of a V still held.
#[test]
fn a_terminals_paste_lets_go_of_v_as_the_windows_does() {
    use crate::view_keys::{State, ViewKeys};
    let mut decoder = Decoder::start();
    for enc in ENCODINGS {
        let mut input = TermInput::new(HERE);
        let mut keys = ViewKeys::new(RELEASES, []);
        let pasted = translated(&mut input, &decoder.decode(b"\x1b[200~notes\x1b[201~"));
        keys.route(State::Free, &pasted);
        let f9 = translated(
            &mut input,
            &decoder.decode(&sent(Key::F9, PLAIN, enc).unwrap()),
        );
        keys.route(State::Free, &f9);
        assert!(keys.armed(), "{enc:?}: F9 arms");
        let v = translated(
            &mut input,
            &decoder.decode(&sent(Key::V, PLAIN, enc).unwrap()),
        );
        keys.route(State::Free, &v);
        assert!(!keys.armed(), "{enc:?}: v after F9 ends it");
    }
}

// ---------------------------------------------------------------------------------------------
// Alt+digits, Alt+G, F12 (T7, T33, T4).

/// Alt+1…0 and Alt+G arrive as their key with `alt` alone and no text, in both encodings — `ESC
/// <char>` in the legacy one — so no key-table row and no letter jump takes them in any context:
/// the shell's input hook maps them (§9, `FKeyInput`). F12 arrives as F12, no row's either.
#[test]
fn t7_t33_alt_digits_alt_g_and_f12_arrive_as_keys_no_row_takes() {
    let mut decoder = Decoder::start();
    let mut digits = Default::default();
    let mut keys: Vec<Key> = (0..10)
        .map(|d| Key::from_name(&d.to_string()).unwrap())
        .collect();
    keys.push(Key::G);
    assert_eq!(sent(Key::G, ALT, Enc::Legacy).unwrap(), b"\x1bg");
    assert_eq!(sent(Key::Num1, ALT, Enc::Legacy).unwrap(), b"\x1b1");
    for enc in ENCODINGS {
        for k in keys.iter().copied() {
            let decoded = decoder.decode(&sent(k, ALT, enc).unwrap());
            let events = plain(translated(&mut TermInput::new(HERE), &decoded));
            assert_eq!(
                events,
                [key(k, true, Modifiers::ALT), key(k, false, Modifiers::ALT)],
                "{enc:?} Alt+{}",
                k.name()
            );
            for ctx in Context::ALL {
                let (commands_, _) = pass(events.clone(), ctx, &mut digits);
                assert_eq!(commands_, [], "{enc:?} Alt+{} in {ctx:?}", k.name());
            }
        }
        let f12 = decoder.decode(&sent(Key::F12, PLAIN, enc).unwrap());
        let events = plain(translated(&mut TermInput::new(HERE), &f12));
        assert_eq!(
            events,
            [
                key(Key::F12, true, Modifiers::NONE),
                key(Key::F12, false, Modifiers::NONE)
            ]
        );
        for ctx in Context::ALL {
            assert_eq!(commands(events.clone(), ctx, &mut digits), [], "{ctx:?}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Releases.

/// Every press is followed by its release, so the shell decides each digit at its press: legacy
/// Ctrl+5 then a plain 5 is F5, then the digit — without the release the 5 would still be F5
/// (`FKeyInput` keeps a digit's meaning until it comes up) — and egui sees two first presses of a
/// key typed twice and nothing left down. A release the terminal reports (REPORT_EVENT_TYPES) is
/// not passed on, a repeat is a press, and a modifier key alone (flag 8) changes the modifiers.
#[test]
fn every_press_comes_up_at_once() {
    use Command as C;
    let mut decoder = Decoder::start();
    let mut input = TermInput::new(HERE);
    let ctrl5 = translated(&mut input, &decoder.decode(b"\x1d"));
    let five = translated(&mut input, &decoder.decode(b"5"));
    let mut digits = Default::default();
    assert_eq!(
        commands(ctrl5.clone(), Context::List, &mut digits),
        [C::Copy]
    );
    assert_eq!(
        commands(five.clone(), Context::List, &mut digits),
        [C::Jump('5')]
    );
    let downs = |events: Vec<Event>| -> Vec<Event> {
        let up = |e: &Event| matches!(e, Event::Key { pressed: false, .. });
        events.into_iter().filter(|e| !up(e)).collect()
    };
    let mut digits = Default::default();
    assert_eq!(
        commands(downs(ctrl5), Context::List, &mut digits),
        [C::Copy]
    );
    assert_eq!(
        commands(downs(five), Context::List, &mut digits),
        [C::Copy],
        "the negative control: the 5 held as F5"
    );
    let n = translated(&mut input, &decoder.decode(b"n"));
    let seen = |events: Vec<Event>| {
        let ctx = egui::Context::default();
        let mut seen = (0, 0, false);
        let raw = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            ui.input(|i| {
                for e in &i.events {
                    if let Event::Key {
                        key: Key::N,
                        pressed: true,
                        repeat,
                        ..
                    } = e
                    {
                        seen.0 += 1;
                        seen.1 += usize::from(*repeat);
                    }
                }
                seen.2 = i.keys_down.is_empty();
            })
        });
        out.textures_delta.clear();
        seen
    };
    assert_eq!(seen([n.clone(), n.clone()].concat()), (2, 0, true));
    assert_eq!(
        seen(downs([n.clone(), n].concat())),
        (2, 1, false),
        "the negative control"
    );
    // The kitty protocol's own releases and repeats, and a modifier key alone.
    let mut input = TermInput::new(HERE);
    let r = translated(&mut input, &decoder.decode(b"\x1b[114;5u"));
    let r_up = decoder.decode(b"\x1b[114;5:3u");
    assert_eq!(
        r_up,
        [CEvent::Key(KeyEvent::new_with_kind(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
            KeyEventKind::Release
        ))]
    );
    assert_eq!(
        translated(&mut input, &r_up),
        [],
        "its release came with {r:?}"
    );
    let repeat = translated(&mut input, &decoder.decode(b"\x1b[110;1:2u"));
    assert_eq!(
        repeat,
        [
            Event::ModifiersChanged(Modifiers::NONE),
            key(Key::N, true, Modifiers::NONE),
            Event::Text("n".into()),
            key(Key::N, false, Modifiers::NONE)
        ]
    );
    let ctrl_down = translated(&mut input, &decoder.decode(b"\x1b[57442;5u"));
    assert_eq!(ctrl_down, [Event::ModifiersChanged(CTRL_COMMAND)]);
    assert_eq!(input.modifiers(), CTRL_COMMAND);
    let ctrl_up = translated(&mut input, &decoder.decode(b"\x1b[57442;5:3u"));
    assert_eq!(ctrl_up, [Event::ModifiersChanged(Modifiers::NONE)]);
}

// ---------------------------------------------------------------------------------------------
// The mouse, focus, size.

/// A mouse report is the pointer at its cell's centre (8×16 cells), a press and release there; a
/// drag or move the pointer alone; the wheel by lines with winit's signs; its modifiers are
/// reported. Focus reports are `WindowFocused`, losing it forgets the modifiers. A resize (no
/// bytes: the terminal says it by SIGWINCH) is the size.
#[test]
fn the_mouse_at_the_cell_centre_the_wheel_by_lines_focus_as_window_focus() {
    let mut decoder = Decoder::start();
    let mut input = TermInput::new(HERE);
    let mut play = |bytes: &[u8]| translated(&mut input, &decoder.decode(bytes));
    let at = cell_centre(10, 5);
    assert_eq!(at, Pos2::new(84.0, 88.0));
    let button = |button, pressed, modifiers| Event::PointerButton {
        pos: at,
        button,
        pressed,
        modifiers,
    };
    let none = Modifiers::NONE;
    use PointerButton::{Middle, Primary, Secondary};
    assert_eq!(
        play(b"\x1b[<0;11;6M"),
        [Event::PointerMoved(at), button(Primary, true, none)]
    );
    assert_eq!(
        play(b"\x1b[<0;11;6m"),
        [Event::PointerMoved(at), button(Primary, false, none)]
    );
    assert_eq!(
        play(b"\x1b[<2;11;6M"),
        [Event::PointerMoved(at), button(Secondary, true, none)]
    );
    assert_eq!(
        play(b"\x1b[<1;11;6M"),
        [Event::PointerMoved(at), button(Middle, true, none)]
    );
    assert_eq!(
        play(b"\x1b[<32;12;6M"),
        [Event::PointerMoved(cell_centre(11, 5))]
    );
    assert_eq!(
        play(b"\x1b[<35;1;1M"),
        [Event::PointerMoved(Pos2::new(4.0, 8.0))]
    );
    for (cb, x, y) in [
        (64, 0.0, 1.0),
        (65, 0.0, -1.0),
        (66, 1.0, 0.0),
        (67, -1.0, 0.0),
    ] {
        let wheel = Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(x, y),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: none,
        };
        let bytes = format!("\x1b[<{cb};11;6M");
        assert_eq!(
            play(bytes.as_bytes()),
            [Event::PointerMoved(at), wheel],
            "{cb}"
        );
    }
    assert_eq!(
        play(b"\x1b[<16;11;6M"),
        [
            Event::ModifiersChanged(CTRL_COMMAND),
            Event::PointerMoved(at),
            button(Primary, true, CTRL_COMMAND)
        ]
    );
    assert_eq!(play(b"\x1b[<0;11;6m")[0], Event::ModifiersChanged(none));
    assert_eq!(play(b"\x1b[O"), [Event::WindowFocused(false)]);
    assert_eq!(play(b"\x1b[I"), [Event::WindowFocused(true)]);
    play(b"\x12");
    assert_eq!(play(b"\x1b[O"), [Event::WindowFocused(false)]);
    assert!(!input.focused());
    assert_eq!(input.modifiers(), none, "forgotten with the focus");
    assert_eq!(input.translate(&CEvent::Resize(120, 40)), []);
    assert_eq!(input.size(), Some((120, 40)));
}

// ---------------------------------------------------------------------------------------------
// The spike's three sequences and its Hangul one.

enum Input {
    Key(Key, Held),
    Typed(&'static str),
}

/// The three tests' sequences the spike replayed (`keys_move_enter_go_up_switch_panels_and_jump`,
/// `k_s1_selection_by_space_pattern_invert_and_all`, `k_n1_f2_renames_in_place`) and its Hangul
/// one, as terminal bytes in both encodings, a typed character at a time: each step gives the
/// command the original's window press gives, and what a field has the typing for reaches it
/// whole — Ctrl+A too, as egui's select-all. Their state checks read the browser's own fields
/// (`browser.rs:707-708`), so they stay with the browser's tests.
#[test]
fn the_three_sequences_and_hangul_give_the_windows_commands() {
    use keys::Step::{Down, End, Home};
    use Command as C;
    use Context::{Dialog, List};
    use Key as K;
    let k = |ctx, key, held, want: &[C]| (ctx, Input::Key(key, held), want.to_vec());
    let t = |ctx, typed, want: &[C]| (ctx, Input::Typed(typed), want.to_vec());
    let sequences = [
        vec![
            k(List, K::ArrowDown, PLAIN, &[C::Bar(Down)]),
            k(List, K::Enter, PLAIN, &[C::Open]),
            k(List, K::Backspace, PLAIN, &[C::Up]),
            k(List, K::Enter, PLAIN, &[C::Open]),
            k(List, K::Enter, PLAIN, &[C::Open]),
            t(List, "n", &[C::Jump('n')]),
            t(List, "Z", &[C::Jump('Z')]),
            t(List, "a", &[C::Jump('a')]),
            k(List, K::End, PLAIN, &[C::Bar(End)]),
            k(List, K::Home, PLAIN, &[C::Bar(Home)]),
            k(List, K::Tab, PLAIN, &[C::OtherSide]),
            t(List, "b", &[C::Jump('b')]),
            k(List, K::Tab, SHIFT, &[C::OtherSide]),
        ],
        vec![
            k(List, K::ArrowDown, PLAIN, &[C::Bar(Down)]),
            k(List, K::Space, PLAIN, &[C::Select]),
            k(List, K::Space, PLAIN, &[C::Select]),
            k(List, K::Space, PLAIN, &[C::Select]),
            k(List, K::Insert, PLAIN, &[C::Select]),
            t(List, "-", &[C::UnselectGroup]),
            k(Dialog, K::Enter, PLAIN, &[C::Accept]),
            t(List, "+", &[C::SelectGroup]),
            t(Dialog, "*.RS", &[]),
            k(Dialog, K::Enter, PLAIN, &[C::Accept]),
            t(List, "*", &[C::InvertSelection]),
            k(List, K::A, CTRL, &[C::SelectAll]),
            k(List, K::R, CTRL, &[C::Reread]),
            t(List, "a", &[C::Jump('a')]),
            k(List, K::Enter, PLAIN, &[C::Open]),
        ],
        vec![
            t(List, "m", &[C::Jump('m')]),
            k(List, K::F2, PLAIN, &[C::Rename]),
            t(Dialog, "notes", &[]),
            k(Dialog, K::A, CTRL, &[]),
            t(Dialog, "notes.txt", &[]),
            k(Dialog, K::Enter, PLAIN, &[C::Accept]),
            k(Dialog, K::A, CTRL, &[]),
            t(Dialog, "prog.rs", &[]),
            k(Dialog, K::Enter, PLAIN, &[C::Accept]),
            k(List, K::F2, PLAIN, &[C::Rename]),
            t(Dialog, "gone", &[]),
            k(Dialog, K::Escape, PLAIN, &[C::Cancel]),
            k(List, K::Home, PLAIN, &[C::Bar(Home)]),
            k(List, K::F2, PLAIN, &[C::Rename]),
        ],
        vec![
            // Ctrl+7, the F7 alias: one of the four the legacy encoding keeps.
            k(List, K::Num7, CTRL, &[C::MakeFolder]),
            k(Dialog, K::A, CTRL, &[]),
            t(Dialog, "새 폴더 2026", &[]),
            k(Dialog, K::Enter, PLAIN, &[C::Accept]),
            k(List, K::Home, PLAIN, &[C::Bar(Home)]),
            t(List, "새", &[C::Jump('새')]),
        ],
    ];
    let mut decoder = Decoder::start();
    for enc in ENCODINGS {
        let mut input = TermInput::new(HERE);
        let mut digits = Default::default();
        for (n, sequence) in sequences.iter().enumerate() {
            for (step, (ctx, what, want)) in sequence.iter().enumerate() {
                let decoded = match what {
                    Input::Key(k, held) => decoder.decode(&sent(*k, *held, enc).unwrap()),
                    Input::Typed(typed) => typed
                        .chars()
                        .flat_map(|c| decoder.decode(c.to_string().as_bytes()))
                        .collect(),
                };
                let events = translated(&mut input, &decoded);
                let (got, left) = pass(events, *ctx, &mut digits);
                assert_eq!(&got, want, "{enc:?} sequence {n} step {step}");
                match (ctx, what) {
                    (Dialog, Input::Typed(typed)) => assert_eq!(text(&left), *typed),
                    (Dialog, Input::Key(K::A, _)) => assert!(
                        left.contains(&key(K::A, true, CTRL_COMMAND)),
                        "the field's select-all: {left:?}"
                    ),
                    _ => {}
                }
            }
        }
    }
}
