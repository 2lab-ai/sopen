//! The file browser and its text viewer (spec §3a), the image window (spec §6), the binary-file
//! dialog and hex window (spec §6a) and the media window (spec §7). This module is the shared
//! shell: where the opened file lives ([`Origin`]),
//! the window runner (wgpu renderer, glow fallback, activation on start, Cmd/Ctrl+Q through the
//! close prompt), the receipt hooks ([`Hooks`]) and the screenshot driver. Everything that writes
//! (Save, Save copy, Export, Save to…) is a plain function over [`Origin`] or a local path,
//! called by the window's buttons and by the windowless hooks alike.

pub mod binary;
pub mod browser;
pub mod chrome;
pub mod fonts;
mod help;
pub mod hex;
pub mod image;
pub mod media;
pub mod playback;
mod select;
#[cfg(test)]
pub(crate) mod testkit;
pub mod text;

pub use select::{fit, Map, Selection};

use crate::remote::{RemoteFs, Stat};
use anyhow::{anyhow, bail, Context as _, Result};
use eframe::egui;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Opens a fresh connection to the file's host (the session can die while a window is open).
pub type Reconnect = Box<dyn FnMut() -> Result<Box<dyn RemoteFs + Send>> + Send>;

/// Where the opened file lives and how to write next to it.
pub struct Origin {
    fs: Box<dyn RemoteFs + Send>,
    reconnect: Option<Reconnect>,
    /// `[user@]host` for a remote file, `None` for a local one.
    label: Option<String>,
    /// Absolute path on that host.
    pub abs: String,
    /// The `(size, mtime)` the next overwrite is checked against: the fetch-time stat, then the
    /// stat of our own last successful save.
    pub at: Stat,
}

impl Origin {
    pub fn new(
        fs: Box<dyn RemoteFs + Send>,
        reconnect: Option<Reconnect>,
        label: Option<String>,
        abs: String,
        at: Stat,
    ) -> Self {
        Self {
            fs,
            reconnect,
            label,
            abs,
            at,
        }
    }

    /// `host:/abs` (or the local path) — the window title and how results are printed.
    pub fn display(&self) -> String {
        self.show(&self.abs)
    }

    /// Any path on the same host, printed the same way.
    pub fn show(&self, path: &str) -> String {
        match &self.label {
            Some(label) => format!("{label}:{path}"),
            None => path.to_string(),
        }
    }

    /// A file on this machine (no host).
    pub fn is_local(&self) -> bool {
        self.label.is_none()
    }

    /// Whether this server can replace a file in one step (`posix-rename@openssh.com`).
    pub fn can_replace(&self) -> bool {
        self.fs.can_replace()
    }

    /// Host label used in cache and backup paths.
    pub fn host(&self) -> &str {
        self.label.as_deref().unwrap_or(crate::cache::LOCAL_HOST)
    }

    /// `(dir, name)` of the opened file.
    pub fn parent(&self) -> (String, String) {
        let (dir, name) = crate::remote::split_parent(&self.abs);
        (dir.to_string(), name.to_string())
    }

    /// The file layer, reconnected first when the session died while the window was open.
    /// The liveness probe is a `stat` of the opened file; the caller's operation runs once.
    pub fn fs(&mut self) -> Result<&mut dyn RemoteFs> {
        if let Err(e) = self.fs.stat_opt(&self.abs) {
            let Some(reconnect) = self.reconnect.as_mut() else {
                return Err(e);
            };
            crate::debug!("session lost ({e:#}); reconnecting");
            self.fs = reconnect().context("the connection was lost and could not be reopened")?;
        }
        Ok(self.fs.as_mut())
    }
}

/// What happened in a window, for the usage log and the exit code.
#[derive(Debug, Default, Clone)]
pub struct Outcome {
    pub saved: bool,
    pub exported: bool,
    /// A write was refused at least once because the remote changed.
    pub conflict: bool,
    /// The last save attempt was refused and nothing was saved after it (exit 3).
    pub refused: Option<String>,
}

impl Outcome {
    pub fn action(&self) -> &'static str {
        if self.exported {
            "export"
        } else if self.saved || self.conflict {
            "save"
        } else {
            "open"
        }
    }
}

/// A local file next to `cached` that keeps the stem and the extension (so it still opens by
/// double-click): `<stem>.<tag>-<UTC stamp>-<unique>.<ext>`.
pub fn keep_path(cached: &Path, tag: &str, at: SystemTime, unique: &str) -> PathBuf {
    let stem = cached
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stamp = crate::cache::utc_stamp(at);
    let name = match cached.extension() {
        Some(ext) => format!("{stem}.{tag}-{stamp}-{unique}.{}", ext.to_string_lossy()),
        None => format!("{stem}.{tag}-{stamp}-{unique}"),
    };
    cached.with_file_name(name)
}

/// The name Save copy publishes next to `name`: `<stem>.edited<.ext>` (`shot.png` →
/// `shot.edited.png`, `README` → `README.edited`). The image and hex windows share it.
pub fn edited_name(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{}.edited{}", &name[..i], &name[i..]),
        _ => format!("{name}.edited"),
    }
}

/// Creates a new owner-only file at a [`keep_path`] with a random `unique` part (another one
/// when the name is taken) and returns it open.
pub fn create_keep(cached: &Path, tag: &str) -> Result<(PathBuf, std::fs::File)> {
    crate::cache::create_unique(|_| {
        let random = crate::remote::random_hex();
        keep_path(cached, tag, SystemTime::now(), &random[..8])
    })
}

// ---------------------------------------------------------------------------------------------
// Receipt hooks (AGENTS.md "Receipts"): environment only, not part of the README.

/// One edit the state asks for, applied in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Crop,
    Pixelize,
}

/// `SOPEN_STATE=rect=x,y,w,h;t=3.5;in=2;out=6;op=crop|pixelize;block=16` — source pixels and
/// seconds. `op` may list several, comma-separated, applied in order. The hex window takes
/// `at=OFFSET;patch=OFFSET:HEXBYTES[,OFFSET:HEXBYTES]` (offsets `0x…` or decimal); the file
/// browser `open=NAME`, and `at=OFFSET` for the first text file it opens — by `open=NAME` or by
/// hand, whichever comes first (the row that byte is in at the top), `mode=dual|preview` and
/// `bar=NAME` (02 §9); the image, hex and media windows `close=ask`; the media window
/// `clips=START-END[,START-END]` (its edit list, source seconds) and `stereo=1`; every window
/// `help=1`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub rect: Option<[u32; 4]>,
    pub t: Option<f64>,
    pub in_s: Option<f64>,
    pub out_s: Option<f64>,
    pub ops: Vec<Op>,
    pub block: Option<u32>,
    /// Hex window: where the cursor starts. The file browser: the first text file it opens (by
    /// `open=` or by hand) starts with the row this byte is in at the top; later ones do not.
    pub at: Option<u64>,
    /// Hex window: bytes overwritten at an offset, applied in order (each one an undo step).
    pub patches: Vec<(u64, Vec<u8>)>,
    /// File browser: the entry of the opened folder to open (as Enter would) once it is listed.
    pub open: Option<String>,
    /// Image, hex and media windows: closing is asked for once the state is on screen, so an
    /// edit the state made brings up the unsaved-changes prompt (`close=ask`, the prompt's
    /// receipt).
    pub ask_close: bool,
    /// Media window: the edit list as source ranges in seconds, in output order
    /// (`clips=0-1.5,3-4`).
    pub clips: Vec<(f64, f64)>,
    /// Media window: Mono → Stereo (`stereo=1`).
    pub stereo: bool,
    /// File browser: the mode it opens in, `mode=dual` or `mode=preview` (the default; 02 §9).
    pub(crate) mode: browser::view::Mode,
    /// File browser: the entry of the active panel the bar goes on once it is listed, without
    /// opening it (`bar=NAME`, a preview's receipt; 02 §9).
    pub bar: Option<String>,
    /// Every window: F1 is pressed once what the state asked for is on screen, so the key help
    /// is what the screenshot shows (`help=1`, F1's receipt).
    pub help: bool,
    /// Hex, text and image over a stream: the screenshot is taken at the first frame drawn with
    /// content from the stream, rows still loading shot as they are (`loading=1`, 06 §11).
    pub loading: bool,
}

impl State {
    pub fn parse(text: &str) -> Result<Self> {
        let mut state = State::default();
        for item in text.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let (key, value) = item
                .split_once('=')
                .ok_or_else(|| anyhow!("SOPEN_STATE: '{item}' is not key=value"))?;
            let (key, value) = (key.trim(), value.trim());
            let secs = |v: &str| -> Result<f64> {
                crate::ffmpeg::parse_time(v).with_context(|| format!("SOPEN_STATE {key}"))
            };
            match key {
                "rect" => {
                    let r: crate::ffmpeg::Rect = value.parse().context("SOPEN_STATE rect")?;
                    state.rect = Some([r.x, r.y, r.w, r.h]);
                }
                "t" => state.t = Some(secs(value)?),
                "in" => state.in_s = Some(secs(value)?),
                "out" => state.out_s = Some(secs(value)?),
                "block" => {
                    let block = value
                        .parse()
                        .map_err(|_| anyhow!("SOPEN_STATE block must be a whole number"))?;
                    state.block = Some(block);
                }
                "op" => {
                    for op in value.split(',').map(str::trim) {
                        state.ops.push(match op {
                            "crop" => Op::Crop,
                            "pixelize" => Op::Pixelize,
                            other => bail!("SOPEN_STATE: unknown op '{other}' (crop, pixelize)"),
                        });
                    }
                }
                "at" => state.at = Some(hex::parse_offset(value).context("SOPEN_STATE at")?),
                "open" => state.open = Some(value.to_string()),
                "clips" => {
                    for one in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        let (a, b) = one.split_once('-').ok_or_else(|| {
                            anyhow!("SOPEN_STATE clips: '{one}' is not START-END")
                        })?;
                        let (a, b) = (secs(a.trim())?, secs(b.trim())?);
                        if b <= a {
                            bail!("SOPEN_STATE clips: '{one}' ends before it starts");
                        }
                        state.clips.push((a, b));
                    }
                }
                "stereo" => match value {
                    "1" => state.stereo = true,
                    "0" => state.stereo = false,
                    other => bail!("SOPEN_STATE: stereo must be 1 or 0 (got '{other}')"),
                },
                "close" => match value {
                    "ask" => state.ask_close = true,
                    other => bail!("SOPEN_STATE: close must be 'ask' (got '{other}')"),
                },
                "mode" => {
                    state.mode = match value {
                        "dual" => browser::view::Mode::Dual,
                        "preview" => browser::view::Mode::Preview,
                        other => {
                            bail!("SOPEN_STATE: mode must be 'dual' or 'preview' (got '{other}')")
                        }
                    }
                }
                "bar" if value.is_empty() => bail!("SOPEN_STATE: bar needs an entry's name"),
                "bar" => state.bar = Some(value.to_string()),
                "help" => match value {
                    "1" => state.help = true,
                    other => bail!("SOPEN_STATE: help must be 1 (got '{other}')"),
                },
                "loading" => match value {
                    "1" => state.loading = true,
                    other => bail!("SOPEN_STATE: loading must be 1 (got '{other}')"),
                },
                "patch" => {
                    for one in value.split(',').map(str::trim) {
                        let (at, bytes) = one.split_once(':').ok_or_else(|| {
                            anyhow!("SOPEN_STATE patch: '{one}' is not OFFSET:HEXBYTES")
                        })?;
                        let at = hex::parse_offset(at).context("SOPEN_STATE patch")?;
                        let bytes = hex::parse_bytes(bytes).context("SOPEN_STATE patch")?;
                        state.patches.push((at, bytes));
                    }
                }
                other => bail!("SOPEN_STATE: unknown key '{other}'"),
            }
        }
        Ok(state)
    }
}

/// The answer to the binary-file dialog (spec §6a), given by `SOPEN_CHOICE` instead of a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Fetch into the cache and open the hex window.
    Hex,
    /// Save a copy at a chosen path (`SOPEN_SAVE_TO`, else the download folder).
    Save,
    /// Nothing is downloaded.
    Cancel,
}

impl Choice {
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "hex" => Ok(Choice::Hex),
            "save" => Ok(Choice::Save),
            "cancel" => Ok(Choice::Cancel),
            other => bail!("SOPEN_CHOICE must be hex, save or cancel (got '{other}')"),
        }
    }
}

/// The receipt hooks, read once from the environment.
#[derive(Debug, Clone, Default)]
pub struct Hooks {
    /// `SOPEN_SCREENSHOT=<png>`: render the state, write a screenshot, exit.
    pub screenshot: Option<PathBuf>,
    /// `SOPEN_STATE`.
    pub state: State,
    /// `SOPEN_AUTOSAVE=1`: apply the state and Save, without a window.
    pub autosave: bool,
    /// `SOPEN_AUTOEXPORT=1`: apply the state and Export, without a window.
    pub autoexport: bool,
    /// `SOPEN_AUTOSAVE_DELAY_MS`: wait between applying the state and saving, so a second
    /// process can change the remote meanwhile (the conflict receipt).
    pub save_delay: Option<Duration>,
    /// `SOPEN_EXPORT_HOLD_MS`: the media window starts an Export on open and holds it, at its
    /// first progress report at or past 30 %, for this long (a screenshot mid-export).
    pub export_hold: Option<Duration>,
    /// `SOPEN_WINDOW_SIZE=<w>x<h>`: open the window at this size in points (layout receipts on
    /// a narrow window).
    pub window_size: Option<[f32; 2]>,
    /// `SOPEN_CHOICE=hex|save|cancel`: the binary-file dialog's answer; the dialog is not shown.
    pub choice: Option<Choice>,
    /// `SOPEN_SAVE_TO=<path>`: where "Save to…" saves (the path field's first value), without
    /// the native dialog.
    pub save_to: Option<PathBuf>,
    /// `SOPEN_LIST=1`: a folder prints what the file browser lists for it, without a window.
    pub list: bool,
}

impl Hooks {
    pub fn from_env() -> Result<Self> {
        let flag = |name: &str| std::env::var(name).is_ok_and(|v| v == "1");
        let state = match std::env::var("SOPEN_STATE") {
            Ok(text) => State::parse(&text)?,
            Err(_) => State::default(),
        };
        let millis = |name: &str| -> Result<Option<Duration>> {
            match std::env::var(name) {
                Ok(ms) => Ok(Some(Duration::from_millis(ms.parse().map_err(|_| {
                    anyhow!("{name} must be a whole number of milliseconds")
                })?))),
                Err(_) => Ok(None),
            }
        };
        let save_delay = millis("SOPEN_AUTOSAVE_DELAY_MS")?;
        let export_hold = millis("SOPEN_EXPORT_HOLD_MS")?;
        let window_size = match std::env::var("SOPEN_WINDOW_SIZE") {
            Ok(text) => {
                let bad = || anyhow!("SOPEN_WINDOW_SIZE must be <width>x<height> (got '{text}')");
                let (w, h) = text.split_once('x').ok_or_else(bad)?;
                let (w, h): (f32, f32) =
                    (w.parse().map_err(|_| bad())?, h.parse().map_err(|_| bad())?);
                Some([w, h])
            }
            Err(_) => None,
        };
        let choice = match std::env::var("SOPEN_CHOICE") {
            Ok(text) => Some(Choice::parse(&text)?),
            Err(_) => None,
        };
        Ok(Self {
            screenshot: std::env::var_os("SOPEN_SCREENSHOT")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            state,
            autosave: flag("SOPEN_AUTOSAVE"),
            autoexport: flag("SOPEN_AUTOEXPORT"),
            save_delay,
            export_hold,
            window_size,
            choice,
            save_to: std::env::var_os("SOPEN_SAVE_TO")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            list: flag("SOPEN_LIST"),
        })
    }

    /// A hook drives this run: user input is ignored and closing never prompts.
    pub fn scripted(&self) -> bool {
        self.screenshot.is_some() || self.autosave || self.autoexport
    }
}

// ---------------------------------------------------------------------------------------------
// The window runner.

/// What a window shows; the shell adds activation, the screenshot hook and close handling.
pub trait Window {
    fn ui(&mut self, ui: &mut egui::Ui);
    /// True once what the state asked for is on screen (the screenshot hook waits for it).
    fn ready(&self) -> bool;
    /// The user asked to close. Return `true` to close now, or `false` to keep the window open
    /// (it shows its own prompt and later sends [`egui::ViewportCommand::Close`] again — the
    /// second request must then return `true`).
    fn close_requested(&mut self) -> bool {
        true
    }

    /// The F-keys the window handles itself, for the browser's F-key row while it is hosted
    /// there (a window of its own shows them in its own row, see [`own_keys`]).
    fn keys(&self) -> &'static [chrome::FKey] {
        &[]
    }

    /// The window becomes a preview in the file browser's opposite side (`true`), or a viewer
    /// again (`false`: the hand-over), 02 §4.2 and §4.4. A preview draws as inactive and without
    /// its own bars, takes no input, plays nothing, does not follow a growing file and keeps to
    /// 02 §4.3's budgets; `false` lifts all of that. The default does nothing (S1-B implements it
    /// for the viewers).
    fn set_preview(&mut self, on: bool) {
        let _ = on;
    }
}

/// While a window is hosted inside another (the file browser shows a viewer in its own
/// window): whether it asked to close during its pass.
#[derive(Debug, Clone, Copy, Default)]
struct Hosted {
    asked: bool,
}

fn hosted_id() -> egui::Id {
    egui::Id::unique("sopen-hosted-window")
}

/// A window asks to be closed — after its own prompt was answered, or a write it waited for
/// landed: the viewport closes (the shell asks [`Window::close_requested`] again first). A
/// window hosted inside another one ([`host_pass`]) tells its host instead, which asks the same
/// question: a hosted viewer never closes the window around it.
pub fn request_close(ctx: &egui::Context) {
    let hosted = ctx.data_mut(|d| match d.get_temp::<Hosted>(hosted_id()) {
        Some(_) => {
            d.insert_temp(hosted_id(), Hosted { asked: true });
            true
        }
        None => false,
    });
    if !hosted {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// Whether this pass is a window hosted inside another one ([`host_pass`]): the host draws the
/// rows around it (the F-keys) and reads its keys F3 and F4.
pub fn hosted(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Hosted>(hosted_id()).is_some())
}

/// A window's F-key row at the bottom of `ui` — F1 (its key help, [`help`]), F3 (closes it, as
/// Cmd/Ctrl+W does) and its own `keys` — and F1 and F3 themselves, without modifiers (04 §3
/// rule 1). Nothing while it is hosted: the host's row lists the window's keys with its own, and
/// its F1 and F3 are the host's. Call it before the window's other bottom panels (the row is the
/// window's last).
pub fn own_keys(ui: &mut egui::Ui, keys: &[chrome::FKey]) {
    let ctx = ui.ctx().clone();
    if hosted(&ctx) {
        return;
    }
    let row = chrome::merged(&[chrome::HELP_KEY, chrome::EXIT_KEY], keys);
    chrome::key_panel(ui, "sopen-own-keys", &row);
    // Not while a text field has the keyboard (F-keys are not text, but the field is the
    // owner's).
    if ctx.text_edit_focused() {
        return;
    }
    let plain = browser::keys::Mods::None;
    let (help, exit) = ctx.input_mut(|i| {
        (
            browser::keys::take(i, egui::Key::F1, plain),
            browser::keys::take(i, egui::Key::F3, plain),
        )
    });
    if help {
        help::open(&ctx, help::for_window(&row));
    }
    if exit {
        request_close(&ctx);
    }
}

/// The shell's raw-input hook (04 §6.1) and the key help's passes, as an egui plugin that
/// [`chrome::install`] puts into every window's context (the test kit's driver gets it the same
/// way). Before each pass, once, before any window sees the input: Cmd/Ctrl+1…9, 0 become
/// F1…F10 and a clicked slot of the F-key row ([`chrome::fkey_row`]) its F-key — plain keys,
/// so every window and the browser's key table act on them by the F-key's own rules; not while
/// a text field has the typing (04 §3 rule 4). Then [`help::begin_pass`] before the window's
/// pass and [`help::end_pass`] after it.
#[derive(Debug, Default)]
pub(crate) struct FKeyInput {
    /// What each digit key (1…9, 0) is while it is down.
    digits: [Digit; 10],
}

/// A digit key between its press and its release: the shell decides at the press what it is,
/// and it stays that — its repeats and its release go with it — whatever happens to the
/// modifiers or the typing in between, so every key down the window sees comes up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Digit {
    #[default]
    Up,
    /// Pressed without exactly Cmd/Ctrl (or while a field has the typing): the digit itself.
    Plain,
    /// Pressed with exactly Cmd/Ctrl: its F-key.
    Alias,
}

impl egui::Plugin for FKeyInput {
    fn debug_name(&self) -> &'static str {
        "sopen-fkeys"
    }

    fn input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        // The focus as the last pass left it: what has the typing now.
        let typing = ctx.text_edit_focused();
        let clicked = chrome::take_clicked(ctx);
        fkey_events(
            &mut input.events,
            &mut self.digits,
            typing,
            input.focused,
            clicked,
        );
    }

    fn on_begin_pass(&mut self, ui: &mut egui::Ui) {
        help::begin_pass(ui.ctx());
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        help::end_pass(ui.ctx());
    }
}

/// The F-key events of a pass (see [`FKeyInput`]): a digit pressed with exactly Cmd/Ctrl, not
/// while a field has the typing, is that F-key pressed without modifiers until it is let go —
/// its repeats are the F-key's (the digit they type is dropped: egui-winit puts a key's typed
/// text right after its key event), its release the F-key's. A digit pressed otherwise stays
/// the digit, its repeats too. Losing the window's focus lets go of every F-key held so (winit
/// may never send those releases; egui clears its own keys then too). The digit is the
/// physical key's first, so on a layout whose digits need Shift, Cmd/Ctrl and the key printed 5
/// still make F5. `clicked` adds a press and a release of its F-key.
fn fkey_events(
    events: &mut Vec<egui::Event>,
    digits: &mut [Digit; 10],
    typing: bool,
    focused: bool,
    clicked: Option<u8>,
) {
    use egui::Key::*;
    let number = |key: egui::Key| -> Option<u8> {
        let n = [Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9, Num0]
            .iter()
            .position(|k| *k == key)?;
        Some(n as u8 + 1)
    };
    let fkey = |n: u8, pressed: bool| egui::Event::Key {
        key: chrome::fkey(n).expect("F1…F10"),
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let let_go = |digits: &mut [Digit; 10], out: &mut Vec<egui::Event>| {
        for (n, digit) in (1..=10u8).zip(digits.iter_mut()) {
            if std::mem::take(digit) == Digit::Alias {
                out.push(fkey(n, false));
            }
        }
    };
    let mut out = Vec::with_capacity(events.len() + 2);
    let mut after_alias = false;
    for event in events.drain(..) {
        let typed_by_alias = std::mem::take(&mut after_alias);
        let (key, physical_key, pressed, modifiers) = match &event {
            egui::Event::Text(_) if typed_by_alias => continue,
            egui::Event::WindowFocused(false) => {
                let_go(digits, &mut out);
                out.push(event);
                continue;
            }
            egui::Event::Key {
                key,
                physical_key,
                pressed,
                modifiers,
                ..
            } => (*key, *physical_key, *pressed, *modifiers),
            _ => {
                out.push(event);
                continue;
            }
        };
        let Some(n) = physical_key.and_then(number).or_else(|| number(key)) else {
            out.push(event);
            continue;
        };
        let digit = &mut digits[usize::from(n - 1)];
        let now = match (pressed, *digit) {
            (true, Digit::Up) if !typing && browser::keys::Mods::Command.matches(modifiers) => {
                Digit::Alias
            }
            (true, Digit::Up) => Digit::Plain,
            (true, held) => held,
            (false, _) => Digit::Up,
        };
        if *digit == Digit::Alias || now == Digit::Alias {
            out.push(fkey(n, pressed));
            after_alias = pressed;
        } else {
            out.push(event);
        }
        *digit = now;
    }
    if !focused {
        let_go(digits, &mut out);
    }
    if let Some(n) = clicked.filter(|n| !typing && chrome::fkey(*n).is_some()) {
        out.extend([fkey(n, true), fkey(n, false)]);
    }
    *events = out;
}

/// Runs a hosted window's pass (`pass`) and says whether the window asked to close in it
/// ([`request_close`]).
pub fn host_pass<R>(ctx: &egui::Context, pass: impl FnOnce() -> R) -> (R, bool) {
    ctx.data_mut(|d| d.insert_temp(hosted_id(), Hosted::default()));
    let out = pass();
    let asked = ctx
        .data_mut(|d| d.remove_temp::<Hosted>(hosted_id()))
        .is_some_and(|h| h.asked);
    (out, asked)
}

/// A window made to be hosted inside another one (the file browser's viewers), and how its
/// writes end.
pub struct Embedded {
    pub window: Box<dyn Window>,
    /// After the window is gone: `Some(outcome)` once no write it started is still running — a
    /// write that finished since is printed and counted, as the standalone window's opener
    /// does — or `None` while one runs. `true` waits for it.
    pub settle: Box<dyn FnMut(bool) -> Option<Outcome>>,
}

impl Outcome {
    /// One window's outcome added to the session's (the browser's viewers, one after
    /// another): any save or export counts, and a window whose last save was refused makes it
    /// the session's word (exit 3).
    pub fn merge(&mut self, next: Outcome) {
        self.saved |= next.saved;
        self.exported |= next.exported;
        self.conflict |= next.conflict;
        if next.refused.is_some() {
            self.refused = next.refused;
        }
    }
}

struct Shot {
    path: PathBuf,
    title: String,
    ready_frames: u32,
    requested: bool,
    done: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<String, String>>>>,
}

struct Shell<W: Window> {
    inner: W,
    scripted: bool,
    shot: Option<Shot>,
    frames: u64,
    started: Instant,
    /// When the first focus request went out.
    focus_asked: Option<Instant>,
    raise: Raise,
    /// `help=1`: F1 is pressed once the window is ready.
    press: Press,
}

/// The `help=1` receipt: F1 pressed once what the state asked for is on screen, then the
/// screenshot waits for the frames after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Press {
    /// Until the window is ready.
    Waiting,
    /// In the next pass's input.
    Now,
    /// Pressed, or never asked for.
    Done,
}

/// How long the window may take to report focus after the first focus request.
const FOCUS_GRACE: Duration = Duration::from_millis(300);

/// Bringing a window to the front when focus is refused: macOS keeps an app started over ssh
/// (`--target`) unfocused and behind the active app, whatever it asks for. Still unfocused
/// [`FOCUS_GRACE`] after the focus requests, the window floats above the others
/// (`WindowLevel::AlwaysOnTop`) until it gains focus or the first key, click, scroll or text lands
/// in it, then goes back to the normal level. No timeout: back at the normal level while
/// unfocused, macOS puts it straight behind the active app again (measured). Focus itself is
/// untouched (`focused=` stays truthful).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Raise {
    /// Waiting to see whether the focus requests worked.
    Waiting,
    /// Above the other windows.
    Floating,
    /// Nothing more to do.
    Done,
}

impl Raise {
    /// One frame: the next state and the window level to set, if it changes. `asked` = when the
    /// first focus request went out (`None` before), `requests_done` = all of them went out.
    fn step(
        self,
        asked: Option<Instant>,
        requests_done: bool,
        focused: Option<bool>,
        input: bool,
        now: Instant,
    ) -> (Self, Option<egui::WindowLevel>) {
        match self {
            Raise::Waiting => match asked {
                Some(_) if focused == Some(true) && requests_done => (Raise::Done, None),
                Some(at) if requests_done && now.saturating_duration_since(at) >= FOCUS_GRACE => {
                    (Raise::Floating, Some(egui::WindowLevel::AlwaysOnTop))
                }
                _ => (self, None),
            },
            Raise::Floating if input || focused == Some(true) => {
                (Raise::Done, Some(egui::WindowLevel::Normal))
            }
            other => (other, None),
        }
    }
}

/// A key press, click, scroll or typed text — the user is in the window.
fn user_input(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key { pressed: true, .. }
            | egui::Event::PointerButton { pressed: true, .. }
            | egui::Event::MouseWheel { .. }
            | egui::Event::Text(_)
    )
}

impl<W: Window> Shell<W> {
    fn screenshot_tick(&mut self, ctx: &egui::Context) {
        let Some(shot) = self.shot.as_mut() else {
            return;
        };
        if shot.done.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if !self.inner.ready() || self.press != Press::Done {
            shot.ready_frames = 0;
            return;
        }
        // A few painted passes after "ready", so the frame read back is the content frame.
        ctx.request_repaint();
        shot.ready_frames += 1;
        if shot.ready_frames < 3 || shot.requested {
            return;
        }
        shot.requested = true;
        let (path, done, result) = (shot.path.clone(), shot.done.clone(), shot.result.clone());
        let title = shot.title.clone();
        let focused = ctx.input(|i| i.viewport().focused);
        let elapsed = self.started.elapsed();
        let ctx2 = ctx.clone();
        ctx.request_screenshot(move |img| {
            let [w, h] = img.size;
            let mut rgba = Vec::with_capacity(w * h * 4);
            for p in &img.pixels {
                rgba.extend_from_slice(&p.to_srgba_unmultiplied());
            }
            let written = ::image::save_buffer(
                &path,
                &rgba,
                w as u32,
                h as u32,
                ::image::ExtendedColorType::Rgba8,
            );
            *result.lock().unwrap() = Some(match written {
                Ok(()) => Ok(format!(
                    "screenshot {} {w}x{h} title=\"{title}\" focused={} after {} ms",
                    path.display(),
                    focused.map_or("unknown".to_string(), |f| f.to_string()),
                    elapsed.as_millis()
                )),
                Err(e) => Err(format!("cannot write {}: {e}", path.display())),
            });
            done.store(true, Ordering::SeqCst);
            ctx2.request_repaint();
        });
    }
}

impl<W: Window> eframe::App for Shell<W> {
    /// The skin's window body where nothing is painted (eframe's default is a dark grey).
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if self.scripted {
            // A scripted run renders the state it was given; keys typed into whatever window
            // the owner is using while a receipt window flashes up must not edit it.
            raw.events.clear();
        }
        if self.press == Press::Now {
            self.press = Press::Done;
            for pressed in [true, false] {
                raw.events.push(egui::Event::Key {
                    key: egui::Key::F1,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.frames == 0 {
            ctx.options_mut(|o| o.zoom_with_keyboard = false);
        }
        // Started from a terminal, the window can come up behind it. eframe shows the window
        // only after the first painted frame (and a hidden window ignores focus), so ask on the
        // first passes after that.
        if self.frames <= 3 {
            // Passes 1–3 must happen without waiting for input.
            ctx.request_repaint();
        }
        if (1..=3).contains(&self.frames) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.focus_asked.get_or_insert_with(Instant::now);
        }
        self.frames += 1;
        let (focused, input) =
            ctx.input(|i| (i.viewport().focused, i.events.iter().any(user_input)));
        let (raise, level) = self.raise.step(
            self.focus_asked,
            self.frames > 3,
            focused,
            input,
            Instant::now(),
        );
        self.raise = raise;
        if let Some(level) = level {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
        }
        match self.raise {
            Raise::Waiting => ctx.request_repaint_after(Duration::from_millis(50)),
            // Focus and input arrive as window events, which repaint by themselves.
            Raise::Floating | Raise::Done => {}
        }
        closing(&ctx, &mut self.inner, self.scripted);
        self.inner.ui(ui);
        if self.press == Press::Waiting && self.inner.ready() {
            self.press = Press::Now;
            ctx.request_repaint();
        }
        self.screenshot_tick(&ctx);
    }
}

/// The shell's part of closing, before the window's pass: Cmd/Ctrl+Q and +W ask to close, as
/// the window's close button does; a close asked for (by those, or by the window itself —
/// F3, its prompt answered) goes to the window first, and its refusal cancels it (it shows
/// its own prompt). A scripted run never refuses. The test kit runs the same function.
pub(crate) fn closing<W: Window + ?Sized>(ctx: &egui::Context, inner: &mut W, scripted: bool) {
    if command_key(ctx, egui::Key::Q, false) || command_key(ctx, egui::Key::W, false) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    if ctx.input(|i| i.viewport().close_requested()) && !scripted && !inner.close_requested() {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
    }
}

/// `SOPEN_RENDERER=glow|wgpu`; unset = wgpu, with glow when wgpu does not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RendererChoice {
    Auto,
    Wgpu,
    Glow,
}

fn renderer_choice() -> Result<RendererChoice> {
    match std::env::var("SOPEN_RENDERER").as_deref() {
        Err(_) | Ok("") => Ok(RendererChoice::Auto),
        Ok("wgpu") => Ok(RendererChoice::Wgpu),
        Ok("glow") => Ok(RendererChoice::Glow),
        Ok(other) => bail!("SOPEN_RENDERER must be wgpu or glow (got '{other}')"),
    }
}

/// Linux without `DISPLAY` / `WAYLAND_DISPLAY` (an ssh session) has nowhere to show a window;
/// the caller prints the local copy instead.
pub fn display_available() -> bool {
    if cfg!(target_os = "linux") {
        ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()))
    } else {
        true
    }
}

#[cfg(target_os = "macos")]
fn event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
    Some(Box::new(|builder| {
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        // The default menu's Quit item calls `terminate:` and would skip the unsaved-changes
        // prompt; Cmd+Q / Cmd+W are handled in the window instead.
        builder.with_default_menu(false);
    }))
}

#[cfg(not(target_os = "macos"))]
fn event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
    None
}

/// Runs `window` until it closes. With the screenshot hook a watchdog ends a stalled run after
/// `SOPEN_WATCHDOG` seconds (default 30).
pub fn run_window<W: Window + 'static>(
    title: &str,
    size: [f32; 2],
    window: W,
    hooks: &Hooks,
) -> Result<()> {
    run_window_sized(title, size, [480.0, 320.0], window, hooks)
}

/// [`run_window`] with a smallest size of `min` points (the binary dialog is smaller).
pub fn run_window_sized<W: Window + 'static>(
    title: &str,
    size: [f32; 2],
    min: [f32; 2],
    window: W,
    hooks: &Hooks,
) -> Result<()> {
    let choice = renderer_choice()?;
    let shot_done = Arc::new(AtomicBool::new(false));
    let shot_result: Arc<Mutex<Option<Result<String, String>>>> = Arc::default();
    if hooks.screenshot.is_some() {
        let secs: u64 = std::env::var("SOPEN_WATCHDOG")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);
        let done = shot_done.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(secs));
            if !done.load(Ordering::SeqCst) {
                eprintln!("sopen: the screenshot hook stalled for {secs} s; giving up");
                std::process::exit(1);
            }
        });
    }
    let shell = Shell {
        inner: window,
        scripted: hooks.scripted(),
        shot: hooks.screenshot.clone().map(|path| Shot {
            path,
            title: title.to_string(),
            ready_frames: 0,
            requested: false,
            done: shot_done,
            result: shot_result.clone(),
        }),
        frames: 0,
        started: Instant::now(),
        focus_asked: None,
        raise: Raise::Waiting,
        press: if hooks.state.help {
            Press::Waiting
        } else {
            Press::Done
        },
    };
    // The app moves into whichever renderer starts. A renderer that fails before creating the
    // app leaves it here for the next one.
    let slot = RefCell::new(Some(shell));
    let attempt = |renderer: eframe::Renderer| -> Result<(), eframe::Error> {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(title)
                .with_app_id("sopen")
                .with_inner_size(hooks.window_size.unwrap_or(size))
                .with_min_inner_size(
                    hooks
                        .window_size
                        .map_or(min, |[w, h]| [w.min(min[0]), h.min(min[1])]),
                )
                .with_active(true),
            renderer,
            centered: true,
            event_loop_builder: event_loop_hook(),
            ..Default::default()
        };
        eframe::run_native(
            "sopen",
            options,
            Box::new(|cc| {
                // The look, before the first frame.
                chrome::install(&cc.egui_ctx);
                let shell = slot
                    .borrow_mut()
                    .take()
                    .ok_or("the window was already started once")?;
                Ok(Box::new(shell) as Box<dyn eframe::App>)
            }),
        )
    };
    let run = match choice {
        RendererChoice::Glow => attempt(eframe::Renderer::Glow),
        RendererChoice::Wgpu => attempt(eframe::Renderer::Wgpu),
        RendererChoice::Auto => match attempt(eframe::Renderer::Wgpu) {
            Err(e) if slot.borrow().is_some() => {
                eprintln!("sopen: the wgpu renderer did not start ({e}); using OpenGL (glow)");
                attempt(eframe::Renderer::Glow)
            }
            other => other,
        },
    };
    run.map_err(|e| anyhow!("cannot open a window: {e}"))?;
    if hooks.screenshot.is_some() {
        match shot_result.lock().unwrap().take() {
            Some(Ok(line)) => println!("{line}"),
            Some(Err(e)) => bail!("{e}"),
            None => bail!("the window closed before the screenshot was taken"),
        }
    }
    Ok(())
}

/// A status line: errors in the error colour, everything else plain.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub text: String,
    pub error: bool,
}

impl Status {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
        }
    }

    /// On one line, cut to the room left (the full text on hover): a status bar must keep its
    /// height, or the picture above it moves under the pointer.
    pub fn show(&self, ui: &mut egui::Ui) {
        let color = if self.error {
            ui.visuals().error_fg_color
        } else {
            ui.visuals().text_color()
        };
        one_line(ui, &self.text, color);
    }
}

/// A label that never wraps: text longer than the room left is cut with "…" and shown whole
/// on hover. Only for text that may be cut — never for what is lost (see [`disclosures`]).
pub fn one_line(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.add(egui::Label::new(egui::RichText::new(text).color(color)).truncate());
}

/// What the owner must read whole — what Save / Export will not keep, a refused write, where
/// the edit went, what closing during a write does — in its own rows that wrap instead of
/// cutting. Their text changes only on open, when a write reports and when closing is asked
/// for during a write; a change waits while the primary button is down, so they never move the
/// picture under a drag.
pub fn disclosures(ui: &mut egui::Ui, lines: &[(String, egui::Color32)]) {
    for (text, color) in lines {
        ui.add(egui::Label::new(egui::RichText::new(text).color(*color)).wrap());
    }
}

/// `line`, followed by what was not kept: `"<line> — not kept: XMP; alpha is dropped …"`.
pub fn with_notes(line: &str, notes: &[String]) -> String {
    if notes.is_empty() {
        line.to_string()
    } else {
        format!("{line} — {}", notes.join("; "))
    }
}

/// A key pressed without modifiers (the single-letter commands) — not while a text field
/// (the block size box) is being edited.
pub fn plain_key(ctx: &egui::Context, key: egui::Key) -> bool {
    !ctx.egui_wants_keyboard_input() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key))
}

/// A window's own F-key pressed with no modifier at all (04 §3: Shift, Alt or Cmd/Ctrl with it
/// is another key, not this one) — not while a text field is being edited.
pub fn fkey_alone(ctx: &egui::Context, key: egui::Key) -> bool {
    !ctx.egui_wants_keyboard_input()
        && ctx.input_mut(|i| browser::keys::take(i, key, browser::keys::Mods::None))
}

/// Cmd (macOS) / Ctrl (elsewhere) + key, with or without Shift.
pub fn command_key(ctx: &egui::Context, key: egui::Key, shift: bool) -> bool {
    let mods = if shift {
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
    } else {
        egui::Modifiers::COMMAND
    };
    ctx.input_mut(|i| i.consume_key(mods, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_parses() {
        let s =
            State::parse("rect=101,51,401,301;t=3.5;in=2;out=0:06;op=pixelize;block=8").unwrap();
        assert_eq!(s.rect, Some([101, 51, 401, 301]));
        assert_eq!((s.t, s.in_s, s.out_s), (Some(3.5), Some(2.0), Some(6.0)));
        assert_eq!(s.ops, vec![Op::Pixelize]);
        assert_eq!(s.block, Some(8));
        let s = State::parse(" op=crop,pixelize ; ").unwrap();
        assert_eq!(s.ops, vec![Op::Crop, Op::Pixelize]);
        assert!(State::parse("op=blur").is_err());
        assert!(State::parse("zoom=2").is_err());
        assert!(State::parse("rect=1,2,3").is_err());
        assert_eq!(State::parse("").unwrap(), State::default());
    }

    #[test]
    fn hex_state_parses() {
        let s = State::parse("at=0x1f;patch=0x10:DEad,17:00ff").unwrap();
        assert_eq!(s.at, Some(31));
        assert_eq!(
            s.patches,
            vec![(16, vec![0xde, 0xad]), (17, vec![0x00, 0xff])]
        );
        assert_eq!(State::parse("at=42").unwrap().at, Some(42));
        let s = State::parse("open=big.txt;at=524293700").unwrap();
        assert_eq!(
            (s.open.as_deref(), s.at),
            (Some("big.txt"), Some(524_293_700))
        );
        assert_eq!(
            State::parse("open=main.rs").unwrap().open.as_deref(),
            Some("main.rs")
        );
        assert!(State::parse("patch=0:00;close=ask").unwrap().ask_close);
        assert!(!State::parse("at=1").unwrap().ask_close);
        assert!(State::parse("close=now").is_err());
        for bad in [
            "at=x",
            "at=-1",
            "patch=1",
            "patch=1:",
            "patch=1:abc",
            "patch=1:zz",
            "patch=q:00",
        ] {
            assert!(State::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Choice::parse("hex").unwrap(), Choice::Hex);
        assert_eq!(Choice::parse("save").unwrap(), Choice::Save);
        assert_eq!(Choice::parse("cancel").unwrap(), Choice::Cancel);
        assert!(Choice::parse("Hex").is_err());
    }

    /// The media window's edit list and Mono → Stereo, as the receipts give them.
    #[test]
    fn media_state_parses() {
        let s = State::parse("clips=4-6, 0:01-2.5 ,6-8;stereo=1;close=ask").unwrap();
        assert_eq!(s.clips, vec![(4.0, 6.0), (1.0, 2.5), (6.0, 8.0)]);
        assert!(s.stereo && s.ask_close);
        assert!(!State::parse("stereo=0").unwrap().stereo);
        for bad in ["clips=3", "clips=3-2", "clips=a-b", "stereo=yes"] {
            assert!(State::parse(bad).is_err(), "{bad}");
        }
    }

    /// The receipt keys of slice S1 (02 §9): `mode=`, `bar=`, and `help=1`; anything else is
    /// refused, as every key is.
    #[test]
    fn browser_state_keys_parse() {
        use browser::view::Mode;
        assert_eq!(State::default().mode, Mode::Preview);
        assert_eq!(State::parse("mode=dual").unwrap().mode, Mode::Dual);
        assert_eq!(State::parse("mode=preview").unwrap().mode, Mode::Preview);
        let s = State::parse("bar=main.rs;mode=dual;help=1").unwrap();
        assert_eq!(
            (s.bar.as_deref(), s.mode, s.help),
            (Some("main.rs"), Mode::Dual, true)
        );
        assert_eq!(
            State::parse("bar=a b=c.txt").unwrap().bar.as_deref(),
            Some("a b=c.txt")
        );
        assert!(!State::parse("bar=x").unwrap().help);
        for bad in [
            "mode=tree",
            "mode=",
            "mode=Dual",
            "bar=",
            "help=0",
            "help=yes",
        ] {
            assert!(State::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn edited_name_keeps_the_extension() {
        assert_eq!(edited_name("shot.png"), "shot.edited.png");
        assert_eq!(edited_name("a.tar.gz"), "a.tar.edited.gz");
        assert_eq!(edited_name("README"), "README.edited");
        assert_eq!(edited_name(".bashrc"), ".bashrc.edited");
    }

    #[test]
    fn an_unfocused_window_floats_above_the_others_until_it_is_used() {
        use egui::WindowLevel::{AlwaysOnTop, Normal};
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let w = Raise::Waiting;
        // Before or during the focus requests, and within the grace: wait.
        assert_eq!(w.step(None, false, None, false, at(0)), (w, None));
        assert_eq!(
            w.step(Some(t0), false, Some(false), false, at(900)),
            (w, None)
        );
        assert_eq!(
            w.step(Some(t0), true, Some(false), false, at(100)),
            (w, None)
        );
        // Focused: nothing to do, ever.
        assert_eq!(
            w.step(Some(t0), true, Some(true), false, at(100)),
            (Raise::Done, None)
        );
        // Not focused (or not known) after the grace: float.
        for focused in [Some(false), None] {
            assert_eq!(
                w.step(Some(t0), true, focused, false, at(300)),
                (Raise::Floating, Some(AlwaysOnTop))
            );
        }
        // No timeout: still floating, unfocused and untouched, a minute later.
        let f = Raise::Floating;
        for focused in [Some(false), None] {
            assert_eq!(
                f.step(Some(t0), true, focused, false, at(60_000)),
                (f, None)
            );
        }
        // Back to normal when it gains focus, or at the first input.
        assert_eq!(
            f.step(Some(t0), true, Some(true), false, at(60_000)),
            (Raise::Done, Some(Normal))
        );
        assert_eq!(
            f.step(Some(t0), true, Some(false), true, at(400)),
            (Raise::Done, Some(Normal))
        );
        let d = Raise::Done;
        assert_eq!(
            d.step(Some(t0), true, Some(false), true, at(9000)),
            (d, None)
        );
    }

    #[test]
    fn user_input_is_keys_clicks_scrolls_and_text() {
        let click = egui::Event::PointerButton {
            pos: egui::Pos2::ZERO,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        assert!(user_input(&click));
        assert!(user_input(&egui::Event::Text("a".into())));
        assert!(!user_input(&egui::Event::PointerMoved(egui::Pos2::ZERO)));
        let release = egui::Event::PointerButton {
            pos: egui::Pos2::ZERO,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert!(!user_input(&release));
    }

    /// The trait's default `set_preview` changes nothing: a window that does not implement it
    /// draws and answers the same after it, called as the browser holds its viewers
    /// (`dyn Window`).
    #[test]
    fn the_default_set_preview_does_nothing() {
        struct Plain;
        impl Window for Plain {
            fn ui(&mut self, ui: &mut egui::Ui) {
                ui.label("a plain window");
            }
            fn ready(&self) -> bool {
                true
            }
        }
        let mut w = Plain;
        let mut drv = testkit::Driver::new(400.0, 300.0);
        drv.frame(&mut w, Vec::new(), egui::Modifiers::NONE);
        let before = drv.texts();
        assert!(drv.find_text("a plain window").is_some(), "{before:?}");
        for on in [true, false] {
            let dyn_w: &mut dyn Window = &mut w;
            dyn_w.set_preview(on);
            assert!(dyn_w.ready() && dyn_w.keys().is_empty() && dyn_w.close_requested());
            drv.frame(&mut w, Vec::new(), egui::Modifiers::NONE);
            assert_eq!(drv.texts(), before, "set_preview({on})");
        }
    }

    /// Cmd/Ctrl as this machine's backend reports it: ⌘ (`mac_cmd` + `command`) on macOS, Ctrl
    /// (`ctrl` + `command`) elsewhere.
    fn real_command() -> egui::Modifiers {
        if cfg!(target_os = "macos") {
            egui::Modifiers {
                mac_cmd: true,
                command: true,
                ..Default::default()
            }
        } else {
            egui::Modifiers {
                ctrl: true,
                command: true,
                ..Default::default()
            }
        }
    }

    /// The real file browser on a folder holding `main.rs` and `notes.txt`, its F4 recorded.
    struct BrowserFx {
        _tmp: tempfile::TempDir,
        root: String,
        drv: testkit::Driver,
        b: browser::Browser,
        launched: Arc<Mutex<Vec<String>>>,
    }

    impl BrowserFx {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(tmp.path()).unwrap().join("root");
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
            std::fs::write(root.join("notes.txt"), "plain notes\n").unwrap();
            let root = root.to_str().unwrap().to_string();
            let launched: Arc<Mutex<Vec<String>>> = Arc::default();
            let record = launched.clone();
            let b = browser::Browser::new(browser::Start {
                fs: Box::new(crate::remote::LocalFs::new()),
                connect: Arc::new(|| {
                    Ok(Box::new(crate::remote::LocalFs::new()) as Box<dyn RemoteFs + Send>)
                }),
                label: None,
                destination: None,
                abs: root.clone(),
                cache: tmp.path().join("cache"),
                state_dir: tmp.path().join("state"),
                launch: Arc::new(move |abs: &str, _is_dir: bool| {
                    record.lock().unwrap().push(abs.to_string());
                    Ok(format!("opened {abs}"))
                }),
            });
            let mut fx = Self {
                _tmp: tmp,
                root,
                drv: testkit::Driver::new(1000.0, 700.0),
                b,
                launched,
            };
            fx.settle();
            fx
        }

        /// A pass (the row is drawn before the keys of its pass act: the next one shows what
        /// they did), then passes until the browser is ready.
        fn settle(&mut self) {
            self.drv.idle(&mut self.b, 1);
            self.drv.until(&mut self.b, 20, |b| b.ready());
        }

        /// The bar on `main.rs` (the jump letter).
        fn bar_on_main(&mut self) {
            self.drv.frame(
                &mut self.b,
                vec![egui::Event::Text("m".into())],
                egui::Modifiers::NONE,
            );
            self.drv.idle(&mut self.b, 1);
        }

        /// Whether the viewer is open: its row has Exit, the list's View is gone.
        fn viewer(&self) -> bool {
            let exit = self.drv.find_text("Exit   ").is_some();
            assert_ne!(exit, self.drv.find_text("View   ").is_some(), "one row");
            exit
        }

        fn edited(&mut self) -> Vec<String> {
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.launched.lock().unwrap().is_empty() && Instant::now() < deadline {
                self.drv.idle(&mut self.b, 1);
                std::thread::sleep(Duration::from_millis(10));
            }
            self.launched.lock().unwrap().clone()
        }
    }

    /// 04 §6.1, K-R2: in the real browser, Cmd/Ctrl+3 is F3 (the viewer opens; in it, F3 leaves)
    /// and Cmd/Ctrl+4 is F4 (the editor) — the shell turns them into the plain F-key before the
    /// window's pass. Only exactly Cmd/Ctrl: with Shift or Alt it is nothing.
    #[test]
    fn cmd_digits_are_f_keys_in_a_real_window() {
        let mut fx = BrowserFx::new();
        fx.bar_on_main();
        assert!(!fx.viewer());
        for extra in [egui::Modifiers::SHIFT, egui::Modifiers::ALT] {
            fx.drv
                .key(&mut fx.b, egui::Key::Num3, real_command() | extra);
            fx.settle();
            assert!(!fx.viewer(), "{extra:?}: no row");
        }
        fx.drv.key(&mut fx.b, egui::Key::Num3, real_command());
        fx.settle();
        assert!(fx.viewer(), "Cmd/Ctrl+3 = F3 opened main.rs");
        assert!(fx.drv.find_text("fn main() {}").is_some());
        // The test kit's own Cmd/Ctrl (`command` alone) is the same key.
        fx.drv
            .key(&mut fx.b, egui::Key::Num3, egui::Modifiers::COMMAND);
        fx.settle();
        assert!(!fx.viewer(), "F3 in the viewer: back to the list");
        fx.drv.key(&mut fx.b, egui::Key::Num4, real_command());
        assert_eq!(fx.edited(), [format!("{}/main.rs", fx.root)]);
    }

    /// 04 §6.1, K-R2: a click on a slot of the real browser's row is its key: View opens the
    /// file, Exit leaves the viewer, Edit opens the editor; an empty slot is nothing.
    #[test]
    fn a_click_on_a_slot_is_its_key_in_a_real_window() {
        let mut fx = BrowserFx::new();
        fx.bar_on_main();
        let (_, empty) = chrome::key_slot(0.0, 1000.0, 7);
        fx.drv
            .click(&mut fx.b, egui::Pos2::new(empty.center().x, 700.0 - 8.0));
        fx.settle();
        assert!(!fx.viewer());
        fx.drv.click_text(&mut fx.b, "View   ");
        fx.settle();
        assert!(fx.viewer(), "the View slot opened main.rs");
        fx.drv.click_text(&mut fx.b, "Exit   ");
        fx.settle();
        assert!(!fx.viewer(), "the Exit slot left it");
        fx.drv.click_text(&mut fx.b, "Edit   ");
        assert_eq!(fx.edited(), [format!("{}/main.rs", fx.root)]);
    }

    /// A field with the typing and an F5 handler that counts.
    struct Field {
        text: String,
        focus: bool,
        f5: usize,
    }

    impl Window for Field {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let resp = ui.add(chrome::field(&mut self.text));
            if self.focus {
                resp.request_focus();
            } else {
                resp.surrender_focus();
            }
            self.f5 +=
                ui.input_mut(|i| i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::F5));
        }

        fn ready(&self) -> bool {
            true
        }
    }

    /// 04 §3 rule 4, §6.1: while a field has the typing Cmd/Ctrl+5 is not F5; once it has not,
    /// it is.
    #[test]
    fn cmd_digits_wait_while_a_field_has_the_typing() {
        let mut drv = testkit::Driver::new(400.0, 200.0);
        let mut win = Field {
            text: "name".into(),
            focus: true,
            f5: 0,
        };
        drv.idle(&mut win, 2);
        assert!(drv.ctx.text_edit_focused());
        drv.key(&mut win, egui::Key::Num5, real_command());
        assert_eq!((win.f5, win.text.as_str()), (0, "name"));
        win.focus = false;
        drv.idle(&mut win, 2);
        assert!(!drv.ctx.text_edit_focused());
        drv.key(&mut win, egui::Key::Num5, real_command());
        assert_eq!(win.f5, 1);
        // Released with Cmd/Ctrl let go first: the F-key is let go too (not left down).
        drv.frame(
            &mut win,
            vec![egui::Event::Key {
                key: egui::Key::Num5,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: real_command(),
            }],
            real_command(),
        );
        drv.frame(
            &mut win,
            vec![egui::Event::Key {
                key: egui::Key::Num5,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            egui::Modifiers::NONE,
        );
        assert_eq!(win.f5, 2);
        assert!(!drv.ctx.input(|i| i.key_down(egui::Key::F5)), "F5 let go");
    }

    /// Every key event and typed text that reaches the window's pass, in order, and a field
    /// that has the typing while `focus` is set.
    struct Recorder {
        seen: Vec<egui::Event>,
        text: String,
        focus: bool,
    }

    impl Window for Recorder {
        fn ui(&mut self, ui: &mut egui::Ui) {
            ui.input(|i| {
                self.seen.extend(
                    i.events
                        .iter()
                        .filter(|e| matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)))
                        .cloned(),
                );
            });
            let resp = ui.add(chrome::field(&mut self.text));
            if self.focus {
                resp.request_focus();
            } else {
                resp.surrender_focus();
            }
        }

        fn ready(&self) -> bool {
            true
        }
    }

    fn digit(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    /// The keys `seen` leaves down, pressing and releasing in order (a press of a key already
    /// down is its repeat), and the `(key, pressed)` sequence.
    fn left_down(seen: &[egui::Event]) -> (Vec<egui::Key>, Vec<(egui::Key, bool)>) {
        let mut down = Vec::new();
        let mut keys = Vec::new();
        for e in seen {
            if let egui::Event::Key { key, pressed, .. } = e {
                keys.push((*key, *pressed));
                if *pressed {
                    if !down.contains(key) {
                        down.push(*key);
                    }
                } else {
                    down.retain(|k| k != key);
                }
            }
        }
        (down, keys)
    }

    /// A frame for [`alias_run`]: its events, the modifiers held, and the field's focus to set
    /// first (with two passes for it to take).
    type AliasFrame = (Vec<egui::Event>, egui::Modifiers, Option<bool>);

    /// `frames` through a real window (the shell's hook runs before each pass): what the
    /// window saw, and the keys egui still has down at the end.
    fn alias_run(frames: Vec<AliasFrame>) -> (Recorder, Vec<egui::Key>) {
        let mut drv = testkit::Driver::new(400.0, 200.0);
        let mut win = Recorder {
            seen: Vec::new(),
            text: String::new(),
            focus: false,
        };
        drv.idle(&mut win, 2);
        for (events, mods, focus) in frames {
            if let Some(focus) = focus {
                win.focus = focus;
                drv.idle(&mut win, 2);
            }
            drv.frame(&mut win, events, mods);
        }
        let stuck = [egui::Key::Num5, egui::Key::F5]
            .into_iter()
            .filter(|k| drv.ctx.input(|i| i.key_down(*k)))
            .collect();
        (win, stuck)
    }

    /// The window saw exactly `want` as `(key, pressed)`, and nothing is left down — neither in
    /// what it saw (a press of a key already down is its repeat) nor in egui.
    fn assert_balanced(win: &Recorder, stuck: &[egui::Key], want: &[(egui::Key, bool)]) {
        let (down, keys) = left_down(&win.seen);
        assert_eq!(keys, want, "{:?}", win.seen);
        assert!(down.is_empty() && stuck.is_empty(), "{down:?} {stuck:?}");
    }

    fn text5() -> egui::Event {
        egui::Event::Text("5".into())
    }

    /// 04 §6.1: a digit that became an F-key stays that F-key until it is let go: Cmd/Ctrl+5,
    /// Cmd/Ctrl let go, 5 repeats, 5 let go → the repeats are F5's (the typed 5 goes) and the
    /// release is F5's.
    #[test]
    fn an_aliased_digit_repeats_as_its_f_key_after_cmd_goes() {
        use egui::Key::{Num5, F5};
        let (cmd, none) = (real_command(), egui::Modifiers::NONE);
        let (win, stuck) = alias_run(vec![
            (vec![digit(Num5, true, cmd)], cmd, None),
            (vec![], none, None),
            (vec![digit(Num5, true, none), text5()], none, None),
            (vec![digit(Num5, false, none)], none, None),
        ]);
        assert_balanced(&win, &stuck, &[(F5, true), (F5, true), (F5, false)]);
        assert!(!win.seen.iter().any(|e| matches!(e, egui::Event::Text(_))));
    }

    /// Focus lost with Cmd/Ctrl+5 down (its release never comes), then a plain 5: the F-key is
    /// let go at the focus loss, and the plain 5 keeps its own release.
    #[test]
    fn losing_focus_lets_go_of_an_aliased_f_key() {
        use egui::Key::{Num5, F5};
        let (cmd, none) = (real_command(), egui::Modifiers::NONE);
        let (win, stuck) = alias_run(vec![
            (vec![digit(Num5, true, cmd)], cmd, None),
            (vec![egui::Event::WindowFocused(false)], none, None),
            (vec![egui::Event::WindowFocused(true)], none, None),
            (vec![digit(Num5, true, none), text5()], none, None),
            (vec![digit(Num5, false, none)], none, None),
        ]);
        assert_balanced(
            &win,
            &stuck,
            &[(F5, true), (F5, false), (Num5, true), (Num5, false)],
        );
    }

    /// A field takes the typing while Cmd/Ctrl+5 is down: the repeats and the release stay
    /// F5's, and no 5 is typed into the field.
    #[test]
    fn an_aliased_digit_stays_balanced_when_a_field_takes_the_typing() {
        use egui::Key::{Num5, F5};
        let (cmd, none) = (real_command(), egui::Modifiers::NONE);
        let (win, stuck) = alias_run(vec![
            (vec![digit(Num5, true, cmd)], cmd, None),
            (vec![digit(Num5, true, none), text5()], none, Some(true)),
            (vec![digit(Num5, false, none)], none, None),
        ]);
        assert_balanced(&win, &stuck, &[(F5, true), (F5, true), (F5, false)]);
        assert_eq!(win.text, "", "nothing typed");
    }

    /// 5 pressed plain, then Cmd/Ctrl held while it repeats: it stays the digit to its release.
    #[test]
    fn a_plain_digit_stays_plain_when_cmd_comes() {
        use egui::Key::Num5;
        let (cmd, none) = (real_command(), egui::Modifiers::NONE);
        let (win, stuck) = alias_run(vec![
            (vec![digit(Num5, true, none), text5()], none, None),
            (vec![digit(Num5, true, cmd)], cmd, None),
            (vec![digit(Num5, false, cmd)], cmd, None),
        ]);
        assert_balanced(&win, &stuck, &[(Num5, true), (Num5, true), (Num5, false)]);
    }

    /// A window of its own with the hex window's keys: what reaches its pass is counted.
    struct Own {
        f8: usize,
        typed: String,
        clicks: usize,
    }

    impl Window for Own {
        fn ui(&mut self, ui: &mut egui::Ui) {
            own_keys(ui, chrome::HEX_KEYS);
            egui::CentralPanel::default().show(ui, |ui| {
                if ui.button("press me").clicked() {
                    self.clicks += 1;
                }
            });
            ui.input_mut(|i| {
                self.f8 += i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::F8);
                for e in &i.events {
                    if let egui::Event::Text(t) = e {
                        self.typed.push_str(t);
                    }
                }
            });
        }

        fn ready(&self) -> bool {
            true
        }
    }

    /// 04 §2 slot 1: F1 in a window of its own shows its key help over it — its row's keys,
    /// described — and the window gets no key, text or click meanwhile; Esc (or F1, F3) closes
    /// the help, and F3 then closes the window. The Help slot and Cmd/Ctrl+1 open it too.
    #[test]
    fn f1_shows_the_key_help_over_a_window_of_its_own() {
        let cmd = browser::keys::command_name();
        let save = format!("Save (as {cmd}+S)");
        let mut drv = testkit::Driver::new(800.0, 400.0);
        let mut win = Own {
            f8: 0,
            typed: String::new(),
            clicks: 0,
        };
        drv.idle(&mut win, 2);
        assert!(drv.find_text("Help   ").is_some(), "1Help in the row");
        let button = drv.find_text("press me").unwrap().center();
        drv.key(&mut win, egui::Key::F1, egui::Modifiers::NONE);
        assert!(help::is_open(&drv.ctx));
        assert!(drv.find_text(&save).is_some(), "{:?}", drv.texts());
        assert!(drv.find_text("Keys").is_some(), "the title");
        drv.key(&mut win, egui::Key::F8, egui::Modifiers::NONE);
        drv.frame(
            &mut win,
            vec![egui::Event::Text("x".into())],
            egui::Modifiers::NONE,
        );
        drv.click(&mut win, button);
        assert_eq!((win.f8, win.typed.as_str(), win.clicks), (0, "", 0));
        drv.key(&mut win, egui::Key::Escape, egui::Modifiers::NONE);
        assert!(!help::is_open(&drv.ctx));
        assert!(drv.find_text(&save).is_none());
        drv.key(&mut win, egui::Key::F8, egui::Modifiers::NONE);
        drv.click(&mut win, button);
        assert_eq!((win.f8, win.clicks), (1, 1), "the window has its keys back");
        // The Help slot, then F3 closes the help, not the window.
        drv.click_text(&mut win, "Help   ");
        assert!(help::is_open(&drv.ctx));
        drv.key(&mut win, egui::Key::F3, egui::Modifiers::NONE);
        assert!(!help::is_open(&drv.ctx) && !drv.closed());
        drv.key(&mut win, egui::Key::Num1, real_command());
        assert!(help::is_open(&drv.ctx), "Cmd/Ctrl+1 = F1");
        drv.key(&mut win, egui::Key::F1, egui::Modifiers::NONE);
        assert!(!help::is_open(&drv.ctx));
        // Shift+F1 and Shift+F3 have no row in a window of its own.
        drv.key(&mut win, egui::Key::F1, egui::Modifiers::SHIFT);
        drv.key(&mut win, egui::Key::F3, egui::Modifiers::SHIFT);
        assert!(!help::is_open(&drv.ctx) && !drv.closed());
        drv.key(&mut win, egui::Key::F3, egui::Modifiers::NONE);
        assert!(drv.closed(), "F3 closes the window");
        // Cmd/Ctrl+W with the help open: the help goes and the close goes on to the window (its
        // unsaved-changes prompt is not hidden under the help).
        let mut drv = testkit::Driver::new(800.0, 400.0);
        drv.as_shell();
        let mut win = Own {
            f8: 0,
            typed: String::new(),
            clicks: 0,
        };
        drv.idle(&mut win, 2);
        drv.key(&mut win, egui::Key::F1, egui::Modifiers::NONE);
        assert!(help::is_open(&drv.ctx));
        drv.key(&mut win, egui::Key::W, real_command());
        assert!(!help::is_open(&drv.ctx), "the help gave way");
        assert!(drv.window_closed, "Cmd/Ctrl+W reached the window");
    }

    #[test]
    fn outcome_action() {
        let mut o = Outcome::default();
        assert_eq!(o.action(), "open");
        o.conflict = true;
        assert_eq!(o.action(), "save");
        o.exported = true;
        assert_eq!(o.action(), "export");
    }

    #[test]
    fn keep_path_keeps_the_extension() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_389_076);
        assert_eq!(
            keep_path(Path::new("/c/box/tmp/shot.png"), "conflict", at, "1a2b"),
            PathBuf::from("/c/box/tmp/shot.conflict-20261111T092436Z-1a2b.png")
        );
        assert_eq!(
            keep_path(Path::new("/c/box/README"), "unsaved", at, "1a2b"),
            PathBuf::from("/c/box/README.unsaved-20261111T092436Z-1a2b")
        );
    }
}
