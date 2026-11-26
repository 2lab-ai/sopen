//! The terminal backend (10 §4, X1b): `sopen --tui` runs the file browser in this terminal. Each
//! pass is the window's own — a headless `egui::Context` in the skin, the browser's `ui`, the
//! shell's input hook ([`super::FKeyInput`]) — fed by the translator ([`TermInput`]) from
//! crossterm's events, and what it paints in cells ([`cells`]) is laid onto one grid the size of
//! the terminal ([`compose`]) and printed, only the cells that changed ([`Term`]). So the
//! terminal and the window show the same cells (10 §5, TS-1).
//!
//! - **Entering** (T7, T28): raw mode, the alternate screen, mouse reporting (presses, releases
//!   and drags, SGR-encoded), bracketed paste, focus reports, the cursor hidden; the kitty
//!   keyboard protocol's flag 1 pushed when the terminal answers `CSI ? u`, so Ctrl+digits and
//!   the Ctrl+Shift chords arrive whole, else the legacy encoding and its losses (§9); then the
//!   terminal's unread input is dropped — keys typed while it was away were meant for the window.
//! - **Leaving** (T19): every mode off, the flags popped only if pushed, the cursor shown, the
//!   main screen back, raw mode off — once, whatever ends the run: a close, a panic in a pass
//!   (the message printed after the terminal is back), SIGTERM, SIGHUP or SIGINT (exit 128 + n).
//!   A write that fails (a terminal gone with SIGHUP) is ignored there.
//! - **The grid** is the terminal's size in cells, each 8×16 points as in the window; a resize
//!   lays the browser out again at the new size and prints the whole screen.
//! - **Layers** are laid in egui's order — its layer order, then the order of the calls — each
//!   inside its clip, a cell with no RGB background and no glyph leaving what is under it, as the
//!   window paints them. The cursor stands at the caret's cell (a field's: the terminal draws a
//!   Hangul composition there, 10 §4), else it is hidden.
//! - **Hand-off boxes** (T17, 10 §11): cells no layer covers are a pane the window draws in egui
//!   (a viewer not yet in cells; the preview side of an image, a video, a sound or the info card;
//!   the tree, the git panel) — in the terminal a frame saying `F12 shows it in the window`; a
//!   click on it is the switch. The text viewer (X3-text), the hex view (X3-hex) and the preview
//!   side around them (X3-preview) are cells, so they are drawn here as in the window.
//! - **Colours** are sent as RGB when `COLORTERM` says `truecolor` or `24bit`, else through
//!   [`cells::index256`]; never the 16 indexes a theme repaints (T15).
//! - **Ambiguous width** (10 §10, TS-6): the first time the terminal is taken, `─` is written at
//!   its corner and the cursor report read, with the kitty query's answer, in one round trip
//!   ([`probe`]: two seconds' wait at most). A terminal that took two cells for it is sent §10's
//!   stand-ins for sopen's chrome as each screen is printed — frames `+ - = |`, the cut `~`
//!   ([`narrow`]) — and any other glyph of ambiguous width (a name's `é`, `♥`) in two cells
//!   ([`for_ambiguous_wide`]), so no row is broken; the grid itself is the window's.
//! - **The receipt** (10 §15): with `SOPEN_CELLS` the grid is written as text once the screen has
//!   settled ([`cells_text`]), and the run ends; a window writes its own the same way
//!   ([`window_receipt`]), so the two are compared by a diff.
//! - **The clipboard** (§4): egui's `CopyText` is OSC 52; the terminal's paste arrives bracketed.
//! - **The switch** (§7.1–§7.2, T4): F12, Alt+G (the shell's hook, in the terminal only) or a
//!   click on a hand-off box. Where the window would open is read then, as 01 §4a resolves a run;
//!   a HOST says it comes with the remote switch (X2) and no display says how to get one — on the
//!   status row, until the next key. Else the terminal is left, the browser runs in a window of
//!   the terminal's grid size (T16) and comes back with its state when F12 is pressed there or the
//!   window closes (Cmd/Ctrl+Q and +W too); the focused field's edit state goes and comes back
//!   with it. A window that does not open brings the terminal back with the reason (T31).

use super::browser::{self, Browser};
use super::cells::{self, Buffer, Cells, Layer, Pen, Position};
use super::chrome::{self, FKey, CELL};
use super::term_input::TermInput;
use super::{Hooks, Outcome, Window};
use anyhow::Result;
use crossterm::event::{Event as CEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use eframe::egui::{self, Pos2, Rect, Vec2};
use ratatui_core::style::Color;
use std::cell::RefCell;
use std::io::{self, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

/// Entering: the alternate screen, mouse reporting (1000 presses and releases, 1002 drags, 1006
/// SGR coordinates), bracketed paste, focus reports, the cursor hidden, the screen cleared.
pub(crate) const ENTER: &str =
    "\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[?25l\x1b[2J";
/// The kitty keyboard protocol's flag 1 (DISAMBIGUATE_ESCAPE_CODES), pushed and popped.
pub(crate) const KITTY_PUSH: &str = "\x1b[>1u";
pub(crate) const KITTY_POP: &str = "\x1b[<1u";
/// Leaving: focus reports, bracketed paste and every mouse mode off (1015 and 1003 too, as
/// crossterm turns them off), the colours reset, the cursor shown, the main screen back.
pub(crate) const LEAVE: &str = "\x1b[?1004l\x1b[?2004l\x1b[?1006l\x1b[?1015l\x1b[?1003l\
     \x1b[?1002l\x1b[?1000l\x1b[0m\x1b[?25h\x1b[?1049l";

/// The line printed on the main screen while the browser is in a window (10 §7.2).
pub(crate) const IN_A_WINDOW: &str =
    "xtools is in a window now — close it, or press F12 there, to come back here.";
/// What a hand-off box says (10 §11).
pub(crate) const HAND_OFF: &str = "F12 shows it in the window";
/// The status line while a dialog egui draws (not yet cells) is open.
pub(crate) const MODAL_LINE: &str =
    "a dialog the terminal does not draw yet: F12 shows it in the window, Esc closes it";
/// The status line when there is no display for the window (10 §7.1).
pub const NO_DISPLAY: &str =
    "no display here — sopen config target <host>, or --target, opens the window on another machine";

/// The most text sent to the terminal's clipboard (OSC 52): many terminals refuse or cut more.
pub(crate) const OSC52_MAX: usize = 256 * 1024;

/// The longest a wait for input lasts before a signal is looked for again.
const TICK: Duration = Duration::from_millis(100);
/// The shortest time between two passes no input asked for (an animation asking for every
/// frame): the terminal is not a display with a refresh rate.
const IDLE_FLOOR: Duration = Duration::from_millis(8);

// ---------------------------------------------------------------------------------------------
// The terminal.

/// What the backend asks of its terminal besides the bytes it writes: crossterm's and the
/// system's in a real one ([`Real`]), a script in the tests.
pub(crate) trait Tty {
    /// Raw mode on or off — for the real terminal also the signals that end the run.
    fn raw(&mut self, on: bool) -> io::Result<()>;
    /// The terminal's size in cells.
    fn size(&mut self) -> io::Result<(u16, u16)>;
    /// Whether the terminal answers `CSI ? u` (the kitty keyboard protocol) and draws a glyph of
    /// ambiguous width in two cells (10 §10, TS-6): asked once, in one round trip, in raw mode on
    /// the alternate screen ([`probe`]).
    fn probe(&mut self) -> Probed;
    /// Drops what the terminal sent that nobody read (10 §4, T28).
    fn drop_input(&mut self);
    /// The next event, waiting at most `wait` for it.
    fn next(&mut self, wait: Duration) -> io::Result<Option<CEvent>>;
    /// Whether an event is waiting.
    fn pending(&mut self) -> io::Result<bool>;
    /// The signal that ends the run, once one came.
    fn stopped(&self) -> Option<i32>;
}

/// The terminal sopen runs in: crossterm's raw mode, size and events, each read only once the
/// input is known not to have hung up ([`wait_input`]); the start's probe ([`probe`]: the
/// ambiguous width and the kitty keyboard protocol in one round trip); the system's input queue;
/// SIGTERM, SIGHUP and SIGINT caught around raw mode ([`Signals`]); the standard error held
/// while the terminal is ([`Stderr`]).
#[derive(Default)]
struct Real {
    signals: Option<Signals>,
    stderr: Option<Stderr>,
}

impl Real {
    /// The input hung up (a terminal closed): the run ends as SIGHUP would end it.
    fn hung_up(e: io::Error) -> io::Error {
        if e.kind() == io::ErrorKind::BrokenPipe {
            let _ = STOP.compare_exchange(0, libc::SIGHUP, Ordering::SeqCst, Ordering::SeqCst);
        }
        e
    }

    /// Whether an event can be read without waiting: crossterm's own queue (it may hold events
    /// parsed from one read), else input waiting — never asked of crossterm on a hung-up input.
    fn ready(&mut self) -> io::Result<bool> {
        wait_input(libc::STDIN_FILENO, Duration::ZERO).map_err(Self::hung_up)?;
        crossterm::event::poll(Duration::ZERO)
    }
}

impl Tty for Real {
    /// The signal handlers enclose raw mode: on before it, off after it — so a signal never finds
    /// raw mode on without them — and off again when raw mode does not come on.
    fn raw(&mut self, on: bool) -> io::Result<()> {
        if on {
            self.signals = Some(Signals::watch());
            if let Err(e) = crossterm::terminal::enable_raw_mode() {
                self.signals = None;
                return Err(e);
            }
            self.stderr = Stderr::hold().ok();
        } else {
            // Raw mode off first: what was kept prints with its line ends as written.
            let off = crossterm::terminal::disable_raw_mode();
            if let Some(held) = self.stderr.take() {
                held.release();
            }
            self.signals = None;
            off?;
        }
        Ok(())
    }

    fn size(&mut self) -> io::Result<(u16, u16)> {
        crossterm::terminal::size()
    }

    fn probe(&mut self) -> Probed {
        let probed = probe(libc::STDOUT_FILENO, libc::STDIN_FILENO, PROBE_WAIT);
        probed.map_err(Self::hung_up).unwrap_or_default()
    }

    fn drop_input(&mut self) {
        // SAFETY: a plain call on the standard input, a terminal (`--tui` asks for one).
        unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) };
        // And what crossterm already read: bounded, as a key held down keeps sending.
        for _ in 0..4096 {
            match self.ready() {
                Ok(true) if crossterm::event::read().is_ok() => {}
                _ => break,
            }
        }
    }

    fn next(&mut self, wait: Duration) -> io::Result<Option<CEvent>> {
        if self.ready()? {
            return crossterm::event::read().map(Some);
        }
        // Input, a hang-up or a signal (a resize: crossterm's SIGWINCH) ends the wait.
        match wait_input(libc::STDIN_FILENO, wait).map_err(Self::hung_up)? {
            Input::Ready if self.ready()? => crossterm::event::read().map(Some),
            _ => Ok(None),
        }
    }

    fn pending(&mut self) -> io::Result<bool> {
        self.ready()
    }

    fn stopped(&self) -> Option<i32> {
        let signal = STOP.load(Ordering::SeqCst);
        (signal != 0).then_some(signal)
    }
}

/// What the terminal's input says ([`wait_input`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Input {
    /// Something to read.
    Ready,
    /// Nothing within the wait (or a signal ended it).
    Quiet,
}

/// Waits at most `wait` for input on `fd`, a terminal: a hang-up, an error on it or a closed fd
/// is an error (`BrokenPipe`) at once — crossterm's reader would read its empty reads in a loop
/// for ever, never back to look at the time or a signal (crossterm 0.29's `mio` source).
pub(crate) fn wait_input(fd: libc::c_int, wait: Duration) -> io::Result<Input> {
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = libc::c_int::try_from(wait.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: one live pollfd.
    let n = unsafe { libc::poll(&mut poll, 1, ms) };
    if n < 0 {
        let e = io::Error::last_os_error();
        return match e.kind() {
            io::ErrorKind::Interrupted => Ok(Input::Quiet),
            _ => Err(e),
        };
    }
    if poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "the terminal hung up",
        ));
    }
    Ok(if poll.revents & libc::POLLIN != 0 {
        Input::Ready
    } else {
        Input::Quiet
    })
}

/// The start's one round trip (10 §4, §10, TS-6): `─` at the screen's corner, then the cursor
/// position report asked for (DSR 6), the kitty keyboard protocol's flags (`CSI ? u`) and the
/// device attributes (DA1) — answered in that order, so the attributes say that the answers are
/// all in.
pub(crate) const PROBE: &str = "\x1b[1;1H─\x1b[6n\x1b[?u\x1b[c";

/// The longest the probe waits for the terminal's answers (crossterm's own kitty query waited as
/// long).
const PROBE_WAIT: Duration = Duration::from_secs(2);

/// What the terminal answered at start ([`probe`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Probed {
    /// It answers `CSI ? u`: the kitty keyboard protocol's flag 1 is pushed.
    pub kitty: bool,
    /// It drew `─` in two cells: an ambiguous-wide terminal (10 §10).
    pub ambiguous: bool,
}

/// What the terminal behind `out` and `input` (in raw mode) is ([`Probed`]): [`PROBE`] written,
/// then its answers read up to the device attributes' — the last cursor report before them (a key
/// typed ahead can read as one: Shift+F3 is `CSI 1;2R`), whose column is 3 after a glyph that took
/// two cells from the first, and whether the kitty flags came. No answer, or nothing within
/// `wait`: one cell and no kitty protocol — the probe never holds the start for longer. A key
/// typed meanwhile is read with the answers and goes with them: input before the first screen is
/// dropped (T28). A hung-up input is an error (`BrokenPipe`), as [`wait_input`]'s.
pub(crate) fn probe(out: libc::c_int, input: libc::c_int, wait: Duration) -> io::Result<Probed> {
    let mut bytes = PROBE.as_bytes();
    while !bytes.is_empty() {
        // SAFETY: a write of a live buffer to an fd the caller owns.
        let n = unsafe { libc::write(out, bytes.as_ptr().cast(), bytes.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        bytes = &bytes[n as usize..];
    }
    let deadline = Instant::now() + wait;
    let mut got = Vec::new();
    loop {
        let answered = answers(&got);
        let left = deadline.saturating_duration_since(Instant::now());
        if answered.all_in || left.is_zero() {
            return Ok(Probed {
                kitty: answered.kitty,
                ambiguous: answered.report.is_some_and(|(_, column)| column >= 3),
            });
        }
        if wait_input(input, left)? == Input::Quiet {
            continue;
        }
        let mut chunk = [0u8; 256];
        // SAFETY: a read into a live buffer from an fd the caller owns, which has input.
        let n = unsafe { libc::read(input, chunk.as_mut_ptr().cast(), chunk.len()) };
        match n {
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the terminal hung up",
                ))
            }
            n if n < 0 => {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::Interrupted {
                    return Err(e);
                }
            }
            n => got.extend_from_slice(&chunk[..n as usize]),
        }
    }
}

/// What the terminal's answers to [`PROBE`] say so far ([`answers`]).
#[derive(Debug, Default)]
struct Answered {
    /// The last cursor report (`CSI row ; column R`).
    report: Option<(u16, u16)>,
    /// The kitty flags came (`CSI ? flags u`).
    kitty: bool,
    /// The device attributes (`CSI ? … c`), the last answer asked for, came.
    all_in: bool,
}

/// What the answers in `got` (keys typed meanwhile among them) say: all of them, those read with
/// the device attributes too (a terminal may send them first) — the attributes end the wait, not
/// the reading.
fn answers(got: &[u8]) -> Answered {
    let mut out = Answered::default();
    let mut at = 0;
    while let Some(start) = (got[at..].windows(2)).position(|w| w == b"\x1b[") {
        let params = at + start + 2;
        // Parameter and intermediate bytes, then the final one; an unended one is still coming.
        let Some(end) = (params..got.len()).find(|&i| !(0x20..=0x3f).contains(&got[i])) else {
            break;
        };
        if !(0x40..=0x7e).contains(&got[end]) {
            at = params;
            continue;
        }
        let body = std::str::from_utf8(&got[params..end]).unwrap_or("");
        match got[end] {
            b'R' => {
                let read = body
                    .split_once(';')
                    .and_then(|(row, column)| Some((row.parse().ok()?, column.parse().ok()?)));
                out.report = read.or(out.report);
            }
            b'u' if body.starts_with('?') => out.kitty = true,
            b'c' if body.starts_with('?') => out.all_in = true,
            _ => {}
        }
        at = end + 1;
    }
    out
}

/// The most of the standard error kept while the terminal is held ([`Stderr`]): the last bytes.
pub(crate) const STDERR_KEPT: usize = 1024 * 1024;

/// The standard error held while the terminal is (10 §4): what is written to it — a worker's
/// line, a panic on another thread, a child's message — would land on the screen sopen draws and
/// stay there; it goes to a pipe instead, its last [`STDERR_KEPT`] bytes are kept, and they are
/// written to the real standard error when the terminal is let go (after the alternate screen
/// is left), after a line saying how many went before them. A child started meanwhile keeps the
/// pipe as its standard error: what it writes later goes straight to the real one.
pub(crate) struct Stderr {
    /// The real standard error, put back on release.
    saved: libc::c_int,
    held: Arc<Mutex<Held>>,
    /// The reader read the pipe's end.
    done: std::sync::mpsc::Receiver<()>,
}

/// What the reader of a held standard error has: the last bytes, how many went before them, and
/// whether the hold is over (then it passes bytes on).
#[derive(Default)]
struct Held {
    kept: std::collections::VecDeque<u8>,
    dropped: usize,
    released: bool,
}

impl Stderr {
    /// Fd 2 becomes a pipe's end; a thread reads the other.
    pub fn hold() -> io::Result<Stderr> {
        let mut ends = [-1; 2];
        // SAFETY: two out-ints for the pipe; dup, dup2, fcntl and close on fds this process
        // owns.
        let (read, saved) = unsafe {
            if libc::pipe(ends.as_mut_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
            let saved = libc::dup(2);
            if saved < 0 || libc::dup2(ends[1], 2) < 0 {
                let e = io::Error::last_os_error();
                for fd in [ends[0], ends[1], saved] {
                    if fd >= 0 {
                        libc::close(fd);
                    }
                }
                return Err(e);
            }
            libc::close(ends[1]);
            // Children inherit fd 2 (the pipe), never these two.
            for fd in [ends[0], saved] {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            (ends[0], saved)
        };
        // SAFETY: the read end is this process's, owned by the File from here on.
        let mut from = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(read) };
        let held: Arc<Mutex<Held>> = Arc::default();
        let into = held.clone();
        let (end, done) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("sopen-stderr".into())
            .spawn(move || {
                let mut chunk = [0u8; 8192];
                while let Ok(n) = std::io::Read::read(&mut from, &mut chunk) {
                    if n == 0 {
                        break;
                    }
                    let mut held = into.lock().unwrap_or_else(|p| p.into_inner());
                    if held.released {
                        let _ = io::stderr().write_all(&chunk[..n]);
                        continue;
                    }
                    held.kept.extend(&chunk[..n]);
                    let over = held.kept.len().saturating_sub(STDERR_KEPT);
                    held.kept.drain(..over);
                    held.dropped += over;
                }
                let _ = end.send(());
            });
        if let Err(e) = spawned {
            // SAFETY: puts the real standard error back and closes the copy.
            unsafe {
                libc::dup2(saved, 2);
                libc::close(saved);
            }
            return Err(e);
        }
        Ok(Stderr { saved, held, done })
    }

    /// Fd 2 is the real standard error again and what was kept is written there (after how much
    /// went before it); what comes later through the pipe is passed straight on, after it. What
    /// was kept.
    pub fn release(self) -> Vec<u8> {
        // SAFETY: dup2 and close of fds this process owns.
        unsafe {
            libc::dup2(self.saved, 2);
            libc::close(self.saved);
        }
        // The pipe's last write end is closed now — unless a child started meanwhile holds one:
        // its end is not waited for long.
        let _ = self.done.recv_timeout(Duration::from_millis(200));
        let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
        held.released = true;
        let kept: Vec<u8> = std::mem::take(&mut held.kept).into();
        let mut err = io::stderr();
        if held.dropped > 0 {
            let _ = writeln!(err, "… {} bytes of stderr dropped", held.dropped);
        }
        // Under the lock: the reader passes later bytes on only after these.
        let _ = err.write_all(&kept);
        kept
    }
}

/// The signal that ended the run (0: none yet).
static STOP: AtomicI32 = AtomicI32::new(0);

const STOPS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

/// The handler: one atomic store, async-signal-safe.
extern "C" fn stop(signal: libc::c_int) {
    let _ = STOP.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
}

/// SIGTERM, SIGHUP and SIGINT end the run while the terminal is held — the loop sees it within
/// [`TICK`] and leaves the terminal as it found it (T19) — and are put back as they were when it
/// is let go (a switched window ends by them as any window does). A signal that was ignored
/// stays ignored.
struct Signals {
    before: [Option<libc::sigaction>; 3],
}

impl Signals {
    fn watch() -> Signals {
        let mut before = [None; 3];
        for (signal, before) in STOPS.into_iter().zip(&mut before) {
            // SAFETY: whole `sigaction`s; the handler only stores into an atomic.
            unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(signal, std::ptr::null(), &mut old) != 0
                    || old.sa_sigaction == libc::SIG_IGN
                {
                    continue;
                }
                let mut new: libc::sigaction = std::mem::zeroed();
                new.sa_sigaction = stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
                libc::sigemptyset(&mut new.sa_mask);
                new.sa_flags = libc::SA_RESTART;
                if libc::sigaction(signal, &new, std::ptr::null_mut()) == 0 {
                    *before = Some(old);
                }
            }
        }
        Signals { before }
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for (signal, before) in STOPS.into_iter().zip(&self.before) {
            if let Some(before) = before {
                // SAFETY: the disposition found when the watch started.
                unsafe { libc::sigaction(signal, before, std::ptr::null_mut()) };
            }
        }
    }
}

/// A run of the terminal ended by a signal (exit 128 + the signal, as a shell reports it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signalled(pub i32);

impl Signalled {
    pub fn exit_code(&self) -> u8 {
        128u8.saturating_add(u8::try_from(self.0).unwrap_or(0))
    }
}

impl std::fmt::Display for Signalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            libc::SIGHUP => f.write_str("the terminal hung up"),
            libc::SIGINT => f.write_str("interrupted (SIGINT)"),
            libc::SIGTERM => f.write_str("terminated (SIGTERM)"),
            n => write!(f, "ended by signal {n}"),
        }
    }
}

impl std::error::Error for Signalled {}

/// Whether `COLORTERM` (its value) says the terminal takes RGB colours (10 §4).
pub(crate) fn truecolor(colorterm: Option<&str>) -> bool {
    matches!(colorterm, Some("truecolor" | "24bit"))
}

// ---------------------------------------------------------------------------------------------
// The screen.

/// What the terminal shows: the grid as the layers lay it, the cursor's cell, and the hand-off
/// boxes (a click on one is the switch).
#[derive(Debug, Clone)]
pub(crate) struct Screen {
    pub buf: Buffer,
    pub caret: Option<Position>,
    pub hand_offs: Vec<Cells>,
}

/// The cell under `p` (points) on a grid whose cell (0, 0) is at the screen's corner.
fn cell_at(p: Pos2) -> (i32, i32) {
    ((p.x / CELL.x).round() as i32, (p.y / CELL.y).round() as i32)
}

/// The screen of the pass just run in `ctx` on a `cols` × `rows` grid, as the terminal shows it
/// and the window's receipt lays its own ([`window_receipt`]): its `layers` laid out
/// ([`compose`]); where no layer is, a hand-off box titled with the text egui drew in its top line
/// (`titles`: a pane's frame title); a dialog egui draws (not yet in cells) a hand-off box where it
/// stands — its keys still go to it, Esc closing it as in the window — under what is in cells
/// above it in egui's painting order; `said` (the backend's line), or that dialog's, on the status
/// row.
fn screen_of(
    ctx: &egui::Context,
    layers: &[Arc<Layer>],
    titles: &[(Pos2, String)],
    cols: u16,
    rows: u16,
    said: Option<&str>,
) -> Screen {
    let mut screen = compose(ctx, layers, cols, rows);
    let modal = ctx.memory(|m| m.top_modal_layer());
    let egui_only = modal.filter(|m| !layers.iter().any(|l| l.layer == *m));
    let stands = egui_only.and_then(|m| ctx.memory(|mem| mem.area_rect(m.id)));
    for &r in &screen.hand_offs {
        hand_off(&mut screen.buf, r, title_at(titles, r));
    }
    if let Some(r) = stands.and_then(|rect| cells_over(rect, cols, rows)) {
        hand_off(&mut screen.buf, r, title_at(titles, r));
        screen.hand_offs.push(r);
        screen.caret = screen.caret.filter(|c| !r.contains(*c));
        // What is in cells above the dialog in egui's painting order — the key help, a later
        // area of its own order — stays over its box.
        let place = area_place(ctx);
        let above = egui_only.map(&place);
        let mut covered = vec![false; usize::from(cols) * usize::from(rows)];
        for l in painting_order(ctx, layers) {
            if Some(place(l.layer)) > above {
                lay_layer(&mut screen.buf, &mut covered, l, cols, rows);
            }
        }
    }
    if let Some(line) = said.or(stands.map(|_| MODAL_LINE)) {
        say(&mut screen.buf, line);
    }
    screen
}

/// The grid of `cols` × `rows` as `layers` (one pass's, [`cells::layers`]) lay it: the skin's
/// body under everything; then each layer in egui's painting order — its layer order (the
/// areas' order of `ctx`, back to front), then the order of the calls — each cell whose centre
/// is inside the layer's clip. A cell with no RGB background and a blank keeps what is under it
/// (the window paints nothing there); no RGB foreground is the skin's text colour. A wide glyph
/// takes its second cell too, and a glyph written over half of one erases the rest of it, as a
/// terminal does. Cells no layer covers are hand-off boxes.
pub(crate) fn compose(ctx: &egui::Context, layers: &[Arc<Layer>], cols: u16, rows: u16) -> Screen {
    let p = chrome::PALETTE;
    let all = Cells::new(0, 0, cols, rows);
    let mut buf = Buffer::empty(all);
    cells::fill(&mut buf, all, Pen::new(p.text, p.back));
    let mut covered = vec![false; usize::from(cols) * usize::from(rows)];
    let mut caret = None;
    for l in painting_order(ctx, layers) {
        if let Some(at) = lay_layer(&mut buf, &mut covered, l, cols, rows) {
            caret = Some(at);
        }
    }
    let hand_offs = uncovered(&covered, cols, rows);
    for &r in &hand_offs {
        hand_off(&mut buf, r, "");
    }
    Screen {
        buf,
        caret,
        hand_offs,
    }
}

/// `layers` in egui's painting order: its layer order (the areas' order of `ctx`, back to
/// front), then the order of the calls.
fn painting_order<'a>(ctx: &egui::Context, layers: &'a [Arc<Layer>]) -> Vec<&'a Arc<Layer>> {
    let place = area_place(ctx);
    let mut sorted: Vec<(usize, &Arc<Layer>)> = layers.iter().enumerate().collect();
    sorted.sort_by_key(|&(call, l)| (place(l.layer), call));
    sorted.into_iter().map(|(_, l)| l).collect()
}

/// Where egui paints a layer: its order, then its area's place in the areas of `ctx` (back to
/// front; one egui does not list goes last in its order).
fn area_place(ctx: &egui::Context) -> impl Fn(egui::LayerId) -> (egui::Order, usize) {
    let order: Vec<egui::LayerId> = ctx.memory(|m| m.layer_ids().collect());
    move |layer| {
        let area = order.iter().position(|o| *o == layer);
        (layer.order, area.unwrap_or(usize::MAX))
    }
}

/// Lays layer `l` onto `buf` (`cols` × `rows`, see [`compose`]): each cell whose centre is
/// inside its clip, the cells it took marked in `covered`; its caret's cell on screen.
fn lay_layer(
    buf: &mut Buffer,
    covered: &mut [bool],
    l: &Layer,
    cols: u16,
    rows: u16,
) -> Option<Position> {
    let screen = Rect::from_min_size(
        Pos2::ZERO,
        Vec2::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y),
    );
    let visible = l.clip.intersect(screen);
    let (ox, oy) = cell_at(l.origin);
    let on_screen = |x: u16, y: u16| {
        let (sx, sy) = (ox + i32::from(x), oy + i32::from(y));
        let inside = (0..i32::from(cols)).contains(&sx) && (0..i32::from(rows)).contains(&sy);
        let seen = visible.contains(l.rect(Cells::new(x, y, 1, 1)).center());
        (inside && seen).then_some((sx as u16, sy as u16))
    };
    let a = l.buf.area;
    for y in a.top()..a.bottom() {
        let mut x = a.left();
        while x < a.right() {
            let cell = &l.buf[(x, y)];
            let width = (cells::width(cell.symbol()) as u16).clamp(1, 2);
            let width = width.min(a.right() - x);
            // A wide glyph with one of its cells outside the clip is a blank in the cell
            // inside it (the window shows half a glyph there; a terminal cannot), never
            // written past the clip.
            let first = on_screen(x, y);
            let second = (width == 2).then(|| on_screen(x + 1, y));
            let (at, cell, width) = match (first, second) {
                (Some(at), None | Some(Some(_))) => (Some(at), cell.clone(), width),
                (Some(at), Some(None)) => (Some(at), blank_of(cell), 1),
                (None, Some(Some(at))) => (Some(at), blank_of(cell), 1),
                (None, _) => (None, cell.clone(), width),
            };
            if let Some((sx, sy)) = at {
                if lay(buf, sx, sy, &cell, width) {
                    for k in 0..width.min(cols - sx) {
                        covered[usize::from(sy) * usize::from(cols) + usize::from(sx + k)] = true;
                    }
                }
            }
            x += (cells::width(l.buf[(x, y)].symbol()) as u16)
                .clamp(1, 2)
                .min(a.right() - x);
        }
    }
    l.caret
        .and_then(|c| on_screen(c.x, c.y))
        .map(|(x, y)| Position::new(x, y))
}

/// Lays `cell` of a layer, `width` cells wide, at (`x`, `y`) of `buf`; false when it leaves
/// what is under it (no background, no glyph).
fn lay(buf: &mut Buffer, x: u16, y: u16, cell: &ratatui_core::buffer::Cell, width: u16) -> bool {
    let symbol = cell.symbol();
    let blank = symbol.trim().is_empty();
    let bg = match cell.bg {
        Color::Rgb(..) => cell.bg,
        _ if blank => return false,
        _ => buf[(x, y)].bg,
    };
    let fg = match cell.fg {
        Color::Rgb(..) => cell.fg,
        _ => rgb(chrome::PALETTE.text),
    };
    // The cells it takes here; a glyph cut by the screen's edge, a control character (it would
    // move the terminal's cursor) and a wide blank are blanks.
    let span = width.min(buf.area.right() - x);
    let printable = !symbol.chars().any(char::is_control);
    let shown = if printable && !blank && cells::width(symbol) == usize::from(span) {
        symbol
    } else {
        " "
    };
    // Half of a wide glyph to the left written over: the rest of it goes.
    if x > buf.area.left() && cells::width(buf[(x - 1, y)].symbol()) > 1 {
        buf[(x - 1, y)].set_symbol(" ");
    }
    for k in 0..span {
        let c = &mut buf[(x + k, y)];
        c.set_symbol(if k == 0 { shown } else { " " });
        c.fg = fg;
        c.bg = bg;
        c.modifier = ratatui_core::style::Modifier::empty();
    }
    true
}

/// `cell` with a blank in place of its glyph (its colours kept).
fn blank_of(cell: &ratatui_core::buffer::Cell) -> ratatui_core::buffer::Cell {
    let mut blank = cell.clone();
    blank.set_symbol(" ");
    blank
}

fn rgb(c: egui::Color32) -> Color {
    Color::Rgb(c.r(), c.g(), c.b())
}

/// The rectangles of cells no layer covers (`covered`, row by row): from each uncovered cell
/// not taken yet, as far right as the row goes uncovered, then as far down as those cells all
/// do — a pane is a rectangle, so it is found whole.
fn uncovered(covered: &[bool], cols: u16, rows: u16) -> Vec<Cells> {
    let (w, h) = (usize::from(cols), usize::from(rows));
    let mut taken = covered.to_vec();
    let mut out = vec![];
    for y in 0..h {
        for x in 0..w {
            if taken[y * w + x] {
                continue;
            }
            let right = (x..w).take_while(|&k| !taken[y * w + k]).count();
            let down = (y..h)
                .take_while(|&r| (x..x + right).all(|k| !taken[r * w + k]))
                .count();
            for r in y..y + down {
                taken[r * w + x..r * w + x + right].fill(true);
            }
            out.push(Cells::new(x as u16, y as u16, right as u16, down as u16));
        }
    }
    out
}

/// A hand-off box in `r` (10 §11): a single-line frame with `title` (what is there) in its top
/// line, and in its middle [`HAND_OFF`].
fn hand_off(buf: &mut Buffer, r: Cells, title: &str) {
    let p = chrome::PALETTE;
    let inner = cells::frame(buf, r, title, false);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let shown = cells::fit_start(HAND_OFF, usize::from(inner.width));
    let w = cells::width(&shown) as u16;
    let x = inner.x + (inner.width - w) / 2;
    let y = inner.y + (inner.height - 1) / 2;
    cells::put(buf, x, y, &shown, w.into(), Pen::new(p.text, p.back));
}

/// The texts egui itself drew in `shapes` (a cell's glyph is one grapheme a shape), where they
/// start.
fn titles(shapes: &[egui::epaint::ClippedShape]) -> Vec<(Pos2, String)> {
    fn walk(shape: &egui::Shape, out: &mut Vec<(Pos2, String)>) {
        match shape {
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            egui::Shape::Text(t) => {
                let text = t.galley.text().trim();
                if unicode_segmentation::UnicodeSegmentation::graphemes(text, true)
                    .nth(1)
                    .is_some()
                {
                    out.push((t.pos, text.to_string()));
                }
            }
            _ => {}
        }
    }
    let mut out = vec![];
    for c in shapes {
        walk(&c.shape, &mut out);
    }
    out
}

/// The title of what stands at cells `r`: the first text egui drew in its top line (a pane's
/// frame title, a dialog's), else none.
fn title_at(titles: &[(Pos2, String)], r: Cells) -> &str {
    let top = f32::from(r.y) * CELL.y;
    let (left, right) = (f32::from(r.x) * CELL.x, f32::from(r.right()) * CELL.x);
    (titles.iter())
        .find(|(p, _)| p.y >= top && p.y < top + 1.5 * CELL.y && p.x >= left && p.x < right)
        .map_or("", |(_, t)| t.as_str())
}

/// `line` on the status row — the row above the F-key row — in the error colour, from its
/// second cell, cut with "…" to the row less a cell at either end (as `chrome::status_cells`).
fn say(buf: &mut Buffer, line: &str) {
    let a = buf.area;
    if a.height < 2 || a.width < 3 {
        return;
    }
    let p = chrome::PALETTE;
    let y = a.bottom() - 2;
    cells::fill(
        buf,
        Cells::new(a.x, y, a.width, 1),
        Pen::new(p.text, p.back),
    );
    let room = usize::from(a.width) - 2;
    let shown = cells::fit_start(line, room);
    cells::put(buf, a.x + 1, y, &shown, room, Pen::new(p.error, p.back));
}

// ---------------------------------------------------------------------------------------------
// An ambiguous-wide terminal (10 §10, TS-6).

/// The letters of Unicode's East Asian Width A that unicode-width's CJK width keeps one cell wide
/// (0.2.2, against Unicode 17's `EastAsianWidth.txt` through python3's `unicodedata`): Latin-1 and
/// Latin Extended letters, IPA, spacing marks, Greek, Cyrillic, `ⁿ ℓ Ω Å`. 10 §10 counts them
/// ambiguous (python3's `unicodedata`), as TS-6's `é`; a terminal that draws one of them in one
/// cell all the same leaves a blank after it ([`Term::draw`]), never a stale cell.
#[rustfmt::skip]
const A_LETTERS: [(u32, u32); 61] = [
    (0x00A8, 0x00A8), (0x00AA, 0x00AA), (0x00B4, 0x00B4), (0x00B8, 0x00B8), (0x00BA, 0x00BA),
    (0x00C6, 0x00C6), (0x00D0, 0x00D0), (0x00D8, 0x00D8), (0x00DE, 0x00E1), (0x00E6, 0x00E6),
    (0x00E8, 0x00EA), (0x00EC, 0x00ED), (0x00F0, 0x00F0), (0x00F2, 0x00F3), (0x00F8, 0x00FA),
    (0x00FC, 0x00FC), (0x00FE, 0x00FE), (0x0101, 0x0101), (0x0111, 0x0111), (0x0113, 0x0113),
    (0x011B, 0x011B), (0x0126, 0x0127), (0x012B, 0x012B), (0x0131, 0x0133), (0x0138, 0x0138),
    (0x013F, 0x0142), (0x0144, 0x0144), (0x0148, 0x014B), (0x014D, 0x014D), (0x0152, 0x0153),
    (0x0166, 0x0167), (0x016B, 0x016B), (0x01CE, 0x01CE), (0x01D0, 0x01D0), (0x01D2, 0x01D2),
    (0x01D4, 0x01D4), (0x01D6, 0x01D6), (0x01D8, 0x01D8), (0x01DA, 0x01DA), (0x01DC, 0x01DC),
    (0x0251, 0x0251), (0x0261, 0x0261), (0x02C4, 0x02C4), (0x02C7, 0x02C7), (0x02C9, 0x02CB),
    (0x02CD, 0x02CD), (0x02D0, 0x02D0), (0x02D8, 0x02DB), (0x02DD, 0x02DD), (0x02DF, 0x02DF),
    (0x0391, 0x03A1), (0x03A3, 0x03A9), (0x03B1, 0x03C1), (0x03C3, 0x03C9), (0x0401, 0x0401),
    (0x0410, 0x044F), (0x0451, 0x0451), (0x207F, 0x207F), (0x2113, 0x2113), (0x2126, 0x2126),
    (0x212B, 0x212B),
];

/// Whether `c`, one cell wide on the grid, is of ambiguous width (Unicode's East Asian Width A):
/// two cells in an ambiguous-wide terminal.
pub(crate) fn ambiguous(c: char) -> bool {
    UnicodeWidthChar::width(c) == Some(1)
        && (UnicodeWidthChar::width_cjk(c) == Some(2)
            || (A_LETTERS.iter()).any(|&(from, to)| (from..=to).contains(&u32::from(c))))
}

/// The one-cell stand-in an ambiguous-wide terminal is sent for `symbol` (10 §10's table, T38),
/// where it is sopen's own: box lines `+`, `-`, `=` and `|`, blocks and shades `#`, the arrows
/// `^ v < > - |`, the cut's `…` `~`, `·` and the CP437 table's other symbols `.`; `None` for any
/// other glyph — one cell wide everywhere (ASCII, `░ ► ◄ ◙ »`), two everywhere (Hangul,
/// U+1F3B5), or a letter. The printer sends it only for a cell of sopen's chrome ([`chrome`]:
/// frames, scroll bars, the cut); the rest is content, laid in two cells as written
/// ([`for_ambiguous_wide`]). A viewer that knows a cell is a CP437 glyph substitutes it itself
/// (X3-text, X3-hex).
pub(crate) fn narrow(symbol: &str) -> Option<&'static str> {
    let mut chars = symbol.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    if !ambiguous(c) {
        return None;
    }
    Some(match c {
        '─' | '↔' => "-",
        '═' => "=",
        '│' | '║' | '↕' => "|",
        '\u{2500}'..='\u{257f}' => "+",
        '\u{2580}'..='\u{259f}' | '■' => "#",
        '↑' | '▲' => "^",
        '↓' | '▼' => "v",
        '←' => "<",
        '→' => ">",
        '…' => "~",
        c if !c.is_alphabetic() && (0..=255).any(|b| chrome::cp437(b) == c) => ".",
        _ => return None,
    })
}

/// `screen` as an ambiguous-wide terminal is sent it (10 §10, T38), every cell in its own column:
/// a glyph of sopen's chrome ([`chrome`]) that such a terminal draws in two cells is [`narrow`]'s
/// stand-in; any other glyph of ambiguous width is content and is laid in two cells, its run
/// ([`lay_run`]) making room for it. The cell a glyph's second half covers holds no symbol (`""`:
/// never printed). With it, row by row, the grid's column each of the terminal's cells shows
/// ([`Term::grid_column`]: where a click lands).
fn for_ambiguous_wide(screen: &Screen) -> (Screen, Vec<Vec<u16>>) {
    let mut out = screen.clone();
    let a = out.buf.area;
    let mut columns = vec![];
    for y in a.top()..a.bottom() {
        let mut row = Row {
            units: glyphs(&screen.buf, y),
            shown: (a.left()..a.right()).collect(),
            place: (a.left()..a.right()).collect(),
        };
        let mut i = 0;
        while i < row.units.len() {
            let ours = |u: &Glyph, v: &Glyph| {
                (u.cells[0].fg, u.cells[0].bg) == (v.cells[0].fg, v.cells[0].bg)
            };
            let mut j = i;
            while j < row.units.len() && !row.units[j].frame && ours(&row.units[i], &row.units[j]) {
                j += 1;
            }
            if j == i {
                j += 1;
            }
            lay_run(&mut out.buf, &mut row, y, i..j);
            i = j;
        }
        if let Some(c) = out.caret.as_mut().filter(|c| c.y == y) {
            c.x = row.place[usize::from(c.x - a.left())].min(a.right() - 1);
        }
        columns.push(row.shown);
    }
    (out, columns)
}

/// One glyph of a row as the grid has it ([`glyphs`]): its cells (a wide glyph's second one too),
/// where it starts, and how it is sent to an ambiguous-wide terminal.
struct Glyph {
    cells: Vec<ratatui_core::buffer::Cell>,
    x: u16,
    /// The cells it takes there: two for a wide glyph and for a letter of ambiguous width.
    sent: u16,
    /// A frame's cell: a run ends at it.
    frame: bool,
    blank: bool,
}

/// A row being laid: its glyphs, the grid's column each of the terminal's cells shows, and the
/// terminal's cell each of the grid's columns is shown in (where the caret goes).
struct Row {
    units: Vec<Glyph>,
    shown: Vec<u16>,
    place: Vec<u16>,
}

/// Row `y` of `buf` as whole glyphs — a wide one with its second cell — each with its stand-in
/// already where it is chrome ([`chrome`], [`narrow`]).
fn glyphs(buf: &Buffer, y: u16) -> Vec<Glyph> {
    let a = buf.area;
    let mut out = vec![];
    let mut x = a.left();
    while x < a.right() {
        let cell = &buf[(x, y)];
        let width = (cells::width(cell.symbol()) as u16)
            .clamp(1, 2)
            .min(a.right() - x);
        let mut cells: Vec<_> = (x..x + width).map(|k| buf[(k, y)].clone()).collect();
        let symbol = cell.symbol().to_string();
        let chrome = chrome(buf, x, y);
        let mut sent = width;
        if width == 1 && symbol.chars().next().is_some_and(ambiguous) {
            match narrow(&symbol).filter(|_| chrome) {
                Some(stand_in) => {
                    cells[0].set_symbol(stand_in);
                }
                None => sent = 2,
            }
        }
        out.push(Glyph {
            frame: chrome && cells::frame_cell(&symbol, colour(cell.fg), colour(cell.bg)),
            blank: symbol == " " && width == 1,
            cells,
            x,
            sent,
        });
        x += width;
    }
    out
}

/// Whether the cell at (`x`, `y`) of `buf` is sopen's own chrome, which §10's stand-ins are for
/// (content is not substituted, 10 §10): the cut's `…`, or a frame's cell — a box line or block,
/// a scroll bar's arrow ([`cells::frame_cell`]) — but for a box line between two glyphs of its
/// colours that are no blank and no frame's, judged whole (a wide one's second cell is no
/// blank), which is a name's (`a─b`, `한─b`).
pub(crate) fn chrome(buf: &Buffer, x: u16, y: u16) -> bool {
    let cell = &buf[(x, y)];
    let symbol = cell.symbol();
    if symbol == "…" {
        return true;
    }
    if !cells::frame_cell(symbol, colour(cell.fg), colour(cell.bg)) {
        return false;
    }
    let a = buf.area;
    // The glyphs on either side, whole: the one before may be a wide glyph, `x - 1` its second
    // cell.
    let wide_before = x >= a.left() + 2 && cells::width(buf[(x - 2, y)].symbol()) > 1;
    let before = if wide_before {
        x.checked_sub(2)
    } else {
        x.checked_sub(1)
    };
    let letter = |k: u16| {
        let c = &buf[(k, y)];
        (c.fg, c.bg) == (cell.fg, cell.bg)
            && !c.symbol().trim().is_empty()
            && !c.symbol().chars().any(cells::frame_glyph)
    };
    let inside =
        before.is_some_and(|k| k >= a.left() && letter(k)) && x + 1 < a.right() && letter(x + 1);
    !inside
}

/// Lays the glyphs `run` of `row` (a stretch of one pair of colours, or a frame's cell) into
/// `buf` at their own cells, so that no glyph after them leaves its column: a letter laid in two
/// cells takes the room of the run's first blank after it that ends a word — before another
/// blank, or the run's last glyph — never a wide glyph's second cell nor the space between two
/// words; a run with no such blank is cut, a cell earlier, with `~` (01 §3a) — a wide glyph whole,
/// never half.
fn lay_run(buf: &mut Buffer, row: &mut Row, y: u16, run: std::ops::Range<usize>) {
    let units = &row.units[run.clone()];
    let start = units[0].x;
    let room: u16 = units.iter().map(|u| u.cells.len() as u16).sum();
    let mut gone = vec![false; units.len()];
    let mut need: u16 = units.iter().map(|u| u.sent).sum();
    for i in 0..units.len() {
        if need <= room {
            break;
        }
        if units[i].sent <= units[i].cells.len() as u16 {
            continue;
        }
        let ends_word = |k: usize| units[k].blank && (k + 1 == units.len() || units[k + 1].blank);
        if let Some(k) = (i + 1..units.len()).find(|&k| !gone[k] && ends_word(k)) {
            gone[k] = true;
            need -= 1;
        }
    }
    let kept: Vec<usize> = (0..units.len()).filter(|&k| !gone[k]).collect();
    let cut = need > room;
    let left = buf.area.left();
    let mut at = start;
    let idx = |at: u16| usize::from(at - left);
    let mut cut_at = None;
    for &k in &kept {
        let u = &units[k];
        if cut && at + u.sent + 1 > start + room {
            cut_at = Some(k);
            break;
        }
        for (n, cell) in u.cells.iter().enumerate() {
            let x = at + n as u16;
            buf[(x, y)] = cell.clone();
            if n > 0 {
                buf[(x, y)].set_symbol("");
            }
            row.shown[idx(x)] = u.x + n as u16;
        }
        if u.sent > u.cells.len() as u16 {
            // A letter laid in two cells: its second shows its own column.
            let x = at + 1;
            buf[(x, y)] = u.cells[0].clone();
            buf[(x, y)].set_symbol("");
            row.shown[idx(x)] = u.x;
        }
        for n in 0..u.cells.len() as u16 {
            row.place[usize::from(u.x + n - left)] = at + n.min(u.sent - 1);
        }
        at += u.sent;
    }
    if let Some(first) = cut_at {
        // The cut: `~` in the run's colours, then blanks to its end.
        let pen = units[first].cells[0].clone();
        for x in at..start + room {
            buf[(x, y)] = pen.clone();
            buf[(x, y)].set_symbol(if x == at { "~" } else { " " });
            row.shown[idx(x)] = x.max(units[first].x).min(start + room - 1);
        }
        for u in &units[first..] {
            for n in 0..u.cells.len() as u16 {
                row.place[usize::from(u.x + n - left)] = at;
            }
        }
    }
    for (k, u) in units.iter().enumerate() {
        if gone[k] {
            // A blank given up: where the next glyph is shown.
            let next = (k + 1..units.len()).find(|&n| !gone[n]);
            let to = next.map_or(start + room - 1, |n| {
                row.place[usize::from(units[n].x - left)]
            });
            row.place[usize::from(u.x - left)] = to.min(start + room - 1);
        }
    }
}

/// A cell's colour as egui's, when it is RGB.
fn colour(c: Color) -> Option<egui::Color32> {
    match c {
        Color::Rgb(r, g, b) => Some(egui::Color32::from_rgb(r, g, b)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// The receipt (10 §15): `SOPEN_CELLS`.

/// The attribute table's keys, in the order they are given; past 62 pairs of colours a key is
/// two of them (and so on).
const KEYS: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// The `SOPEN_CELLS` receipt of `screen` (10 §15): one format for both backends, so that the
/// receipts of one screen are the same bytes and TS-1 is a diff of two files.
///
/// - `sopen cells <cols>x<rows> caret <x>,<y>` (`caret none`), then a line per row: its text, a
///   wide glyph once (it takes two cells), every blank kept.
/// - `attributes <n>`, then a line per row of `n`-character keys, one per cell (a wide glyph's
///   second cell has its first's, as a terminal paints it), then a line per key in the order the
///   keys first appear: the key, its foreground and its background as `rrggbb`.
///
/// The cells of `masked` — the mode button: `[GUI]` in the terminal, none in a window started
/// directly, the one difference 10 §5 allows — are blanks in their colours. The grid is the one
/// both backends lay: an ambiguous-wide terminal's stand-ins ([`narrow`]) are its printing's.
pub(crate) fn cells_text(screen: &Screen, masked: Option<Cells>) -> String {
    let mut buf = screen.buf.clone();
    if let Some(r) = masked.map(|r| r.intersection(buf.area)) {
        for y in r.top()..r.bottom() {
            for x in r.left()..r.right() {
                buf[(x, y)].set_symbol(" ");
            }
        }
    }
    let a = buf.area;
    let caret = screen
        .caret
        .map_or_else(|| "none".to_string(), |c| format!("{},{}", c.x, c.y));
    let mut out = format!("sopen cells {}x{} caret {caret}\n", a.width, a.height);
    for y in a.top()..a.bottom() {
        out.push_str(&cells::row_text(&buf, y).0);
        out.push('\n');
    }
    let mut pairs: Vec<(Color, Color)> = vec![];
    let mut seen = std::collections::HashMap::new();
    let mut keyed = Vec::with_capacity(buf.content.len());
    for y in a.top()..a.bottom() {
        for x in a.left()..a.right() {
            let pair = (buf[(x, y)].fg, buf[(x, y)].bg);
            keyed.push(*seen.entry(pair).or_insert_with(|| {
                pairs.push(pair);
                pairs.len() - 1
            }));
        }
    }
    let mut n = 1;
    while KEYS.len().pow(n) < pairs.len() {
        n += 1;
    }
    let key = |i: usize| -> String {
        (0..n)
            .rev()
            .map(|k| char::from(KEYS[i / KEYS.len().pow(k) % KEYS.len()]))
            .collect()
    };
    out.push_str(&format!("attributes {n}\n"));
    for row in keyed.chunks(usize::from(a.width).max(1)) {
        out.extend(row.iter().map(|&i| key(i)));
        out.push('\n');
    }
    let hex = |c: Color| match c {
        Color::Rgb(r, g, b) => format!("{r:02x}{g:02x}{b:02x}"),
        _ => "------".to_string(),
    };
    for (i, &(fg, bg)) in pairs.iter().enumerate() {
        out.push_str(&format!("{} {} {}\n", key(i), hex(fg), hex(bg)));
    }
    out
}

/// What the window's `SOPEN_CELLS` receipt keeps of one of its passes, taken inside it after its
/// `ui` ([`keep_window_pass`]): the whole cells its content holds, and the texts egui drew — a
/// hand-off box's title.
pub(crate) struct WindowPass {
    pub cols: u16,
    pub rows: u16,
    titles: Vec<(Pos2, String)>,
}

/// The window's pass for its receipt ([`WindowPass`]), from inside it after its `ui`.
pub(crate) fn keep_window_pass(ctx: &egui::Context) -> WindowPass {
    let size = ctx.content_rect().size();
    let whole = |room: f32, cell: f32| (room / cell).floor().max(0.0) as u16;
    let order: Vec<egui::LayerId> = ctx.memory(|m| m.layer_ids().collect());
    let shapes: Vec<egui::epaint::ClippedShape> = ctx.graphics(|g| {
        (order.iter().filter_map(|&l| g.get(l)))
            .flat_map(|list| list.all_entries().cloned())
            .collect()
    });
    WindowPass {
        cols: whole(size.x, CELL.x),
        rows: whole(size.y, CELL.y),
        titles: titles(&shapes),
    }
}

/// The window's `SOPEN_CELLS` receipt (10 §15) of the pass `pass` was taken in, once that pass
/// has ended (before anything of the next is recorded): the cells it painted ([`cells::layers`],
/// recording on) — what its end painted above the window's own, F1's help, included, as the
/// terminal's receipt has it — laid as the terminal lays its own ([`screen_of`]), its mode button
/// left out ([`cells_text`]).
pub(crate) fn window_receipt(ctx: &egui::Context, pass: &WindowPass) -> String {
    let (cols, rows) = (pass.cols, pass.rows);
    let screen = screen_of(ctx, &cells::layers(ctx), &pass.titles, cols, rows, None);
    cells_text(&screen, mode_cells(ctx, cols, rows))
}

/// The mode button's cells on a `cols` × `rows` grid, where the last pass drew it.
fn mode_cells(ctx: &egui::Context, cols: u16, rows: u16) -> Option<Cells> {
    chrome::mode_button_rect(ctx).and_then(|r| cells_over(r, cols, rows))
}

/// The `SOPEN_CELLS` receipt of a run of the terminal (10 §15): where it goes, written once the
/// screen has been ready for three passes in a row ([`Backend::run`]'s loop); a screen that has
/// not settled within `limit` (`SOPEN_WATCHDOG`) ends the run with an error, the terminal given
/// back as always.
pub(crate) struct Receipt {
    pub path: PathBuf,
    /// How long the screen may take to settle: the run then ends with an error.
    pub limit: Duration,
    since: Instant,
    /// The passes in a row the screen has been ready for.
    ready: u32,
    /// The line it prints once written.
    pub said: Option<String>,
}

impl Receipt {
    pub fn new(path: PathBuf, limit: Duration) -> Receipt {
        Receipt {
            path,
            limit,
            since: Instant::now(),
            ready: 0,
            said: None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Printing.

/// What has been printed into a terminal, and what prints the next screen there.
pub(crate) struct Term<O: Write> {
    pub out: O,
    truecolor: bool,
    /// The terminal draws a glyph of ambiguous width in two cells: each screen goes through
    /// [`for_ambiguous_wide`], and a row that changed is printed whole.
    pub wide: bool,
    /// The screen as the terminal shows it; `None`: unknown, the next is printed whole.
    shown: Option<Buffer>,
    /// In an ambiguous-wide terminal, row by row, the grid's column each shown cell is of
    /// ([`for_ambiguous_wide`]); empty: each its own.
    columns: Vec<Vec<u16>>,
    /// Where the terminal's cursor is shown (`None`: hidden).
    cursor: Option<Position>,
}

impl<O: Write> Term<O> {
    pub fn new(out: O, truecolor: bool) -> Self {
        Term {
            out,
            truecolor,
            wide: false,
            shown: None,
            columns: vec![],
            cursor: None,
        }
    }

    /// The grid's column of the cell the terminal shows at (`x`, `y`): the printer moves cells
    /// for a letter it lays in two in an ambiguous-wide terminal, so a click there lands on the
    /// cell egui drew; elsewhere `x`.
    pub fn grid_column(&self, x: u16, y: u16) -> u16 {
        let row = self.columns.get(usize::from(y));
        row.and_then(|r| r.get(usize::from(x)))
            .copied()
            .unwrap_or(x)
    }

    /// The terminal's screen is unknown (entered, resized): the next is printed whole.
    fn forget(&mut self) {
        self.shown = None;
        self.columns.clear();
        self.cursor = None;
    }

    /// An SGR colour: RGB, or its 256-colour index (10 §5).
    fn colour(&self, c: Color, ground: u8) -> String {
        match c {
            Color::Rgb(r, g, b) if self.truecolor => format!("{ground}8;2;{r};{g};{b}"),
            Color::Rgb(r, g, b) => {
                let i = cells::index256(egui::Color32::from_rgb(r, g, b));
                format!("{ground}8;5;{i}")
            }
            _ => format!("{ground}9"),
        }
    }

    /// Prints `next`: the cells that differ from what is shown (all of them when that is not
    /// known), each run from where the cursor stands, a colour change only where the colours
    /// change; then the cursor at the caret, or hidden. In an ambiguous-wide terminal `next` as
    /// [`for_ambiguous_wide`] lays it, and every row that changed whole.
    pub fn draw(&mut self, next: &Screen) -> io::Result<()> {
        let laid;
        let next = if self.wide {
            let columns;
            (laid, columns) = for_ambiguous_wide(next);
            self.columns = columns;
            &laid
        } else {
            self.columns.clear();
            next
        };
        let mut bytes = String::new();
        let whole;
        let updates = match &self.shown {
            // The terminal's cursor goes two cells for a glyph the grid has in one: a cell of a
            // row is never printed on its own there.
            Some(shown) if shown.area == next.buf.area && self.wide => {
                whole = changed_rows(shown, &next.buf);
                whole
                    .iter()
                    .map(|&(x, y)| (x, y, &next.buf[(x, y)]))
                    .collect()
            }
            Some(shown) if shown.area == next.buf.area => shown.diff(&next.buf),
            _ => {
                // The cursor hidden first: where the terminal shows it is not known (a resize),
                // and it stays hidden unless the screen has a caret.
                bytes.push_str("\x1b[?25l\x1b[2J");
                self.cursor = None;
                whole = every_cell(&next.buf);
                whole
                    .iter()
                    .map(|&(x, y)| (x, y, &next.buf[(x, y)]))
                    .collect()
            }
        };
        let moved = !updates.is_empty();
        let mut at: Option<(u16, u16)> = None;
        let mut pen: Option<(Color, Color)> = None;
        for (x, y, cell) in updates {
            let symbol = cell.symbol();
            if pen != Some((cell.fg, cell.bg)) {
                let fg = self.colour(cell.fg, 3);
                let bg = self.colour(cell.bg, 4);
                bytes.push_str(&format!("\x1b[{fg};{bg}m"));
                pen = Some((cell.fg, cell.bg));
            }
            // A glyph laid in two cells for an ambiguous-wide terminal: its second cell a blank
            // first, so that one whose table draws it in one cell shows no stale cell there.
            let laid = self.wide
                && cells::width(symbol) == 1
                && (next.buf.cell((x + 1, y))).is_some_and(|c| c.symbol().is_empty());
            if laid {
                bytes.push_str(&format!("\x1b[{};{}H ", y + 1, x + 2));
                at = None;
            }
            if at != Some((x, y)) {
                bytes.push_str(&format!("\x1b[{};{}H", y + 1, x + 1));
            }
            bytes.push_str(if symbol.is_empty() { " " } else { symbol });
            // Where the cursor went. In an ambiguous-wide terminal only ASCII and a wide glyph
            // are sure (terminals' tables differ on the rest): after any other the next cell is
            // moved to.
            let w = cells::width(symbol) as u16;
            let sure = !self.wide || w > 1 || symbol.is_ascii();
            at = sure.then_some((x + w.max(1), y));
        }
        match next.caret {
            Some(c) if moved || self.cursor != Some(c) => {
                bytes.push_str(&format!("\x1b[{};{}H", c.y + 1, c.x + 1));
                if self.cursor.is_none() {
                    bytes.push_str("\x1b[?25h");
                }
            }
            None if self.cursor.is_some() => bytes.push_str("\x1b[?25l"),
            _ => {}
        }
        self.out.write_all(bytes.as_bytes())?;
        self.out.flush()?;
        self.shown = Some(next.buf.clone());
        self.cursor = next.caret;
        Ok(())
    }

    /// `text` to the terminal's clipboard: OSC 52 (10 §4).
    pub fn copy(&mut self, text: &str) -> io::Result<()> {
        write!(self.out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
        self.out.flush()
    }
}

/// Every cell of `buf` that starts a glyph, row by row — a wide glyph's second cell left out, and
/// a cell with no symbol (another's second half, [`for_ambiguous_wide`]).
fn every_cell(buf: &Buffer) -> Vec<(u16, u16)> {
    let a = buf.area;
    let mut out = vec![];
    for y in a.top()..a.bottom() {
        let mut x = a.left();
        while x < a.right() {
            if !buf[(x, y)].symbol().is_empty() {
                out.push((x, y));
            }
            x += (cells::width(buf[(x, y)].symbol()) as u16).clamp(1, 2);
        }
    }
    out
}

/// [`every_cell`] of `next` in the rows where it differs from `shown`.
fn changed_rows(shown: &Buffer, next: &Buffer) -> Vec<(u16, u16)> {
    let a = next.area;
    let changed: Vec<bool> = (a.top()..a.bottom())
        .map(|y| (a.left()..a.right()).any(|x| shown[(x, y)] != next[(x, y)]))
        .collect();
    let row = |y: u16| usize::from(y - a.top());
    (every_cell(next).into_iter())
        .filter(|&(_, y)| changed[row(y)])
        .collect()
}

/// `bytes` in base64 (RFC 4648, padded), as OSC 52 carries them.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let byte = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The loop.

/// How a run of the terminal ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    /// The browser closed (Cmd/Ctrl+Q, its own close), as a window closes.
    Closed,
    /// A signal ([`Tty::stopped`]).
    Signal(i32),
}

/// One pass's word to the loop.
struct Passed {
    /// A close went through: the browser ends.
    closed: bool,
    /// The switch was asked for (F12, Alt+G).
    switch: bool,
    /// When egui wants the next pass.
    repaint: Duration,
}

/// The terminal backend: a terminal, the bytes going to it, the context its passes run in.
pub(crate) struct Backend<T: Tty, O: Write> {
    pub tty: T,
    pub term: Term<O>,
    pub ctx: egui::Context,
    input: TermInput,
    /// The grid: the terminal's size in cells.
    pub size: (u16, u16),
    /// What the terminal answered at start (asked once, [`probe`]).
    probed: Option<Probed>,
    /// The kitty flag is pushed now.
    pushed: bool,
    /// The terminal is held (raw mode, the alternate screen, the modes).
    held: bool,
    /// A close the last pass asked for, delivered with the next one.
    close_pending: bool,
    /// The backend's own line on the status row (a switch that did not happen), until the next
    /// key or click.
    said: Option<String>,
    /// The hand-off boxes the last screen showed.
    hand_offs: Vec<Cells>,
    /// The texts egui itself drew in the last pass (not a cell's glyph), where: a hand-off box's
    /// title is the pane's or the dialog's own.
    titles: Vec<(Pos2, String)>,
    /// `SOPEN_CELLS` (10 §15): the receipt the run writes once the screen has settled, then ends.
    pub receipt: Option<Receipt>,
    /// `SOPEN_STATE`'s `help=1`: F1 is still to be pressed once the screen is ready.
    pub help_first: bool,
    /// When a repaint asked for since the last look wants its pass — from any thread
    /// (`Context::set_request_repaint_callback`): a worker done while the terminal is idle.
    asked: Arc<Mutex<Option<Instant>>>,
    /// The passes' clock: `offset` + the time since the backend was made.
    started: Instant,
    pub offset: f64,
}

impl<T: Tty, O: Write> Backend<T, O> {
    /// The backend on `tty` writing `out`, its passes in `ctx` (in the skin already:
    /// [`chrome::install`]), which it marks as the terminal's (Alt+digits and Alt+G, §9) and
    /// records cells in.
    pub fn new(tty: T, out: O, ctx: egui::Context, truecolor: bool) -> Self {
        cells::set_recording(&ctx, true);
        super::mark_terminal(&ctx);
        let asked: Arc<Mutex<Option<Instant>>> = Arc::default();
        let tell = asked.clone();
        ctx.set_request_repaint_callback(move |info| {
            // A delay past what an `Instant` holds is no deadline.
            let Some(at) = Instant::now().checked_add(info.delay.max(IDLE_FLOOR)) else {
                return;
            };
            let mut asked = tell.lock().unwrap_or_else(|p| p.into_inner());
            *asked = Some(asked.map_or(at, |was| was.min(at)));
        });
        Backend {
            tty,
            term: Term::new(out, truecolor),
            ctx,
            input: TermInput::new(cfg!(target_os = "macos")),
            size: (80, 24),
            probed: None,
            pushed: false,
            held: false,
            close_pending: false,
            said: None,
            hand_offs: vec![],
            titles: vec![],
            receipt: None,
            help_first: false,
            asked,
            started: Instant::now(),
            offset: 0.0,
        }
    }

    /// The grid in points: the size a window opened by the switch gets (T16).
    pub fn points(&self) -> [f32; 2] {
        [
            f32::from(self.size.0) * CELL.x,
            f32::from(self.size.1) * CELL.y,
        ]
    }

    /// Takes the terminal (see the module's header): raw mode, the modes, the size; the first
    /// time the start's one round trip ([`probe`]: the ambiguous width and the kitty keyboard
    /// protocol, on the alternate screen, which the first print clears); the kitty flag where the
    /// terminal answers; its unread input dropped. A terminal that hung up meanwhile is asked
    /// nothing more.
    pub fn enter(&mut self) -> io::Result<()> {
        self.tty.raw(true)?;
        self.held = true;
        if let Ok((cols, rows)) = self.tty.size() {
            if cols > 0 && rows > 0 {
                self.size = (cols, rows);
            }
        }
        self.term.out.write_all(ENTER.as_bytes())?;
        // The probe writes past the buffer: what is in it goes first.
        self.term.out.flush()?;
        let probed = match self.probed {
            Some(probed) => probed,
            None => *self.probed.insert(self.tty.probe()),
        };
        // A terminal that hung up meanwhile is asked nothing more: the loop ends the run.
        if self.tty.stopped().is_some() {
            return Ok(());
        }
        self.term.wide = probed.ambiguous;
        cells::set_ambiguous_wide(&self.ctx, self.term.wide);
        if probed.kitty {
            self.term.out.write_all(KITTY_PUSH.as_bytes())?;
            self.pushed = true;
        }
        self.term.out.flush()?;
        self.tty.drop_input();
        self.term.forget();
        Ok(())
    }

    /// Gives the terminal back as it was (T19): once, every write's error ignored.
    pub fn leave(&mut self) {
        if !std::mem::take(&mut self.held) {
            return;
        }
        let mut bytes = String::new();
        if std::mem::take(&mut self.pushed) {
            bytes.push_str(KITTY_POP);
        }
        bytes.push_str(LEAVE);
        let _ = self.term.out.write_all(bytes.as_bytes());
        let _ = self.term.out.flush();
        let _ = self.tty.raw(false);
        self.term.forget();
    }

    fn now(&self) -> f64 {
        self.offset + self.started.elapsed().as_secs_f64()
    }

    /// One pass of `win` with `events`, as the shell runs a window's (and the test kit's driver
    /// in its `as_shell` mode): a close asked for in one pass is delivered with the next, the
    /// shell's part of closing runs first ([`super::closing`]); what egui asked of the platform
    /// is done — a copy goes to the terminal's clipboard.
    fn pass<W: Window + ?Sized>(
        &mut self,
        win: &mut W,
        events: Vec<egui::Event>,
    ) -> io::Result<Passed> {
        let (cols, rows) = self.size;
        let size = Vec2::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
        // The modifiers come as the translator reports their changes (`ModifiersChanged`), never
        // with every pass: a pass with any event makes egui ask for another at once, and an idle
        // terminal would run one every few milliseconds.
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(self.now()),
            max_texture_side: Some(8192),
            events,
            focused: self.input.focused(),
            ..Default::default()
        };
        let delivered = std::mem::take(&mut self.close_pending);
        if delivered {
            (raw.viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .events)
                .push(egui::ViewportEvent::Close);
        }
        chrome::set_mode_button(&self.ctx, "[GUI]");
        let mut out = self.ctx.run_ui(raw, |ui| {
            // Only the cells of the pass whose shapes come out: egui runs this again after a
            // pass it discards.
            cells::clear(ui.ctx());
            super::closing(ui.ctx(), win, false);
            win.ui(ui);
        });
        // No renderer: the textures stay in the context.
        out.textures_delta.clear();
        for command in std::mem::take(&mut out.platform_output.commands) {
            if let egui::OutputCommand::CopyText(text) = command {
                if text.len() > OSC52_MAX {
                    let kib = text.len().div_ceil(1024);
                    self.said = Some(format!(
                        "too large to copy through the terminal ({kib} KiB)"
                    ));
                } else {
                    self.term.copy(&text)?;
                }
            }
        }
        self.titles = titles(&out.shapes);
        let mut sent = vec![];
        let mut repaint = Duration::MAX;
        for (id, viewport) in std::mem::take(&mut out.viewport_output) {
            if id == egui::ViewportId::ROOT {
                repaint = viewport.repaint_delay;
            }
            sent.extend(viewport.commands);
        }
        let cancelled = sent.contains(&egui::ViewportCommand::CancelClose);
        self.close_pending = sent.contains(&egui::ViewportCommand::Close);
        Ok(Passed {
            closed: delivered && !cancelled,
            switch: super::switch_asked(&self.ctx),
            repaint,
        })
    }

    /// The screen of the last pass ([`screen_of`]), with the backend's line.
    pub fn screen(&self) -> Screen {
        let (cols, rows) = self.size;
        let layers = cells::layers(&self.ctx);
        let said = self.said.as_deref();
        screen_of(&self.ctx, &layers, &self.titles, cols, rows, said)
    }

    /// Prints the last pass's screen.
    pub fn draw(&mut self) -> io::Result<()> {
        let screen = self.screen();
        self.hand_offs = screen.hand_offs.clone();
        self.term.draw(&screen)
    }

    /// One crossterm event: egui's events of it, and whether it is a click on a hand-off box
    /// (the switch). A key or a click puts away the backend's line; a resize lays out anew.
    /// During a receipt run only a resize is read.
    fn take(&mut self, ev: &CEvent) -> (Vec<egui::Event>, bool) {
        // A receipt is `SOPEN_KEYS`'s alone, as a window's is (10 §15): what the terminal sends
        // goes unread; its size does not.
        if self.receipt.is_some() && !matches!(ev, CEvent::Resize(..)) {
            return (vec![], false);
        }
        // Where the terminal shows the pointer, on the grid (an ambiguous-wide terminal's row
        // can have cells moved: [`Term::grid_column`]).
        let ev = &match ev {
            CEvent::Mouse(m) => CEvent::Mouse(MouseEvent {
                column: self.term.grid_column(m.column, m.row),
                ..*m
            }),
            other => other.clone(),
        };
        match ev {
            CEvent::Resize(cols, rows) if *cols > 0 && *rows > 0 => {
                self.size = (*cols, *rows);
                self.term.forget();
            }
            CEvent::Key(k) if k.kind != KeyEventKind::Release => self.said = None,
            CEvent::Mouse(m) if matches!(m.kind, MouseEventKind::Down(_)) => {
                self.said = None;
                let at = Position::new(m.column, m.row);
                let left = m.kind == MouseEventKind::Down(MouseButton::Left);
                // While the key help is open every click is its own (`help::input_hook`).
                let free = !super::help::is_open(&self.ctx);
                if left && free && self.hand_offs.iter().any(|r| r.contains(at)) {
                    return (vec![], true);
                }
            }
            _ => {}
        }
        (self.input.translate(ev), false)
    }

    /// Runs `win` in the terminal until it closes or a signal comes: one pass per event and
    /// whenever egui asks for one, the screen printed once no event is waiting. `switch` is the
    /// switch to the window (F12, Alt+G, a hand-off box): its answer is a line for the status
    /// row (it did not happen) or none. The terminal is taken first and given back whatever
    /// ends the run — a panic in a pass too, which then goes on.
    pub fn run<W: Window + ?Sized>(
        &mut self,
        win: &mut W,
        switch: &mut dyn FnMut(&mut Self) -> io::Result<Option<String>>,
    ) -> io::Result<End> {
        self.enter()?;
        let ran =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.serve(win, switch)));
        self.leave();
        match ran {
            // A terminal gone with its hang-up fails a read or a write: the signal says why.
            Ok(Err(e)) => self.tty.stopped().map(End::Signal).ok_or(e),
            Ok(end) => end,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    fn serve<W: Window + ?Sized>(
        &mut self,
        win: &mut W,
        switch: &mut dyn FnMut(&mut Self) -> io::Result<Option<String>>,
    ) -> io::Result<End> {
        let mut due = Some(Instant::now());
        loop {
            if let Some(signal) = self.tty.stopped() {
                return Ok(End::Signal(signal));
            }
            if let Some(r) = self
                .receipt
                .as_ref()
                .filter(|r| r.since.elapsed() > r.limit)
            {
                return Err(io::Error::other(format!(
                    "the cells hook stalled for {:?}: the screen did not settle",
                    r.limit
                )));
            }
            let wait = due.map_or(TICK, |d| d.saturating_duration_since(Instant::now()));
            let next = self.tty.next(wait.min(TICK))?;
            // A repaint asked for meanwhile, from any thread.
            if let Some(at) = self.asked.lock().unwrap_or_else(|p| p.into_inner()).take() {
                due = Some(due.map_or(at, |d| d.min(at)));
            }
            // A receipt run reads no input but Ctrl+C, which ends it as SIGINT would.
            if self.receipt.is_some() && next.as_ref().is_some_and(ctrl_c) {
                return Ok(End::Signal(libc::SIGINT));
            }
            let (mut events, hand_off) = match next {
                Some(ev) => self.take(&ev),
                None if due.is_some_and(|d| Instant::now() >= d) => (vec![], false),
                None => continue,
            };
            // `help=1`: F1, once the screen is ready, as the window's shell presses it.
            if self.help_first && win.ready() {
                self.help_first = false;
                events.extend([true, false].map(|pressed| egui::Event::Key {
                    key: egui::Key::F1,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }));
            }
            let passed = self.pass(win, events)?;
            if passed.closed {
                return Ok(End::Closed);
            }
            if passed.switch || hand_off {
                // A signal that came during this pass ends the run: no window for it.
                if let Some(signal) = self.tty.stopped() {
                    return Ok(End::Signal(signal));
                }
                self.said = switch(self)?;
                due = Some(Instant::now());
                continue;
            }
            if !self.tty.pending()? {
                self.draw()?;
            }
            // A close asked for is delivered with the next pass, at once (as eframe does).
            let repaint = if self.close_pending {
                Duration::ZERO
            } else {
                passed.repaint.max(IDLE_FLOOR)
            };
            due = Instant::now().checked_add(repaint);
            if let Some(at) = self.asked.lock().unwrap_or_else(|p| p.into_inner()).take() {
                due = Some(due.map_or(at, |d| d.min(at)));
            }
            if self.help_first && win.ready() {
                due = Some(Instant::now());
            }
            if self.take_receipt(&*win, &mut due)? {
                return Ok(End::Closed);
            }
        }
    }

    /// `SOPEN_CELLS` after a pass (10 §15): once the screen has been ready for three passes in a
    /// row — `SOPEN_KEYS` played, nothing on its way, `help=1`'s F1 pressed, as the screenshot
    /// hook waits — the receipt
    /// of the last one is written, and the run ends (true). Meanwhile a ready pass asks for the
    /// next at once: an idle terminal runs none.
    fn take_receipt<W: Window + ?Sized>(
        &mut self,
        win: &W,
        due: &mut Option<Instant>,
    ) -> io::Result<bool> {
        let settled = win.ready() && !self.help_first;
        let ready = match self.receipt.as_mut() {
            Some(r) => {
                r.ready = if settled { r.ready + 1 } else { 0 };
                r.ready
            }
            None => return Ok(false),
        };
        if ready == 0 {
            return Ok(false);
        }
        if ready < 3 {
            *due = Some(Instant::now());
            return Ok(false);
        }
        let (cols, rows) = self.size;
        let text = cells_text(&self.screen(), mode_cells(&self.ctx, cols, rows));
        let Some(r) = self.receipt.as_mut() else {
            return Ok(false);
        };
        std::fs::write(&r.path, text).map_err(|e| {
            io::Error::new(e.kind(), format!("cannot write {}: {e}", r.path.display()))
        })?;
        r.said = Some(format!("cells {} {cols}x{rows}", r.path.display()));
        Ok(true)
    }
}

impl<T: Tty, O: Write> Drop for Backend<T, O> {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Whether `ev` is Ctrl+C pressed.
fn ctrl_c(ev: &CEvent) -> bool {
    matches!(ev, CEvent::Key(k) if k.kind != KeyEventKind::Release
        && k.code == crossterm::event::KeyCode::Char('c')
        && k.modifiers == crossterm::event::KeyModifiers::CONTROL)
}

/// The cells `rect` (points) touches on a `cols` × `rows` grid; `None` when it is off it.
fn cells_over(rect: Rect, cols: u16, rows: u16) -> Option<Cells> {
    let clamp = |v: f32, cell: f32, n: u16| (v / cell).clamp(0.0, f32::from(n));
    let (x0, y0) = (
        clamp(rect.min.x, CELL.x, cols).floor(),
        clamp(rect.min.y, CELL.y, rows).floor(),
    );
    let (x1, y1) = (
        clamp(rect.max.x, CELL.x, cols).ceil(),
        clamp(rect.max.y, CELL.y, rows).ceil(),
    );
    (x1 > x0 && y1 > y0)
        .then(|| Cells::new(x0 as u16, y0 as u16, (x1 - x0) as u16, (y1 - y0) as u16))
}

// ---------------------------------------------------------------------------------------------
// The switch (10 §7.1–§7.2).

/// Where the switch opens the window (10 §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Place {
    /// This machine, which has a display: in this process.
    Here,
    /// Another machine (X2's remote switch).
    Host(String),
    /// This machine, which has no display.
    NoDisplay,
}

/// The run's `--here` and `--target`: with `SOPEN_TARGET` and the config's `target`, read when
/// the owner switches, they say where the window opens (10 §7.1, 01 §4a).
#[derive(Debug, Clone, Default)]
pub struct Destination {
    pub here: bool,
    pub target: Option<String>,
}

impl Destination {
    fn place(&self) -> Result<Place> {
        let chosen = crate::forward::choose(
            self.here,
            self.target.as_deref(),
            crate::forward::target_env,
            || Ok(crate::config::load(&crate::config::path()?)?.target),
        )?;
        let hostname = crate::forward::hostname();
        Ok(match chosen {
            Some(host) if !crate::forward::is_here(&host, hostname.as_deref()) => Place::Host(host),
            _ if !super::display_available() => Place::NoDisplay,
            _ => Place::Here,
        })
    }
}

/// What moves with the browser between the terminal's context and the window's: what has the
/// keyboard, and the edit state (cursor, selection) of the field that has it — a context's own,
/// so a dialog's caret would be lost on the way — and F1's help, open where it was scrolled to
/// or closed (the context's too).
#[derive(Clone, Default)]
pub(crate) struct Carry {
    focus: Option<egui::Id>,
    edit: Option<egui::text_edit::TextEditState>,
    help: super::help::Carried,
}

impl Carry {
    pub fn of(ctx: &egui::Context) -> Carry {
        let focus = ctx.memory(|m| m.focused());
        Carry {
            focus,
            edit: focus.and_then(|id| egui::text_edit::TextEditState::load(ctx, id)),
            help: super::help::carried(ctx),
        }
    }

    pub fn apply(&self, ctx: &egui::Context) {
        super::help::carry_into(ctx, &self.help);
        match self.focus {
            Some(id) => {
                if let Some(edit) = self.edit.clone() {
                    edit.store(ctx, id);
                }
                ctx.memory_mut(|m| m.request_focus(id));
            }
            // Nothing had the keyboard there: nothing has it here either.
            None => ctx.memory_mut(|m| {
                if let Some(had) = m.focused() {
                    m.surrender_focus(had);
                }
            }),
        }
    }
}

/// The browser in the terminal: shared with the window a switch opens.
struct Shared(Rc<RefCell<Browser>>);

impl Window for Shared {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.0.borrow_mut().ui(ui);
    }

    fn ready(&self) -> bool {
        self.0.borrow().ready()
    }

    fn close_requested(&mut self) -> bool {
        self.0.borrow_mut().close_requested()
    }

    fn keys(&self) -> &'static [FKey] {
        self.0.borrow().keys()
    }
}

/// The browser in a window the switch opened (10 §7.2): F12 there, its title row's `[TUI]`, or
/// any close — the close button, Cmd/Ctrl+Q and +W — ends the window and brings the browser back
/// to the terminal as it is, nothing asked (unsaved edits cross unsaved: one process holds
/// them).
pub(crate) struct Switched {
    browser: Rc<RefCell<Browser>>,
    carry: Rc<RefCell<Carry>>,
    first: bool,
}

impl Window for Switched {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.first) {
            self.carry.borrow().apply(&ctx);
            super::gestures_ended(&ctx);
        }
        chrome::set_mode_button(&ctx, "[TUI]");
        self.browser.borrow_mut().ui(ui);
        *self.carry.borrow_mut() = Carry::of(&ctx);
        if super::switch_asked(&ctx) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn ready(&self) -> bool {
        self.browser.borrow().ready()
    }

    fn close_requested(&mut self) -> bool {
        true
    }

    fn keys(&self) -> &'static [FKey] {
        self.browser.borrow().keys()
    }
}

/// The switch from the terminal (10 §7.1–§7.2) to `place`: a HOST or no display is a line for
/// the status row and nothing else; this machine leaves the terminal, says [`IN_A_WINDOW`],
/// runs the browser in `window` and takes the terminal again — its unread input dropped — with
/// the browser as the window left it. A window that did not open is a line (T31).
pub(crate) fn to_window<T: Tty, O: Write>(
    b: &mut Backend<T, O>,
    browser: &Rc<RefCell<Browser>>,
    place: Result<Place>,
    window: impl FnOnce(Switched) -> Result<()>,
) -> io::Result<Option<String>> {
    match place {
        Err(e) => return Ok(Some(format!("{e:#}"))),
        Ok(Place::Host(host)) => {
            return Ok(Some(format!(
                "switching to {host} comes with the remote switch; --here switches on this \
                 machine"
            )))
        }
        Ok(Place::NoDisplay) => return Ok(Some(NO_DISPLAY.to_string())),
        Ok(Place::Here) => {}
    }
    let carry = Rc::new(RefCell::new(Carry::of(&b.ctx)));
    b.leave();
    let _ = writeln!(b.term.out, "{IN_A_WINDOW}");
    let _ = b.term.out.flush();
    let ran = window(Switched {
        browser: browser.clone(),
        carry: carry.clone(),
        first: true,
    });
    b.enter()?;
    carry.borrow().apply(&b.ctx);
    super::gestures_ended(&b.ctx);
    Ok(ran
        .err()
        .map(|e| match e.downcast_ref::<super::NotStarted>() {
            Some(not) => format!(
                "the window did not open: {} — check the display, or set a target",
                not.reason
            ),
            None => format!("the window ended: {e:#}"),
        }))
}

// ---------------------------------------------------------------------------------------------
// The run.

/// A panic on the terminal's thread while it is held: its message, kept by the panic hook to be
/// printed once the terminal is back (on the alternate screen it would go with it).
static HELD_BY: Mutex<Option<(std::thread::ThreadId, Option<String>)>> = Mutex::new(None);

/// The process's panic hook, once: a panic on the thread [`HELD_BY`] names is kept there;
/// every other goes to the hook that was there.
fn keep_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let before = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let mut held = HELD_BY.lock().unwrap_or_else(|p| p.into_inner());
            match held.as_mut() {
                Some((id, kept)) if *id == std::thread::current().id() => {
                    *kept = Some(info.to_string());
                }
                _ => {
                    drop(held);
                    before(info);
                }
            }
        }));
    });
}

/// Shows the browser `start` opens in this terminal until it closes (10 §11 row 3) — with
/// `SOPEN_CELLS`, until its receipt is written ([`Receipt`]) — the switch to `to` (§7) on F12,
/// Alt+G or a hand-off box; then waits for what is still writing, as the window's
/// [`browser::open`] does. A signal that ended it is the error ([`Signalled`]).
pub fn open(start: browser::Start, hooks: &Hooks, to: Destination) -> Result<Outcome> {
    let mut signal = None;
    let shot = hooks.screenshot.clone();
    let limit = Duration::from_secs(super::watchdog_secs());
    let receipt = hooks.cells.clone().map(|path| Receipt::new(path, limit));
    let help = hooks.state.help;
    let outcome = browser::open_on(start, hooks, |title, browser| {
        signal = here(title, browser, &to, shot, receipt, help)?;
        Ok(())
    })?;
    match signal {
        Some(n) => Err(Signalled(n).into()),
        None => Ok(outcome),
    }
}

/// The browser in this terminal; the signal that ended it, if one did. `shot` (the
/// `SOPEN_SCREENSHOT` receipt): a window the switch opens is shot, then closes. `receipt`
/// (`SOPEN_CELLS`): the terminal's screen is written once it has settled, the run ends, and the
/// receipt's line is printed once the terminal is back. `help` (`SOPEN_STATE`'s `help=1`): F1 is
/// pressed once the screen is ready, as in the window.
fn here(
    title: &str,
    browser: Browser,
    to: &Destination,
    shot: Option<std::path::PathBuf>,
    receipt: Option<Receipt>,
    help: bool,
) -> Result<Option<i32>> {
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let shared = Rc::new(RefCell::new(browser));
    let truecolor = truecolor(std::env::var("COLORTERM").ok().as_deref());
    let out = io::BufWriter::with_capacity(1 << 16, io::stdout());
    let mut backend = Backend::new(Real::default(), out, ctx, truecolor);
    backend.receipt = receipt;
    backend.help_first = help;
    let mut switch = |b: &mut Backend<Real, _>| {
        let size = b.points();
        let hooks = Hooks {
            window_size: Some(size),
            screenshot: shot.clone(),
            ..Hooks::default()
        };
        to_window(b, &shared, to.place(), |switched| {
            super::run_window(title, size, switched, &hooks)
        })
    };
    keep_panics();
    // Once per run: a signal caught while a switch's window runs (its handlers off then) or
    // between two holds of the terminal is still the run's.
    STOP.store(0, Ordering::SeqCst);
    *HELD_BY.lock().unwrap_or_else(|p| p.into_inner()) = Some((std::thread::current().id(), None));
    let mut tui = Shared(shared.clone());
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        backend.run(&mut tui, &mut switch)
    }));
    let kept = HELD_BY.lock().unwrap_or_else(|p| p.into_inner()).take();
    let written = backend.receipt.as_mut().and_then(|r| r.said.take());
    drop(backend);
    match ran {
        Ok(end) => Ok(match end? {
            End::Closed => {
                if let Some(line) = written {
                    println!("{line}");
                }
                None
            }
            End::Signal(n) => Some(n),
        }),
        Err(panic) => {
            if let Some((_, Some(message))) = kept {
                eprintln!("{message}");
            }
            std::panic::resume_unwind(panic)
        }
    }
}

#[cfg(test)]
mod tests;
