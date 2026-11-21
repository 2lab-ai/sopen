//! The preview host (02 §4): in preview mode, the viewer of the file under the active list's bar,
//! drawn in the opposite side after the bar has rested 150 ms — told it is a preview
//! (`Window::set_preview(true)`), drawn disabled with the frame's events withheld, prepared one at
//! a time on the window's one preview connection, and handed over in place to a full viewer by
//! Enter or F3 (`set_preview(false)`). Every exit of 02 §4.4's table is defined here. S1-E wrote
//! it (S1-M adds its media row).
//!
//! - **What the side shows** is the browser's committed display ([`super::view`]): [`Host::sync`]
//!   is told it on every pass, with the file the bar is on right now. The bar leaving a file
//!   cancels its preparation at once; the side keeps what it drew — a finished preview as it
//!   is, a running one frozen with `…` — until the display commits something else.
//! - **One preparation at a time** ([`Prep`]): the next starts only once the worker before it
//!   has ended — its result came, or its channel closed — and was joined; a cancel flag alone
//!   frees nothing. So the window has at most one preview worker and one preview session.
//! - **The exits**: the bar on another file, a folder or `..`, F9, Tab — the display changes,
//!   the preparation is cancelled and the preview dropped (its private copy with it); F4
//!   changes nothing; Enter or F3 takes the preview away as a viewer ([`Host::take`]); the
//!   window closing cancels and does not wait ([`Host::cancel`]).

use super::prepare::{self, Budget, HandedOver, Preview, PreviewRequest, Previewed, Reuse};
use super::Connect;
use crate::cache::PreviewSpace;
use crate::gui::chrome;
use crate::remote::RemoteFs;
use anyhow::Result;
use eframe::egui::{self, Pos2, Rect, Vec2};
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
        self.worker.take()
    }
}

/// What the side draws for the file it shows.
enum Shown {
    /// The viewer, told it is a preview, or the info card.
    Ready(Box<Preview>),
    /// Why there is none (no preview folder, a worker that died).
    Failed(String),
}

/// What Enter or F3 on a file gets from the host (02 §4.4).
pub(super) enum Taken {
    /// The finished preview, a viewer now: handed over.
    Viewer(HandedOver),
    /// The file shows an info card: it opens as from the list, a complete private copy
    /// published instead of read again.
    Card(Option<Reuse>),
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
    /// The window's preview copies, made at the first preview.
    space: Option<Arc<PreviewSpace>>,
    /// The file the committed display shows a preview of.
    display: Option<Want>,
    shown: Option<Shown>,
    /// The one preparation: running, or cancelled and not ended yet.
    prep: Option<Prep>,
    /// What the frozen side says of a preparation that was cancelled.
    last_progress: String,
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
            space: None,
            display: None,
            shown: None,
            prep: None,
            last_progress: String::new(),
            #[cfg(test)]
            spawned: 0,
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
        // The bar left the file being prepared: cancelled at once, whatever the side shows.
        if let Some(prep) = &self.prep {
            if bar != Some(prep.want.path.as_str()) {
                prep.cancel.store(true, Ordering::Relaxed);
            }
        }
        if display != self.display {
            // Committed: the old preview is dropped (its private copy with it).
            self.shown = None;
            self.last_progress.clear();
            self.display = display;
        }
        if let Some(ended) = self.prep.as_mut().and_then(Prep::poll) {
            let prep = self.prep.take().expect("polled above");
            let ours = self.display.as_ref() == Some(&prep.want);
            match ended {
                Ended::Ready(previewed) if ours => {
                    self.shown = Some(Shown::Ready(Box::new(previewed.show())))
                }
                // Another file's: dropped, its copy removed.
                Ended::Ready(_) | Ended::Cancelled => {}
                Ended::Died if ours && !prep.cancelled() => {
                    let why = "no preview: its preparation stopped without a result";
                    self.shown = Some(Shown::Failed(why.to_string()));
                }
                Ended::Died => {}
            }
        }
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
        let space = match &self.space {
            Some(space) => space.clone(),
            None => match PreviewSpace::new(&self.cache) {
                Ok(space) => self.space.insert(Arc::new(space)).clone(),
                Err(e) => {
                    self.shown = Some(Shown::Failed(format!("no preview: {e:#}")));
                    return;
                }
            },
        };
        let (done, total) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let (cancel, budget) = (Arc::new(AtomicBool::new(false)), Budget::preview());
        let (tx, rx) = mpsc::channel();
        let (conn, connect, label, cache) = (
            self.conn.clone(),
            self.connect.clone(),
            self.label.clone(),
            self.cache.clone(),
        );
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
            let req = PreviewRequest {
                conn: &mut conn,
                connect: &connect,
                label: label.as_deref(),
                abs: &path,
                cache: &cache,
                space: &space,
            };
            let result = prepare::preview(req, &limits, &stop, &mut |now, all| {
                d.store(now, Ordering::Relaxed);
                t.store(all, Ordering::Relaxed);
                waker.request_repaint();
            });
            drop(conn);
            // Nobody may be listening (the window closed): the preview, and its copy, go here.
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
            rx,
            worker: Some(worker),
        });
    }

    /// Enter or F3 on the file at `path` (02 §4.4): what the host has of it is taken away —
    /// the finished preview as a viewer, a card's copy, or the running preparation with its
    /// budget lifted. A preparation of another file is cancelled.
    pub fn take(&mut self, path: &str) -> Taken {
        let shows = self.display.as_ref().is_some_and(|w| w.path == path);
        if shows {
            match self.shown.take() {
                Some(Shown::Ready(preview)) => {
                    return match preview.hand_over() {
                        Ok(viewer) => Taken::Viewer(viewer),
                        Err(card) => Taken::Card(card.into_reuse()),
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
    /// reads, and removes its own private copy).
    pub fn cancel(&mut self) {
        if let Some(prep) = &self.prep {
            prep.cancel.store(true, Ordering::Relaxed);
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

    /// The side: its frame — single-lined, the side does not have the keyboard — titled with
    /// the file, and in it the preview, drawn disabled with this pass's input withheld (02
    /// §4.4: no key, click, drag or wheel reaches a previewed viewer), or what is known of its
    /// preparation.
    pub fn ui(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let Some(want) = self.display.clone() else {
            return;
        };
        let ctx = ui.ctx().clone();
        let title = match &self.label {
            Some(label) => format!("{label}:{}", want.path),
            None => want.path.clone(),
        };
        let painter = ui.painter_at(rect);
        let inner = chrome::panel_frame(&painter, rect, &title, false);
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
            }
            Some(Shown::Failed(why)) => {
                let why = why.clone();
                say(ui, inner, &why);
            }
            None => {
                let line = self.progress(&want);
                say(ui, inner, &line);
            }
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

/// One line (wrapped at the side's width) at the top of the side.
fn say(ui: &egui::Ui, inner: Rect, text: &str) {
    let painter = ui.painter_at(inner);
    let p = chrome::Palette::of(ui.visuals());
    let galley = painter.layout(
        text.to_string(),
        chrome::mono(ui),
        p.text,
        inner.width().max(chrome::CELL.x),
    );
    painter.galley(Pos2::new(inner.left(), inner.top()), galley, p.text);
}

impl Drop for Host {
    /// The window is gone: a running preparation is told to stop; nothing waits for it.
    fn drop(&mut self) {
        self.cancel();
    }
}
