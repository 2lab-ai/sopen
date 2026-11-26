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
//! without a pty sends no SIGHUP, 10 §7.3). Nothing reads this side's output then: the session
//! is let go — standard input, output and error on `/dev/null`, so nothing more is read from it
//! or written to it (a closed pipe fails every write, and `eprintln!` panics on that). With
//! nothing to lose the window closes, no cue; with unsaved edits or a job that writes
//! ([`Browser::would_lose`]) it stays, after [`KEPT_CUE`], until it is closed here, by its own
//! close prompt. A run a hook drives (`SOPEN_CELLS`, `SOPEN_SCREENSHOT`) ends by its hook.
//!
//! The cues are fixed lines, as [`crate::url::TUNNEL_OPEN`] is, written ignoring write errors
//! ([`crate::url::write_cue`]).

use super::browser::switch_state::{self, SwitchState};
use super::browser::{self, Browser, Start};
use super::{Hooks, Outcome, Window};
use anyhow::Result;
use eframe::egui;
use std::cell::RefCell;
use std::io::{BufRead, Read, Write};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

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
/// its input watched to its end; prints the cue once it has handed the browser back and what
/// still wrote has ended. A state `start` cannot take is [`NotGiven`], before any window.
pub fn run(start: Start, hooks: &Hooks, sent: Sent) -> Result<Outcome> {
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

/// Standard input, output and error on `/dev/null`: what anything in this process — a debug
/// line, a report printed after the window, sopen's own exit — or a child started later writes
/// goes nowhere, never to a pipe whose other end is gone.
fn let_go_of_the_session() {
    use std::os::fd::AsRawFd;
    let _ = std::io::stdout().flush();
    let Ok(null) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
    else {
        return;
    };
    for fd in 0..=2 {
        // SAFETY: dup2 of a descriptor this process owns onto its own standard ones.
        unsafe {
            libc::dup2(null.as_raw_fd(), fd);
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
/// prompt first), the state taken just before it — a viewer it closes is in it. The input's end
/// is heard on the next pass.
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
}

impl Switched {
    fn ctx(&self) -> egui::Context {
        self.ctx.clone().unwrap_or_default()
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
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.ctx.is_none() {
            // Before the input's end is looked at: an end after this wakes the window.
            let _ = self.input.wake.set(ctx.clone());
            self.ctx = Some(ctx.clone());
        }
        super::chrome::set_mode_button(&ctx, "[TUI]");
        self.browser.ui(ui);
        // Both looked at, so a played F12 never stays in the input.
        let played = played_switch(&ctx);
        if super::switch_asked(&ctx) || played {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.scripted {
            *self.handed.borrow_mut() = Some(self.browser.take_state(&ctx));
            return;
        }
        if self.session == Session::Open && self.input.ended() {
            if self.browser.would_lose() {
                self.keep();
            } else {
                self.session = Session::Ending;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
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
                    return false;
                }
                *self.handed.borrow_mut() = Some(state);
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
            },
        )
    })?;
    if let Some(state) = handed.take() {
        cues.borrow_mut()
            .say(&format!("{STATE_CUE} {}", state.line()));
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests;
