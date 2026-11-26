//! The file browser and its text viewer (spec §3a), the image window (spec §6), the binary-file
//! dialog and hex window (spec §6a) and the media window (spec §7). This module is the shared
//! shell: where the opened file lives ([`Origin`]),
//! the window runner (one event loop per process, each window `eframe::create_native` on it;
//! wgpu renderer, glow fallback, activation on start, Cmd/Ctrl+Q through the close prompt), the
//! input hook (F-key aliases, F12 the switch), the receipt hooks ([`Hooks`]), the screenshot
//! driver and the windows' feed of the
//! macOS F-key switch (`Feed`: whether a window is in front, acted on by a thread of its own;
//! `.prd/04-file-keys.md` §6.2). Everything that writes
//! (Save, Save copy, Export, Save to…) is a plain function over [`Origin`] or a local path,
//! called by the window's buttons and by the windowless hooks alike.

pub mod binary;
pub mod browser;
pub mod cells;
pub mod chrome;
pub mod fonts;
mod help;
pub mod hex;
pub mod image;
pub mod media;
pub mod playback;
pub mod save_as;
mod select;
pub mod streamed;
pub mod switch_host;
pub mod term;
pub mod term_input;
#[cfg(test)]
pub(crate) mod testkit;
pub mod text;
pub mod web;

pub use select::{fit, Map, Selection};

use crate::config;
use crate::fnkeys::{self, Fkeys, Switch};
use crate::remote::{RemoteFs, Stat};
use anyhow::{anyhow, bail, Context as _, Result};
use eframe::egui;
use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock, PoisonError};
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

    /// The `Origin` of a remote viewer whose connection is not made yet (06 §2, 02 §4.4): it
    /// holds a placeholder that fails every call, so its first use — [`Origin::fs`]'s probe, at
    /// a Save, a Save copy or an Export — connects through `reconnect`, and the viewer works on
    /// that session from then on. Until then nothing is asked of the server: what it can do is
    /// answered as given — yes for a streamed viewer, whose first Save asks before it fetches
    /// or writes anything (06 §2); a preview's own connection's answers for a viewer that read
    /// a copy (the same server).
    pub fn placeholder(
        reconnect: Option<Reconnect>,
        label: String,
        abs: String,
        at: Stat,
        can: Placeholder,
    ) -> Self {
        Self::new(Box::new(can), reconnect, Some(label), abs, at)
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

/// A connection that is not there yet ([`Origin::placeholder`]): every call fails, and what
/// the server can do is answered from here, without a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placeholder {
    pub can_replace: bool,
    pub can_link: bool,
}

impl Placeholder {
    /// A streamed viewer's (06 §2): the answer is taken as yes until the first use connects.
    pub const YES: Placeholder = Placeholder {
        can_replace: true,
        can_link: true,
    };
}

fn not_connected() -> anyhow::Error {
    anyhow!("not connected yet")
}

impl RemoteFs for Placeholder {
    fn stat_opt(&mut self, _: &str) -> Result<Option<Stat>> {
        Err(not_connected())
    }
    fn lstat_opt(&mut self, _: &str) -> Result<Option<Stat>> {
        Err(not_connected())
    }
    fn realpath(&mut self, _: &str) -> Result<String> {
        Err(not_connected())
    }
    fn read_head(&mut self, _: &str, _: usize) -> Result<Vec<u8>> {
        Err(not_connected())
    }
    fn list_dir(&mut self, _: &str) -> Result<Vec<String>> {
        Err(not_connected())
    }
    fn list_entries(&mut self, _: &str) -> Result<Vec<crate::remote::Entry>> {
        Err(not_connected())
    }
    fn read_to(
        &mut self,
        _: &str,
        _: &mut dyn std::io::Write,
        _: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        Err(not_connected())
    }
    fn create_from(
        &mut self,
        _: &Path,
        _: &str,
        _: Option<crate::remote::Like>,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        Err(crate::remote::CreateError::new(
            crate::remote::Created::No,
            not_connected(),
        ))
    }
    fn set_mode(&mut self, _: &str, _: u32) -> Result<()> {
        Err(not_connected())
    }
    fn rename_over(&mut self, _: &str, _: &str) -> Result<()> {
        Err(not_connected())
    }
    fn link_new(&mut self, _: &str, _: &str) -> Result<()> {
        Err(not_connected())
    }
    fn can_link(&self) -> bool {
        self.can_link
    }
    fn can_replace(&self) -> bool {
        self.can_replace
    }
    fn remove(&mut self, _: &str) -> Result<()> {
        Err(not_connected())
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
    /// The window could not show its file after all ([`Window::failure`]): the open failed.
    pub failed: Option<String>,
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
/// hand, whichever comes first (the row that byte is in at the top), `view=1|2|3` for the first
/// viewer it opens (08 §8), `mode=dual|preview` and
/// `bar=NAME` (02 §9); the image, hex and media windows `close=ask`; the media window
/// `clips=START-END[,START-END]` (its edit list, source seconds) and `stereo=1`; the image window
/// `restore=1`; the hex window, and text, an image or a sound read through a stream, `loading=1`;
/// every window `help=1`.
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
    /// The file browser: the first text file it opens (as `at=`) reads its lines in this
    /// encoding (`enc=UTF-16LE`, any case; 08 D34).
    pub enc: Option<crate::encoding::Encoding>,
    /// The file browser: the first viewer it opens switches to view 1, 2 or 3 once it is on
    /// screen, as the plain digit would (`view=2`, 08 §8).
    pub view: Option<u8>,
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
    /// Image window: once the rows of the journals earlier sessions left are on screen, Restore
    /// on the first row is pressed, as a click would — where that row offers it (`restore=1`, a
    /// crash recovery's receipt).
    pub restore: bool,
    /// Hex, text, image and a sound in the media window over a stream: the screenshot is taken at
    /// the first frame drawn with content from the stream — rows still loading, a partial picture,
    /// a sound's first peaks — shot as they are (`loading=1`, 06 §11).
    pub loading: bool,
    /// File browser: the git panel of the folder's repository is pinned to the other side once
    /// detection answers, and has the keyboard (`git=pin`, 02 §9).
    pub git_pin: bool,
    /// File browser, with `git=pin`: the bar on this revision (any git accepts) once its page is
    /// loaded (`select=REV`).
    pub select: Option<String>,
    /// File browser, with `git=pin`: the details pane of the bar's row opens (`details=1`).
    pub details: bool,
    /// File browser, with `git=pin`: the label cursor on the bar's row's Nth label (`label=N`).
    pub label: Option<usize>,
    /// File browser, with `git=pin`: that action's dialog on the object under the bar, once the
    /// rest is done (`dialog=ID`, 03 §7's ids — §7.8's history edits among them — 02 §9): a
    /// confirmation's receipt.
    pub dialog: Option<DialogId>,
}

/// What `SOPEN_STATE dialog=` names (02 §9): an action of 03 §7.1–§7.7
/// (`git::actions::ActionId`), or a history edit of §7.8.1 (`git::rebase::EditId`, S4). No
/// spelling is in both (S4-A's id test), so the action's is tried first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogId {
    Action(crate::git::actions::ActionId),
    Edit(crate::git::rebase::EditId),
}

impl DialogId {
    /// The id spelled `text` as 03 §7 spells it, or `None`.
    pub fn parse(text: &str) -> Option<DialogId> {
        crate::git::actions::ActionId::parse(text)
            .map(DialogId::Action)
            .or_else(|| crate::git::rebase::EditId::parse(text).map(DialogId::Edit))
    }

    /// Its spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            DialogId::Action(id) => id.as_str(),
            DialogId::Edit(id) => id.as_str(),
        }
    }
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
                "enc" => {
                    let enc = crate::encoding::Encoding::from_label(value)
                        .ok_or_else(|| anyhow!("SOPEN_STATE enc: '{value}' is not an encoding"))?;
                    state.enc = Some(enc);
                }
                "view" => match value {
                    "1" | "2" | "3" => state.view = value.parse().ok(),
                    other => bail!("SOPEN_STATE: view must be 1, 2 or 3 (got '{other}')"),
                },
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
                "restore" => match value {
                    "1" => state.restore = true,
                    other => bail!("SOPEN_STATE: restore must be 1 (got '{other}')"),
                },
                "loading" => match value {
                    "1" => state.loading = true,
                    other => bail!("SOPEN_STATE: loading must be 1 (got '{other}')"),
                },
                "git" => match value {
                    "pin" => state.git_pin = true,
                    other => bail!("SOPEN_STATE: git must be 'pin' (got '{other}')"),
                },
                "select" if value.is_empty() => bail!("SOPEN_STATE: select needs a revision"),
                "select" => state.select = Some(value.to_string()),
                "details" => match value {
                    "1" => state.details = true,
                    other => bail!("SOPEN_STATE: details must be 1 (got '{other}')"),
                },
                "label" => {
                    let n = value
                        .parse()
                        .map_err(|_| anyhow!("SOPEN_STATE label must be a whole number"))?;
                    state.label = Some(n);
                }
                "dialog" => {
                    let id = DialogId::parse(value).ok_or_else(|| {
                        anyhow!("SOPEN_STATE dialog: '{value}' is not an action id of 03 §7")
                    })?;
                    state.dialog = Some(id);
                }
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
    /// Open the hex window: a remote file as a stream, nothing fetched (06 §7.5); a local file
    /// in place.
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
    /// `SOPEN_CELLS=<path>` (10 §15): once the window or the terminal has settled, write its
    /// grid there as text ([`term::cells_text`]), exit. A local path: never forwarded.
    pub cells: Option<PathBuf>,
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
    /// `SOPEN_SAVE_HOLD_MS`: a "Save to…" without a window (`SOPEN_CHOICE=save`) holds, at its
    /// first progress report, for this long or until it is cancelled — so a signal reaches it
    /// mid-way (the interrupt's receipt). `<ms>:stalled` holds for the whole time whatever
    /// comes, as a transfer whose remote has stalled does: it never looks at a cancel.
    pub save_hold: Option<Duration>,
    pub save_stalled: bool,
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
    /// `SOPEN_LIST=git`: a folder in a work tree prints its git panel's first page, without a
    /// window (02 §9); any other folder its listing and why there is no panel.
    pub list_git: bool,
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
        let (save_hold, save_stalled) = match std::env::var("SOPEN_SAVE_HOLD_MS") {
            Ok(text) => {
                let (ms, stalled) = match text.strip_suffix(":stalled") {
                    Some(ms) => (ms, true),
                    None => (text.as_str(), false),
                };
                let ms = ms.parse().map_err(|_| {
                    anyhow!("SOPEN_SAVE_HOLD_MS must be <milliseconds> or <milliseconds>:stalled")
                })?;
                (Some(Duration::from_millis(ms)), stalled)
            }
            Err(_) => (None, false),
        };
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
            cells: std::env::var_os("SOPEN_CELLS")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            state,
            autosave: flag("SOPEN_AUTOSAVE"),
            autoexport: flag("SOPEN_AUTOEXPORT"),
            save_delay,
            export_hold,
            save_hold,
            save_stalled,
            window_size,
            choice,
            save_to: std::env::var_os("SOPEN_SAVE_TO")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            list: flag("SOPEN_LIST"),
            list_git: std::env::var("SOPEN_LIST").is_ok_and(|v| v == "git"),
        })
    }

    /// A hook drives this run: user input is ignored and closing never prompts.
    pub fn scripted(&self) -> bool {
        self.screenshot.is_some() || self.cells.is_some() || self.autosave || self.autoexport
    }
}

/// How long a receipt may wait for its window or terminal to settle: `SOPEN_WATCHDOG` seconds,
/// default 30.
pub(crate) fn watchdog_secs() -> u64 {
    std::env::var("SOPEN_WATCHDOG")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
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

    /// The window shows a file that has no place to be saved (`true`): a file at a revision or
    /// a diff, written to the cache's `.revisions` root (03 §6.2, §6.3). Its opener says so
    /// before the first pass. Save, Save copy, Save As and Export are off, the reason on hover
    /// (03 §6.3: `a file at a revision has no place to be saved`); no edit starts, nothing is
    /// kept in a journal and no earlier session's journal is looked for. `false` lifts it. The
    /// default does nothing (S2-E implements it for the viewers that save; the text viewer has
    /// nothing to save).
    fn set_read_only(&mut self, _read_only: bool) {}

    /// The window is on screen (`true`), or held without being drawn (`false`: the file
    /// browser keeps a preview it does not show while a viewer covers the panels). A window
    /// that reads through a stream passes it on (`Source::set_visible`): a hidden one does
    /// not poll its file for changes (06 §5). On screen by default; the default does nothing.
    fn set_shown(&mut self, shown: bool) {
        let _ = shown;
    }

    /// Why the window cannot show its file after all (it found out once it was on screen — a
    /// video none of whose frames decode): it has asked to be closed, and whoever opened it
    /// says this. `None`: nothing failed.
    fn failure(&self) -> Option<String> {
        None
    }

    // The view hooks (08 §8, VM-7): what the file browser's viewer asks of the view on screen
    // and tells it. A window on its own has no views (D24) and is never asked.

    /// The window is a view of the file browser's viewer (`true`): the hex window opens viewing
    /// there (D35), its typing waiting for Enter or Insert. Told before its first pass. The
    /// default does nothing.
    fn set_in_viewer(&mut self, in_viewer: bool) {
        let _ = in_viewer;
    }

    /// What the window declares to the view keys as of this pass (08 §4): a question it shows
    /// (its close prompt, Save As, a confirmation) is [`crate::view_keys::State::Question`]; the
    /// hex window in the viewer is `Hex`. A field with the typing is the host's to see
    /// (`text_edit_focused`). Free by default.
    fn view_state(&mut self) -> crate::view_keys::State {
        crate::view_keys::State::Free
    }

    /// What the window holds (07 §10): unsaved edits, a Save running — what a switch of views
    /// asks or waits for (08 §5, D2). Clean by default.
    fn hold(&mut self) -> crate::views::Hold {
        crate::views::Hold::Clean
    }

    /// How many Saves of this window wrote its file so far — every one, whether the window then
    /// closes, keeps its notes on screen or asks again (08 D25: the hidden views of the file are
    /// stale from then on). 0 for a window that never writes its file (the default).
    fn saves_landed(&self) -> u64 {
        0
    }

    /// A view key's command for the window itself (08 §4, D35): Enter or Insert starts editing
    /// the hex view (`StartEditing`), Esc or F9 ends it (`EndEditing`, `Arm(Editing)`), a
    /// read-only one says it never edits (`ReadOnly`). The default does nothing.
    fn view_key(&mut self, command: crate::view_keys::Command) {
        let _ = command;
    }

    /// A byte view's place (08 §2): the text view's first byte of its top row, the hex view's
    /// cursor. `None`: no byte place (the default).
    fn byte_place(&self) -> Option<u64> {
        None
    }

    /// The place followed the owner from another byte view (D11): the hex view's cursor on that
    /// byte, its row at the top; the text view at the row holding it, as `at=` opens it. The
    /// default does nothing.
    fn go_to_byte(&mut self, at: u64) {
        let _ = at;
    }

    /// The text view as Raw (`true`, D8: highlighting off) or Text. The default does nothing.
    fn set_raw(&mut self, raw: bool) {
        let _ = raw;
    }

    /// The text view's encoding (08 §7.5): the one its lines are read in now, and the default
    /// §7.2 picked (with whether it was guessed) once its first bytes were read. `None`: the
    /// window decodes no text (the default).
    fn encoding(&self) -> Option<(crate::encoding::Encoding, Option<crate::encoding::Pick>)> {
        None
    }

    /// The text view's lines read in `enc` from now on, chosen by the owner (F8's list, 08
    /// §7.5): the index is built again only when the LF unit changes (D22). The default does
    /// nothing.
    fn set_encoding(&mut self, enc: crate::encoding::Encoding) {
        let _ = enc;
    }

    /// Whether the text view wraps its lines (W, F2): what the switch's state carries of it (10
    /// §7.4). `None`: the window has no wrap (the default).
    fn wrapped(&self) -> Option<bool> {
        None
    }

    /// The text view wraps its lines (`true`) or not from the next pass, as W or F2 leaves it —
    /// the switch's state given (10 §7.4). The default does nothing.
    fn set_wrapped(&mut self, wrapped: bool) {
        let _ = wrapped;
    }

    /// Before every pass and, while the window is hidden (minimised, covered), instead of it —
    /// the shell's `logic`: what the window must hear even when it is not drawn (HOST's switched
    /// window, its input's end: 10 §7.3). The default does nothing.
    fn logic(&mut self, ctx: &egui::Context) {
        let _ = ctx;
    }
}

/// The view keys of the file browser's viewer while one is open (08 §8, VM-7): what the view on
/// screen declared as of the last pass, the keys' own state ([`crate::view_keys::ViewKeys`]), and
/// the commands taken since the browser last read them. The shell's input hook
/// ([`FKeyInput`]) takes the view keys from each pass's input before any window sees it — after
/// the Cmd/Ctrl+digit aliases, so Cmd/Ctrl+1…3 stay F1…F3 — and the browser's own scripted keys
/// (`SOPEN_KEYS`) go through the same ([`route_view_keys`]).
#[derive(Debug)]
pub(crate) struct ViewKeySlot {
    pub state: crate::view_keys::State,
    pub keys: crate::view_keys::ViewKeys,
    pub commands: Vec<crate::view_keys::Command>,
}

/// Where the context keeps the viewer's [`ViewKeySlot`].
#[derive(Clone)]
struct ViewKeysAt(Arc<Mutex<ViewKeySlot>>);

fn view_keys_id() -> egui::Id {
    egui::Id::unique("sopen-view-keys")
}

/// The viewer's view keys from the next pass on (`Some`: a viewer opened), or none (`None`: it
/// closed — the list's letters and digits are its own again).
pub(crate) fn set_view_keys(ctx: &egui::Context, slot: Option<Arc<Mutex<ViewKeySlot>>>) {
    ctx.data_mut(|d| match slot {
        Some(slot) => {
            d.insert_temp(view_keys_id(), ViewKeysAt(slot));
        }
        None => {
            d.remove::<ViewKeysAt>(view_keys_id());
        }
    });
}

/// Takes the view keys out of `events` while a viewer is open: each taken press (its key, its
/// typed text, its release) is removed, and what the viewer does about it is kept for the
/// browser's pass ([`ViewKeySlot::commands`]).
pub(crate) fn route_view_keys(ctx: &egui::Context, events: &mut Vec<egui::Event>) {
    let Some(ViewKeysAt(slot)) = ctx.data(|d| d.get_temp::<ViewKeysAt>(view_keys_id())) else {
        return;
    };
    let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
    let state = slot.state;
    let taken = slot.keys.take(state, events);
    slot.commands.extend(taken);
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

/// A window's F-key row at the bottom of `ui`, in cells (10 §4, §6) — F1 (its key help,
/// [`help`]), F3 (closes it, as Cmd/Ctrl+W does) and its own `keys` — and F1 and F3 themselves,
/// without modifiers (04 §3 rule 1); above the row, while the F-key switch did not take effect,
/// a status row in cells saying so ([`fkeys_status`], 04 §6.3: the row's slots and
/// Cmd/Ctrl+digits still work). Each is one row tall, so what is above them stays on the grid.
/// Nothing while it is hosted: the host's row lists the window's keys with its own, its F1 and
/// F3 are the host's, and so is its status row. Call it before the window's other bottom panels
/// (the row is the window's last).
pub fn own_keys(ui: &mut egui::Ui, keys: &[chrome::FKey]) {
    let ctx = ui.ctx().clone();
    if hosted(&ctx) {
        return;
    }
    let row = chrome::merged(&[chrome::HELP_KEY, chrome::EXIT_KEY], keys);
    chrome::key_panel_cells(ui, "sopen-own-keys", &row);
    if let Some(line) = fkeys_status(&ctx) {
        chrome::status_panel_cells(ui, "sopen-own-fkeys", &Status::error(line));
    }
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
/// a text field has the typing (04 §3 rule 4). F12 is the switch (10 §9, T4): taken in every
/// context, a field's included, and said to the pass ([`switch_asked`]). In the terminal
/// backend's context only ([`mark_terminal`]) Alt+1…0 are F1…F10 too and Alt+G is the switch
/// (10 §9, T7, T33): in a window Option+digit and Option+G type characters on macOS. While the
/// key help is open, the pointer is its own after the mode button's ([`help::input_hook`]).
/// Then, while the file browser's viewer is open, its view keys are taken (08 §8,
/// [`route_view_keys`]). Then [`help::begin_pass`] before the window's pass and [`help::end_pass`] after it, and last the
/// page views' after-frame step ([`web::after_pass`], 09 §2.2), so that a view under the help or
/// anything else painted above it is hidden in the same frame.
#[derive(Debug, Default)]
pub(crate) struct FKeyInput {
    /// What each digit key (1…9, 0) is while it is down.
    digits: [Digit; 10],
    /// G is down as Alt+G, the terminal's switch: its repeats and release are taken too.
    g: bool,
    /// The primary button went down on the title row's mode button: its release is taken too.
    mode: bool,
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
        // A switch between backends ended every gesture under way: their releases went to the
        // other backend, or were dropped with the terminal's unread input (10 §7.2).
        if ctx
            .data_mut(|d| d.remove_temp::<GesturesEnded>(gestures_id()))
            .is_some()
        {
            self.mode = false;
        }
        // The focus as the last pass left it: what has the typing now.
        let typing = ctx.text_edit_focused();
        let clicked = chrome::take_clicked(ctx);
        let switch = key_aliases(
            in_terminal(ctx),
            &mut input.events,
            &mut self.digits,
            &mut self.g,
            typing,
            input.focused,
            clicked,
        ) | mode_button(
            &mut input.events,
            &mut self.mode,
            chrome::mode_button_rect(ctx),
        );
        help::input_hook(ctx, &mut input.events);
        // The viewer's view keys (08 §8), once the digits that are F-keys are F-keys.
        route_view_keys(ctx, &mut input.events);
        // This pass's word only: a switch nothing took is not left for a later pass.
        ctx.data_mut(|d| {
            if switch {
                d.insert_temp(switch_id(), SwitchAsked);
            } else {
                d.remove::<SwitchAsked>(switch_id());
            }
        });
    }

    fn on_begin_pass(&mut self, ui: &mut egui::Ui) {
        help::begin_pass(ui.ctx());
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        help::end_pass(ui.ctx());
        web::after_pass(ui.ctx());
    }
}

/// [`key_aliases`] of a window's pass (no terminal: Alt+digits and Alt+G stay keys), as the
/// translator's tests drive the hook.
#[cfg(test)]
fn fkey_events(
    events: &mut Vec<egui::Event>,
    digits: &mut [Digit; 10],
    typing: bool,
    focused: bool,
    clicked: Option<u8>,
) -> bool {
    let mut g = false;
    key_aliases(false, events, digits, &mut g, typing, focused, clicked)
}

/// The key events of a pass (see [`FKeyInput`]); `terminal`: the terminal backend's pass
/// ([`in_terminal`]). A digit pressed with exactly Cmd/Ctrl — in a terminal also exactly Alt
/// (10 T7) — not while a field has the typing, is that F-key pressed without modifiers until it
/// is let go — its repeats are the F-key's (the digit they type is dropped: egui-winit puts a
/// key's typed text right after its key event), its release the F-key's. A digit pressed
/// otherwise stays the digit, its repeats too. Losing the window's focus lets go of every F-key
/// held so (winit may never send those releases; egui clears its own keys then too). The digit
/// is the physical key's first, so on a layout whose digits need Shift, Cmd/Ctrl and the key
/// printed 5 still make F5. `clicked` adds a press and a release of its F-key.
///
/// The switch (10 §9, T4, T33), a field's typing or not, as it types nothing: every F12 event
/// is taken — no window or key-table row has one (`keys.rs`'s `no_duplicate_bindings`) — and
/// its press without modifiers asks for it; in a terminal G pressed with exactly Alt asks for
/// it and is taken until it is let go (`g`: its repeats and release, Alt still held or not; a
/// focus lost forgets it). A repeat asks nothing. Whether this pass asked.
fn key_aliases(
    terminal: bool,
    events: &mut Vec<egui::Event>,
    digits: &mut [Digit; 10],
    g: &mut bool,
    typing: bool,
    focused: bool,
    clicked: Option<u8>,
) -> bool {
    use browser::keys::Mods;
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
    let mut switch = false;
    for event in events.drain(..) {
        let typed_by_alias = std::mem::take(&mut after_alias);
        let (key, physical_key, pressed, repeat, modifiers) = match &event {
            egui::Event::Text(_) if typed_by_alias => continue,
            egui::Event::WindowFocused(false) => {
                let_go(digits, &mut out);
                *g = false;
                out.push(event);
                continue;
            }
            egui::Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                modifiers,
            } => (*key, *physical_key, *pressed, *repeat, *modifiers),
            _ => {
                out.push(event);
                continue;
            }
        };
        if key == F12 {
            switch |= pressed && !repeat && Mods::None.matches(modifiers);
            continue;
        }
        if key == G && terminal {
            let alt_g = match (pressed, *g) {
                (true, false) if Mods::Alt.matches(modifiers) => {
                    switch |= !repeat;
                    true
                }
                (_, held) => held,
            };
            if alt_g {
                *g = pressed;
                continue;
            }
        }
        let Some(n) = physical_key.and_then(number).or_else(|| number(key)) else {
            out.push(event);
            continue;
        };
        let digit = &mut digits[usize::from(n - 1)];
        let alias = Mods::Command.matches(modifiers) || (terminal && Mods::Alt.matches(modifiers));
        let now = match (pressed, *digit) {
            (true, Digit::Up) if !typing && alias => Digit::Alias,
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
        *g = false;
    }
    if let Some(n) = clicked.filter(|n| !typing && chrome::fkey(*n).is_some()) {
        out.extend([fkey(n, true), fkey(n, false)]);
    }
    *events = out;
    switch
}

/// The title row's mode button (10 §7, T4; `chrome::mode_button_rect`, where the last pass drew
/// it): a press of the primary button there is the switch and is taken, with its release, before
/// anything sees them — a modal's backdrop would close its dialog on that click (§9: the button
/// always works). The press's release is the next primary release only: a primary press elsewhere
/// or the focus lost ends the gesture (as a switch does, [`gestures_ended`]), so a release it
/// never got does not take another click's. Whether this pass asked.
fn mode_button(events: &mut Vec<egui::Event>, down: &mut bool, at: Option<egui::Rect>) -> bool {
    let mut asked = false;
    events.retain(|e| match e {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            ..
        } => {
            let on = at.is_some_and(|r| r.contains(*pos));
            *down = on;
            asked |= on;
            !on
        }
        egui::Event::PointerButton {
            button: egui::PointerButton::Primary,
            pressed: false,
            ..
        } if *down => {
            *down = false;
            false
        }
        egui::Event::WindowFocused(false) => {
            *down = false;
            true
        }
        _ => true,
    });
    asked
}

/// The backend switched ([`gestures_ended`]): the next pass's hook forgets the gestures under way.
#[derive(Debug, Clone, Copy, Default)]
struct GesturesEnded;

fn gestures_id() -> egui::Id {
    egui::Id::unique("sopen-gestures-ended")
}

/// `ctx`'s backend gives the screen to the other, or takes it back (10 §7.2): a press under way
/// there — the mode button's, whose release the other backend got or the terminal's dropped
/// input took — is over; the shell's input hook forgets it at the next pass ([`FKeyInput`]).
pub fn gestures_ended(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(gestures_id(), GesturesEnded));
}

/// A pass asked for the switch ([`switch_asked`]).
#[derive(Debug, Clone, Copy)]
struct SwitchAsked;

fn switch_id() -> egui::Id {
    egui::Id::unique("sopen-switch-asked")
}

/// Whether this pass's input asked for the switch to the other backend (10 §7, T4): F12, or
/// Alt+G in a terminal, taken before the pass by the shell's input hook ([`FKeyInput`]); the
/// next pass asks again or not. For the backend that switches. A window started directly has
/// no terminal to go back to while there is no TUI, so nothing reads it there and F12 does
/// nothing (10 §7.2: `[TUI]` "only when its process has a terminal on input and output (F12
/// then takes it), else neither works there").
pub fn switch_asked(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<SwitchAsked>(switch_id()).is_some())
}

/// The terminal backend's context ([`mark_terminal`]).
#[derive(Debug, Clone, Copy)]
struct InTerminal;

fn terminal_id() -> egui::Id {
    egui::Id::unique("sopen-in-terminal")
}

/// Marks `ctx` as the terminal backend's (10 §4: its headless passes run the same input hook):
/// from its next pass on, the hook reads Alt+1…0 as F1…F10 and Alt+G as the switch (10 §9 —
/// "in the TUI only": in a window Option+digit and Option+G type characters on macOS).
pub fn mark_terminal(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(terminal_id(), InTerminal));
}

/// Whether `ctx` is the terminal backend's ([`mark_terminal`]).
pub fn in_terminal(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<InTerminal>(terminal_id()).is_some())
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
        if next.failed.is_some() {
            self.failed = next.failed;
        }
    }
}

struct Shot {
    path: PathBuf,
    title: String,
    ready_frames: u32,
    /// The page views' pictures, asked for once the window is ready (09 §2.2): the read-back is
    /// requested when every one has answered.
    pictures: Option<web::Shots>,
    requested: bool,
    done: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<String, String>>>>,
}

/// A window run with `SOPEN_CELLS` (10 §15): its grid is written as the terminal writes its own
/// ([`term::window_receipt`]) once it has been ready for three passes in a row, as the
/// screenshot waits — with `help=1`, the help open — from the third pass's whole record, so what
/// that pass's end paints above the window (F1's help, `help::end_pass`) is in it; then it
/// closes — after the screenshot, when one is asked for too. Every other run's window passes
/// through as it is.
struct Receipted<W: Window> {
    inner: W,
    dump: Option<Dump>,
}

struct Dump {
    path: PathBuf,
    /// A screenshot is taken too: it closes the window.
    shot: bool,
    /// `help=1`: the key help is open before the passes are counted.
    help: bool,
    ready_frames: u32,
    /// The pass the receipt is of, ended: it is written at the start of the next.
    taken: Option<term::WindowPass>,
    written: bool,
    result: Arc<Mutex<Option<Result<String, String>>>>,
}

impl<W: Window> Window for Receipted<W> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(dump) = self.dump.as_mut() else {
            self.inner.ui(ui);
            return;
        };
        if let Some(pass) = dump.taken.take() {
            let text = term::window_receipt(&ctx, &pass);
            cells::set_recording(&ctx, false);
            *dump.result.lock().unwrap() = Some(match std::fs::write(&dump.path, text) {
                Ok(()) => Ok(format!(
                    "cells {} {}x{}",
                    dump.path.display(),
                    pass.cols,
                    pass.rows
                )),
                Err(e) => Err(format!("cannot write {}: {e}", dump.path.display())),
            });
            dump.written = true;
        }
        if dump.written {
            self.inner.ui(ui);
            if !dump.shot {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        // This pass's cells only (recording on, the record cleared): egui runs the pass again
        // after one it discards.
        cells::set_recording(&ctx, true);
        self.inner.ui(ui);
        if !self.inner.ready() || (dump.help && !help::is_open(&ctx)) {
            dump.ready_frames = 0;
            return;
        }
        ctx.request_repaint();
        dump.ready_frames += 1;
        if dump.ready_frames >= 3 {
            dump.taken = Some(term::keep_window_pass(&ctx));
        }
    }

    fn ready(&self) -> bool {
        self.inner.ready()
    }

    fn close_requested(&mut self) -> bool {
        self.inner.close_requested()
    }

    fn keys(&self) -> &'static [chrome::FKey] {
        self.inner.keys()
    }

    fn logic(&mut self, ctx: &egui::Context) {
        self.inner.logic(ctx);
    }
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
    /// This process's F-key switch, told on every pass whether a window is in front (04 §6.2);
    /// none in a scripted run, with `fkeys = "system"` or off macOS.
    fkeys: Option<Arc<Feed>>,
    /// Set once the shell let a close through ([`closing`]): the window ends as asked. Its
    /// runner reads it after the loop ([`Ended::closed`]).
    closed: Arc<AtomicBool>,
    /// The window's key for its page views (09 §2.1), given with the window's handle at the
    /// first pass ([`web::set_parent`]); its views are torn down before the window goes.
    web: Option<web::WindowKey>,
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
        // A native page view is not in the read-back (09 §2.2): its picture first.
        let pictures = shot.pictures.get_or_insert_with(|| web::snapshots(ctx));
        if !pictures.ready() {
            return;
        }
        let pictures = pictures.take();
        let pixels_per_point = ctx.pixels_per_point();
        shot.requested = true;
        let (path, done, result) = (shot.path.clone(), shot.done.clone(), shot.result.clone());
        let title = shot.title.clone();
        let focused = ctx.input(|i| i.viewport().focused);
        let elapsed = self.started.elapsed();
        let ctx2 = ctx.clone();
        ctx.request_screenshot(move |img| {
            let mut img = Arc::unwrap_or_clone(img);
            web::compose(&mut img, pixels_per_point, &pictures);
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

/// The window's end — eframe drops the shell as its loop ends — lets the switch go now (04
/// §6.2), not when sopen exits, which can be much later (the browser waits for its file
/// operations): at most [`CLOSE_WAIT`], the window already closing.
impl<W: Window> Drop for Shell<W> {
    fn drop(&mut self) {
        // Its page views, if the window's end left any, and its handle (09 §2.1).
        if let Some(key) = self.web {
            web::window_gone(key);
        }
        if let Some(feed) = &self.fkeys {
            if !feed.closed(CLOSE_WAIT) {
                crate::debug!("F-key switch: not let go within {CLOSE_WAIT:?} of the window's end");
            }
        }
    }
}

impl<W: Window> eframe::App for Shell<W> {
    /// The skin's window body where nothing is painted (eframe's default is a dark grey).
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    /// Before every `ui` and, while the window is hidden (minimised, covered), instead of it:
    /// the F-key switch hears whether a window of this process is in front, and the window its
    /// own ([`Window::logic`]).
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(feed) = &self.fkeys {
            feed.pass(ctx);
        }
        self.inner.logic(ctx);
    }

    /// The window is about to go — eframe runs this before it destroys the window and its
    /// renderer (`save_and_destroy`): its page views first (09 §2.1, §2.5).
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(key) = self.web {
            web::teardown_window(key);
        }
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

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.web.is_none() {
            self.web = Some(web::set_parent(&ctx, web_parent(frame)));
        }
        if self.frames == 0 {
            ctx.options_mut(|o| o.zoom_with_keyboard = false);
            // 10 TW-1's time: from the shell made to its window's first pass.
            crate::debug!(
                "window: first frame after {} ms",
                self.started.elapsed().as_millis()
            );
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
        if closing(&ctx, &mut self.inner, self.scripted) {
            self.closed.store(true, Ordering::SeqCst);
            // Before the window goes: every page view in it (09 §2.1). The window's pass below
            // may still draw them; they do nothing more.
            web::teardown(&ctx);
        }
        self.inner.ui(ui);
        if self.press == Press::Waiting && self.inner.ready() {
            self.press = Press::Now;
            ctx.request_repaint();
        }
        self.screenshot_tick(&ctx);
    }
}

/// The window's handle for its page views (09 §2.1): winit's NSView on macOS, else (and when
/// eframe has none to give, as its test frame) the plain window.
fn web_parent(frame: &eframe::Frame) -> web::Parent {
    use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    match frame.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::AppKit(appkit)) => web::Parent::AppKit(appkit.ns_view),
        _ => web::Parent::Window,
    }
}

/// The shell's part of closing, before the window's pass: Cmd/Ctrl+Q and +W ask to close, as
/// the window's close button does; a close asked for (by those, or by the window itself —
/// F3, its prompt answered) goes to the window first, and its refusal cancels it (it shows
/// its own prompt). A scripted run never refuses. The test kit runs the same function. Whether
/// a close went through in this pass (asked for, not cancelled): the window ends.
pub(crate) fn closing<W: Window + ?Sized>(
    ctx: &egui::Context,
    inner: &mut W,
    scripted: bool,
) -> bool {
    if command_key(ctx, egui::Key::Q, false) || command_key(ctx, egui::Key::W, false) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    let asked = ctx.input(|i| i.viewport().close_requested());
    if asked && !scripted && !inner.close_requested() {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        return false;
    }
    asked
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

type EventLoop = winit::event_loop::EventLoop<eframe::UserEvent>;

thread_local! {
    /// This process's one event loop (10 §4, T34; 04 §6.4.3), built by its first window with
    /// [`event_loop_hook`] applied — `create_native` never runs `NativeOptions`'s builder hook —
    /// and kept for every window after it and for the glow retry: winit refuses a second loop
    /// in a process (`winit-0.30.13/src/event_loop.rs:119`), and the windows all run on the main
    /// thread. A build that failed is kept as its reason: winit counts the loop as made before
    /// the platform's part runs, so trying again could only say "recreation attempt".
    static EVENT_LOOP: RefCell<Option<Result<EventLoop, String>>> = const { RefCell::new(None) };
}

/// Runs `run` on this process's event loop ([`EVENT_LOOP`]), building it first; the reason
/// when it cannot be built, or a window already runs on it.
fn on_the_event_loop<R>(run: impl FnOnce(&mut EventLoop) -> R) -> Result<R, String> {
    EVENT_LOOP.with(|cell| {
        let mut cell = cell
            .try_borrow_mut()
            .map_err(|_| "a window already runs on this thread's event loop".to_string())?;
        let made = cell.get_or_insert_with(|| {
            let mut builder = EventLoop::with_user_event();
            if let Some(hook) = event_loop_hook() {
                hook(&mut builder);
            }
            builder.build().map_err(|e| format!("no event loop: {e}"))
        });
        match made {
            Ok(event_loop) => Ok(run(event_loop)),
            Err(why) => Err(why.clone()),
        }
    })
}

/// One run of the app in `slot` with `options` on this process's event loop: eframe's handler
/// (`eframe::create_native`), run by `run_app_on_demand` until the window closes, then dropped
/// — the app with it, if it started. `Ok` with the error eframe logged meanwhile, if any
/// ([`hearing_eframe`]): a renderer that does not start leaves the app in its slot and the
/// loop returns `Ok` (see [`start`]). `Err` only when the loop itself failed.
fn attempt<A: eframe::App + 'static>(
    slot: &RefCell<Option<A>>,
    options: eframe::NativeOptions,
) -> Result<Option<String>, String> {
    use winit::platform::run_on_demand::EventLoopExtRunOnDemand as _;
    let (ran, error) = hearing_eframe(|| {
        on_the_event_loop(|event_loop| {
            let mut app = eframe::create_native(
                "sopen",
                options,
                Box::new(|cc| {
                    // The look, before the first frame; `⌘` in one face (fonts.rs).
                    chrome::install(&cc.egui_ctx);
                    fonts::pin_symbols(&cc.egui_ctx);
                    let app = slot
                        .borrow_mut()
                        .take()
                        .ok_or("the window was already started once")?;
                    Ok(Box::new(app) as Box<dyn eframe::App>)
                }),
                event_loop,
            );
            let ran = event_loop.run_app_on_demand(&mut app);
            drop(app);
            ran.map_err(|e| format!("the event loop failed: {e}"))
        })
    });
    ran?.map(|()| error)
}

/// eframe's error-level `log` records, kept for the attempt running on the thread that logged
/// them ([`hearing_eframe`]); everything else is dropped. sopen installs no other logger.
struct EframeErrors;

thread_local! {
    /// The last eframe error heard by the attempt running on this thread; `None` outside one.
    static HEARD: RefCell<Option<Option<String>>> = const { RefCell::new(None) };
}

/// The words eframe puts before the error that ended its loop (`native/run.rs:178`).
const EFRAME_EXITING: &str = "Exiting because of error: ";

impl log::Log for EframeErrors {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() == log::Level::Error && metadata.target().starts_with("eframe")
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let text = record.args().to_string();
        let text = text
            .strip_prefix(EFRAME_EXITING)
            .unwrap_or(&text)
            .to_string();
        let _ = HEARD.try_with(|heard| {
            if let Ok(mut heard) = heard.try_borrow_mut() {
                if let Some(last) = heard.as_mut() {
                    *last = Some(text);
                }
            }
        });
    }

    fn flush(&self) {}
}

/// Runs `run`, keeping the last error eframe logs on this thread meanwhile (10 T31): eframe
/// returns no `Result` from `create_native` and keeps a renderer's error to
/// `log::error!("Exiting because of error: {err}")` (`native/run.rs:178`), so that record —
/// [`EFRAME_EXITING`] taken off — is the reason a window did not start, or ended. The logger
/// is installed once per process; if another one already is, nothing is heard.
fn hearing_eframe<R>(run: impl FnOnce() -> R) -> (R, Option<String>) {
    static EFRAME: EframeErrors = EframeErrors;
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        if log::set_logger(&EFRAME).is_ok() {
            log::set_max_level(log::LevelFilter::Error);
        }
    });
    HEARD.with(|heard| *heard.borrow_mut() = Some(None));
    let out = run();
    let heard = HEARD.with(|heard| heard.borrow_mut().take().flatten());
    (out, heard)
}

/// A window that did not open (10 T31, 04 §6.4.3): no renderer started it, or the event loop
/// could not run — nothing of the window ran. [`run_window`]'s error carries it, so a caller
/// that goes on without the window can say why (`reason`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotStarted {
    pub reason: String,
}

impl std::fmt::Display for NotStarted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot open a window: {}", self.reason)
    }
}

impl std::error::Error for NotStarted {}

/// The renderers `choice` tries, in order.
fn renderers(choice: RendererChoice) -> &'static [eframe::Renderer] {
    use eframe::Renderer::{Glow, Wgpu};
    match choice {
        RendererChoice::Auto => &[Wgpu, Glow],
        RendererChoice::Wgpu => &[Wgpu],
        RendererChoice::Glow => &[Glow],
    }
}

fn renderer_name(renderer: eframe::Renderer) -> &'static str {
    match renderer {
        eframe::Renderer::Wgpu => "wgpu",
        eframe::Renderer::Glow => "glow",
    }
}

/// How one [`attempt`]'s loop ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Ended {
    /// The error eframe logged in it ([`hearing_eframe`]).
    error: Option<String>,
    /// The shell let a close through ([`closing`]): the window ended as asked.
    closed: bool,
}

/// The line when wgpu did not start and glow is tried, with wgpu's error when eframe said it.
fn retry_line(error: Option<&str>) -> String {
    let why = error.map(|e| format!(" ({e})")).unwrap_or_default();
    format!("sopen: the wgpu renderer did not start{why}; using OpenGL (glow)")
}

/// Starts the app in `slot` with the first of `choice`'s renderers that takes it; `run` is one
/// renderer's [`attempt`]. `create_native` returns no `Result`: a renderer's error stays
/// inside eframe (`native/run.rs:96, 177-191, 541`) and the loop returns `Ok`, so a renderer
/// started exactly when the app has left its slot (04 §6.4.3) and while it is still there the
/// next one tries: glow after wgpu, with [`retry_line`]. None took it → [`NotStarted`], naming
/// each renderer's error. The loop failing is no renderer's failure and ends it at once:
/// [`NotStarted`] with its reason, or — under a window that started — an error. A window that
/// started ends as a close, unless eframe's error ended it without the shell letting a close
/// through: then an error names it (as `run_native`'s `Err` did, exit 1).
fn start<A>(
    choice: RendererChoice,
    slot: &RefCell<Option<A>>,
    mut run: impl FnMut(eframe::Renderer) -> Result<Ended, String>,
) -> Result<()> {
    let mut failed: Vec<(eframe::Renderer, Option<String>)> = Vec::new();
    for &renderer in renderers(choice) {
        if let Some((_, error)) = failed.last() {
            eprintln!("{}", retry_line(error.as_deref()));
        }
        let ran = run(renderer);
        // The app creator runs only once the window and its renderer are up: wgpu sets its
        // surface (`set_window`, eframe-0.37.0 `native/wgpu_integration.rs:272`) before it
        // (`:325-345`); glow makes its window and GL context
        // (`native/glow_integration.rs:275-279`) before it (`:431`). An error before that
        // leaves the app here. Re-check both on an eframe upgrade.
        let started = slot.borrow().is_none();
        match ran {
            Ok(Ended {
                error: Some(e),
                closed: false,
            }) if started => bail!("the window ended on an error: {e}"),
            Ok(_) if started => return Ok(()),
            Ok(Ended { error, .. }) => failed.push((renderer, error)),
            Err(why) if started => bail!("the window's event loop failed: {why}"),
            Err(reason) => return Err(NotStarted { reason }.into()),
        }
    }
    let reason = match (choice, failed.as_slice()) {
        (RendererChoice::Auto, failed) => {
            let why: Vec<String> = failed
                .iter()
                .filter_map(|(r, e)| Some(format!("{}: {}", renderer_name(*r), e.as_ref()?)))
                .collect();
            if why.is_empty() {
                "neither wgpu nor glow started".to_string()
            } else {
                format!("neither wgpu nor glow started ({})", why.join("; "))
            }
        }
        (_, [(renderer, error)]) => {
            let name = renderer_name(*renderer);
            let why = error.as_ref().map(|e| format!(": {e}")).unwrap_or_default();
            format!("the {name} renderer did not start (SOPEN_RENDERER={name}){why}")
        }
        (_, _) => unreachable!("a forced renderer is tried alone"),
    };
    Err(NotStarted { reason }.into())
}

/// sopen gives up (`why`) and exits 1 without unwinding (the screenshot hook's watchdog: a
/// receipt hook). Nothing is stopped here: on Linux an ffprobe or ffmpeg a preview runs dies
/// with sopen (the kernel ends it: `PR_SET_PDEATHSIG`); on macOS the one that runs may be left
/// to end by itself — it is bounded by its input.
pub(crate) fn give_up(why: &str) -> ! {
    eprintln!("sopen: {why}; giving up");
    // `--state-stdin`: the line reaches the session before the exit (a no-op otherwise).
    switch_host::end_of_run();
    std::process::exit(1);
}

/// The receipt hooks' watchdog: `secs` from now sopen gives up (exit 1, [`give_up`]) unless
/// `done` is set by then.
pub(crate) fn arm_watchdog(secs: u64, hook: &'static str, done: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(secs));
        if !done.load(Ordering::SeqCst) {
            give_up(&format!("the {hook} hook stalled for {secs} s"));
        }
    });
}

/// Runs `window` until it closes. With the screenshot or the cells hook a watchdog ends a
/// stalled run after `SOPEN_WATCHDOG` seconds (default 30, [`watchdog_secs`]).
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
    // The installed fonts are enumerated while the window starts: its first frame pins `⌘`'s
    // face (`fonts::pin_symbols`).
    fonts::prefetch();
    let shot_done = Arc::new(AtomicBool::new(false));
    // The watchdog's word: set once the window is gone, however it went (a window that closed
    // or did not start stalls nothing), so it never ends a process that goes on — the terminal
    // backend's, which still holds its terminal — and never before (a receipt written, then a
    // window that does not close, is still a stall).
    let disarm = Arc::new(AtomicBool::new(false));
    let shot_result: Arc<Mutex<Option<Result<String, String>>>> = Arc::default();
    let dump_result: Arc<Mutex<Option<Result<String, String>>>> = Arc::default();
    let closed = Arc::new(AtomicBool::new(false));
    if hooks.screenshot.is_some() || hooks.cells.is_some() {
        let hook = if hooks.screenshot.is_some() {
            "screenshot"
        } else {
            "cells"
        };
        arm_watchdog(watchdog_secs(), hook, disarm.clone());
    }
    let dump = hooks.cells.clone().map(|path| Dump {
        path,
        shot: hooks.screenshot.is_some(),
        help: hooks.state.help,
        ready_frames: 0,
        taken: None,
        written: false,
        result: dump_result.clone(),
    });
    let shell = Shell {
        inner: Receipted {
            inner: window,
            dump,
        },
        scripted: hooks.scripted(),
        shot: hooks.screenshot.clone().map(|path| Shot {
            path,
            title: title.to_string(),
            ready_frames: 0,
            pictures: None,
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
        fkeys: this_runs_fkeys(),
        closed: closed.clone(),
        web: None,
    };
    // The app moves into whichever renderer starts. A renderer that fails before creating the
    // app leaves it here for the next one.
    let slot = RefCell::new(Some(shell));
    let viewport = egui::ViewportBuilder::default()
        .with_title(title)
        .with_app_id("sopen")
        .with_inner_size(hooks.window_size.unwrap_or(size))
        .with_min_inner_size(
            hooks
                .window_size
                .map_or(min, |[w, h]| [w.min(min[0]), h.min(min[1])]),
        )
        .with_active(true);
    let started = start(choice, &slot, |renderer| {
        let options = eframe::NativeOptions {
            viewport: viewport.clone(),
            renderer,
            centered: true,
            ..Default::default()
        };
        attempt(&slot, options).map(|error| Ended {
            error,
            closed: closed.load(Ordering::SeqCst),
        })
    });
    disarm.store(true, Ordering::SeqCst);
    started?;
    if hooks.cells.is_some() {
        match dump_result.lock().unwrap().take() {
            Some(Ok(line)) => println!("{line}"),
            Some(Err(e)) => bail!("{e}"),
            None => bail!("the window closed before its cells were written"),
        }
    }
    if hooks.screenshot.is_some() {
        match shot_result.lock().unwrap().take() {
            Some(Ok(line)) => println!("{line}"),
            Some(Err(e)) => bail!("{e}"),
            None => bail!("the window closed before the screenshot was taken"),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The macOS F-key switch, fed by the windows (04 §6.2).

/// The windows' side of this process's F-key switch ([`fnkeys`]): one per process, as the
/// holder is the process. Every pass of a window ([`Shell`]'s `logic`, which eframe runs while
/// the window is hidden too) says whether a window of this process is in front ([`in_front`]),
/// and a thread of the switch's own acts on it ([`serve`]). Building the switch (the start's
/// clean-up), starting its watchdog, acquiring and releasing can all wait — for the lock a
/// stopped sopen holds, for the watchdog to say it is ready — so none of it runs on the
/// window's thread, which only sends (a send never waits) and reads the status line the
/// switch's thread leaves.
pub(crate) struct Feed {
    told: mpsc::Sender<Told>,
    said: Arc<Mutex<Said>>,
    /// The last word sent: [`UNSAID`], [`NOT_FRONT`] or [`FRONT`].
    sent: AtomicU8,
}

const UNSAID: u8 = 0;
const NOT_FRONT: u8 = 1;
const FRONT: u8 = 2;

/// What the windows tell the switch's thread.
enum Told {
    /// Whether a window of this process is in front now.
    Front(bool),
    /// The window closed: let go now, without [`Timing::hold`], and say when that is done.
    Closed(mpsc::Sender<()>),
    /// What the system said about the Mac, and — for [`Mac::Sleeps`] — where to say that the
    /// switch has acted on it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Mac(Mac, Option<mpsc::Sender<()>>),
}

/// What the system says about the Mac (04 §6.2), in the order it says it: NSWorkspace's
/// notifications on macOS ([`observe_the_mac`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum Mac {
    /// It is about to sleep (`NSWorkspaceWillSleepNotification`): let go.
    Sleeps,
    /// It woke (`NSWorkspaceDidWakeNotification`): taken again if a window is in front.
    Woke,
    /// This login session became active (`true`, `NSWorkspaceSessionDidBecomeActiveNotification`:
    /// taken again if a window is in front) or was switched away from (`false`,
    /// `NSWorkspaceSessionDidResignActiveNotification`: let go).
    Session(bool),
}

/// What the switch's thread leaves for the windows.
#[derive(Default)]
struct Said {
    /// [`Fkeys::status`], as of its last change.
    status: Option<String>,
    /// The window to repaint when it changes.
    ctx: Option<egui::Context>,
}

/// How the switch's thread paces itself.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timing {
    /// How long no window may be in front before the switch is let go: focus moving from one
    /// window of the process to another can pass through a pass in which neither has it.
    pub hold: Duration,
    /// How often the thread wakes while a window is in front, to notice that the Mac slept.
    pub tick: Duration,
    /// The wall clock ran this much further than the monotonic one between two looks: the Mac
    /// slept in between (Rust's `Instant` — `CLOCK_UPTIME_RAW` on macOS, `CLOCK_MONOTONIC` on
    /// Linux — does not count a sleep; the wall clock does).
    pub slept: Duration,
}

impl Timing {
    pub const DEFAULT: Timing = Timing {
        hold: Duration::from_millis(250),
        tick: Duration::from_secs(1),
        slept: Duration::from_secs(5),
    };
}

/// How long a window's end waits for the switch to be let go: past it (a stopped sopen holds
/// the lock) the thread goes on trying, and once sopen has exited its watchdog restores.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// How long the Mac's will-sleep notification waits for the switch to be let go: the Mac may
/// suspend once its observers return. The one wait on AppKit's main thread while windows run.
const SLEEP_WAIT: Duration = Duration::from_secs(1);

/// Where the system's word about the Mac goes ([`observe_the_mac`]'s observers): the switch's
/// thread, through the windows' own channel, so it is acted on in the order said.
#[derive(Clone)]
pub(crate) struct MacEvents {
    told: mpsc::Sender<Told>,
    sleep_wait: Duration,
}

impl MacEvents {
    /// Tells the switch's thread `event`. A sleep then waits until the switch has acted on it —
    /// let go, or tried to — at most `sleep_wait` ([`SLEEP_WAIT`]); every other event only
    /// sends. Whether the switch acted in time (`true` too when there is no switch any more).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn tell(&self, event: Mac) -> bool {
        if event != Mac::Sleeps {
            let _ = self.told.send(Told::Mac(event, None));
            return true;
        }
        let (done, acted) = mpsc::channel();
        if self.told.send(Told::Mac(event, Some(done))).is_err() {
            return true;
        }
        !matches!(
            acted.recv_timeout(self.sleep_wait),
            Err(mpsc::RecvTimeoutError::Timeout)
        )
    }
}

impl Feed {
    /// The feed of a run (04 §6.2): none in a run a hook drives ([`fnkeys::scripted_run`]; `var`
    /// reads the environment) — no thread, and neither `read` nor `build` is called. Else the
    /// feed's own thread reads the config (`read`) and, unless it says `fkeys = "system"` or
    /// cannot be read (the run goes on: only a run the config decides stops at it, 01 §4a; the
    /// switch is left alone, as the config may say "system"), makes the switch (`build`) and
    /// then hands `built` where the system's word goes (the NSWorkspace observers). The calling
    /// window's thread only starts the thread.
    fn for_run<S: Switch + 'static>(
        var: impl Fn(&str) -> Option<OsString>,
        read: impl FnOnce() -> Result<config::Config> + Send + 'static,
        build: impl FnOnce(&config::Config) -> Option<Fkeys<S>> + Send + 'static,
        built: impl FnOnce(MacEvents) + Send + 'static,
    ) -> Option<Feed> {
        if fnkeys::scripted_run(var) {
            return None;
        }
        let make = move || match read() {
            Ok(config) if config.fkeys() == config::Fkeys::System => None,
            Ok(config) => build(&config),
            Err(e) => {
                crate::debug!("F-key switch: left alone: {e:#}");
                None
            }
        };
        Some(Feed::start(make, built, Timing::DEFAULT, SystemTime::now))
    }

    /// A feed whose thread builds its switch with `build` (none: the thread ends), then hands
    /// `built` where the system's word goes, paced by `timing`, reading the wall clock with
    /// `wall`.
    fn start<S: Switch + 'static>(
        build: impl FnOnce() -> Option<Fkeys<S>> + Send + 'static,
        built: impl FnOnce(MacEvents) + Send + 'static,
        timing: Timing,
        wall: impl FnMut() -> SystemTime + Send + 'static,
    ) -> Feed {
        let (told, heard) = mpsc::channel();
        let said = Arc::new(Mutex::new(Said::default()));
        let shared = said.clone();
        let events = MacEvents {
            told: told.clone(),
            sleep_wait: SLEEP_WAIT,
        };
        let spawned = std::thread::Builder::new()
            .name("sopen-fkeys".into())
            .spawn(move || {
                if let Some(switch) = build() {
                    built(events);
                    serve(switch, &heard, &shared, timing, wall);
                }
            });
        if let Err(e) = spawned {
            crate::debug!("F-key switch: no thread for it: {e}");
        }
        Feed {
            told,
            said,
            sent: AtomicU8::new(UNSAID),
        }
    }

    /// A pass of a window: whether a window is in front goes to the switch's thread when that
    /// changed, and the thread's status line goes where the window's rows read it
    /// ([`fkeys_status`]).
    pub(crate) fn pass(&self, ctx: &egui::Context) {
        let front = ctx.input(|i| in_front(&i.raw));
        let word = if front { FRONT } else { NOT_FRONT };
        if self.sent.swap(word, Ordering::SeqCst) != word {
            let _ = self.told.send(Told::Front(front));
        }
        let status = {
            let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
            said.ctx = Some(ctx.clone());
            said.status.clone()
        };
        ctx.data_mut(|d| match status {
            Some(line) => {
                d.insert_temp(fkeys_line_id(), FkeysLine(line));
            }
            None => d.remove::<FkeysLine>(fkeys_line_id()),
        });
    }

    /// The window closed: the switch is let go now, waiting at most `wait` for it. Whether it
    /// was, or there is nothing to let go (no switch: its thread has ended).
    pub(crate) fn closed(&self, wait: Duration) -> bool {
        self.sent.store(UNSAID, Ordering::SeqCst);
        let (done, ended) = mpsc::channel();
        if self.told.send(Told::Closed(done)).is_err() {
            return true;
        }
        match ended.recv_timeout(wait) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => true,
            Err(mpsc::RecvTimeoutError::Timeout) => false,
        }
    }
}

/// Whether a window of this process is in front: focused, and neither minimised nor fully
/// covered — over every viewport eframe reports (`RawInput::viewports`), one that does not say
/// whether it is focused taking the pass's own focus (`RawInput::focused`). eframe refreshes
/// both for `App::logic` while the window is hidden too.
fn in_front(raw: &egui::RawInput) -> bool {
    let this = raw.viewport_id;
    let front = |id: egui::ViewportId, v: &egui::ViewportInfo| {
        v.focused.unwrap_or(id == this && raw.focused)
            && v.minimized != Some(true)
            && v.occluded != Some(true)
    };
    raw.viewports.iter().any(|(id, v)| front(*id, v))
        || (raw.focused && !raw.viewports.contains_key(&this))
}

/// The switch's thread: acts on what the windows and the system say until the feed is gone,
/// then drops the switch (which lets go). A window in front acquires at once; none in front
/// releases once that has lasted [`Timing::hold`], or at once when the window closed. The
/// system's word ([`Mac`]) is acted on in the order it came: a sleep or a session switched away
/// lets go (a sleep then says it is done), a wake or the session's return takes it again if a
/// window is in front by the windows' word heard before it — told to the switch first, so it
/// never resumes on a focus it held before (`Fkeys::sleep`, `wake`, `session`). A wake whose
/// notification did not come is seen
/// afterwards — the wall clock ran ahead of the monotonic one ([`Timing::slept`]), looked at on
/// every message and every [`Timing::tick`] while a window is in front — and the switch is then
/// let go and taken again: acquired anew, read back, if a window is still in front. A release
/// that failed is tried again when it is due ([`Fkeys::next_retry`]), here, without a repaint.
fn serve<S: Switch>(
    mut switch: Fkeys<S>,
    heard: &mpsc::Receiver<Told>,
    said: &Mutex<Said>,
    timing: Timing,
    mut wall: impl FnMut() -> SystemTime,
) {
    // The windows' last word, and what the switch was last told.
    let (mut front, mut held) = (false, false);
    // What the system said last: the Mac sleeps, the session is switched away.
    let (mut asleep, mut away_session) = (false, false);
    // Since when no window has been in front while the switch is still told one is.
    let mut away: Option<Instant> = None;
    let mut looked = (wall(), Instant::now());
    loop {
        let now = Instant::now();
        let mut wait: Option<Duration> = None;
        let mut sooner = |d: Duration| wait = Some(wait.map_or(d, |w| w.min(d)));
        if held {
            sooner(timing.tick);
        }
        if let Some(since) = away {
            sooner((since + timing.hold).saturating_duration_since(now));
        }
        // A release that failed runs again only while the switch is not wanted (`Fkeys::update`):
        // while it is, its time passing must not wake the thread.
        let wanted = held && !asleep && !away_session;
        if let Some(at) = switch.next_retry().filter(|_| !wanted) {
            sooner(at.saturating_duration_since(now));
        }
        let got = match wait {
            Some(wait) => heard.recv_timeout(wait),
            None => heard
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        let mut words = Vec::new();
        match got {
            Ok(first) => {
                words.push(first);
                words.extend(heard.try_iter());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let mut closed = Vec::new();
        // In the order said, the system's word each at once, before the focus this turn
        // settles on. A resume first tells the switch the windows' word as it stands, without
        // the hold: `Fkeys` resumes on the focus it was last told.
        for word in words {
            match word {
                Told::Front(is) => front = is,
                Told::Closed(done) => {
                    front = false;
                    closed.push(done);
                }
                Told::Mac(Mac::Sleeps, done) => {
                    asleep = true;
                    switch.sleep();
                    if let Some(done) = done {
                        let _ = done.send(());
                    }
                }
                Told::Mac(Mac::Woke, _) => {
                    asleep = false;
                    (held, away) = (front, None);
                    switch.focus(held);
                    switch.wake();
                }
                Told::Mac(Mac::Session(active), _) => {
                    away_session = !active;
                    if active {
                        (held, away) = (front, None);
                        switch.focus(held);
                    }
                    switch.session(active);
                }
            }
        }
        let then = std::mem::replace(&mut looked, (wall(), Instant::now()));
        let walked = looked.0.duration_since(then.0).unwrap_or_default();
        if walked > looked.1.duration_since(then.1) + timing.slept {
            switch.sleep();
            asleep = false;
            (held, away) = (front, None);
            switch.focus(held);
            switch.wake();
        }
        if front {
            (held, away) = (true, None);
        } else if held {
            let since = *away.get_or_insert_with(Instant::now);
            if !closed.is_empty() || since.elapsed() >= timing.hold {
                (held, away) = (false, None);
            }
        }
        // Acts only on a change; while not wanted it also runs a release that is due.
        switch.focus(held);
        publish(said, switch.status());
        for done in closed {
            let _ = done.send(());
        }
    }
}

/// Leaves `status` for the windows, and repaints the window when it changed.
fn publish(said: &Mutex<Said>, status: Option<&str>) {
    let ctx = {
        let mut said = said.lock().unwrap_or_else(PoisonError::into_inner);
        if said.status.as_deref() == status {
            return;
        }
        said.status = status.map(str::to_string);
        said.ctx.clone()
    };
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
}

/// The switch's status line in a window's context, for its rows ([`fkeys_status`]).
#[derive(Debug, Clone)]
struct FkeysLine(String);

fn fkeys_line_id() -> egui::Id {
    egui::Id::unique("sopen-fkeys-line")
}

/// The status row's line while the F-key switch did not take effect (04 §6.3,
/// [`Fkeys::status`]), as this window's shell left it for the pass; `None` when it did, or
/// there is no switch.
pub fn fkeys_status(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<FkeysLine>(fkeys_line_id()))
        .map(|line| line.0)
}

/// The state folder of this run's F-key switch, set by the binary before any window
/// ([`fkeys_for_run`]).
static FKEYS_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
/// This process's feed, made with its first window ([`this_runs_fkeys`]).
static FKEYS: OnceLock<Option<Arc<Feed>>> = OnceLock::new();

/// This run may open windows: they feed the F-key switch, whose files go in `state_dir` (04
/// §6.3). Nothing is read or started before the first window.
pub fn fkeys_for_run(state_dir: PathBuf) {
    *FKEYS_DIR.lock().unwrap_or_else(PoisonError::into_inner) = Some(state_dir);
}

/// This process's feed, made at its first window ([`Feed::for_run`]): the environment is looked
/// at there, the config read and the switch ([`fnkeys::for_this_run`]'s) built on the feed's own
/// thread; `None` without [`fkeys_for_run`] (the tests never call it) or in a scripted run.
fn this_runs_fkeys() -> Option<Arc<Feed>> {
    FKEYS
        .get_or_init(|| {
            let dir = FKEYS_DIR
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()?;
            Feed::for_run(
                |name| std::env::var_os(name),
                || config::load(&config::path()?),
                move |config| fnkeys::for_this_run(config, &dir),
                // Once the switch is built (not "system", not a scripted run): once per process.
                |events| {
                    #[cfg(target_os = "macos")]
                    observe_the_mac(move |event| {
                        events.tell(event);
                    });
                    #[cfg(not(target_os = "macos"))]
                    let _ = events;
                },
            )
            .map(Arc::new)
        })
        .clone()
}

/// NSWorkspace's notifications of sleep, wake and session switch (04 §6.2), told to `tell` for
/// the rest of the process: `NSWorkspaceWillSleepNotification`, `…DidWakeNotification`,
/// `…SessionDidResignActiveNotification`, `…SessionDidBecomeActiveNotification`, observed on
/// `NSWorkspace.sharedWorkspace.notificationCenter`; registered from the switch's thread once it
/// is built. Each block calls `tell` ([`MacEvents::tell`]: a send, and for a sleep a wait of at
/// most [`SLEEP_WAIT`] for the release): it runs on the thread that posts the notification (no
/// queue is given), AppKit's main thread, which never takes the switch's lock.
#[cfg(target_os = "macos")]
fn observe_the_mac(tell: impl Fn(Mac) + Send + Sync + 'static) {
    use objc2_app_kit::{
        NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceSessionDidBecomeActiveNotification,
        NSWorkspaceSessionDidResignActiveNotification, NSWorkspaceWillSleepNotification,
    };
    use objc2_foundation::NSNotification;
    use std::ptr::NonNull;
    let tell = Arc::new(tell);
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    // SAFETY: AppKit's constant notification names, valid for the life of the process.
    let names = unsafe {
        [
            (NSWorkspaceWillSleepNotification, Mac::Sleeps),
            (NSWorkspaceDidWakeNotification, Mac::Woke),
            (
                NSWorkspaceSessionDidResignActiveNotification,
                Mac::Session(false),
            ),
            (
                NSWorkspaceSessionDidBecomeActiveNotification,
                Mac::Session(true),
            ),
        ]
    };
    for (name, event) in names {
        let tell = tell.clone();
        let block = block2::RcBlock::new(move |_: NonNull<NSNotification>| tell(event));
        // SAFETY: no object to match, no queue (the block runs where the notification is
        // posted), and the block is sendable: it holds only a `Send + Sync` closure.
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
        };
        // Observed until the process ends: the token is never handed back.
        std::mem::forget(observer);
    }
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
        assert!(State::parse("op=crop;restore=1").unwrap().restore);
        assert!(!State::parse("op=crop").unwrap().restore);
        assert!(State::parse("restore=yes").is_err());
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
                git: None,
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

        /// Whether the viewer is open: its row has Exit, the list's Rename is gone.
        fn viewer(&self) -> bool {
            let exit = self.drv.find_text("Exit   ").is_some();
            assert_ne!(exit, self.drv.find_text("Rename ").is_some(), "one row");
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

    /// 04 §6.1, K-R2: a click on a slot of the real browser's row is its key: MkDir (7, S6) asks
    /// a new folder's name, View opens the file, Exit leaves the viewer, Edit opens the editor;
    /// an empty slot — 7 in the text viewer, which has no Save As — is nothing.
    #[test]
    fn a_click_on_a_slot_is_its_key_in_a_real_window() {
        let mut fx = BrowserFx::new();
        fx.bar_on_main();
        let (_, seven) = chrome::key_slot(0.0, 1000.0, 7);
        let at_seven = egui::Pos2::new(seven.center().x, 700.0 - 8.0);
        let asks = format!("Make a folder in {}", fx.root);
        fx.drv.click(&mut fx.b, at_seven);
        fx.settle();
        // The folder's path makes the line wider than the dialog: it wraps by cells.
        let shown = |fx: &BrowserFx| browser::shown_wrapped(&fx.drv, &asks);
        assert!(shown(&fx), "the MkDir slot asked");
        fx.drv
            .key(&mut fx.b, egui::Key::Escape, egui::Modifiers::NONE);
        fx.settle();
        assert!(!shown(&fx), "Esc closed it");
        assert!(!fx.viewer());
        fx.drv.click_text(&mut fx.b, "View   ");
        fx.settle();
        assert!(fx.viewer(), "the View slot opened main.rs");
        fx.drv.click(&mut fx.b, at_seven);
        fx.settle();
        assert!(fx.viewer(), "an empty slot: still the viewer");
        fx.drv.click_text(&mut fx.b, "Exit   ");
        fx.settle();
        assert!(!fx.viewer(), "the Exit slot left it");
        // Over a viewer the list's dialogs are not drawn; back in the list, one the empty slot
        // had opened would be.
        assert!(!shown(&fx), "and no dialog");
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
        let save = "Save (as Ctrl/⌘+S)";
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
        assert!(drv.find_text(save).is_some(), "{:?}", drv.texts());
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
        assert!(drv.find_text(save).is_none());
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

    // -- The F-key switch fed by the windows (04 §6.2), against a fake switch.

    use crate::fnkeys::{Holder, Machine, Watchdog};
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    /// The HID value as the switch sees it, shared as processes share the real one. (S7-A's
    /// fake is private to `fnkeys::tests`.) A write first waits while the test holds `stall`.
    #[derive(Clone, Default)]
    struct Hid(Arc<Mutex<HidState>>, Arc<Mutex<()>>);

    #[derive(Default)]
    struct HidState {
        value: i64,
        /// Every write, in order, those that failed too.
        writes: Vec<i64>,
        /// A set to 1 fails.
        refuse: bool,
        /// The next this many writes of 0 fail.
        fail_zeros: usize,
        /// A write of 0 takes this long.
        zero_takes: Duration,
    }

    impl Switch for Hid {
        fn read(&mut self) -> Result<i64> {
            Ok(self.0.lock().unwrap().value)
        }

        fn write(&mut self, value: i64) -> Result<()> {
            drop(self.1.lock().unwrap());
            let takes = self.0.lock().unwrap().zero_takes;
            if value == fnkeys::OFF {
                std::thread::sleep(takes);
            }
            let mut hid = self.0.lock().unwrap();
            hid.writes.push(value);
            if value == fnkeys::ON && hid.refuse {
                bail!("refused by the fake");
            }
            if value == fnkeys::OFF && hid.fail_zeros > 0 {
                hid.fail_zeros -= 1;
                bail!("the fake failed this write");
            }
            hid.value = value;
            Ok(())
        }
    }

    impl Hid {
        fn writes(&self) -> Vec<i64> {
            self.0.lock().unwrap().writes.clone()
        }

        fn value(&self) -> i64 {
            self.0.lock().unwrap().value
        }

        fn with(&self, change: impl FnOnce(&mut HidState)) {
            change(&mut self.0.lock().unwrap())
        }
    }

    /// The made-up sopen process the tests' switches belong to.
    const ME: Holder = Holder {
        pid: 4242,
        start: 4242,
    };

    /// This process's switch over `hid`, its files in `dir`, no watchdog; only [`ME`] runs.
    fn switch(dir: &Path, hid: &Hid) -> Fkeys<Hid> {
        Fkeys::new(
            dir.to_path_buf(),
            hid.clone(),
            Ok(ME),
            Machine {
                runs: Arc::new(|h| *h == ME),
                boot: None,
            },
            Watchdog::off(),
        )
    }

    /// No hold: a window not in front lets go at once.
    const QUICK: Timing = Timing {
        hold: Duration::ZERO,
        tick: Duration::from_secs(1),
        slept: Duration::from_secs(5),
    };

    /// A feed whose thread builds [`switch`], paced by `timing` (not waited for: the build can
    /// wait for the lock).
    fn feed(dir: &Path, hid: &Hid, timing: Timing) -> Arc<Feed> {
        let (dir, hid) = (dir.to_path_buf(), hid.clone());
        Arc::new(Feed::start(
            move || Some(switch(&dir, &hid)),
            |_| {},
            timing,
            SystemTime::now,
        ))
    }

    /// The same, and where the system's word goes once the switch is built (what the
    /// NSWorkspace observers get on macOS).
    fn feed_hearing(dir: &Path, hid: &Hid, timing: Timing) -> (Arc<Feed>, MacEvents) {
        let (dir, hid) = (dir.to_path_buf(), hid.clone());
        let (give, events) = mpsc::channel();
        let feed = Arc::new(Feed::start(
            move || Some(switch(&dir, &hid)),
            move |events| give.send(events).unwrap(),
            timing,
            SystemTime::now,
        ));
        let events = events.recv_timeout(Duration::from_secs(10)).unwrap();
        (feed, events)
    }

    struct Blank;

    impl Window for Blank {
        fn ui(&mut self, _ui: &mut egui::Ui) {}

        fn ready(&self) -> bool {
            true
        }
    }

    /// The shell around a blank window, as [`run_window_sized`] makes it, fed into `feed`.
    fn shell(feed: &Arc<Feed>) -> Shell<Blank> {
        Shell {
            inner: Blank,
            scripted: false,
            shot: None,
            frames: 0,
            started: Instant::now(),
            focus_asked: None,
            raise: Raise::Done,
            press: Press::Done,
            fkeys: Some(feed.clone()),
            closed: Arc::default(),
            web: None,
        }
    }

    /// A window of the process as eframe reports it, shown.
    fn win(focused: bool) -> egui::ViewportInfo {
        egui::ViewportInfo {
            focused: Some(focused),
            minimized: Some(false),
            occluded: Some(false),
            ..Default::default()
        }
    }

    /// One pass of the shell's root viewport as eframe runs it (`epi_integration`): `logic`
    /// then `ui` while the root is shown, `logic` alone (`run_logic`) while it is hidden. The
    /// process's windows are `windows`, the root first.
    fn pass(shell: &mut Shell<Blank>, ctx: &egui::Context, windows: &[egui::ViewportInfo]) {
        let mut raw = egui::RawInput::default();
        for (n, info) in windows.iter().enumerate() {
            let id = match n {
                0 => egui::ViewportId::ROOT,
                n => egui::ViewportId::from_hash_of(("another window", n)),
            };
            raw.viewports.insert(id, info.clone());
        }
        let root = windows.first().cloned().unwrap_or_default();
        raw.focused = root.focused == Some(true);
        let mut frame = eframe::Frame::_new_kittest();
        if root.minimized != Some(true) && root.occluded != Some(true) {
            let mut out = ctx.run_ui(raw, |ui| {
                eframe::App::logic(shell, ui.ctx(), &mut frame);
                eframe::App::ui(shell, ui, &mut frame);
            });
            // No renderer: the texture uploads are dropped on purpose.
            out.textures_delta.clear();
        } else {
            let _ = ctx.run_logic(&raw, |ctx| eframe::App::logic(shell, ctx, &mut frame));
        }
    }

    /// Waits, at most 10 s, until `what` reads `want`.
    fn wait_until<T: std::fmt::Debug + PartialEq>(what: impl Fn() -> T, want: T) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let now = what();
            if now == want {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{now:?}, not {want:?}, after 10 s"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// 04 §6.2 "When", through the shell as eframe runs it: a window in front acquires (the
    /// fake reads 0 and is set to 1), none in front releases (0), and again; a window that keeps
    /// its focus but is fully covered or minimised is not in front — its pass is `logic` alone
    /// and still says so; a pass says only what changed; the window's end — the shell dropped,
    /// as eframe drops it when its loop ends — lets go at once.
    #[test]
    fn s7b_focus_through_the_shell_reaches_the_switch() {
        // What the shell sends: only changes.
        let (told, heard) = mpsc::channel();
        let quiet = Arc::new(Feed {
            told,
            said: Arc::default(),
            sent: AtomicU8::new(UNSAID),
        });
        let (ctx, mut sh) = (egui::Context::default(), shell(&quiet));
        for front in [true, true, true, false, false, true] {
            pass(&mut sh, &ctx, &[win(front)]);
        }
        let sent: Vec<Option<bool>> = heard
            .try_iter()
            .map(|t| match t {
                Told::Front(front) => Some(front),
                Told::Closed(_) | Told::Mac(..) => None,
            })
            .collect();
        assert_eq!(sent, [Some(true), Some(false), Some(true)]);
        drop(heard);

        let tmp = tempfile::tempdir().unwrap();
        let hid = Hid::default();
        let feed = feed(tmp.path(), &hid, QUICK);
        let (ctx, mut sh) = (egui::Context::default(), shell(&feed));
        pass(&mut sh, &ctx, &[win(true)]);
        wait_until(|| hid.writes(), vec![1]);
        assert!(tmp.path().join(fnkeys::STATE).exists());
        pass(&mut sh, &ctx, &[win(false)]);
        wait_until(|| hid.writes(), vec![1, 0]);
        wait_until(|| tmp.path().join(fnkeys::STATE).exists(), false);
        pass(&mut sh, &ctx, &[win(true)]);
        wait_until(|| hid.writes(), vec![1, 0, 1]);
        let covered = egui::ViewportInfo {
            occluded: Some(true),
            ..win(true)
        };
        pass(&mut sh, &ctx, &[covered]);
        wait_until(|| hid.writes(), vec![1, 0, 1, 0]);
        pass(&mut sh, &ctx, &[win(true)]);
        wait_until(|| hid.writes(), vec![1, 0, 1, 0, 1]);
        let minimised = egui::ViewportInfo {
            minimized: Some(true),
            ..win(true)
        };
        pass(&mut sh, &ctx, &[minimised]);
        wait_until(|| hid.writes(), vec![1, 0, 1, 0, 1, 0]);
        pass(&mut sh, &ctx, &[win(true)]);
        wait_until(|| hid.writes(), vec![1, 0, 1, 0, 1, 0, 1]);
        assert_eq!(fkeys_status(&ctx), None, "it took effect");
        // The window ends while in front: let go by the time its teardown returns.
        drop(sh);
        assert_eq!(hid.writes(), [1, 0, 1, 0, 1, 0, 1, 0]);
        assert_eq!(hid.value(), 0);
        assert!(!tmp.path().join(fnkeys::STATE).exists());
    }

    /// 04 §6.2: the windows of one process hand focus to each other without a release. Focus
    /// moving between two in one pass is still a window in front; moving over two passes, with
    /// a pass in between where neither has it, is held for `hold`, and a window in front within
    /// it keeps the switch. One write over all of it: the acquisition. Only none in front past
    /// the hold lets go.
    #[test]
    fn s7b_focus_moving_between_two_windows_never_lets_go() {
        let tmp = tempfile::tempdir().unwrap();
        let hid = Hid::default();
        let hold = Duration::from_millis(800);
        let feed = feed(tmp.path(), &hid, Timing { hold, ..QUICK });
        let (ctx, mut sh) = (egui::Context::default(), shell(&feed));
        pass(&mut sh, &ctx, &[win(true), win(false)]);
        wait_until(|| hid.writes(), vec![1]);
        // In one pass: the root lets go, the other window takes it.
        pass(&mut sh, &ctx, &[win(false), win(true)]);
        // Over two passes, each way.
        pass(&mut sh, &ctx, &[win(false), win(false)]);
        pass(&mut sh, &ctx, &[win(true), win(false)]);
        pass(&mut sh, &ctx, &[win(false), win(false)]);
        pass(&mut sh, &ctx, &[win(false), win(true)]);
        std::thread::sleep(hold * 2);
        assert_eq!(hid.writes(), [1], "let go while focus moved");
        assert_eq!(hid.value(), 1);
        pass(&mut sh, &ctx, &[win(false), win(false)]);
        wait_until(|| hid.writes(), vec![1, 0]);
        assert!(feed.closed(Duration::from_secs(10)));
    }

    /// 04 §6.2 "never in a scripted run", and K-F4 at the windows' end. A run any of the five
    /// hooks drives ([`fnkeys::scripted_run`]) gets no feed: no thread, the config is not read,
    /// no switch is built, nothing hears the system. Any other run starts the feed's thread
    /// and the window's thread does nothing more: the config is read there; with `fkeys =
    /// "system"` or a config that cannot be read no switch is built and nothing hears the
    /// system; else the switch is built, once, and then where the system's word goes is handed
    /// out (the NSWorkspace observers), once. An empty hook does not count.
    #[test]
    fn s7b_scripted_runs_never_build_the_switch() {
        let tmp = tempfile::tempdir().unwrap();
        let hid = Hid::default();
        /// The threads the config was read on; switches built; times the system's word was
        /// handed out.
        type Calls = (Vec<Option<String>>, usize, usize);
        let calls: Arc<Mutex<Calls>> = Arc::default();
        let run = |var: &dyn Fn(&str) -> Option<OsString>, text: &'static str| {
            let (read, build, built) = (calls.clone(), calls.clone(), calls.clone());
            let (dir, hid) = (tmp.path().to_path_buf(), hid.clone());
            Feed::for_run(
                var,
                move || {
                    let thread = std::thread::current().name().map(str::to_string);
                    read.lock().unwrap().0.push(thread);
                    config::parse(text).map_err(|e| anyhow!("config.toml: {e}"))
                },
                move |_: &config::Config| {
                    build.lock().unwrap().1 += 1;
                    Some(switch(&dir, &hid))
                },
                move |_| built.lock().unwrap().2 += 1,
            )
        };
        let counts = || {
            let calls = calls.lock().unwrap();
            (calls.0.len(), calls.1, calls.2)
        };
        for hook in [
            "SOPEN_SCREENSHOT",
            "SOPEN_CELLS",
            "SOPEN_AUTOSAVE",
            "SOPEN_AUTOEXPORT",
            "SOPEN_LIST",
            "SOPEN_KEYS",
        ] {
            let var = |name: &str| (name == hook).then(|| OsString::from("1"));
            assert!(run(&var, "").is_none(), "{hook}");
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(counts(), (0, 0, 0), "a scripted run read, built or heard");
        let none = |_: &str| None;
        let _system = run(&none, "fkeys = \"system\"\n").expect("a feed");
        wait_until(counts, (1, 0, 0));
        let _broken = run(&none, "colour = \"red\"\n").expect("a feed");
        wait_until(counts, (2, 0, 0));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(counts(), (2, 0, 0), "built with system or a broken config");
        let empty = |name: &str| (name == "SOPEN_LIST").then(OsString::new);
        let _default = run(&empty, "").expect("a feed");
        wait_until(counts, (3, 1, 1));
        let _function = run(&none, "fkeys = \"function\"\n").expect("a feed");
        wait_until(counts, (4, 2, 2));
        let threads = calls.lock().unwrap().0.clone();
        assert!(
            threads.iter().all(|t| t.as_deref() == Some("sopen-fkeys")),
            "the config was read on {threads:?}"
        );
    }

    /// The window's thread never waits for the switch: with its lock held (a sopen stopped
    /// while it holds it), passes that change the focus all return at once, and the switch's
    /// own thread waits instead; once the lock is let go it acts on the last word. Both waits:
    /// the first acquisition's, and the build's (the start's clean-up of a state a dead holder
    /// left, which restores 0 first).
    #[test]
    fn s7b_a_pass_never_waits_for_the_switchs_lock() {
        for left in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().to_path_buf();
            let hid = Hid::default();
            if left {
                let dead = fnkeys::State {
                    original: fnkeys::OFF,
                    holders: vec![Holder { pid: 7, start: 7 }],
                    boot: None,
                };
                fnkeys::write_state(&dir, &dead).unwrap();
            }
            let open = || {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(dir.join(fnkeys::LOCK))
                    .unwrap()
            };
            let held = open();
            held.lock().unwrap();
            assert!(
                matches!(open().try_lock(), Err(std::fs::TryLockError::WouldBlock)),
                "a second open in this process waits for the lock"
            );
            let feed = feed(&dir, &hid, QUICK);
            let (done, finished) = mpsc::channel();
            let passes = {
                let feed = feed.clone();
                std::thread::spawn(move || {
                    let (ctx, mut sh) = (egui::Context::default(), shell(&feed));
                    for front in [true, false, true, false, true] {
                        pass(&mut sh, &ctx, &[win(front)]);
                    }
                    let _ = done.send(());
                    // The window stays open: its end would be a word of its own.
                    sh
                })
            };
            let returned = finished.recv_timeout(Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(100));
            let under_the_lock = hid.writes();
            drop(held);
            let sh = passes.join().unwrap();
            assert!(
                returned.is_ok(),
                "left={left}: a pass waited for the switch's lock"
            );
            assert!(
                under_the_lock.is_empty(),
                "left={left}: the switch acted under the lock: {under_the_lock:?}"
            );
            let want = if left { vec![0, 1] } else { vec![1] };
            wait_until(|| hid.writes(), want);
            drop(sh);
            assert_eq!(hid.value(), 0, "left={left}");
        }
    }

    /// 04 §6.2 "acquire again on wake": sopen hears nothing as the Mac sleeps or wakes. While a
    /// window is in front the switch's thread looks every tick; the wall clock having run far
    /// ahead of the monotonic one says the Mac slept, and the switch is let go and taken again
    /// (0, then 1, set and read back anew). Ticks without a sleep do nothing, and with no window
    /// in front a wake does nothing.
    #[test]
    fn s7b_a_wake_lets_go_and_takes_the_switch_again() {
        let tmp = tempfile::tempdir().unwrap();
        let hid = Hid::default();
        let ahead = Arc::new(AtomicU64::new(0));
        let timing = Timing {
            tick: Duration::from_millis(20),
            ..QUICK
        };
        let (dir, fake, clock) = (tmp.path().to_path_buf(), hid.clone(), ahead.clone());
        let feed = Arc::new(Feed::start(
            move || Some(switch(&dir, &fake)),
            |_| {},
            timing,
            move || SystemTime::now() + Duration::from_secs(clock.load(Ordering::SeqCst)),
        ));
        let (ctx, mut sh) = (egui::Context::default(), shell(&feed));
        pass(&mut sh, &ctx, &[win(true)]);
        wait_until(|| hid.writes(), vec![1]);
        std::thread::sleep(timing.tick * 10);
        assert_eq!(hid.writes(), [1], "ticks without a sleep");
        // An hour asleep.
        ahead.fetch_add(3600, Ordering::SeqCst);
        wait_until(|| hid.writes(), vec![1, 0, 1]);
        std::thread::sleep(timing.tick * 10);
        assert_eq!(hid.writes(), [1, 0, 1], "one wake, taken once again");
        assert_eq!(hid.value(), 1);
        pass(&mut sh, &ctx, &[win(false)]);
        wait_until(|| hid.writes(), vec![1, 0, 1, 0]);
        ahead.fetch_add(3600, Ordering::SeqCst);
        // Something for the thread, which waits without a tick now, to hear.
        feed.told.send(Told::Front(false)).unwrap();
        std::thread::sleep(timing.tick * 10);
        assert_eq!(hid.writes(), [1, 0, 1, 0], "no window in front");
        assert!(feed.closed(Duration::from_secs(10)));
    }

    /// 04 §6.2 "When": before the Mac sleeps and when the session is switched away the switch is
    /// let go; on wake and on the session's return it is taken again if a window is still in
    /// front — and not while either holds, whatever the windows say. The events go through the
    /// same channel as the windows' word ([`MacEvents::tell`], what the NSWorkspace observers
    /// call on macOS) and reach the fake in the order they were sent. A real NSWorkspace
    /// notification cannot be posted from a test: the observers themselves are not run here.
    #[test]
    fn s7b_sleep_and_session_events_reach_the_switch_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let hid = Hid::default();
        let (feed, events) = feed_hearing(tmp.path(), &hid, QUICK);
        let mac = |event| {
            events.tell(event);
        };
        let (ctx, mut sh) = (egui::Context::default(), shell(&feed));
        pass(&mut sh, &ctx, &[win(true)]);
        wait_until(|| hid.writes(), vec![1]);
        mac(Mac::Sleeps);
        wait_until(|| hid.writes(), vec![1, 0]);
        mac(Mac::Woke);
        wait_until(|| hid.writes(), vec![1, 0, 1]);
        mac(Mac::Session(false));
        wait_until(|| hid.writes(), vec![1, 0, 1, 0]);
        // Away: the windows' word changes nothing.
        pass(&mut sh, &ctx, &[win(false)]);
        pass(&mut sh, &ctx, &[win(true)]);
        mac(Mac::Session(true));
        wait_until(|| hid.writes(), vec![1, 0, 1, 0, 1]);
        // A burst, in its order: let go, taken, let go, taken.
        for event in [
            Mac::Sleeps,
            Mac::Woke,
            Mac::Session(false),
            Mac::Session(true),
        ] {
            mac(event);
        }
        wait_until(|| hid.writes(), vec![1, 0, 1, 0, 1, 0, 1, 0, 1]);
        // Asleep, a window comes to the front: nothing until the wake.
        pass(&mut sh, &ctx, &[win(false)]);
        wait_until(|| hid.writes(), vec![1, 0, 1, 0, 1, 0, 1, 0, 1, 0]);
        mac(Mac::Sleeps);
        pass(&mut sh, &ctx, &[win(true)]);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(hid.writes().len(), 10, "taken while the Mac sleeps");
        mac(Mac::Woke);
        wait_until(|| hid.writes().len(), 11);
        assert_eq!(hid.value(), 1);
        // No window in front: a wake and a session's return take nothing.
        pass(&mut sh, &ctx, &[win(false)]);
        wait_until(|| hid.writes().len(), 12);
        mac(Mac::Woke);
        mac(Mac::Session(true));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(hid.writes().len(), 12, "taken with no window in front");
        assert_eq!(hid.value(), 0);
        assert!(feed.closed(Duration::from_secs(10)));
        assert!(!tmp.path().join(fnkeys::STATE).exists());
    }

    /// The macOS half: the observers [`observe_the_mac`] registers on NSWorkspace's
    /// notification center turn each of the four notifications into its event, in order. The
    /// system's own (a real sleep, a real session switch) cannot be made in a test; the same names
    /// are posted on the same center by the test instead — a center in this process only, heard
    /// by a recorder, so nothing reaches a switch.
    #[cfg(target_os = "macos")]
    #[test]
    fn s7b_the_workspace_observers_tell_each_notification() {
        use objc2_app_kit::{
            NSWorkspace, NSWorkspaceDidWakeNotification,
            NSWorkspaceSessionDidBecomeActiveNotification,
            NSWorkspaceSessionDidResignActiveNotification, NSWorkspaceWillSleepNotification,
        };
        let heard: Arc<Mutex<Vec<Mac>>> = Arc::default();
        let record = heard.clone();
        observe_the_mac(move |event| record.lock().unwrap().push(event));
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        // SAFETY: AppKit's constant names; no object.
        unsafe {
            for name in [
                NSWorkspaceWillSleepNotification,
                NSWorkspaceDidWakeNotification,
                NSWorkspaceSessionDidResignActiveNotification,
                NSWorkspaceSessionDidBecomeActiveNotification,
            ] {
                center.postNotificationName_object(name, None);
            }
        }
        assert_eq!(
            *heard.lock().unwrap(),
            [
                Mac::Sleeps,
                Mac::Woke,
                Mac::Session(false),
                Mac::Session(true)
            ]
        );
    }

    /// Review round 1, item 2: a resume (a wake, the session's return) acts on the windows'
    /// word heard before it, never on the focus the switch held before it let go. Held for 30 s
    /// (`hold`), the switch is still told a window is in front when "none is" is heard; a wake
    /// queued right after it, on the same channel, takes nothing. The same for a session that
    /// was away and comes back.
    #[test]
    fn s7b_a_resume_takes_the_windows_word_as_it_stands() {
        let tmp = tempfile::tempdir().unwrap();
        let hid = Hid::default();
        let timing = Timing {
            hold: Duration::from_secs(30),
            ..QUICK
        };
        let (feed, events) = feed_hearing(tmp.path(), &hid, timing);
        let (ctx, mut sh) = (egui::Context::default(), shell(&feed));
        pass(&mut sh, &ctx, &[win(true)]);
        wait_until(|| hid.writes(), vec![1]);
        assert!(events.tell(Mac::Sleeps));
        assert_eq!(hid.writes(), [1, 0]);
        pass(&mut sh, &ctx, &[win(false)]);
        events.tell(Mac::Woke);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            hid.writes(),
            [1, 0],
            "taken on the focus held before the sleep"
        );
        pass(&mut sh, &ctx, &[win(true)]);
        wait_until(|| hid.writes(), vec![1, 0, 1]);
        events.tell(Mac::Session(false));
        wait_until(|| hid.writes(), vec![1, 0, 1, 0]);
        pass(&mut sh, &ctx, &[win(false)]);
        events.tell(Mac::Session(true));
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            hid.writes(),
            [1, 0, 1, 0],
            "taken on the focus held before the session went"
        );
        pass(&mut sh, &ctx, &[win(true)]);
        wait_until(|| hid.writes(), vec![1, 0, 1, 0, 1]);
        drop(sh);
        assert_eq!(hid.value(), 0);
    }

    /// Review round 1, item 3: the will-sleep notification's observer returns only once the
    /// switch has acted on the sleep — its release of 200 ms done, the 0 written — so the Mac
    /// does not suspend first; with a switch that never finishes it returns at about
    /// [`SLEEP_WAIT`] (1 s) all the same. Other events only send.
    #[test]
    fn s7b_the_sleep_notification_waits_for_the_release_at_most_a_second() {
        let tmp = tempfile::tempdir().unwrap();
        let hid = Hid::default();
        let (feed, events) = feed_hearing(tmp.path(), &hid, QUICK);
        let (ctx, mut sh) = (egui::Context::default(), shell(&feed));
        pass(&mut sh, &ctx, &[win(true)]);
        wait_until(|| hid.writes(), vec![1]);
        hid.with(|h| h.zero_takes = Duration::from_millis(200));
        let t0 = Instant::now();
        let acted = events.tell(Mac::Sleeps);
        let took = t0.elapsed();
        assert!(acted, "not acted on within {SLEEP_WAIT:?}");
        assert_eq!(hid.writes(), [1, 0], "returned before the release");
        assert!(took >= Duration::from_millis(200), "{took:?}");
        hid.with(|h| h.zero_takes = Duration::ZERO);
        let t0 = Instant::now();
        assert!(events.tell(Mac::Woke));
        assert!(t0.elapsed() < Duration::from_millis(100), "a wake waited");
        wait_until(|| hid.writes(), vec![1, 0, 1]);
        // A switch that never finishes its write: the test holds the fake's stall.
        let stall = hid.1.lock().unwrap();
        let t0 = Instant::now();
        let acted = events.tell(Mac::Sleeps);
        let took = t0.elapsed();
        assert!(!acted);
        assert!(
            took >= SLEEP_WAIT && took < SLEEP_WAIT + Duration::from_secs(2),
            "{took:?}"
        );
        assert_eq!(hid.writes(), [1, 0, 1]);
        drop(stall);
        wait_until(|| hid.writes(), vec![1, 0, 1, 0]);
        drop(sh);
        assert_eq!(hid.value(), 0);
    }

    /// 04 §6.3 "a release that fails … is tried again": the switch's thread runs it when it is
    /// due ([`Fkeys::next_retry`]) by itself — no window pass comes after the one that let go —
    /// and, while a window is in front (an acquisition whose undo failed is due only once none
    /// is), it waits for the next word instead of spinning. Its turns are counted through the
    /// wall clock it reads once a turn.
    #[test]
    fn s7b_a_failed_release_is_retried_by_the_thread_without_spinning() {
        let tmp = tempfile::tempdir().unwrap();
        let hid = Hid::default();
        let turns = Arc::new(AtomicUsize::new(0));
        let timing = Timing {
            tick: Duration::from_millis(100),
            ..QUICK
        };
        let (dir, fake, counted) = (tmp.path().to_path_buf(), hid.clone(), turns.clone());
        let feed = Arc::new(Feed::start(
            move || Some(switch(&dir, &fake)),
            |_| {},
            timing,
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
                SystemTime::now()
            },
        ));
        let (ctx, mut sh) = (egui::Context::default(), shell(&feed));
        // Let go, the restore fails once: tried again with no pass after it.
        pass(&mut sh, &ctx, &[win(true)]);
        wait_until(|| hid.writes(), vec![1]);
        hid.with(|h| h.fail_zeros = 1);
        pass(&mut sh, &ctx, &[win(false)]);
        wait_until(|| hid.writes(), vec![1, 0, 0]);
        assert_eq!(hid.value(), 0);
        wait_until(|| tmp.path().join(fnkeys::STATE).exists(), false);
        // In front, the set refused and its undo failed: a release is due, but not while a
        // window is in front.
        hid.with(|h| {
            h.refuse = true;
            h.fail_zeros = 1;
        });
        pass(&mut sh, &ctx, &[win(true)]);
        wait_until(|| hid.writes(), vec![1, 0, 0, 1, 0]);
        let before = turns.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(300));
        let spun = turns.load(Ordering::SeqCst) - before;
        assert!(spun <= 10, "{spun} turns in 300 ms with a window in front");
        assert_eq!(hid.writes(), [1, 0, 0, 1, 0]);
        pass(&mut sh, &ctx, &[win(true)]);
        let said = fkeys_status(&ctx).unwrap_or_default();
        assert!(said.starts_with(fnkeys::NOT_IN_EFFECT), "{said}");
        pass(&mut sh, &ctx, &[win(false)]);
        wait_until(|| hid.writes(), vec![1, 0, 0, 1, 0, 0]);
        assert_eq!(hid.value(), 0);
        wait_until(|| tmp.path().join(fnkeys::STATE).exists(), false);
        assert!(feed.closed(Duration::from_secs(10)));
    }

    /// 04 §6.3: an acquisition that did not take effect (the fake refuses the set) is said in
    /// the status row — the browser's, over its last result and its keys, and a window of its
    /// own's, above its F-key row — until one does.
    #[test]
    fn s7b_the_status_row_says_the_switch_did_not_take_effect() {
        let tmp = tempfile::tempdir().unwrap();
        let hid = Hid::default();
        hid.with(|h| h.refuse = true);
        let feed = feed(tmp.path(), &hid, QUICK);
        let (ctx, mut sh) = (egui::Context::default(), shell(&feed));
        let deadline = Instant::now() + Duration::from_secs(10);
        let line = loop {
            pass(&mut sh, &ctx, &[win(true)]);
            if let Some(line) = fkeys_status(&ctx) {
                break line;
            }
            assert!(Instant::now() < deadline, "no status line after 10 s");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(
            line,
            format!("{}: refused by the fake", fnkeys::NOT_IN_EFFECT)
        );
        assert_eq!(hid.writes(), [1, 0], "set, refused, the original set back");
        assert!(feed.closed(Duration::from_secs(10)));

        let said = |ctx: &egui::Context, line: Option<&str>| {
            ctx.data_mut(|d| match line {
                Some(line) => {
                    d.insert_temp(fkeys_line_id(), FkeysLine(line.to_string()));
                }
                None => d.remove::<FkeysLine>(fkeys_line_id()),
            })
        };
        let mut fx = BrowserFx::new();
        // The list's keys, said when nothing else is.
        let hint = |fx: &BrowserFx| !fx.drv.texts_starting_with("Enter open").is_empty();
        assert!(hint(&fx));
        said(&fx.drv.ctx, Some(&line));
        fx.drv.idle(&mut fx.b, 1);
        assert!(fx.drv.find_text(&line).is_some(), "{:?}", fx.drv.texts());
        assert!(!hint(&fx));
        said(&fx.drv.ctx, None);
        fx.drv.idle(&mut fx.b, 1);
        assert!(fx.drv.find_text(&line).is_none());
        assert!(hint(&fx));

        let mut drv = testkit::Driver::new(800.0, 400.0);
        let mut own = Own {
            f8: 0,
            typed: String::new(),
            clicks: 0,
        };
        drv.idle(&mut own, 2);
        assert!(drv.find_text(&line).is_none());
        said(&drv.ctx, Some(&line));
        drv.idle(&mut own, 2);
        assert!(drv.find_text(&line).is_some(), "{:?}", drv.texts());
        assert!(drv.find_text("Help   ").is_some(), "the row is still there");
        drv.key(&mut own, egui::Key::F8, egui::Modifiers::NONE);
        assert_eq!(own.f8, 1, "and its keys work");
    }

    // -----------------------------------------------------------------------------------------
    // XW-shell: F12, Alt+digits and Alt+G (10 §9, T4, T7, T33).

    /// `key` pressed and let go with `modifiers` held, no text typed (F12, and a chord with
    /// Alt from a terminal, type none: 10 §4).
    fn press(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        [true, false]
            .map(|pressed| digit(key, pressed, modifiers))
            .to_vec()
    }

    /// The keys `win` saw since the last call, as `(key, pressed)`.
    fn seen_keys(win: &mut Recorder) -> Vec<(egui::Key, bool)> {
        left_down(&std::mem::take(&mut win.seen)).1
    }

    /// 10 §9, T4: F12 is the switch, taken by the shell's input hook in every context — a field
    /// with the typing included — so neither the window nor a key-table row sees it; the pass
    /// it was pressed in asks for the switch, the next one does not. A held F12's repeat and
    /// Shift+F12 ask nothing. (F12 comes with no text, from a window or a terminal.)
    #[test]
    fn xw_shell_f12_is_the_switch_in_every_context() {
        use egui::Key::F12;
        let (none, shift) = (egui::Modifiers::NONE, egui::Modifiers::SHIFT);
        for focus in [false, true] {
            let mut drv = testkit::Driver::new(400.0, 200.0);
            let mut win = Recorder {
                seen: Vec::new(),
                text: "name".into(),
                focus,
            };
            drv.idle(&mut win, 2);
            assert_eq!(drv.ctx.text_edit_focused(), focus);
            drv.frame(&mut win, press(F12, none), none);
            assert!(switch_asked(&drv.ctx), "focus {focus}: F12 asks");
            drv.idle(&mut win, 1);
            assert!(
                !switch_asked(&drv.ctx),
                "focus {focus}: the next pass does not"
            );
            let repeat = egui::Event::Key {
                key: F12,
                physical_key: None,
                pressed: true,
                repeat: true,
                modifiers: none,
            };
            drv.frame(&mut win, vec![repeat], none);
            assert!(!switch_asked(&drv.ctx), "focus {focus}: a repeat");
            drv.frame(&mut win, press(F12, shift), shift);
            assert!(!switch_asked(&drv.ctx), "focus {focus}: Shift+F12");
            assert_eq!(seen_keys(&mut win), [], "focus {focus}: no F12 reached it");
            assert!(!drv.ctx.input(|i| i.key_down(F12)));
        }
    }

    /// 10 §9: in a window Alt+digits and Alt+G stay the keys they are — Option+digit and
    /// Option+G type characters on macOS — and Alt+G asks for no switch (the negative control
    /// of the terminal's test below: the same keys, the context not marked).
    #[test]
    fn xw_shell_alt_digits_and_alt_g_stay_keys_in_a_window() {
        use egui::Key::{Num5, G};
        let alt = egui::Modifiers::ALT;
        let mut drv = testkit::Driver::new(400.0, 200.0);
        let mut win = Recorder {
            seen: Vec::new(),
            text: String::new(),
            focus: false,
        };
        drv.idle(&mut win, 2);
        drv.frame(&mut win, press(Num5, alt), alt);
        drv.frame(&mut win, press(G, alt), alt);
        assert!(!switch_asked(&drv.ctx));
        assert_eq!(
            seen_keys(&mut win),
            [(Num5, true), (Num5, false), (G, true), (G, false)]
        );
    }

    /// 10 §9, T7, T33: in the terminal backend's context ([`mark_terminal`]) Alt+1…0 are F1…F10,
    /// alongside Cmd/Ctrl+digits, but not while a field has the typing (04 §6.1); Alt+G is the
    /// switch, taken even from a field (it types nothing); a plain G stays G.
    #[test]
    fn xw_shell_in_a_terminal_alt_digits_are_f_keys_and_alt_g_the_switch() {
        use egui::Key::{Num5, F5, G};
        let (alt, none, cmd) = (egui::Modifiers::ALT, egui::Modifiers::NONE, real_command());
        let mut drv = testkit::Driver::new(400.0, 200.0);
        mark_terminal(&drv.ctx);
        let mut win = Recorder {
            seen: Vec::new(),
            text: String::new(),
            focus: false,
        };
        drv.idle(&mut win, 2);
        drv.frame(&mut win, press(Num5, alt), alt);
        assert_eq!(seen_keys(&mut win), [(F5, true), (F5, false)], "Alt+5");
        drv.frame(&mut win, press(Num5, cmd), cmd);
        assert_eq!(seen_keys(&mut win), [(F5, true), (F5, false)], "Cmd/Ctrl+5");
        drv.frame(&mut win, press(G, alt), alt);
        assert!(switch_asked(&drv.ctx), "Alt+G asks");
        assert_eq!(seen_keys(&mut win), [], "and is taken");
        drv.frame(&mut win, press(G, none), none);
        assert!(!switch_asked(&drv.ctx), "a plain G does not");
        assert_eq!(seen_keys(&mut win), [(G, true), (G, false)]);

        win.focus = true;
        drv.idle(&mut win, 2);
        assert!(drv.ctx.text_edit_focused());
        seen_keys(&mut win);
        drv.frame(&mut win, press(Num5, alt), alt);
        assert_eq!(
            seen_keys(&mut win),
            [(Num5, true), (Num5, false)],
            "the field's"
        );
        drv.frame(&mut win, press(G, alt), alt);
        assert!(switch_asked(&drv.ctx), "Alt+G from the field");
        assert_eq!(seen_keys(&mut win), []);
        assert!(!drv.ctx.input(|i| i.key_down(F5) || i.key_down(G)));
    }

    /// Alt+G in a terminal is decided at its press, as a digit is: its release is taken too
    /// when Alt was let go first, and a focus lost while it is down forgets it, so the next
    /// plain G is G again.
    #[test]
    fn xw_shell_alt_g_keeps_its_release_after_alt_goes() {
        use egui::Key::G;
        let (alt, none) = (egui::Modifiers::ALT, egui::Modifiers::NONE);
        let mut drv = testkit::Driver::new(400.0, 200.0);
        mark_terminal(&drv.ctx);
        let mut win = Recorder {
            seen: Vec::new(),
            text: String::new(),
            focus: false,
        };
        drv.idle(&mut win, 2);
        drv.frame(&mut win, vec![digit(G, true, alt)], alt);
        assert!(switch_asked(&drv.ctx));
        drv.frame(&mut win, vec![digit(G, false, none)], none);
        assert_eq!(seen_keys(&mut win), [], "the release went with its press");
        assert!(!drv.ctx.input(|i| i.key_down(G)));
        drv.frame(&mut win, vec![digit(G, true, alt)], alt);
        drv.frame(&mut win, vec![egui::Event::WindowFocused(false)], none);
        drv.frame(&mut win, vec![egui::Event::WindowFocused(true)], none);
        drv.frame(&mut win, press(G, none), none);
        assert!(!switch_asked(&drv.ctx));
        assert_eq!(
            seen_keys(&mut win),
            [(G, true), (G, false)],
            "a plain G after the focus came back"
        );
    }

    // -----------------------------------------------------------------------------------------
    // XW-shell: the start and the glow retry (10 T31, T34; 04 §6.4.3).

    /// [`start`] over renderers whose run takes the shell from its slot when `takes` names them
    /// and whose loop then ends with `ends`: the renderers run, the outcome, and whether the
    /// shell is still in its slot.
    fn started(
        choice: RendererChoice,
        takes: &[eframe::Renderer],
        ends: impl Fn(eframe::Renderer) -> Result<Ended, String>,
    ) -> (Vec<eframe::Renderer>, Result<()>, bool) {
        let slot = RefCell::new(Some("the shell"));
        let mut ran = Vec::new();
        let out = start(choice, &slot, |renderer| {
            ran.push(renderer);
            if takes.contains(&renderer) {
                slot.borrow_mut().take();
            }
            ends(renderer)
        });
        let left = slot.borrow().is_some();
        (ran, out, left)
    }

    /// A loop that ended with eframe's error `error` (none: `""`), the shell having let a
    /// close through or not.
    fn ended(error: &str, closed: bool) -> Result<Ended, String> {
        Ok(Ended {
            error: (!error.is_empty()).then(|| error.to_string()),
            closed,
        })
    }

    /// The reason of a start that never ran the window ([`NotStarted`]).
    fn not_started(out: &Result<()>) -> Option<String> {
        let e = out.as_ref().err()?.downcast_ref::<NotStarted>()?;
        Some(e.reason.clone())
    }

    /// 04 §6.4.3: `create_native` returns no `Result`, and a renderer's failure stays inside
    /// eframe while the loop returns `Ok`, so the retry's trigger is the shell still in its
    /// slot: wgpu's loop ends `Ok` with the shell there → glow runs and takes it. A wgpu that
    /// takes it runs alone. The retry's line names wgpu's error (10 T31), as eframe logged it.
    #[test]
    fn xw_shell_the_glow_retry_fires_on_the_shell_still_in_its_slot() {
        use eframe::Renderer::{Glow, Wgpu};
        let (ran, out, left) = started(RendererChoice::Auto, &[Glow], |r| match r {
            Wgpu => ended("WGPU error: no surface", false),
            Glow => ended("", true),
        });
        assert_eq!(ran, [Wgpu, Glow]);
        assert!(out.is_ok() && !left, "{out:?}");
        let (ran, out, left) = started(RendererChoice::Auto, &[Wgpu, Glow], |_| ended("", true));
        assert_eq!(ran, [Wgpu]);
        assert!(out.is_ok() && !left, "{out:?}");
        assert_eq!(
            retry_line(Some("WGPU error: no surface")),
            "sopen: the wgpu renderer did not start (WGPU error: no surface); using OpenGL (glow)"
        );
        assert_eq!(
            retry_line(None),
            "sopen: the wgpu renderer did not start; using OpenGL (glow)"
        );
    }

    /// Still in its slot after glow too → `cannot open a window: neither wgpu nor glow started`
    /// (04 §6.4.3) with each renderer's error eframe logged (10 T31), the shell never run; a
    /// forced renderer (`SOPEN_RENDERER`) runs alone and is named.
    #[test]
    fn xw_shell_no_renderer_started_says_which_and_why() {
        use eframe::Renderer::{Glow, Wgpu};
        let (ran, out, left) = started(RendererChoice::Auto, &[], |r| match r {
            Wgpu => ended("WGPU error: no surface", false),
            Glow => ended("no GL config", false),
        });
        assert_eq!(ran, [Wgpu, Glow]);
        assert!(left);
        assert_eq!(
            format!("{:#}", out.unwrap_err()),
            "cannot open a window: neither wgpu nor glow started \
             (wgpu: WGPU error: no surface; glow: no GL config)"
        );
        let (_, out, _) = started(RendererChoice::Auto, &[], |_| ended("", false));
        assert_eq!(
            not_started(&out).as_deref(),
            Some("neither wgpu nor glow started"),
            "no error heard"
        );
        for (choice, only, name) in [
            (RendererChoice::Wgpu, Wgpu, "wgpu"),
            (RendererChoice::Glow, Glow, "glow"),
        ] {
            let (ran, out, left) = started(choice, &[], |_| ended("it broke", false));
            assert_eq!(ran, [only]);
            assert!(left);
            assert_eq!(
                not_started(&out),
                Some(format!(
                    "the {name} renderer did not start (SOPEN_RENDERER={name}): it broke"
                ))
            );
        }
    }

    /// The event loop failing — it cannot be built (no display) or cannot run — is no
    /// renderer's failure: it ends at once, glow untried, with the loop's reason (10 T31's
    /// line names it). A loop failing under a window that started is an error, not a start
    /// that failed.
    #[test]
    fn xw_shell_a_failed_event_loop_is_not_retried() {
        use eframe::Renderer::Wgpu;
        let (ran, out, left) = started(RendererChoice::Auto, &[], |_| {
            Err("no event loop: no display".into())
        });
        assert_eq!(ran, [Wgpu]);
        assert!(left);
        assert_eq!(
            not_started(&out).as_deref(),
            Some("no event loop: no display")
        );
        let (ran, out, left) = started(RendererChoice::Auto, &[Wgpu], |_| {
            Err("the display went".into())
        });
        assert_eq!(ran, [Wgpu]);
        assert!(
            !left && out.is_err() && not_started(&out).is_none(),
            "{out:?}"
        );
    }

    /// A window that started and then ended on eframe's error without the shell having let a
    /// close through is no silent close: an error naming it (exit 1, as `run_native`'s error
    /// was). With a close let through, or no error, it ended as a close.
    #[test]
    fn xw_shell_an_error_under_a_started_window_is_said() {
        use eframe::Renderer::Wgpu;
        let (ran, out, _) = started(RendererChoice::Auto, &[Wgpu], |_| {
            ended("WGPU error: surface lost", false)
        });
        assert_eq!(ran, [Wgpu]);
        assert_eq!(
            format!("{:#}", out.unwrap_err()),
            "the window ended on an error: WGPU error: surface lost"
        );
        for (error, closed) in [("WGPU error: surface lost", true), ("", false)] {
            let (_, out, _) = started(RendererChoice::Auto, &[Wgpu], |_| ended(error, closed));
            assert!(out.is_ok(), "{error:?} closed {closed}: {out:?}");
        }
    }

    /// eframe's error-level records reach the attempt that heard them (10 T31: eframe keeps a
    /// renderer's error to `log::error!("Exiting because of error: {err}")`, `native/run.rs:178`),
    /// that prefix taken off, the last one kept; another crate's records and records outside an
    /// attempt are kept nowhere.
    #[test]
    fn xw_shell_eframes_error_record_is_heard() {
        let ((), heard) = hearing_eframe(|| {
            log::error!(target: "eframe::native::wgpu_integration", "Failed to create window: x");
            log::error!(target: "egui_wgpu::winit", "not eframe's");
            log::error!(
                target: "eframe::native::run",
                "Exiting because of error: WGPU error: no surface"
            );
            log::warn!(target: "eframe::native::run", "a warning");
        });
        assert_eq!(heard.as_deref(), Some("WGPU error: no surface"));
        log::error!(target: "eframe::native::run", "Exiting because of error: outside");
        let ((), heard) = hearing_eframe(|| {});
        assert_eq!(heard, None);
    }
}
