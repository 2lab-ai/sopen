//! The image window (spec §6) and the media window (spec §7). This module is the shared shell:
//! where the opened file lives ([`Origin`]), the window runner (wgpu renderer, glow fallback,
//! activation on start, Cmd/Ctrl+Q through the close prompt), the receipt hooks ([`Hooks`]) and
//! the screenshot driver. Everything that writes (Save, Save copy, Export) is a plain function
//! over [`Origin`], called by the window's buttons and by the windowless hooks alike.

pub mod image;
pub mod media;
pub mod playback;
mod select;

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
/// double-click): `<stem>.<tag>-<UTC stamp>.<ext>`.
pub fn keep_path(cached: &Path, tag: &str, at: SystemTime) -> PathBuf {
    let stem = cached
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stamp = crate::cache::utc_stamp(at);
    let name = match cached.extension() {
        Some(ext) => format!("{stem}.{tag}-{stamp}.{}", ext.to_string_lossy()),
        None => format!("{stem}.{tag}-{stamp}"),
    };
    cached.with_file_name(name)
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
/// seconds. `op` may list several, comma-separated, applied in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub rect: Option<[u32; 4]>,
    pub t: Option<f64>,
    pub in_s: Option<f64>,
    pub out_s: Option<f64>,
    pub ops: Vec<Op>,
    pub block: Option<u32>,
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
                other => bail!("SOPEN_STATE: unknown key '{other}'"),
            }
        }
        Ok(state)
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
        Ok(Self {
            screenshot: std::env::var_os("SOPEN_SCREENSHOT")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            state,
            autosave: flag("SOPEN_AUTOSAVE"),
            autoexport: flag("SOPEN_AUTOEXPORT"),
            save_delay,
            export_hold,
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
}

/// How long the window may take to report focus after the first focus request.
const FOCUS_GRACE: Duration = Duration::from_millis(300);
/// How long a window that did not get focus floats above the others.
const RAISE_FOR: Duration = Duration::from_millis(1500);

/// Bringing a window to the front when focus is refused: macOS keeps an app started over ssh
/// (`--target`) unfocused and behind the active app, whatever it asks for. Still unfocused
/// [`FOCUS_GRACE`] after the focus requests, the window floats above the others
/// (`WindowLevel::AlwaysOnTop`) until [`RAISE_FOR`] has passed or the first key or click lands in
/// it, then goes back to the normal level. Focus itself is untouched (`focused=` stays truthful).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Raise {
    /// Waiting to see whether the focus requests worked.
    Waiting,
    /// Above the other windows since this instant.
    Floating(Instant),
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
                    (Raise::Floating(now), Some(egui::WindowLevel::AlwaysOnTop))
                }
                _ => (self, None),
            },
            Raise::Floating(since)
                if input || now.saturating_duration_since(since) >= RAISE_FOR =>
            {
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
        if !self.inner.ready() {
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
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if self.scripted {
            // A scripted run renders the state it was given; keys typed into whatever window
            // the owner is using while a receipt window flashes up must not edit it.
            raw.events.clear();
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
            Raise::Floating(since) => {
                ctx.request_repaint_after(RAISE_FOR.saturating_sub(since.elapsed()))
            }
            Raise::Done => {}
        }
        if command_key(&ctx, egui::Key::Q, false) || command_key(&ctx, egui::Key::W, false) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ctx.input(|i| i.viewport().close_requested())
            && !self.scripted
            && !self.inner.close_requested()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.inner.ui(ui);
        self.screenshot_tick(&ctx);
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
    };
    // The app moves into whichever renderer starts. A renderer that fails before creating the
    // app leaves it here for the next one.
    let slot = RefCell::new(Some(shell));
    let attempt = |renderer: eframe::Renderer| -> Result<(), eframe::Error> {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(title)
                .with_app_id("sopen")
                .with_inner_size(size)
                .with_min_inner_size([480.0, 320.0])
                .with_active(true),
            renderer,
            centered: true,
            event_loop_builder: event_loop_hook(),
            ..Default::default()
        };
        eframe::run_native(
            "sopen",
            options,
            Box::new(|_cc| {
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

    pub fn show(&self, ui: &mut egui::Ui) {
        if self.error {
            ui.colored_label(ui.visuals().error_fg_color, &self.text);
        } else {
            ui.label(&self.text);
        }
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
    fn an_unfocused_window_floats_above_the_others_for_a_while() {
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
                (Raise::Floating(at(300)), Some(AlwaysOnTop))
            );
        }
        // Back to normal at RAISE_FOR, or at the first input, whichever comes first.
        let f = Raise::Floating(at(300));
        assert_eq!(
            f.step(Some(t0), true, Some(false), false, at(1700)),
            (f, None)
        );
        assert_eq!(
            f.step(Some(t0), true, Some(false), false, at(1800)),
            (Raise::Done, Some(Normal))
        );
        assert_eq!(
            f.step(Some(t0), true, Some(true), true, at(400)),
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
            keep_path(Path::new("/c/box/tmp/shot.png"), "conflict", at),
            PathBuf::from("/c/box/tmp/shot.conflict-20261111T092436Z.png")
        );
        assert_eq!(
            keep_path(Path::new("/c/box/README"), "unsaved", at),
            PathBuf::from("/c/box/README.unsaved-20261111T092436Z")
        );
    }
}
