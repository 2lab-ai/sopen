//! The switch to a window on another machine (10 §7.1's HOST, §7.3, §7.4; T28, T29; X2-switch):
//! F12 in the terminal with a HOST destination runs HOST's half (`sopen --here --gui
//! --state-stdin -- FOLDER`, X2-host) over 01 §4a's forward ([`forward::Switch`]) and comes back
//! with the state its window hands back.
//!
//! - **Before it goes** (§7.4): the browser asks first ([`Browser::before_switch`]) — a job that
//!   writes makes it wait (`waiting for <it>…`), unsaved edits in the viewer ask `Save the
//!   changes to host:/abs before switching to HOST?`, the git panel's open dialog or the
//!   planner's plan (which the state does not carry yet) asks whether to switch without it — and
//!   asks for the switch again once what held it is over ([`Browser::switch_due`], heard by the
//!   loop), which decides anew: only the question's answer lets what it asked about go, never the
//!   end of a job waited for (a viewer closed while its save or export still writes among the
//!   jobs). Save and Save As… press the viewer's own F5 and F7; with Discard HOST opens the file
//!   as stored, and the viewer's edits stay through the run — dropped only once HOST's state has
//!   come back and been given ([`Browser::discard_viewer`], `Window::discard`: the viewer's own
//!   Discard); without it they are still here, and the line says so (X2-switch's review).
//! - **The state out**: taken from the browser ([`Browser::take_state`]), its paths rewritten as
//!   §4a rewrites SOURCE ([`Ways::out`]: this machine's `/abs` is `<this machine>:/abs`, HOST's
//!   own `HOST:/abs` its `/abs`), one line on ssh's input, which then stays open. FOLDER is the
//!   rewritten state's active folder, so the two never disagree.
//! - **Meanwhile** (T28): raw mode and the alternate screen stay, nothing is left; the screen is
//!   printed whole once more, dimmed — every cell's glyph in the skin's shadow colours (dark grey
//!   on black, 10 §6's shadow) — under a box with [`away_line`], HOST's last standard-error line
//!   on the status row. No protocol picture is drawn in the terminal before X7, so none is left
//!   to delete: the whole reprint is what clears a cell-drawn one. Only Ctrl+C is read; every
//!   other key goes unread, and a resize lays the browser out again under the dimming.
//! - **Ctrl+C** closes ssh's input: HOST's input ends, and its window closes, or stays with
//!   something to lose and says so ([`forward::SwitchSaid::Kept`]) — then HOST has let the
//!   session go, but its script's shell holds it open while that window lives, so this run's ssh
//!   client is ended at once (its pid alone) and the terminal comes back with the kept line.
//!   While HOST answers, the box says so ([`ending_line`]); **a second Ctrl+C ends this run's ssh client** — its own pid,
//!   never `ssh -O exit` on the master other sessions share — and otherwise ssh's own keepalives
//!   (`ServerAliveInterval=10`, `ServerAliveCountMax=3`: about 30 s) end a dead link
//!   (X2-switch's decision, 2026-11-27; reversible). A signal that ends the terminal's run ends
//!   ssh's client the same way, and the run ends.
//! - **Coming back** ([`outcome`]): each of §7.3's ends is its line on the status row and the
//!   terminal shows the browser again. A cue's state is used whatever the code: read
//!   ([`switch_state::read`]: a newer `v` is §7.4's line), its paths rewritten back
//!   ([`Ways::back`]), and given to a fresh browser — `give_state` takes only one not drawn yet —
//!   made from the same place as the first ([`Remote::fresh`], a new connection) without
//!   `SOPEN_STATE`, which swaps in for the old one; one it refuses keeps the browser as it was,
//!   its reason on the status row. Without a cue the browser is the one from before the switch,
//!   and it lists again ([`Browser::switch_back`]: both panels re-read, each bar where it was).
//!   What the old one still holds is settled at once (a viewer's claim on its file), and every
//!   browser the switch made is finished once the run is over ([`Remote::made`]), as
//!   `browser::open_on` finishes the first.

use super::{ctrl_c, say, Backend, Screen, Shared, Tty};
use crate::forward::{self, Switch, SwitchSaid};
use crate::gui::browser::switch_state::{self, SwitchState};
use crate::gui::browser::{self, Before, Browser};
use crate::gui::cells::{self, Cells, Pen};
use crate::gui::chrome::{self, cga};
use anyhow::Result;
use crossterm::event::Event as CEvent;
use std::cell::RefCell;
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitStatus;
use std::rc::Rc;
use std::time::Duration;

/// The longest a look at the terminal waits before HOST's half is looked at again.
const WAIT: Duration = Duration::from_millis(50);

/// The box's line while the window is on HOST (10 §7.3).
fn away_line(host: &str) -> String {
    format!("xtools is in a window on {host} — close it or press F12 there; Ctrl+C ends it here")
}

/// The box's line once Ctrl+C has ended HOST's input, while HOST answers.
fn ending_line(host: &str) -> String {
    format!(
        "{host}'s input is closed: its window closes, or stays if it has unsaved edits — Ctrl+C \
         again ends ssh here"
    )
}

/// ssh's connection options for a host.
type Connection = Box<dyn Fn(&str) -> Result<Vec<OsString>>>;

/// What the switch to HOST needs besides the browser: how its half is run (the ssh program, its
/// connection, the hooks, the install paths, this machine's name), how a browser not drawn yet
/// is made on the terminal's place, and the browsers it made.
pub(crate) struct Remote {
    pub ssh: OsString,
    /// ssh's connection to HOST, the forward's master ([`crate::remote::connection_args`]).
    pub connection: Connection,
    /// The hooks HOST's half gets, given the terminal's grid in points
    /// ([`forward::switch_env`]).
    pub env: Box<dyn Fn([f32; 2]) -> Result<Vec<String>>>,
    pub install_paths: String,
    /// This machine's host name ([`forward::hostname`]).
    pub hostname: Option<String>,
    /// A start on the terminal's place: its folder over a new connection.
    pub fresh: Box<dyn Fn() -> Result<browser::Start>>,
    /// The leftovers of the browsers the switch made, for the run's end.
    pub made: RefCell<Vec<browser::Leftovers>>,
}

impl Remote {
    /// The switch as a run makes it: `ssh` on PATH over the master in `state_dir`, the hooks of
    /// this process, the install paths, this machine's name; `fresh` makes a browser's start.
    pub(crate) fn real(
        state_dir: std::path::PathBuf,
        fresh: Box<dyn Fn() -> Result<browser::Start>>,
    ) -> Remote {
        Remote {
            ssh: "ssh".into(),
            connection: Box::new(move |host| crate::remote::connection_args(host, &state_dir)),
            env: Box::new(|grid| forward::switch_env(|key| std::env::var_os(key), Some(grid))),
            install_paths: forward::INSTALL_PATHS.to_string(),
            hostname: forward::hostname(),
            fresh,
            made: RefCell::default(),
        }
    }
}

/// `host:/abs`, or the local path.
fn show(label: Option<&str>, abs: &str) -> String {
    match label {
        Some(label) => format!("{label}:{abs}"),
        None => abs.to_string(),
    }
}

/// The host a place is on, as the browser labels it (`None`: the machine it is read on).
fn label_of(place: &str) -> Result<Option<String>> {
    Ok(
        match crate::target::parse(place).map_err(anyhow::Error::msg)? {
            crate::target::Target::Remote(spec) => Some(spec.label()),
            crate::target::Target::Local(_) => None,
        },
    )
}

/// The maps of the state's paths out to HOST and back (10 §7.3): out as §4a rewrites SOURCE
/// ([`forward::source_for`]); back by the one pair of labels the folders went by — the terminal
/// browser's (`here`) and the one HOST's browser has for them (`there`) — so the state comes
/// back with the label `give_state` compares with, whatever user or spelling the target has; a
/// place on any other machine (a Copy / Move target) back by [`forward::source_back`].
struct Ways {
    host: String,
    hostname: Option<String>,
    here: Option<String>,
    there: Option<String>,
}

impl Ways {
    fn new(host: &str, here: Option<&str>, hostname: Option<&str>) -> Result<Ways> {
        let mut ways = Ways {
            host: host.to_string(),
            hostname: hostname.map(str::to_string),
            here: here.map(str::to_string),
            there: None,
        };
        ways.there = label_of(&ways.out(&show(here, "/"))?)?;
        Ok(ways)
    }

    fn out(&self, place: &str) -> Result<String> {
        let hostname = self.hostname.as_deref();
        forward::source_for(&self.host, place, hostname, |abs| Ok(abs.to_string()))
    }

    fn back(&self, place: &str) -> Result<String> {
        if label_of(place)? == self.there {
            let abs = match crate::target::parse(place).map_err(anyhow::Error::msg)? {
                crate::target::Target::Remote(spec) => spec.path,
                crate::target::Target::Local(path) => path.to_string_lossy().into_owned(),
            };
            return Ok(show(self.here.as_deref(), &abs));
        }
        forward::source_back(&self.host, place, self.hostname.as_deref())
    }
}

/// The switch from the terminal to a window on `host` (see the module's header), the browser
/// `shared`'s: the line for the status row once the terminal shows the browser again, or none.
/// A failure of the switch is its line — only the terminal's own (a hang-up) is an error.
pub(crate) fn to_host<T: Tty, O: Write>(
    b: &mut Backend<T, O>,
    shared: &Rc<RefCell<Browser>>,
    host: &str,
    remote: &Remote,
) -> io::Result<Option<String>> {
    if shared.borrow_mut().before_switch(host) == Before::Held {
        return Ok(None);
    }
    let state = shared.borrow().take_state(&b.ctx);
    // Discard's answer: the viewer keeps its edits through the run, dropped only once HOST's
    // state has come back and been given (`give_back`).
    let discard = shared.borrow_mut().take_discard();
    let discarded_file = (state.viewer.as_ref()).map(|v| show(state.host.as_deref(), &v.file));
    let cannot = |e: &dyn std::fmt::Display| format!("cannot switch to {host}: {e}");
    let started = Ways::new(host, state.host.as_deref(), remote.hostname.as_deref())
        .map_err(|e| cannot(&format!("{e:#}")))
        .and_then(|ways| {
            let sent = state
                .rewritten(|place| ways.out(place))
                .map_err(|e| cannot(&e))?;
            let folder = show(sent.host.as_deref(), &sent.panels[sent.active].folder);
            let run = (remote.connection)(host)
                .and_then(|connection| {
                    let env = (remote.env)(b.points())?;
                    Switch {
                        ssh: &remote.ssh,
                        connection,
                        host,
                        folder: &folder,
                        env: &env,
                        install_paths: &remote.install_paths,
                    }
                    .start(&sent.line())
                })
                .map_err(|e| cannot(&format!("{e:#}")))?;
            Ok((ways, run))
        });
    let (ways, mut run) = match started {
        Ok(started) => started,
        Err(line) => {
            return Ok(Some(
                kept_here(Some(line), discard, &discarded_file).unwrap_or_default(),
            ))
        }
    };
    let mut away = Away {
        host: host.to_string(),
        err: None,
        ending: false,
    };
    let mut cue = None;
    let mut kept = false;
    // The browser as it goes — the question answered — under the dimming: one pass, no input.
    b.pass(&mut Shared(shared.clone()), vec![])?;
    b.term.forget();
    draw_away(b, &away)?;
    let status = loop {
        let mut redraw = take_said(&run.said(), &mut away, &mut cue, &mut kept);
        // HOST kept its window (unsaved edits) and let the session go; its script's shell still
        // holds the session open while that window lives, so ssh would never end by itself:
        // this run's client ends (its pid alone), and is reaped below.
        if kept && cue.is_none() {
            run.kill();
        }
        match run.ended() {
            Ok(Some(status)) => {
                take_said(&run.said(), &mut away, &mut cue, &mut kept);
                break Ok(status);
            }
            Ok(None) => {}
            Err(e) => break Err(e),
        }
        if b.tty.stopped().is_some() {
            // The terminal's run ends: so does this run's ssh client (`SwitchRun`'s drop).
            return Ok(None);
        }
        match b.tty.next(WAIT)? {
            Some(ev) if ctrl_c(&ev) => {
                if std::mem::replace(&mut away.ending, true) {
                    run.kill();
                } else {
                    run.end_input();
                }
                redraw = true;
            }
            Some(CEvent::Resize(cols, rows)) if cols > 0 && rows > 0 => {
                b.size = (cols, rows);
                b.term.forget();
                b.pass(&mut Shared(shared.clone()), vec![])?;
                redraw = true;
            }
            // Nothing else is read (T28).
            _ => {}
        }
        if redraw {
            draw_away(b, &away)?;
        }
    };
    drop(run);
    let mut said = match &status {
        Ok(status) => outcome(host, *status, cue.is_some(), kept, away.ending, &away.err),
        Err(e) => Some(format!("ssh to {host} could not be waited for: {e}")),
    };
    let given = cue.map(|json| give_back(shared, remote, &ways, host, &json, discard));
    if let Some(Err(why)) = &given {
        said = Some(match said {
            Some(line) => format!("{line} · {why}"),
            None => why.clone(),
        });
    }
    // The state from before: the terminal lists again, as a browser given HOST's state does —
    // and Discard's edits are still the viewer's.
    if !matches!(given, Some(Ok(()))) {
        shared.borrow_mut().switch_back(&b.ctx);
        said = kept_here(said, discard, &discarded_file);
    }
    // The terminal shows the browser again: printed whole, its unread input dropped (T28).
    b.term.forget();
    b.tty.drop_input();
    Ok(said)
}

/// `said`, and — when Discard was the answer but HOST's state did not come back — that the
/// edits are still here.
fn kept_here(said: Option<String>, discard: bool, file: &Option<String>) -> Option<String> {
    let Some(file) = file.as_ref().filter(|_| discard) else {
        return said;
    };
    let kept = format!("the edits to {file} are kept here");
    Some(match said {
        Some(line) => format!("{line} · {kept}"),
        None => kept,
    })
}

/// What the terminal shows while the window is on HOST.
struct Away {
    host: String,
    /// HOST's last standard-error line (ssh's own among them).
    err: Option<String>,
    /// Ctrl+C has closed HOST's input.
    ending: bool,
}

/// What HOST said, taken: its state's JSON, its kept cue, its last standard-error line —
/// whether the screen changes.
fn take_said(
    said: &[SwitchSaid],
    away: &mut Away,
    cue: &mut Option<String>,
    kept: &mut bool,
) -> bool {
    let mut changed = false;
    for s in said {
        match s {
            SwitchSaid::State(json) => *cue = Some(json.clone()),
            SwitchSaid::Kept => *kept = true,
            SwitchSaid::Err(line) => {
                away.err = Some(line.clone());
                changed = true;
            }
        }
    }
    changed
}

/// The last pass's screen dimmed (see the module's header), the box with the line over it,
/// HOST's last line on the status row; no caret.
fn away_screen<T: Tty, O: Write>(b: &Backend<T, O>, line: &str, err: Option<&str>) -> Screen {
    let mut screen = b.screen();
    screen.caret = None;
    let buf = &mut screen.buf;
    let a = buf.area;
    for y in a.top()..a.bottom() {
        for x in a.left()..a.right() {
            let cell = &mut buf[(x, y)];
            cell.fg = super::rgb(cga::DARK_GRAY);
            cell.bg = super::rgb(cga::BLACK);
        }
    }
    let room = usize::from(a.width.saturating_sub(8)).max(1);
    let rows = crate::gui::browser::ops::dialog::wrap(line, room);
    let w = rows.iter().map(|r| cells::width(r)).max().unwrap_or(0) as u16 + 4;
    let h = rows.len() as u16 + 2;
    let x = a.width.saturating_sub(w) / 2;
    let y = a.height.saturating_sub(h) / 2;
    let inner = cells::frame(
        buf,
        Cells::new(x, y, w.min(a.width), h.min(a.height)),
        "",
        false,
    );
    let p = chrome::PALETTE;
    for (k, row) in rows.iter().enumerate() {
        let at = inner.y + k as u16;
        if at < inner.bottom() {
            let width = usize::from(inner.width.saturating_sub(2));
            cells::put(buf, inner.x + 1, at, row, width, Pen::new(p.text, p.back));
        }
    }
    if let Some(err) = err {
        say(buf, err);
    }
    screen
}

/// Prints the screen of [`away_screen`] for `away`.
fn draw_away<T: Tty, O: Write>(b: &mut Backend<T, O>, away: &Away) -> io::Result<()> {
    let line = match away.ending {
        false => away_line(&away.host),
        true => ending_line(&away.host),
    };
    let screen = away_screen(b, &line, away.err.as_deref());
    b.term.draw(&screen)
}

/// The line for HOST's end (10 §7.3's table): ssh's `status`, whether a state's cue came (then a
/// code 0 says nothing), the kept cue, Ctrl+C, HOST's last standard-error line. After Ctrl+C with
/// neither cue, ssh's own failure or its end by a signal is the link lost (its window, if it has
/// unsaved edits, stays).
fn outcome(
    host: &str,
    status: ExitStatus,
    cue: bool,
    kept: bool,
    ending: bool,
    err: &Option<String>,
) -> Option<String> {
    let line = |text: String| match err {
        Some(err) => format!("{text}: {err}"),
        None => text,
    };
    if kept && !cue {
        return Some(format!(
            "the window on {host} stays open: it has unsaved edits"
        ));
    }
    let code = status.code();
    if ending && !cue && matches!(code, Some(255) | None) {
        return Some(format!(
            "lost the link to {host}; a window there with unsaved edits stays open"
        ));
    }
    Some(match code {
        Some(0) if cue => return None,
        Some(0) => format!("sopen on {host} ended without handing the screen back"),
        Some(1) => line(format!("sopen on {host} failed")),
        Some(2) => line(format!(
            "sopen on {host} is too old for the switch, or a usage error"
        )),
        Some(3) => line(format!("the window on {host} ended with a refused save")),
        Some(127) => {
            format!("sopen is not installed on {host} (brew install 2lab-ai/tap/sopen-preview)")
        }
        Some(255) => format!(
            "{} — run ssh {host} once in a shell to answer its questions",
            line(format!("could not reach {host}"))
        ),
        Some(n @ 129..=254) => format!("sopen on {host} ended by signal {}", n - 128),
        // Not one sopen exits with: said as it came.
        Some(n) => line(format!("sopen on {host} ended with status {n}")),
        None => format!("ssh to {host} ended without an exit code ({status})"),
    })
}

/// The state HOST handed back (`json`), as this machine names its paths, given to a fresh
/// browser on the terminal's place, which takes the old one's place in `shared`; why not.
fn give_back(
    shared: &Rc<RefCell<Browser>>,
    remote: &Remote,
    ways: &Ways,
    host: &str,
    json: &str,
    discard: bool,
) -> std::result::Result<(), String> {
    let state: SwitchState = switch_state::read(json, host)?;
    let state = state.rewritten(|place| ways.back(place))?;
    let start = (remote.fresh)().map_err(|e| format!("{e:#}"))?;
    let mut fresh = Browser::new(start);
    fresh.give_state(state)?;
    remote.made.borrow_mut().push(fresh.leftovers());
    let mut old = std::mem::replace(&mut *shared.borrow_mut(), fresh);
    // Discard: now that HOST's state is this terminal's, the old viewer's edits go with it.
    if discard {
        old.discard_viewer();
    }
    let left = old.leftovers();
    drop(old);
    left.settle();
    Ok(())
}

#[cfg(test)]
mod tests;
