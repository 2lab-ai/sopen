//! HOST's half of the switch (10 §7.2–§7.3, T30; X2-host): `sopen --here --gui --state-stdin --
//! FOLDER` reads the switch's state (10 §7.4) from the first line of its input and opens the
//! file browser at it in a switched window. F12, the title row's `[TUI]`, the close button and
//! Cmd/Ctrl+Q and +W (the shell's close, `closing()`) return: unsaved edits first ask the
//! window's own close prompt (07 §10; its Cancel keeps the window), what still writes — a
//! viewer's Save, a file operation, a git action — is waited for once the window is gone, as
//! every browser window waits ([`browser::open`]), and then the state the window handed back is
//! printed as one line, [`STATE_CUE`] and its JSON.
//!
//! The rest of the input is read to its end on a thread of its own, as `--hold-until-eof` waits
//! for it: its end is the other side's sopen gone (Ctrl+C there, the link dropping — a session
//! without a pty sends no SIGHUP, 10 §7.3). The window hears it in the shell's `logic`
//! ([`Window::logic`]), which runs before every pass and in place of it while the window is
//! hidden. With nothing to lose the window closes, no cue; with unsaved edits or a job that writes
//! ([`Browser::would_lose`]) it stays, after [`KEPT_CUE`], until it is closed here, by its own
//! close prompt — a window started directly from then on, no `[TUI]`. Either way the session is
//! let go: nothing more is read from it or written to it. A run a hook drives (`SOPEN_CELLS`,
//! `SOPEN_SCREENSHOT`) ends by its hook.
//!
//! The session can die at any time, the window open or gone, while something still writes — a
//! file operation's report after the window, a viewer's `waiting…`, a completion line in a pass.
//! A write to its dead pipe fails, `println!` and `eprintln!` panic on that, and what still
//! writes is cut short. So the run's standard output and error are relayed ([`relay_stdio`]):
//! fds 1 and 2 are pipes, and a thread copies each to the session's stream, which it keeps aside
//! and alone writes to; once a write there fails it only drains its pipe. Nothing in sopen ever
//! writes to a dead session. The relays answer requests ([`Ask`]): each first passes on all that
//! is in its pipe, then answers — so letting the session go ([`Ask::LetGo`], at most 2 s) never
//! drops the kept cue said just before it, and [`end_of_run`] ([`Ask::Flush`]) waits for the state
//! cue to reach a slow peer, up to [`CUE_WAIT`]. A dead session fails the write at once, so it is
//! answered at once. Order within each stream is kept.
//!
//! The cues are fixed lines, as [`crate::url::TUNNEL_OPEN`] is, written ignoring write errors
//! ([`crate::url::write_cue`]).

use super::browser::switch_state::{self, SwitchState};
use super::browser::{self, Browser, Start};
use super::{Hooks, Outcome, Window};
use anyhow::{Context as _, Result};
use eframe::egui;
use std::cell::RefCell;
use std::io::{BufRead, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// The line HOST's sopen prints once its window has handed the browser back (10 §7.3), a space
/// and the state's JSON line after it: the cue the switch's other side reads instead of passing
/// it on. Fixed, as [`crate::url::TUNNEL_OPEN`] is.
pub const STATE_CUE: &str = "sopen-xtools-state";

/// The line HOST's sopen prints when its input ended while the window holds something it would
/// lose (10 §7.3): the window stays, and nothing more is written. Fixed, as [`STATE_CUE`].
pub const KEPT_CUE: &str = "sopen-xtools-kept";

/// Who §7.4's refusal names: the input carries no name of the other machine, and its address
/// (`SSH_CONNECTION`) is not one to print.
const SENDER: &str = "the sender";

/// A switch state read from the sender's line and checked (10 §7.4): what [`run`] opens the
/// browser at.
pub struct Sent(SwitchState);

/// The state on the first line of `input` (the other side writes it, then holds the input open):
/// `Err` with the line that says why it is refused — a newer `v` with §7.4's, no line, one that
/// does not read — before anything is opened (10 §7.3: a usage error, exit 2).
pub fn read_state(input: &mut impl BufRead) -> Result<Sent, String> {
    let mut line = String::new();
    let read = input
        .read_line(&mut line)
        .map_err(|e| format!("cannot read the switch's state: {e}"))?;
    if read == 0 {
        return Err("the input ended before the switch's state".to_string());
    }
    let line = line.strip_suffix('\n').unwrap_or(&line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    switch_state::read(line, SENDER).map(Sent)
}

/// A state the browser cannot take ([`Browser::give_state`]): another host's — its paths are not
/// this window's. A usage error, as a refused line is (10 §7.3: exit 2).
#[derive(Debug)]
pub struct NotGiven(pub String);

impl std::fmt::Display for NotGiven {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotGiven {}

/// Opens the browser `start` makes at `sent` in a switched window (see the module's header),
/// its input watched to its end and its output relayed to the session ([`relay_stdio`]; `main`
/// calls [`end_of_run`] last); prints the cue once it has handed the browser back and what still
/// wrote has ended. A state `start` cannot take is [`NotGiven`], before any window.
pub fn run(start: Start, hooks: &Hooks, sent: Sent) -> Result<Outcome> {
    relay_stdio().context("cannot relay the output to the session")?;
    let input = Input::default();
    input.watch(std::io::stdin())?;
    run_on(start, hooks, sent, input, Cues::stdio(), |title, window| {
        super::run_window(title, browser::WINDOW, window, hooks)
    })
}

/// The rest of the input: whether it ended, and the window's context, woken when it does.
#[derive(Clone, Default)]
pub(crate) struct Input {
    ended: Arc<AtomicBool>,
    wake: Arc<OnceLock<egui::Context>>,
}

impl Input {
    /// Reads `from` to its end — or an error reading it: either way the other side is gone — on
    /// a thread of its own, then [`Input::end`].
    fn watch(&self, from: impl Read + Send + 'static) -> std::io::Result<()> {
        let input = self.clone();
        std::thread::Builder::new()
            .name("sopen-state-input".into())
            .spawn(move || {
                let mut from = from;
                let _ = std::io::copy(&mut from, &mut std::io::sink());
                input.end();
            })
            .map(drop)
    }

    /// The input ended: the window hears it on its next pass, which this asks for.
    pub(crate) fn end(&self) {
        self.ended.store(true, Ordering::SeqCst);
        if let Some(ctx) = self.wake.get() {
            ctx.request_repaint();
        }
    }

    fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }
}

/// Where the cues go — this process's standard output — and how the session is let go once
/// the input has ended: nothing is said after that.
pub(crate) struct Cues {
    out: Box<dyn Write>,
    let_go: Box<dyn FnMut()>,
    gone: bool,
}

impl Cues {
    fn stdio() -> Cues {
        Cues::new(Box::new(std::io::stdout()), Box::new(let_go_of_the_session))
    }

    pub(crate) fn new(out: Box<dyn Write>, let_go: Box<dyn FnMut()>) -> Cues {
        Cues {
            out,
            let_go,
            gone: false,
        }
    }

    /// `line`, unless the session was let go.
    fn say(&mut self, line: &str) {
        if !self.gone {
            crate::url::write_cue(&mut *self.out, line);
        }
    }

    /// Nothing more is read from the session or written to it.
    fn let_go(&mut self) {
        if !std::mem::replace(&mut self.gone, true) {
            let _ = self.out.flush();
            (self.let_go)();
        }
    }
}

/// The session let go: each relay asked to pass on all it was given, then to stop writing to the
/// session and close its stream there ([`Ask::LetGo`]) — waited for at most [`RELAY_WAIT`] — and
/// the input put on `/dev/null`. Without relays, fds 1 and 2 on `/dev/null` too.
fn let_go_of_the_session() {
    let _ = std::io::stdout().flush();
    let mut relays = RELAYS.lock().unwrap_or_else(PoisonError::into_inner);
    match relays.as_mut() {
        Some(relays) => {
            relays.ask(Ask::LetGo, RELAY_WAIT);
            on_null([0]);
        }
        None => on_null([0, 1, 2]),
    }
}

/// `/dev/null` put on each of `fds`, this process's standard ones: what is written there goes
/// nowhere, what is read there has ended. They stay inheritable, as standard streams are.
fn on_null(fds: impl IntoIterator<Item = RawFd>) {
    let Ok(null) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
    else {
        return;
    };
    for fd in fds {
        // SAFETY: dup2 onto a descriptor this process owns; atomic, so a write in flight on it
        // ends on what it was.
        unsafe {
            libc::dup2(null.as_raw_fd(), fd);
        }
    }
}

/// The relays of this process's standard output and error while they run ([`relay_stdio`]).
static RELAYS: Mutex<Option<Relays>> = Mutex::new(None);

/// The way to each relay, and their word once they have ended.
struct Relays {
    lines: Vec<Line>,
    /// A word from each relay once its pipe has ended and all it read is passed on.
    ended: mpsc::Receiver<()>,
}

impl Relays {
    /// `ask` of every relay, each answered once done — or by its end — waited for at most `wait`
    /// in all.
    fn ask(&mut self, ask: Ask, wait: Duration) {
        let answers: Vec<_> = self.lines.iter_mut().map(|line| line.ask(ask)).collect();
        let deadline = Instant::now() + wait;
        for answer in answers {
            let left = deadline.saturating_duration_since(Instant::now());
            if let Err(mpsc::RecvTimeoutError::Timeout) = answer.recv_timeout(left) {
                return;
            }
        }
    }
}

/// How long letting the session go waits for the relays (on the window's thread, once), and
/// [`end_of_run`] for them to end: a child that inherited fd 1 or 2 and still runs holds its pipe
/// open past sopen's own end.
const RELAY_WAIT: Duration = Duration::from_secs(2);

/// How long [`end_of_run`] waits for the relays to pass on what sopen wrote — the state's cue
/// among it — to a session still read but slowly (a slow link). A peer that keeps the session open
/// and stops reading holds sopen's end this long (as it held its writes before the relays).
const CUE_WAIT: Duration = Duration::from_secs(30);

/// What a relay is asked: first, always, to pass on all that is in its pipe now (children may
/// still hold its write end: no waiting for its end), then nothing more ([`Ask::Flush`]) or to
/// stop writing to the session and close its stream there ([`Ask::LetGo`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    Flush,
    LetGo,
}

/// One request and where its answer goes: a relay that has ended drops it unanswered, which is
/// an answer too (all it was given is passed on, or it never will be).
struct Request {
    ask: Ask,
    answer: mpsc::Sender<()>,
}

/// The way to one relay: its requests, and the pipe whose byte wakes it from its `poll`.
struct Line {
    requests: mpsc::Sender<Request>,
    wake: std::io::PipeWriter,
}

impl Line {
    /// `ask` sent; its answer's receiver (one that has ended is answered at once).
    fn ask(&mut self, ask: Ask) -> mpsc::Receiver<()> {
        let (answer, answered) = mpsc::channel();
        if self.requests.send(Request { ask, answer }).is_ok() {
            // An error: the relay has ended, and its requests went with it.
            let _ = self.wake.write_all(&[1]);
        }
        answered
    }
}

/// Fds 1 and 2 put on pipes, each relayed by a thread of its own ([`relay`]) to the session's
/// stream it replaces, which the relay keeps aside and alone writes to (see the module's header).
/// A child started from then on inherits the pipe as it inherited the session. Once per process;
/// a second call does nothing.
pub(crate) fn relay_stdio() -> std::io::Result<()> {
    let mut relays = RELAYS.lock().unwrap_or_else(PoisonError::into_inner);
    if relays.is_some() {
        return Ok(());
    }
    let _ = std::io::stdout().flush();
    let (done, ended) = mpsc::channel();
    let mut lines = Vec::new();
    let mut ends = Vec::new();
    for fd in [1, 2] {
        let (from, into) = std::io::pipe()?;
        // SAFETY: a copy of this process's own standard stream, close-on-exec: no child holds
        // the session through it. The relay owns it from here.
        let kept = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if kept < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `kept` is this process's, open, and owned by nothing else.
        let session = std::fs::File::from(unsafe { std::os::fd::OwnedFd::from_raw_fd(kept) });
        lines.push(start_relay(fd, from, session, done.clone())?);
        ends.push((into, fd));
    }
    // Only once both relays run does anything write into a pipe.
    for (into, fd) in ends {
        // SAFETY: dup2 of a pipe's write end, this process's own, onto its own fd 1 or 2.
        unsafe {
            libc::dup2(into.as_raw_fd(), fd);
        }
    }
    *relays = Some(Relays { lines, ended });
    Ok(())
}

/// A relay of `from` to `to` on a thread of its own (named for fd `fd`), which says `done` when
/// it has ended; the way to ask it.
fn start_relay<W: Write + Send + 'static>(
    fd: RawFd,
    from: std::io::PipeReader,
    to: W,
    done: mpsc::Sender<()>,
) -> std::io::Result<Line> {
    let (requests, asked) = mpsc::channel();
    let (woken, wake) = std::io::pipe()?;
    std::thread::Builder::new()
        .name(format!("sopen-relay-{fd}"))
        .spawn(move || {
            relay(from, to, asked, woken);
            let _ = done.send(());
        })?;
    Ok(Line { requests, wake })
}

/// Copies `from` to `to` until `from` ends, and answers what it is asked on `asked` (each request
/// with a byte on `woken`): it passes on all that is in `from` now, then answers — after
/// [`Ask::LetGo`] it writes nothing more and closes `to`. Once a write to `to` fails — the session
/// gone — it only drains `from`, so whatever writes into it never sees an error. A slow peer holds
/// it in its write; a request is answered after that.
fn relay(
    from: std::io::PipeReader,
    to: impl Write,
    asked: mpsc::Receiver<Request>,
    woken: std::io::PipeReader,
) {
    let mut to = Some(to);
    let mut from = from;
    let mut woken = Some(woken);
    let mut chunk = [0u8; 8192];
    loop {
        let mut fds = [
            libc::pollfd {
                fd: from.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                // A negative fd is not looked at: the way to ask is gone.
                fd: woken.as_ref().map_or(-1, |w| w.as_raw_fd()),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: two pollfds of this process's own descriptors.
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if fds[1].revents != 0 {
            let mut bytes = [0u8; 64];
            if let Some(w) = woken.as_mut() {
                if matches!(w.read(&mut bytes), Ok(0) | Err(_)) {
                    woken = None;
                }
            }
            while let Ok(request) = asked.try_recv() {
                let ended = pass_on_what_is_there(&mut from, &mut to, &mut chunk);
                if request.ask == Ask::LetGo {
                    to = None;
                }
                let _ = request.answer.send(());
                if ended {
                    return;
                }
            }
        }
        if fds[0].revents != 0 {
            match from.read(&mut chunk) {
                Ok(0) => return,
                Ok(n) => pass_on(&mut to, &chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }
}

/// Reads `from` while it has something now — no wait between reads — and passes it on; whether
/// `from` has ended.
fn pass_on_what_is_there(
    from: &mut std::io::PipeReader,
    to: &mut Option<impl Write>,
    chunk: &mut [u8],
) -> bool {
    loop {
        let mut fd = libc::pollfd {
            fd: from.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd of this process's own descriptor, no wait.
        match unsafe { libc::poll(&mut fd, 1, 0) } {
            0 => return false,
            n if n < 0 => {
                if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    return false;
                }
            }
            _ => match from.read(chunk) {
                Ok(0) => return true,
                Ok(n) => pass_on(to, &chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return true,
            },
        }
    }
}

/// `bytes` written to the session, unless it is gone: a failed write — the session's reader gone
/// — closes it here, and nothing is written to it again.
fn pass_on(to: &mut Option<impl Write>, bytes: &[u8]) {
    #[cfg(test)]
    test_delay();
    if let Some(out) = to.as_mut() {
        if out.write_all(bytes).is_err() {
            *to = None;
        }
    }
}

/// Tests: each relay waits this long before a write it passes on (`SOPEN_TEST_RELAY_DELAY_MS`),
/// as a slow peer holds it — read only in a test build.
#[cfg(test)]
fn test_delay() {
    let ms = std::env::var("SOPEN_TEST_RELAY_DELAY_MS").ok();
    if let Some(ms) = ms.and_then(|ms| ms.parse().ok()) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// [`end_of_run`] when it goes: `main` holds one from its first line, so the relays finish
/// however `main` ends — returned, or unwound by a panic whose message is in a pipe.
pub struct EndOfRun;

impl Drop for EndOfRun {
    fn drop(&mut self) {
        end_of_run();
    }
}

/// The end of a `--state-stdin` run, `main`'s last step ([`EndOfRun`]; `gui::give_up` before its
/// exit too): each relay asked to pass on all sopen wrote — the cue among it — and waited for at
/// most [`CUE_WAIT`]; then fds 1 and 2 put on `/dev/null`, so the pipes' write ends close, and the
/// relays given at most [`RELAY_WAIT`] to end. Nothing for any other run.
pub fn end_of_run() {
    let Some(mut relays) = RELAYS.lock().unwrap_or_else(PoisonError::into_inner).take() else {
        return;
    };
    let _ = std::io::stdout().flush();
    relays.ask(Ask::Flush, CUE_WAIT);
    on_null([1, 2]);
    let deadline = Instant::now() + RELAY_WAIT;
    for _ in &relays.lines {
        let left = deadline.saturating_duration_since(Instant::now());
        if relays.ended.recv_timeout(left).is_err() {
            return;
        }
    }
}

/// Where the window is in its session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    /// The other side waits for the state.
    Open,
    /// A return went through: the state is handed back.
    Returned,
    /// The input ended with nothing to lose: the window closes.
    Ending,
    /// The input ended and the window stays ([`KEPT_CUE`]), until it is closed here.
    Kept,
}

/// The browser in HOST's switched window (10 §7.2): `[TUI]` on its title row; F12, `[TUI]` and
/// every close return through the browser's own close ([`Browser::close_requested`]: a viewer's
/// prompt first), the state taken just before it — a viewer it closes is in it; after the
/// viewer's prompt, the state taken when it came up, so its Save or Discard returns with the
/// viewer too. The input's end is heard in the shell's `logic` — drawn or hidden — and at the top
/// of a pass.
pub(crate) struct Switched {
    browser: Browser,
    /// The window's context, from its first pass: the state's dialog caret is its edit state.
    ctx: Option<egui::Context>,
    input: Input,
    cues: Rc<RefCell<Cues>>,
    /// A hook drives the run: the shell never asks it about a close, so the state is taken at
    /// the end of every pass and the input's end is left to the hook.
    scripted: bool,
    session: Session,
    /// The state the window hands back.
    handed: Rc<RefCell<Option<SwitchState>>>,
    /// The state taken at the close that brought up the viewer's prompt, its viewer in it: handed
    /// back on the close that the prompt's answer (Save, Discard) brings, once the browser has
    /// dropped the viewer; its Cancel drops it (X2-switch).
    asked: Option<SwitchState>,
}

impl Switched {
    fn ctx(&self) -> egui::Context {
        self.ctx.clone().unwrap_or_default()
    }

    /// The window's context, kept the first time it is seen — before the input's end is looked
    /// at: an end after this wakes the window.
    fn bind(&mut self, ctx: &egui::Context) {
        if self.ctx.is_none() {
            let _ = self.input.wake.set(ctx.clone());
            self.ctx = Some(ctx.clone());
        }
    }

    /// The input's end, once, while the other side still waits for the state: with nothing to
    /// lose the window asks to close; else it is kept. A hook's run is left to its hook.
    fn heed_the_input(&mut self, ctx: &egui::Context) {
        if self.scripted || self.session != Session::Open || !self.input.ended() {
            return;
        }
        if self.browser.would_lose() {
            self.keep();
        } else {
            self.session = Session::Ending;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// The input ended while the window holds something it would lose: [`KEPT_CUE`], the
    /// session let go, the window kept.
    fn keep(&mut self) {
        let mut cues = self.cues.borrow_mut();
        cues.say(KEPT_CUE);
        cues.let_go();
        self.session = Session::Kept;
    }
}

impl Window for Switched {
    fn logic(&mut self, ctx: &egui::Context) {
        self.bind(ctx);
        self.heed_the_input(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.bind(&ctx);
        // Before the browser's pass, should the shell's `logic` not have run first.
        self.heed_the_input(&ctx);
        // The viewer's prompt was answered by staying (Cancel, a Save that did not land): the
        // state taken when it came up is not this window's any more.
        if self.asked.is_some() && self.browser.viewer_stays() {
            self.asked = None;
        }
        // The session's button; once it has ended, none, as a window started directly.
        let open = self.session == Session::Open;
        if open {
            super::chrome::set_mode_button(&ctx, "[TUI]");
        } else {
            super::chrome::clear_mode_button(&ctx);
        }
        self.browser.ui(ui);
        // Both looked at, so a played F12 never stays in the input.
        let played = played_switch(&ctx);
        if (super::switch_asked(&ctx) || played) && open {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.scripted {
            *self.handed.borrow_mut() = Some(self.browser.take_state(&ctx));
        }
    }

    fn ready(&self) -> bool {
        self.browser.ready()
    }

    fn close_requested(&mut self) -> bool {
        match self.session {
            Session::Open => {
                let state = self.browser.take_state(&self.ctx());
                if !self.browser.close_requested() {
                    // The viewer's prompt is up: its answer's close comes with the viewer gone.
                    self.asked = Some(state);
                    return false;
                }
                *self.handed.borrow_mut() = Some(self.asked.take().unwrap_or(state));
                self.session = Session::Returned;
                true
            }
            Session::Ending => {
                if self.browser.close_requested() {
                    self.cues.borrow_mut().let_go();
                    true
                } else {
                    // Its prompt is up after all: something to lose.
                    self.keep();
                    false
                }
            }
            Session::Returned | Session::Kept => self.browser.close_requested(),
        }
    }

    fn keys(&self) -> &'static [super::chrome::FKey] {
        self.browser.keys()
    }
}

/// A played F12 in this pass's input (`SOPEN_KEYS`: the browser plays its keys after the shell's
/// input hook, which takes every F12 the keyboard sends, [`super::switch_asked`]) — the switch,
/// as the owner's own is; taken out of the input.
fn played_switch(ctx: &egui::Context) -> bool {
    ctx.input_mut(|i| {
        let mut asked = false;
        i.events.retain(|e| match e {
            egui::Event::Key {
                key: egui::Key::F12,
                pressed,
                repeat,
                modifiers,
                ..
            } => {
                asked |= *pressed && !*repeat && modifiers.is_none();
                false
            }
            _ => true,
        });
        asked
    })
}

/// [`run`] with the input watched by `input`, the cues said on `cues` and the window shown by
/// `show` (its title, the window): the state given to the browser before it is shown, and the
/// cue said after [`browser::open_on`] has waited for what still writes.
pub(crate) fn run_on(
    start: Start,
    hooks: &Hooks,
    sent: Sent,
    input: Input,
    cues: Cues,
    show: impl FnOnce(&str, Switched) -> Result<()>,
) -> Result<Outcome> {
    let cues = Rc::new(RefCell::new(cues));
    let handed = Rc::new(RefCell::new(None));
    let scripted = hooks.scripted();
    let watched = input.clone();
    let outcome = browser::open_on(start, hooks, |title, mut browser| {
        browser.give_state(sent.0).map_err(NotGiven)?;
        show(
            title,
            Switched {
                browser,
                ctx: None,
                input,
                cues: cues.clone(),
                scripted,
                session: Session::Open,
                handed: handed.clone(),
                asked: None,
            },
        )
    })?;
    match handed.take() {
        Some(state) => cues
            .borrow_mut()
            .say(&format!("{STATE_CUE} {}", state.line())),
        // Closed for the input's end — while hidden eframe closes it unasked: let go here.
        None if !scripted && watched.ended() => cues.borrow_mut().let_go(),
        None => {}
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests;
