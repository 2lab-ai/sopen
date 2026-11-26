//! The preview host (02 §4): in preview mode, the viewer of the file under the active list's bar,
//! drawn in the opposite side after the bar has rested 150 ms — told it is a preview
//! (`Window::set_preview(true)`), drawn disabled with the frame's events withheld, prepared one at
//! a time on the window's one preview connection, and handed over in place to a full viewer by
//! Enter or F3 (`set_preview(false)`). Every exit of 02 §4.4's table is defined here. S1-E wrote
//! it; S1-M added the media row: a video's or a sound's preparation runs ffprobe and ffmpeg,
//! which a cancel stops — killed and reaped — as it stops a read.
//!
//! - **What the side shows** is the browser's committed display ([`super::view`]): [`Host::sync`]
//!   is told it on every pass, with the file the bar is on right now. The bar leaving a file
//!   cancels its preparation at once; the side keeps what it drew — a finished preview as it
//!   is, a running one frozen with `…` — until the display commits something else.
//! - **One preparation at a time** ([`Prep`]): the next starts only once the worker before it
//!   has ended — its result came, or its channel closed — and was joined; a cancel flag alone
//!   frees nothing. So the window has at most one preview worker and one preview session.
//! - **The exits**: the bar on another file, a folder or `..`, F9, Tab — the display changes,
//!   the preparation is cancelled and the preview dropped (a remote one's stream gives its
//!   connection back for the next preparation, 06 §13; nothing of it is on disk); F4 changes
//!   nothing; Enter or F3 takes the preview away as a viewer ([`Host::take`]); the window
//!   closing cancels and does not wait ([`Host::cancel`]).
//! - **Held, not drawn** (a viewer covers the panels): the preview is told
//!   (`Window::set_shown(false)`), so a streamed one does not poll its file (06 §5).
//! - **The side in cells** ([`Host::ui`], 10 §17's X3-preview): its frame titled with the file,
//!   and the line of what is known of its preparation, wrapped by cells; a text or hex preview
//!   paints its own cells inside the frame. A preview egui draws keeps egui's frame until X6.

use super::ops::dialog;
use super::prepare::{self, Budget, HandedOver, Pages, Preview, PreviewRequest, Previewed};
use super::Connect;
use crate::gui::cells::{self, Buffer, Cells, Grid, Pen};
#[cfg(test)]
use crate::gui::Window;
use crate::gui::{chrome, help};
use crate::kind::Kind;
use crate::remote::RemoteFs;
use anyhow::Result;
use eframe::egui::{self, Vec2};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A file to preview: its path, and its name as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Want {
    pub path: String,
    pub name: String,
}

/// A preparation on its worker.
pub(super) struct Prep {
    pub want: Want,
    /// Stops it (the bar left, the window closes; Esc once it is a viewer's opening).
    pub cancel: Arc<AtomicBool>,
    /// Its budget (02 §4.3), lifted when Enter makes it the viewer's opening.
    budget: Arc<Budget>,
    /// A remote read's progress.
    pub done: Arc<AtomicU64>,
    pub total: Arc<AtomicU64>,
    /// The answer of [`Pages::here`] it runs under.
    pages: Pages,
    rx: Receiver<Result<Previewed>>,
    worker: Option<JoinHandle<()>>,
}

/// How a preparation ended.
pub(super) enum Ended {
    Ready(Box<Previewed>),
    /// Cancelled: nothing was kept.
    Cancelled,
    /// Its worker ended without a result.
    Died,
}

impl Prep {
    /// Its end, once its worker has ended (joined here); `None` while it runs.
    pub fn poll(&mut self) -> Option<Ended> {
        let ended = match self.rx.try_recv() {
            Ok(Ok(previewed)) => Ended::Ready(Box::new(previewed)),
            Ok(Err(_)) => Ended::Cancelled,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Ended::Died,
        };
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        Some(ended)
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// The worker, for whoever waits for it after the window (a handed-over opening).
    pub fn into_worker(mut self) -> Option<JoinHandle<()>> {
        self.cancel.store(true, Ordering::Relaxed);
        self.budget.stop_children();
        self.worker.take()
    }
}

/// What the side draws for the file it shows.
enum Shown {
    /// The viewer, told it is a preview, or the info card.
    Ready(Box<Preview>),
    /// Why there is none (a worker that died).
    Failed(String),
}

/// How the side is framed (10 §4): in cells, around a preview that paints cells and the lines of
/// one being prepared; or by egui, around a preview egui draws and its lines — one hand-off box
/// in a terminal, never a row of cells inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    Cells,
    Egui,
}

/// What Enter or F3 on a file gets from the host (02 §4.4).
pub(super) enum Taken {
    /// The finished preview, a viewer now: handed over.
    Viewer(HandedOver),
    /// The file shows an info card: it opens as from the list.
    Card,
    /// Still being prepared: the same preparation goes on as the viewer's opening, its budget
    /// lifted.
    Running(Prep),
    /// Nothing of it here: it opens as from the list.
    Nothing,
}

pub(super) struct Host {
    connect: Connect,
    /// `[user@]host`; `None` on this machine.
    label: Option<String>,
    cache: PathBuf,
    /// The window's one preview connection (02 §4.4): opened by the first preparation, kept
    /// until the window closes. Only the one worker ever holds the lock.
    conn: Arc<Mutex<Option<Box<dyn RemoteFs + Send>>>>,
    /// Where that connection comes back from while a preview's stream reads on it (06 §13).
    back: Arc<prepare::Back>,
    /// The preview was drawn this pass ([`Host::ui`]); looked at by the next [`Host::sync`].
    drawn: bool,
    /// The file the committed display shows a preview of.
    display: Option<Want>,
    shown: Option<Shown>,
    /// How the side framed the last ready preview of the display's file: kept while that file is
    /// prepared again ([`Host::refresh`]), so the side does not change frames meanwhile; cleared
    /// when the display commits another file, or its page is made for another backend.
    framed: Option<Frame>,
    /// The one preparation: running, or cancelled and not ended yet.
    prep: Option<Prep>,
    /// What the frozen side says of a preparation that was cancelled.
    last_progress: String,
    /// Makes a preparation's budget (02 §4.3's; tests give one on a clock of their own).
    budget: Box<dyn Fn() -> Arc<Budget>>,
    /// The folder the browser was opened on: a page's root when the page is inside it (09
    /// §3.1).
    opened_on: Option<String>,
    /// A page view the engine refused in this window: page files preview as their source.
    page_failed: Option<crate::gui::web::Unavailable>,
    /// The engine refused the preview's page in the last pass: for the window to say.
    refused: Option<crate::gui::web::Unavailable>,
    #[cfg(test)]
    spawned: usize,
}

impl Host {
    pub fn new(connect: Connect, label: Option<String>, cache: PathBuf) -> Self {
        Self {
            connect,
            label,
            cache,
            conn: Arc::default(),
            back: Arc::default(),
            drawn: false,
            display: None,
            shown: None,
            framed: None,
            prep: None,
            last_progress: String::new(),
            budget: Box::new(Budget::preview),
            opened_on: None,
            page_failed: None,
            refused: None,
            #[cfg(test)]
            spawned: 0,
        }
    }

    /// The folder the browser was opened on (09 §3.1's root).
    pub fn set_opened_on(&mut self, folder: String) {
        self.opened_on = Some(folder);
    }

    /// The engine refused the preview's page (09 §4) since the last call: why. The preview shows
    /// the source in its place already.
    pub fn take_refused(&mut self) -> Option<crate::gui::web::Unavailable> {
        self.refused.take()
    }

    /// The engine refused a page view in this window (09 §4): page files preview as their source
    /// from the next preparation on.
    pub fn set_page_failed(&mut self, why: crate::gui::web::Unavailable) {
        self.page_failed = Some(why);
    }

    /// Tests: the budget each preparation gets.
    #[cfg(test)]
    pub fn set_budget(&mut self, budget: Box<dyn Fn() -> Arc<Budget>>) {
        self.budget = budget;
    }

    /// Tests: the preparation's worker has ended (its result, if any, is in the channel).
    #[cfg(test)]
    pub fn worker_ended(&self) -> bool {
        let worker = self.prep.as_ref().and_then(|p| p.worker.as_ref());
        worker.is_none_or(|w| w.is_finished())
    }

    /// Tests: which window the side shows — the address of the finished preview's window (it
    /// stays the same window through the hand-over).
    #[cfg(test)]
    pub fn shown_window(&self) -> Option<usize> {
        match &self.shown {
            Some(Shown::Ready(preview)) => {
                Some(&*preview.embedded.window as *const dyn Window as *const () as usize)
            }
            _ => None,
        }
    }

    /// Tests: a preparation is there and was told to stop.
    #[cfg(test)]
    pub fn cancelling(&self) -> bool {
        self.prep.as_ref().is_some_and(Prep::cancelled)
    }

    /// Tests: the window's preview session is back from what borrowed it (06 §13) — its
    /// connection waits in `back` for the next preparation. Looked at without taking it: what is
    /// there is put back as it was.
    #[cfg(test)]
    pub fn session_back(&self) -> bool {
        let mut back = self.back.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rx) = back.take() else {
            return false;
        };
        match rx.try_recv() {
            Ok(conn) => {
                let (tx, again) = mpsc::channel();
                let _ = tx.send(conn);
                *back = Some(again);
                true
            }
            Err(_) => {
                *back = Some(rx);
                false
            }
        }
    }

    /// The file at `path` changed (a re-read found another size or time): a preview of it is
    /// prepared again — what is shown is dropped, a preparation of it cancelled.
    pub fn refresh(&mut self, path: &str) {
        if self.display.as_ref().is_some_and(|w| w.path == path) {
            self.shown = None;
            self.last_progress.clear();
        }
        if let Some(prep) = self.prep.as_ref().filter(|p| p.want.path == path) {
            prep.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Tests: how many preparation workers were started.
    #[cfg(test)]
    pub fn spawned(&self) -> usize {
        self.spawned
    }

    /// Each pass: `display` is the file the committed display shows a preview of (`None`: the
    /// side shows something else), `bar` the file the bar is on right now.
    pub fn sync(&mut self, ctx: &egui::Context, display: Option<Want>, bar: Option<&str>) {
        // A preview held but not drawn last pass (a viewer covers the panels) is told so, and
        // told again when it is back: hidden, a streamed one does not poll its file (06 §5).
        let drawn = std::mem::take(&mut self.drawn);
        if let Some(Shown::Ready(preview)) = &mut self.shown {
            preview.set_shown(drawn);
        }
        // The bar left the file being prepared, or the display stopped showing its preview
        // (F9, Tab, a pin: 02 §4.4): cancelled at once, whatever the side still draws.
        if let Some(prep) = &self.prep {
            let path = Some(prep.want.path.as_str());
            if bar != path || display.as_ref().map(|w| w.path.as_str()) != path {
                prep.cancel.store(true, Ordering::Relaxed);
            }
        }
        if display != self.display {
            // Committed: the old preview is dropped (its stream with it).
            self.shown = None;
            self.framed = None;
            self.last_progress.clear();
            self.display = display;
        }
        if let Some(ended) = self.prep.as_mut().and_then(Prep::poll) {
            let prep = self.prep.take().expect("polled above");
            // A cancelled preparation's result is never shown, its file still on display or
            // not: the side stays frozen until the display commits (02 §3.1).
            let ours = self.display.as_ref() == Some(&prep.want) && !prep.cancelled();
            match ended {
                Ended::Ready(previewed) if ours => {
                    self.shown = Some(Shown::Ready(Box::new(previewed.show())));
                    // On screen until a pass goes by without drawing it.
                    self.drawn = true;
                }
                // Another file's: dropped.
                Ended::Ready(_) | Ended::Cancelled => {}
                Ended::Died if ours => {
                    let why = "no preview: its preparation stopped without a result";
                    self.shown = Some(Shown::Failed(why.to_string()));
                }
                Ended::Died => {}
            }
        }
        self.follow_backend(ctx);
        // The next starts only now: the worker before it has ended. And only for a file the
        // bar is still on.
        let wanted = self
            .display
            .clone()
            .filter(|w| bar == Some(w.path.as_str()));
        if let (Some(want), None, None) = (wanted, &self.shown, &self.prep) {
            self.start(ctx, want);
        }
    }

    fn start(&mut self, ctx: &egui::Context, want: Want) {
        let (done, total) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let (cancel, budget) = (Arc::new(AtomicBool::new(false)), (self.budget)());
        let (tx, rx) = mpsc::channel();
        // The window's preview session, as a remote page lends it (06 §13).
        let session = crate::gui::web::page::PreviewSession {
            conn: self.conn.clone(),
            back: self.back.clone(),
        };
        let (conn, back, connect, label, cache) = (
            self.conn.clone(),
            self.back.clone(),
            self.connect.clone(),
            self.label.clone(),
            self.cache.clone(),
        );
        let began = std::time::Instant::now();
        // 09 §4–§6, on the frame: whether a page file shows its page here.
        let opened_on = self.opened_on.clone();
        let pages = Pages::here(ctx, &want.name, opened_on, self.page_failed.as_ref());
        let under = pages.clone();
        let (d, t, stop, limits, waker, path) = (
            done.clone(),
            total.clone(),
            cancel.clone(),
            budget.clone(),
            ctx.clone(),
            want.path.clone(),
        );
        let worker = std::thread::spawn(move || {
            let mut conn = conn.lock().unwrap_or_else(|p| p.into_inner());
            let repaint = waker.clone();
            let wake: crate::stream::Waker = Arc::new(move || repaint.request_repaint());
            let req = PreviewRequest {
                conn: &mut conn,
                connect: &connect,
                label: label.as_deref(),
                abs: &path,
                cache: &cache,
                back: &back,
                wake: &wake,
                began,
            };
            let result = prepare::preview_paged(
                req,
                &pages,
                Some(session),
                &limits,
                &stop,
                &mut |now, all| {
                    d.store(now, Ordering::Relaxed);
                    t.store(all, Ordering::Relaxed);
                    waker.request_repaint();
                },
            );
            drop(conn);
            // Nobody may be listening (the window closed): the preview, and its stream, go here.
            let _ = tx.send(result);
            waker.request_repaint();
        });
        #[cfg(test)]
        {
            self.spawned += 1;
        }
        self.last_progress.clear();
        self.prep = Some(Prep {
            want,
            cancel,
            budget,
            done,
            total,
            pages: under,
            rx,
            worker: Some(worker),
        });
    }

    /// How a page file named `name` opens in `ctx`'s backend, as this host asks it.
    fn pages_here(&self, ctx: &egui::Context, name: &str) -> Pages {
        let opened_on = self.opened_on.clone();
        Pages::here(ctx, name, opened_on, self.page_failed.as_ref())
    }

    /// Whether `previewed`, of the file named `name`, was prepared as `ctx`'s backend prepares
    /// it: any file but a page file, or a page file under the same [`Pages::here`] answer (a
    /// window's page or source, a terminal's source: 10 T21).
    pub fn fits(&self, ctx: &egui::Context, name: &str, previewed: &Previewed) -> bool {
        !crate::kind::page_file(name) || *previewed.made_under() == self.pages_here(ctx, name)
    }

    /// 10 T21: a page file's preview is the one `ctx`'s backend makes. One made under another
    /// answer (the backend switched since) is dropped, and a preparation under it stopped, so
    /// the preview is made again here — what Enter hands over is what an open here would make.
    fn follow_backend(&mut self, ctx: &egui::Context) {
        let Some(want) = self.display.as_ref() else {
            return;
        };
        if !crate::kind::page_file(&want.name) {
            return;
        }
        let here = self.pages_here(ctx, &want.name);
        if let Some(Shown::Ready(preview)) = &self.shown {
            if *preview.made_under() != here {
                self.shown = None;
                self.framed = None;
                self.last_progress.clear();
            }
        }
        if let Some(prep) = self.prep.as_ref().filter(|p| p.pages != here) {
            prep.cancel.store(true, Ordering::Relaxed);
            // What is prepared next is this backend's: its frame is decided again.
            self.framed = None;
        }
    }

    /// Enter or F3 on the file at `path` (02 §4.4): what the host has of it is taken away —
    /// the finished preview as a viewer, a card, or the running preparation with its
    /// budget lifted. A preparation of another file is cancelled, and so is a page file's
    /// preview made for the other backend (`ctx`'s answer is another: 10 T21).
    pub fn take(&mut self, ctx: &egui::Context, path: &str) -> Taken {
        self.follow_backend(ctx);
        let shows = self.display.as_ref().is_some_and(|w| w.path == path);
        if shows {
            match self.shown.take() {
                Some(Shown::Ready(preview)) => {
                    return match preview.hand_over() {
                        Ok(viewer) => Taken::Viewer(viewer),
                        Err(_card) => Taken::Card,
                    }
                }
                Some(Shown::Failed(_)) | None => {}
            }
        }
        match self.prep.take() {
            Some(prep) if prep.want.path == path && !prep.cancelled() => {
                prep.budget.lift();
                Taken::Running(prep)
            }
            Some(prep) => {
                prep.cancel.store(true, Ordering::Relaxed);
                self.prep = Some(prep);
                Taken::Nothing
            }
            None => Taken::Nothing,
        }
    }

    /// The window closes: the preparation is cancelled; nothing waits for its worker (it only
    /// reads, and writes nothing) — so an ffprobe or ffmpeg it runs for a video
    /// or a sound is killed and reaped here, at once: the process may end before the worker
    /// would have looked at its stop flag.
    pub fn cancel(&mut self) {
        if let Some(prep) = &self.prep {
            prep.cancel.store(true, Ordering::Relaxed);
            prep.budget.stop_children();
        }
    }

    /// What the display asks for is on screen: nothing, the preview (drawn once), or why not.
    pub fn settled(&self) -> bool {
        match (&self.display, &self.shown) {
            (None, _) => true,
            (Some(_), Some(Shown::Ready(preview))) => preview.embedded.window.ready(),
            (Some(_), Some(Shown::Failed(_))) => true,
            (Some(_), None) => false,
        }
    }

    /// A preparation is running (the window repaints to see its end).
    pub fn busy(&self) -> bool {
        self.prep.is_some()
    }

    /// The side at cells `side` of `grid` (10 §4): its frame — single-lined, the side does not
    /// have the keyboard — titled with the file, and in it the preview, drawn disabled with this
    /// pass's input withheld (02 §4.4: no key, click, drag or wheel reaches a previewed viewer), or
    /// what is known of its preparation. The frame and that line are cells — those `under` a
    /// dialog of the list's, and under the key help, blanked first (cells.rs: one buffer over
    /// another) — and a text or hex preview paints its own cells inside them. A preview egui
    /// still draws (an image, a video, a sound, a page, the info card) keeps egui's frame, its
    /// line egui's too, until X6: its side is egui's whole, one hand-off box in a terminal, never
    /// a box in a frame. Which frame ([`Frame`]): a ready preview's own; else the last ready one's
    /// of the same file (it is prepared again); else the file's name's ([`Host::frame_by_name`]).
    pub fn ui(&mut self, ui: &mut egui::Ui, grid: &Grid, side: Cells, under: &[Cells]) {
        let Some(want) = self.display.clone() else {
            return;
        };
        let ctx = ui.ctx().clone();
        let title = match &self.label {
            Some(label) => format!("{label}:{}", want.path),
            None => want.path.clone(),
        };
        let rect = grid.rect(side);
        let painter = ui.painter_at(rect);
        let line = match &self.shown {
            Some(Shown::Ready(_)) => None,
            Some(Shown::Failed(why)) => Some(why.clone()),
            None => Some(self.progress(&want)),
        };
        let frame = match &self.shown {
            Some(Shown::Ready(preview)) if in_cells(preview) => Frame::Cells,
            Some(Shown::Ready(_)) => Frame::Egui,
            _ => match self.framed {
                Some(frame) => frame,
                None => self.frame_by_name(&ctx, &want.name),
            },
        };
        let inner = match frame {
            Frame::Egui => {
                let inner = chrome::panel_frame(&painter, rect, &title, false);
                if let Some(line) = &line {
                    say_in_egui(ui, inner, line);
                }
                inner
            }
            Frame::Cells => {
                let p = chrome::PALETTE;
                let body = Pen::new(p.text, p.back);
                let mut buf = Buffer::empty(side);
                let inner = cells::frame(&mut buf, side, &title, false);
                if let Some(line) = &line {
                    say(&mut buf, inner, line);
                }
                for r in under.iter().copied().chain(help::covered(&ctx, grid)) {
                    cells::fill(&mut buf, r, body);
                }
                // The side's body under its cells, as the list fills its own.
                painter.rect_filled(rect, 0.0, p.back);
                cells::paint(&painter, grid.origin, &buf, None);
                grid.rect(inner)
            }
        };
        match &mut self.shown {
            Some(Shown::Ready(preview)) => {
                let saved = ctx.input(|i| i.clone());
                ctx.input_mut(|i| {
                    i.events.clear();
                    i.pointer = Default::default();
                    i.smooth_scroll_delta = Vec2::ZERO;
                    i.keys_down.clear();
                });
                let builder = egui::UiBuilder::new()
                    .id_salt("sopen-browser-preview")
                    .max_rect(inner);
                ui.scope_builder(builder, |ui| {
                    // Disabled, not greyed: it is looked at.
                    ui.visuals_mut().disabled_alpha = 1.0;
                    ui.disable();
                    ui.set_clip_rect(inner);
                    preview.embedded.window.ui(ui);
                });
                ctx.input_mut(|i| *i = saved);
                self.drawn = true;
                self.framed = Some(frame);
                // 09 §4: the engine refused its page — the source in its place, at once. The
                // page's server ended with the view it never had: the preview session comes
                // back for the next preparation, which is the source's: its frame is decided
                // again.
                if let Some(why) = crate::gui::web::page::take_preview_failure(&ctx) {
                    self.page_failed = Some(why.clone());
                    self.refused = Some(why);
                    self.shown = None;
                    self.framed = None;
                    self.last_progress.clear();
                    ctx.request_repaint();
                }
            }
            // Its line is in the frame, drawn above.
            Some(Shown::Failed(_)) | None => {}
        }
    }

    /// The frame of a side whose file, named `name`, has no ready preview yet: egui's where
    /// what is made of it is egui's by its name — a picture, a video or a sound by their
    /// extension, a page file whose page shows here (09 §4) — and cells otherwise. A name that
    /// does not tell (a picture with no extension, a file over the preview's limit that becomes
    /// the info card) changes frames once, when its preview is ready.
    fn frame_by_name(&self, ctx: &egui::Context, name: &str) -> Frame {
        let egui = matches!(
            crate::kind::by_extension(name),
            Some(Kind::Image | Kind::Video | Kind::Audio)
        ) || (crate::kind::page_file(name) && self.pages_here(ctx, name).shown());
        if egui {
            Frame::Egui
        } else {
            Frame::Cells
        }
    }

    /// What the side says while its file is not ready: the read's progress, `…` once the
    /// preparation was cancelled (the side is frozen until the display commits) or while it
    /// waits for the one before it to end.
    fn progress(&mut self, want: &Want) -> String {
        let name = &want.name;
        match &self.prep {
            Some(prep) if prep.want == *want && !prep.cancelled() => {
                let (d, t) = (
                    prep.done.load(Ordering::Relaxed),
                    prep.total.load(Ordering::Relaxed),
                );
                self.last_progress = if t > 0 {
                    let pct = (d * 100).checked_div(t).unwrap_or(100);
                    format!(
                        "reading {name} {pct}% · {} / {}",
                        crate::human(d),
                        crate::human(t)
                    )
                } else {
                    format!("reading {name}")
                };
                format!("{}…", self.last_progress)
            }
            _ if self.last_progress.is_empty() => format!("{name} …"),
            _ => format!("{} …", self.last_progress),
        }
    }
}

/// Whether `preview` paints its own cells (10 §4, [`crate::gui::Window::paints_cells`]): the
/// text and hex views, in cells since X3-text and X3-hex, a page file's source among them (how
/// a terminal previews a page, VM-8). An image, a video, a sound, a page and the info card are
/// egui's until X6.
fn in_cells(preview: &Preview) -> bool {
    preview.embedded.window.paints_cells()
}

/// `text` at the top of `inner` in an egui frame, wrapped at its width by egui: in a terminal the
/// side is one hand-off box, and no row of cells cuts it into strips.
fn say_in_egui(ui: &egui::Ui, inner: egui::Rect, text: &str) {
    let painter = ui.painter_at(inner);
    let p = chrome::PALETTE;
    let width = inner.width().max(chrome::CELL.x);
    let galley = painter.layout(text.to_string(), chrome::mono(ui), p.text, width);
    painter.galley(inner.min, galley, p.text);
}

/// `text` at the top of `inner`, wrapped by cells at its width (`dialog::wrap`: at a blank, a
/// word longer than a row after its last whole glyph), as many rows as it has.
fn say(buf: &mut Buffer, inner: Cells, text: &str) {
    let p = chrome::PALETTE;
    let room = usize::from(inner.width);
    if room == 0 {
        return;
    }
    for (y, row) in (inner.top()..inner.bottom()).zip(dialog::wrap(text, room)) {
        cells::put(buf, inner.x, y, &row, room, Pen::new(p.text, p.back));
    }
}

impl Drop for Host {
    /// The window is gone: a running preparation is told to stop; nothing waits for it.
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod cells_tests;

#[cfg(test)]
mod tests {
    //! The host's own isolation (02 §4.4), on a stand-in window that reacts to everything it is
    //! given: the merged viewers refuse input themselves once told they are previews, so only a
    //! window that does not can show what the host withholds.

    use super::*;
    use crate::gui::testkit::Driver;
    use crate::gui::{Embedded, Outcome, Window};
    use eframe::egui::{Event, Key, Modifiers, Pos2};

    /// What the stand-in was given, over all its passes.
    #[derive(Default, Debug, PartialEq)]
    struct Got {
        events: usize,
        pointer_seen: bool,
        scrolled: bool,
        button_clicks: usize,
        enabled: bool,
    }

    /// The stand-in, painting cells (`true`: its side is framed in cells) or not.
    struct Eager(Arc<Mutex<Got>>, bool);

    impl Window for Eager {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let mut got = self.0.lock().unwrap();
            ui.ctx().input(|i| {
                got.events += i
                    .events
                    .iter()
                    .filter(|e| !matches!(e, Event::ModifiersChanged(_)))
                    .count();
                got.pointer_seen |= i.pointer.any_down() || i.pointer.any_click();
                got.scrolled |= i.smooth_scroll_delta != Vec2::ZERO;
            });
            got.enabled |= ui.is_enabled();
            let button = ui.put(ui.max_rect(), egui::Button::new("press me"));
            got.button_clicks += usize::from(button.clicked());
        }

        fn ready(&self) -> bool {
            true
        }

        fn paints_cells(&self) -> bool {
            self.1
        }
    }

    /// The host around the stand-in, as the browser draws a side; what the rest of the pass
    /// still sees of the frame's input after the preview's pass.
    struct Side {
        host: Host,
        want: Want,
        after: usize,
    }

    impl Window for Side {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let ctx = ui.ctx().clone();
            self.host
                .sync(&ctx, Some(self.want.clone()), Some(&self.want.path));
            let grid = Grid::over(ui.max_rect());
            self.host.ui(ui, &grid, grid.all(), &[]);
            self.after += ctx.input(|i| {
                i.events
                    .iter()
                    .filter(|e| !matches!(e, Event::ModifiersChanged(_)))
                    .count()
            });
        }

        fn ready(&self) -> bool {
            true
        }
    }

    /// What a stand-in was told of being on screen, in order; it paints cells or not.
    struct Told(Arc<Mutex<Vec<bool>>>, bool);

    impl Window for Told {
        fn ui(&mut self, _: &mut egui::Ui) {}

        fn ready(&self) -> bool {
            true
        }

        fn paints_cells(&self) -> bool {
            self.1
        }

        fn set_shown(&mut self, shown: bool) {
            self.0.lock().unwrap().push(shown);
        }
    }

    /// The host's passes as the browser makes them: `sync` every pass, the side drawn only
    /// while the panels show (not while a viewer covers them).
    struct Covered {
        host: Host,
        want: Want,
        panels: bool,
    }

    impl Window for Covered {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let ctx = ui.ctx().clone();
            self.host
                .sync(&ctx, Some(self.want.clone()), Some(&self.want.path));
            if self.panels {
                let grid = Grid::over(ui.max_rect());
                self.host.ui(ui, &grid, grid.all(), &[]);
            }
        }

        fn ready(&self) -> bool {
            true
        }
    }

    /// Whether the last pass framed the side (all of `drv`'s grid) in cells.
    fn framed_in_cells(drv: &Driver) -> bool {
        let all = Grid::over(drv.ctx.content_rect()).all();
        cells::layers(&drv.ctx).iter().any(|l| l.buf.area == all)
    }

    /// 06 §5 (W8a): a preview the host holds but does not draw — a viewer covers the panels —
    /// is told it is hidden (`Window::set_shown(false)`: a streamed one then stops polling),
    /// once; drawn again, it is told so; and the hand-over makes it shown, whatever it was —
    /// framed in cells or by egui.
    #[test]
    fn a_preview_held_but_not_drawn_is_told_it_is_hidden() {
        for cells in [false, true] {
            told_when_hidden(cells);
        }
    }

    fn told_when_hidden(cells: bool) {
        let told = Arc::new(Mutex::new(Vec::new()));
        let want = Want {
            path: "/d/a.txt".to_string(),
            name: "a.txt".to_string(),
        };
        let connect: Connect = Arc::new(|| anyhow::bail!("no connection in this test"));
        let mut host = Host::new(connect, None, PathBuf::from("/nonexistent"));
        host.display = Some(want.clone());
        host.shown = Some(Shown::Ready(Box::new(Preview::standing_in(Embedded {
            window: Box::new(Told(told.clone(), cells)),
            settle: Box::new(|_| Some(Outcome::default())),
        }))));
        // As `sync` leaves it when a preparation's preview comes ready.
        host.drawn = true;
        let mut side = Covered {
            host,
            want,
            panels: true,
        };
        let mut drv = Driver::new(500.0, 400.0);
        let said = || told.lock().unwrap().clone();
        drv.idle(&mut side, 5);
        assert_eq!(framed_in_cells(&drv), cells, "its frame");
        assert_eq!(said(), [] as [bool; 0], "on screen: nothing to tell");
        side.panels = false;
        drv.idle(&mut side, 5);
        assert_eq!(said(), [false], "hidden, told once");
        side.panels = true;
        drv.idle(&mut side, 5);
        assert_eq!(said(), [false, true], "back on screen");
        // Enter while it is hidden: the viewer it becomes is on screen.
        side.panels = false;
        drv.idle(&mut side, 3);
        assert_eq!(said(), [false, true, false]);
        let Taken::Viewer(_viewer) = side.host.take(&drv.ctx, "/d/a.txt") else {
            panic!("not handed over")
        };
        assert_eq!(said(), [false, true, false, true], "a viewer is shown");
        assert_eq!(side.host.spawned(), 0, "nothing was prepared");
    }

    /// No key, typed text, click, drag or wheel reaches a previewed window, and its widgets
    /// are disabled — whatever the window itself would do with them; the frame's input is
    /// whole again after its pass (the list's keys are read from it). The same in both of the
    /// side's frames: egui's, and cells around a window that paints cells.
    #[test]
    fn the_host_withholds_the_frames_input_from_a_preview() {
        for cells in [false, true] {
            withheld(cells);
        }
    }

    fn withheld(cells: bool) {
        let got = Arc::new(Mutex::new(Got::default()));
        let want = Want {
            path: "/d/a.txt".to_string(),
            name: "a.txt".to_string(),
        };
        let connect: Connect = Arc::new(|| anyhow::bail!("no connection in this test"));
        let mut host = Host::new(connect, None, PathBuf::from("/nonexistent"));
        host.display = Some(want.clone());
        host.shown = Some(Shown::Ready(Box::new(Preview::standing_in(Embedded {
            window: Box::new(Eager(got.clone(), cells)),
            settle: Box::new(|_| Some(Outcome::default())),
        }))));
        let mut side = Side {
            host,
            want,
            after: 0,
        };
        let mut drv = Driver::new(500.0, 400.0);
        let centre = Pos2::new(250.0, 200.0);
        drv.idle(&mut side, 2);
        assert_eq!(framed_in_cells(&drv), cells, "its frame");
        drv.key(&mut side, Key::Space, Modifiers::NONE);
        drv.frame(&mut side, vec![Event::Text("abc".into())], Modifiers::NONE);
        drv.click(&mut side, centre);
        drv.drag(
            &mut side,
            egui::PointerButton::Primary,
            centre - Vec2::new(50.0, 50.0),
            centre + Vec2::new(50.0, 50.0),
        );
        drv.wheel(&mut side, centre, -4.0);
        drv.paste_text(&mut side, "pasted");
        assert_eq!(
            *got.lock().unwrap(),
            Got::default(),
            "the preview was given nothing, and was never enabled"
        );
        assert!(drv.find_text("press me").is_some(), "it was drawn");
        assert!(
            side.after >= 8,
            "the frame's input is whole after it: {}",
            side.after
        );
        assert_eq!(side.host.spawned(), 0, "nothing was prepared");
    }
}
