//! The file browser (spec §3a, owner message 12): a folder opens in sopen's own window, two file
//! panels side by side as in Total Commander, laid out where PC Shell puts things (the program's
//! name on top, the path in the panel's frame, the counts at its foot, the F-keys at the
//! bottom — [`super::chrome`] draws all of it). Enter on a file shows the viewer for its kind in
//! the same window: text in the text viewer, an image in the image window, video and audio in
//! the media window, anything else in the hex window. F4 opens the highlighted file or folder in
//! VS Code or Zed. The browser views; it never copies, moves, deletes or renames.
//!
//! Every way out of a viewer — Esc, F3, closing the window — goes through the viewer's own
//! [`Window::close_requested`], so its unsaved-changes prompt and its running saves are honoured
//! exactly as in its own window: a viewer is dropped only once it said yes, and what it may still
//! be writing is waited for before sopen exits ([`open`]).
//!
//! Nothing slow runs on a frame: listing, opening a connection, fetching, decoding and probing
//! run on workers, and Esc stops a fetch.

mod panel;
mod prepare;

use super::chrome::{self, FKey};
use super::{Embedded, Hooks, Outcome, Status, Window};
use crate::remote::{EntryKind, RemoteFs};
use anyhow::Result;
use eframe::egui::{self, Key, Modifiers, Pos2, Rect, Sense, Vec2};
pub use panel::Panel;
use prepare::Prepared;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// The window's size when it opens.
pub const WINDOW: [f32; 2] = [1100.0, 700.0];

/// Opens a new connection to the folder's host. Each viewer gets one of its own (its Save
/// uses it while the browser goes on listing on its own), and the browser's is opened again
/// when its session died.
pub type Connect = Arc<dyn Fn() -> Result<Box<dyn RemoteFs + Send>> + Send + Sync>;

/// Opens `(abs, is_dir)` in the external editor (VS Code, else Zed); the line to show.
pub type Launch = Arc<dyn Fn(&str, bool) -> Result<String> + Send + Sync>;

/// What the browser starts from.
pub struct Start {
    /// The connection the folder was found over.
    pub fs: Box<dyn RemoteFs + Send>,
    pub connect: Connect,
    /// `[user@]host`; `None` on this machine.
    pub label: Option<String>,
    /// The folder (absolute).
    pub abs: String,
    /// sopen's cache root.
    pub cache: PathBuf,
    pub launch: Launch,
}

/// Shows the browser until its window closes; then waits for anything a viewer may still be
/// writing (printed as the viewer's own window prints it) and returns what the viewers did.
pub fn open(start: Start, hooks: &Hooks) -> Result<Outcome> {
    let title = show(start.label.as_deref(), &start.abs);
    let mut browser = Browser::new(start);
    browser.open_first = hooks.state.open.clone();
    let leftover = browser.leftover.clone();
    let run = super::run_window(&title, WINDOW, browser, hooks);
    let outcome = leftover.borrow_mut().finish();
    run?;
    Ok(outcome)
}

/// What the browser lists for `abs`, as text: the path, a row per entry, the footer — the
/// `SOPEN_LIST` hook's output (one listing, no window).
pub fn listing(fs: &mut dyn RemoteFs, label: Option<&str>, abs: &str) -> Result<Vec<String>> {
    let entries = fs.list_entries(abs)?;
    let mut panel = Panel::new(abs);
    panel.fill(abs.to_string(), entries, fs.free_space(abs), None);
    let mut out = vec![show(label, abs)];
    out.extend(panel.entries.iter().map(panel::row_text));
    out.push(panel.footer());
    Ok(out)
}

/// `host:/abs`, or the local path.
fn show(label: Option<&str>, abs: &str) -> String {
    match label {
        Some(label) => format!("{label}:{abs}"),
        None => abs.to_string(),
    }
}

/// Why an entry whose name is not UTF-8 is not opened (sopen's paths are UTF-8; the name as
/// shown, with U+FFFD, may even be another file's).
fn not_utf8(shown: &str) -> String {
    format!("{shown}: the name is not valid UTF-8; sopen cannot open it")
}

/// What the browser is doing on a worker.
enum Task {
    /// Listing `dir` for one panel (`None`: both, at the start); `select` = the entry to put
    /// the bar on.
    Listing {
        panel: Option<usize>,
        dir: String,
        select: Option<String>,
        rx: Receiver<Result<Listed>>,
    },
    /// Making the file at `abs` ready for its viewer.
    Opening {
        abs: String,
        name: String,
        remote: bool,
        done: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        cancel: Arc<AtomicBool>,
        rx: Receiver<Result<Prepared>>,
        worker: Option<JoinHandle<()>>,
    },
}

/// A listing: the entries and the free space.
type Listed = (Vec<crate::remote::Entry>, Option<u64>);

/// The viewer on screen.
struct Viewer {
    embedded: Embedded,
    /// The cached copy it shows, held until it is gone.
    hold: Option<crate::cache::Claim>,
    /// The file (for F4).
    abs: String,
}

/// What a viewer's close leads to: back to the list (Esc, F3), or the window closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    List,
    Quit,
}

/// A viewer that is gone, and what it may still be writing.
struct Closing {
    settle: Box<dyn FnMut(bool) -> Option<Outcome>>,
    hold: Option<crate::cache::Claim>,
}

/// What outlives the window: fetches stopping, viewers' writes, the viewers' outcome.
#[derive(Default)]
struct Leftover {
    workers: Vec<JoinHandle<()>>,
    closing: Vec<Closing>,
    outcome: Outcome,
}

impl Leftover {
    /// Takes what has settled (`wait`: waits for all of it).
    fn settle(&mut self, wait: bool) {
        let mut still = Vec::new();
        for mut c in self.closing.drain(..) {
            match (c.settle)(wait) {
                Some(outcome) => {
                    self.outcome.merge(outcome);
                    drop(c.hold.take());
                }
                None => still.push(c),
            }
        }
        self.closing = still;
    }

    /// After the window: stopped fetches end, every write is waited for.
    fn finish(&mut self) -> Outcome {
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        self.settle(true);
        self.outcome.clone()
    }
}

pub(crate) struct Browser {
    /// `host:/abs` of the folder opened.
    title: String,
    label: Option<String>,
    /// The browser's own connection (listing), shared with its listing worker.
    conn: Arc<Mutex<Box<dyn RemoteFs + Send>>>,
    connect: Connect,
    launch: Launch,
    cache: PathBuf,
    panels: [Panel; 2],
    active: usize,
    task: Option<Task>,
    viewer: Option<Viewer>,
    exit: Exit,
    status: Status,
    launches: Vec<Receiver<Result<String>>>,
    leftover: Rc<RefCell<Leftover>>,
    /// A widget had the keyboard at the end of the last pass: an Esc that took it away was that
    /// widget's (a text field's), not a request to leave the viewer.
    focused_before: bool,
    /// Rows each panel shows (the last pass's).
    visible: usize,
    painted: bool,
    /// `SOPEN_STATE` `open=NAME`: opened once the folder is first listed (receipts of a viewer).
    open_first: Option<String>,
}

impl Drop for Browser {
    /// Whatever is still running when the window goes away is handed to [`open`], which waits.
    fn drop(&mut self) {
        let mut leftover = self.leftover.borrow_mut();
        if let Some(viewer) = self.viewer.take() {
            leftover.closing.push(Closing {
                settle: viewer.embedded.settle,
                hold: viewer.hold,
            });
        }
        if let Some(Task::Opening { cancel, worker, .. }) = self.task.take() {
            cancel.store(true, Ordering::Relaxed);
            leftover.workers.extend(worker);
        }
    }
}

fn list_id() -> egui::Id {
    egui::Id::unique("sopen-browser-list")
}

/// The keys the status row names when nothing else is to be said.
const LIST_HINT: &str =
    "Enter open · Tab other panel · Backspace up · a letter jumps to a name · F4 VS Code / Zed";
const VIEWER_HINT: &str = "Esc or F3 back to the list · F4 VS Code / Zed";

impl Browser {
    pub(crate) fn new(start: Start) -> Self {
        let mut browser = Self {
            title: show(start.label.as_deref(), &start.abs),
            label: start.label,
            conn: Arc::new(Mutex::new(start.fs)),
            connect: start.connect,
            launch: start.launch,
            cache: start.cache,
            panels: [Panel::new(&start.abs), Panel::new(&start.abs)],
            active: 0,
            task: None,
            viewer: None,
            exit: Exit::List,
            status: Status::default(),
            launches: Vec::new(),
            leftover: Rc::default(),
            focused_before: false,
            visible: 1,
            painted: false,
            open_first: None,
        };
        browser.list(None, start.abs, None, None);
        browser
    }

    /// Lists `dir` on a worker for `panel` (`None`: both).
    fn list(
        &mut self,
        panel: Option<usize>,
        dir: String,
        select: Option<String>,
        ctx: Option<&egui::Context>,
    ) {
        let (tx, rx) = mpsc::channel();
        let (conn, connect, remote) = (
            self.conn.clone(),
            self.connect.clone(),
            self.label.is_some(),
        );
        let (path, waker) = (dir.clone(), ctx.cloned());
        std::thread::spawn(move || {
            let mut fs = conn.lock().unwrap_or_else(|p| p.into_inner());
            let once = |fs: &mut Box<dyn RemoteFs + Send>| -> Result<Listed> {
                let entries = fs.list_entries(&path)?;
                Ok((entries, fs.free_space(&path)))
            };
            let mut result = once(&mut fs);
            // The session may have died since the last listing: one more try on a new one.
            if result.is_err() && remote && fs.stat_opt(&path).is_err() {
                if let Ok(fresh) = connect() {
                    *fs = fresh;
                    result = once(&mut fs);
                }
            }
            let _ = tx.send(result);
            if let Some(ctx) = waker {
                ctx.request_repaint();
            }
        });
        self.task = Some(Task::Listing {
            panel,
            dir,
            select,
            rx,
        });
    }

    /// Makes the file at `abs` ready for its viewer, on a worker.
    fn start_open(&mut self, ctx: &egui::Context, abs: String, name: String) {
        let (done, total) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let (connect, label, cache) =
            (self.connect.clone(), self.label.clone(), self.cache.clone());
        let (d, t, stop, waker, path) = (
            done.clone(),
            total.clone(),
            cancel.clone(),
            ctx.clone(),
            abs.clone(),
        );
        let worker = std::thread::spawn(move || {
            let req = prepare::Request {
                connect: &connect,
                label: label.as_deref(),
                abs: &path,
                cache: &cache,
            };
            let result = prepare::prepare(&req, &stop, &mut |now, all| {
                d.store(now, Ordering::Relaxed);
                t.store(all, Ordering::Relaxed);
                waker.request_repaint();
            });
            let _ = tx.send(result);
            waker.request_repaint();
        });
        self.status = Status::default();
        self.task = Some(Task::Opening {
            abs,
            name,
            remote: self.label.is_some(),
            done,
            total,
            cancel,
            rx,
            worker: Some(worker),
        });
    }

    /// Results from the workers: a listing, a file ready (its viewer shows), an F4 launch; and
    /// writes of viewers already gone.
    fn poll(&mut self, ctx: &egui::Context) {
        match &mut self.task {
            Some(Task::Listing {
                panel,
                dir,
                select,
                rx,
            }) => match rx.try_recv() {
                Ok(result) => {
                    let (panel, dir, select) = (*panel, dir.clone(), select.take());
                    self.task = None;
                    match result {
                        Ok((entries, free)) => {
                            if self.status.error {
                                // An earlier listing's failure is over.
                                self.status = Status::default();
                            }
                            for i in panel.map_or(vec![0, 1], |p| vec![p]) {
                                let (e, s) = (entries.clone(), select.as_deref());
                                self.panels[i].fill(dir.clone(), e, free, s);
                            }
                            if let Some(name) = self.open_first.take() {
                                let panel = &mut self.panels[self.active];
                                match panel.entries.iter().position(|e| e.name == name) {
                                    Some(i) => {
                                        panel.highlight = i;
                                        self.enter(ctx, false);
                                    }
                                    None => {
                                        self.status = Status::error(format!(
                                            "SOPEN_STATE open: no {name} in {dir}"
                                        ))
                                    }
                                }
                            }
                        }
                        Err(e) => self.status = Status::error(format!("{e:#}")),
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.task = None;
                    self.status = Status::error("the listing stopped without a result");
                }
            },
            Some(Task::Opening { rx, .. }) => match rx.try_recv() {
                Ok(result) => {
                    if let Some((abs, name, cancelled)) = self.end_opening() {
                        if cancelled && result.is_ok() {
                            // Esc came after the last byte (or there was none to fetch): the
                            // file is ready but was not wanted any more. Dropping it releases
                            // its copy and its connection.
                            drop(result);
                            let kept = if self.label.is_some() {
                                " (its copy is in sopen's cache)"
                            } else {
                                ""
                            };
                            self.status =
                                Status::info(format!("cancelled; {name} was not opened{kept}"));
                        } else {
                            self.opened(ctx, result, abs, name);
                        }
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.end_opening();
                    self.status = Status::error("opening stopped without a result");
                }
            },
            None => {}
        }
        self.launches.retain(|rx| match rx.try_recv() {
            Ok(Ok(line)) => {
                self.status = Status::info(line);
                false
            }
            Ok(Err(e)) => {
                self.status = Status::error(format!("{e:#}"));
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
        });
        self.leftover.borrow_mut().settle(false);
    }

    /// The opening task is over: its worker is done (it sent its result). The file it opened,
    /// and whether Esc asked to stop it.
    fn end_opening(&mut self) -> Option<(String, String, bool)> {
        let Some(Task::Opening {
            abs,
            name,
            worker,
            cancel,
            ..
        }) = self.task.take()
        else {
            return None;
        };
        if let Some(worker) = worker {
            let _ = worker.join();
        }
        Some((abs, name, cancel.load(Ordering::Relaxed)))
    }

    /// The file at `abs` came back ready: its viewer shows, in this window.
    fn opened(&mut self, ctx: &egui::Context, result: Result<Prepared>, abs: String, name: String) {
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(e) if crate::remote::is_cancelled(&e) => {
                self.status = Status::info("cancelled; nothing was kept");
                return;
            }
            Err(e) => {
                self.status = Status::error(format!("{e:#}"));
                return;
            }
        };
        let title = show(self.label.as_deref(), &abs);
        let (embedded, hold) = match prepared {
            Prepared::Image {
                file,
                doc,
                notices,
                claim,
            } => (super::image::embed(file, doc, notices), claim),
            Prepared::Media {
                origin,
                media,
                notices,
                claim,
            } => (
                super::media::embed(origin, media, &self.cache, notices),
                claim,
            ),
            Prepared::Hex { file, notices } => (super::hex::embed(file, notices), None),
            Prepared::Text {
                doc,
                notices,
                claim,
            } => (super::text::embed(doc, title, &name, notices), claim),
        };
        // The list gave up the keyboard: the viewer's keys are its own now.
        ctx.memory_mut(|m| m.surrender_focus(list_id()));
        self.status = Status::default();
        self.exit = Exit::List;
        // One viewer at a time, and never one replaced without its own say (it leaves through
        // `leave` / `close_requested` first): nothing opens while one shows.
        debug_assert!(self.viewer.is_none(), "a viewer was replaced unasked");
        self.viewer = Some(Viewer {
            embedded,
            hold,
            abs,
        });
    }

    /// Leaves the viewer — for the list or for good — if it agrees; else it shows its own
    /// prompt and asks again ([`super::request_close`]) once that is answered.
    fn leave(&mut self, ctx: &egui::Context, exit: Exit) {
        self.exit = exit;
        let agreed = self
            .viewer
            .as_mut()
            .is_some_and(|v| v.embedded.window.close_requested());
        if agreed {
            self.drop_viewer();
            self.after_viewer(ctx);
        }
    }

    /// The viewer is gone: what it may still be writing is kept until it settles.
    fn drop_viewer(&mut self) {
        if let Some(viewer) = self.viewer.take() {
            self.leftover.borrow_mut().closing.push(Closing {
                settle: viewer.embedded.settle,
                hold: viewer.hold,
            });
        }
    }

    /// Where the closed viewer's exit leads: the list as it was, or the window closing.
    fn after_viewer(&mut self, ctx: &egui::Context) {
        self.status = Status::default();
        if std::mem::replace(&mut self.exit, Exit::List) == Exit::Quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// F4: the entry (or the viewer's file) in VS Code or Zed, on a worker (the editor's CLI
    /// may take a moment to hand over).
    fn edit(&mut self, ctx: &egui::Context, abs: String, is_dir: bool) {
        let (tx, rx) = mpsc::channel();
        let (launch, waker) = (self.launch.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(launch(&abs, is_dir));
            // The window may be idle (no input, no other work): wake it for the result.
            waker.request_repaint();
        });
        self.status = Status::info("opening in the editor…");
        self.launches.push(rx);
    }

    /// Enter / F3 on the entry under the bar: a folder is entered (`..` goes up); a file opens
    /// in its viewer (`files_only`: F3 leaves folders alone).
    fn enter(&mut self, ctx: &egui::Context, files_only: bool) {
        let panel = &self.panels[self.active];
        let Some(entry) = panel.current().cloned() else {
            return;
        };
        if entry.lossy {
            self.status = Status::error(not_utf8(&entry.name));
            return;
        }
        let path = panel.path_of(&entry);
        let shown = show(self.label.as_deref(), &path);
        match entry.kind {
            _ if entry.name == panel::UP => {
                if !files_only {
                    self.up(ctx);
                }
            }
            EntryKind::Dir => {
                if !files_only {
                    self.list(Some(self.active), path, None, Some(ctx));
                }
            }
            EntryKind::File | EntryKind::Unknown => self.start_open(ctx, path, entry.name),
            EntryKind::Special => {
                self.status = Status::error(format!(
                    "{shown} is not a regular file (a fifo, socket or device); sopen does not open it"
                ));
            }
            EntryKind::Broken => {
                self.status = Status::error(format!("{shown} is a link to nothing"));
            }
        }
    }

    /// Backspace, `..`: the parent folder, the bar on the folder just left.
    fn up(&mut self, ctx: &egui::Context) {
        let dir = self.panels[self.active].dir.clone();
        if dir == "/" {
            return;
        }
        let (parent, name) = crate::remote::split_parent(&dir);
        let (parent, name) = (parent.to_string(), name.to_string());
        self.list(Some(self.active), parent, Some(name), Some(ctx));
    }

    /// The list's keys. While a file is being made ready only Esc works (it stops a fetch);
    /// while a folder is listed, none (it takes a moment).
    fn list_keys(&mut self, ctx: &egui::Context) {
        let pressed = |key: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key));
        match &self.task {
            Some(Task::Opening { cancel, .. }) => {
                if pressed(Key::Escape) {
                    cancel.store(true, Ordering::Relaxed);
                }
                return;
            }
            Some(Task::Listing { .. }) => return,
            None => {}
        }
        let shift_tab = ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Tab));
        if pressed(Key::Tab) || shift_tab {
            self.active = 1 - self.active;
        }
        let page = self.visible.saturating_sub(1).max(1) as isize;
        let panel = &mut self.panels[self.active];
        if pressed(Key::ArrowDown) {
            panel.step(1);
        }
        if pressed(Key::ArrowUp) {
            panel.step(-1);
        }
        if pressed(Key::PageDown) {
            panel.step(page);
        }
        if pressed(Key::PageUp) {
            panel.step(-page);
        }
        if pressed(Key::Home) {
            panel.home();
        }
        if pressed(Key::End) {
            panel.end();
        }
        let typed: Vec<char> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Text(t) => Some(t.chars().collect::<Vec<_>>()),
                    _ => None,
                })
                .flatten()
                .collect()
        });
        for c in typed.into_iter().filter(|c| !c.is_whitespace()) {
            panel.jump(c);
        }
        if pressed(Key::Enter) {
            self.enter(ctx, false);
        } else if pressed(Key::F3) {
            self.enter(ctx, true);
        } else if pressed(Key::Backspace) {
            self.up(ctx);
        } else if pressed(Key::F4) {
            let panel = &self.panels[self.active];
            match panel.current() {
                Some(entry) if entry.lossy => self.status = Status::error(not_utf8(&entry.name)),
                Some(entry) => {
                    let is_dir = entry.kind == EntryKind::Dir;
                    let path = panel.path_of(entry);
                    self.edit(ctx, path, is_dir);
                }
                None => {}
            }
        }
    }

    /// The viewer's pass, then the browser's keys it left unused: Esc and F3 leave it, F4 opens
    /// its file in the editor. `esc_was_a_fields`: this frame's Esc took the keyboard from a
    /// text field (the viewer's Go to field): it does not leave.
    fn viewer_ui(&mut self, ui: &mut egui::Ui, esc_was_a_fields: bool) {
        let ctx = ui.ctx().clone();
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        let ((), asked) = super::host_pass(&ctx, || viewer.embedded.window.ui(ui));
        if asked {
            // Its prompt was answered, or a write it waited for landed: ask it again.
            if viewer.embedded.window.close_requested() {
                self.drop_viewer();
                self.after_viewer(&ctx);
            }
            return;
        }
        let pressed = |key: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key));
        if pressed(Key::F4) {
            let abs = viewer.abs.clone();
            self.edit(&ctx, abs, false);
        }
        let esc = !esc_was_a_fields && pressed(Key::Escape);
        if esc || pressed(Key::F3) {
            self.leave(&ctx, Exit::List);
        }
    }

    /// The two panels, side by side.
    fn panels_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let area = ui.max_rect();
        // The list holds the keyboard: Tab, the arrows and Esc are its keys, not focus moves.
        let resp = ui.interact(area, list_id(), Sense::click());
        if ctx.memory(|m| m.focused()) != Some(list_id()) {
            resp.request_focus();
        }
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(
                list_id(),
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
        let half = (area.width() / 2.0).floor();
        for i in 0..2 {
            let rect = Rect::from_min_size(
                Pos2::new(area.left() + half * i as f32, area.top()),
                Vec2::new(half, area.height()),
            )
            .shrink(2.0);
            self.panel_ui(ui, i, rect);
        }
    }

    /// One panel: its frame with the path in the top border, the column heads, the rows, the
    /// footer. A click puts the bar on a row (and makes the panel active); a double click
    /// opens it.
    fn panel_ui(&mut self, ui: &mut egui::Ui, i: usize, rect: Rect) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter_at(rect);
        let active = i == self.active && self.viewer.is_none();
        let title = show(self.label.as_deref(), &self.panels[i].dir);
        let inner = chrome::panel_frame(&painter, rect, &title, active);
        let p = chrome::Palette::of(ui.visuals());
        let font = chrome::mono(ui);
        let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
        let row_h = (text_h + 2.0).ceil();
        // Columns, in characters: Name (what is left) · Ext · Size · Date · Time.
        let chars = (inner.width() / cw).floor().max(10.0) as usize;
        let dated = chars >= 48;
        let (ext_w, size_w, date_w, time_w) = (5, 14, 10, 5);
        let fixed = ext_w + size_w + 2 + if dated { date_w + time_w + 2 } else { 0 };
        let name_w = chars.saturating_sub(fixed + 1).max(4);
        let col = |c: usize| inner.left() + cw * c as f32;
        let (x_ext, x_size_end) = (col(name_w + 1), col(name_w + 1 + ext_w + 1 + size_w));
        let (x_date, x_time) = (
            col(name_w + ext_w + size_w + 4),
            col(name_w + ext_w + size_w + date_w + 5),
        );
        let head_y = inner.top();
        let cell = |painter: &egui::Painter, x: f32, y: f32, text: &str, color| {
            painter.text(
                Pos2::new(x, y),
                egui::Align2::LEFT_TOP,
                text,
                font.clone(),
                color,
            );
        };
        cell(&painter, inner.left(), head_y, "Name", p.dim);
        cell(&painter, x_ext, head_y, "Ext", p.dim);
        painter.text(
            Pos2::new(x_size_end, head_y),
            egui::Align2::RIGHT_TOP,
            "Size",
            font.clone(),
            p.dim,
        );
        if dated {
            cell(&painter, x_date, head_y, "Date", p.dim);
            cell(&painter, x_time, head_y, "Time", p.dim);
        }
        let rows_top = head_y + row_h;
        let foot_y = inner.bottom() - row_h;
        let visible = (((foot_y - rows_top - 4.0) / row_h).floor() as usize).max(1);
        self.visible = visible;
        let panel = &mut self.panels[i];
        // The wheel scrolls the rows (the bar stays where it is).
        if ui.rect_contains_pointer(rect) {
            let lines: f32 = ctx.input(|inp| {
                inp.events
                    .iter()
                    .map(|e| match e {
                        egui::Event::MouseWheel { unit, delta, .. } => match unit {
                            egui::MouseWheelUnit::Point => -delta.y / row_h,
                            _ => -delta.y * 3.0,
                        },
                        _ => 0.0,
                    })
                    .sum()
            });
            let max_top = panel.entries.len().saturating_sub(visible);
            panel.top = (panel.top as f32 + lines)
                .round()
                .clamp(0.0, max_top as f32) as usize;
        }
        if i == self.active {
            panel.follow(visible);
        }
        let mut clicked = None;
        for (row, idx) in (panel.top..panel.entries.len()).take(visible).enumerate() {
            let entry = &panel.entries[idx];
            let y = rows_top + row as f32 * row_h;
            let row_rect = Rect::from_min_size(
                Pos2::new(inner.left() - 2.0, y),
                Vec2::new(inner.width() + 4.0, row_h),
            );
            if idx == panel.highlight {
                chrome::bar(&painter, row_rect, active);
            }
            let color = if idx == panel.highlight && active {
                p.bar_text
            } else if panel::hidden(entry) {
                p.dim
            } else if entry.kind == EntryKind::Dir {
                p.folder
            } else {
                p.text
            };
            let (name, ext) = panel::split_name(entry);
            let name_rect =
                Rect::from_min_max(row_rect.min, Pos2::new(x_ext - cw, row_rect.bottom()));
            cell(
                &painter.with_clip_rect(name_rect),
                inner.left(),
                y,
                name,
                color,
            );
            cell(&painter, x_ext, y, ext, color);
            painter.text(
                Pos2::new(x_size_end, y),
                egui::Align2::RIGHT_TOP,
                panel::size_cell(entry),
                font.clone(),
                color,
            );
            if dated {
                let (date, time) = panel::date_cells(entry);
                cell(&painter, x_date, y, &date, color);
                cell(&painter, x_time, y, &time, color);
            }
            let resp = ui.interact(row_rect, list_id().with((i, idx)), Sense::click());
            if resp.double_clicked() {
                clicked = Some((idx, true));
            } else if resp.clicked() {
                clicked = Some((idx, false));
            }
        }
        chrome::rule(&painter, inner.left(), inner.right(), foot_y - 2.0);
        cell(&painter, inner.left(), foot_y, &panel.footer(), p.text);
        if let Some((idx, open)) = clicked {
            if self.task.is_none() && self.viewer.is_none() {
                self.active = i;
                self.panels[i].highlight = idx;
                if open {
                    self.enter(&ctx, false);
                }
            }
        }
    }

    /// The status row: a running fetch with its progress, else the last result, else the keys.
    fn status_line(&self) -> Status {
        match &self.task {
            Some(Task::Opening {
                name,
                remote,
                done,
                total,
                cancel,
                ..
            }) => {
                if cancel.load(Ordering::Relaxed) {
                    return Status::info(format!("cancelling {name}…"));
                }
                let (d, t) = (done.load(Ordering::Relaxed), total.load(Ordering::Relaxed));
                if *remote && t > 0 {
                    let pct = (d * 100).checked_div(t).unwrap_or(100);
                    Status::info(format!(
                        "fetching {name} {pct}% · {} / {} · Esc cancels",
                        crate::human(d),
                        crate::human(t)
                    ))
                } else {
                    Status::info(format!("opening {name}…"))
                }
            }
            Some(Task::Listing { dir, .. }) => Status::info(format!("listing {dir}…")),
            None if !self.status.text.is_empty() => self.status.clone(),
            None if self.viewer.is_some() => Status::info(VIEWER_HINT),
            None => Status::info(LIST_HINT),
        }
    }
}

impl Window for Browser {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Before anything else this pass: did this frame's Esc take the keyboard from a field?
        let esc_was_a_fields = self.focused_before && ctx.memory(|m| m.focused()).is_none();
        self.poll(&ctx);
        let keys: &[FKey] = if self.viewer.is_some() {
            chrome::VIEWER_KEYS
        } else {
            chrome::LIST_KEYS
        };
        egui::Panel::top("sopen-browser-title").show(ui, |ui| {
            ui.add_space(2.0);
            chrome::title_row(ui, &self.title);
            ui.add_space(1.0);
        });
        egui::Panel::bottom("sopen-browser-keys").show(ui, |ui| {
            ui.add_space(2.0);
            chrome::fkey_row(ui, keys);
            ui.add_space(2.0);
        });
        let status = self.status_line();
        egui::Panel::bottom("sopen-browser-status").show(ui, |ui| {
            ui.add_space(2.0);
            chrome::status_row(ui, &status);
            ui.add_space(1.0);
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                if self.viewer.is_some() {
                    self.viewer_ui(ui, esc_was_a_fields);
                } else {
                    self.list_keys(&ctx);
                    self.panels_ui(ui);
                }
            });
        if self.task.is_some() || !self.launches.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.focused_before = ctx.memory(|m| m.focused()).is_some();
        self.painted = true;
    }

    fn ready(&self) -> bool {
        self.painted
            && self.task.is_none()
            && self
                .viewer
                .as_ref()
                .is_none_or(|v| v.embedded.window.ready())
    }

    /// Closing the window: a viewer on screen is asked first (its prompt, its running save);
    /// a fetch is stopped.
    fn close_requested(&mut self) -> bool {
        if let Some(viewer) = self.viewer.as_mut() {
            self.exit = Exit::Quit;
            if !viewer.embedded.window.close_requested() {
                return false;
            }
            self.exit = Exit::List;
            self.drop_viewer();
        }
        if let Some(Task::Opening { cancel, .. }) = &self.task {
            cancel.store(true, Ordering::Relaxed);
        }
        true
    }
}

#[cfg(test)]
mod tests;
