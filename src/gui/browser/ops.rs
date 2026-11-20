//! The browser's side of the file operations (04 §4–§5): the select-group dialog (`+`, `-`),
//! the Copy / Move dialog, the inline rename field, the name-in-the-way dialog, the progress line
//! and the report — and drawing helpers for the rename field and the selection colours, which
//! the browser's `panel_ui` (S1-E) calls. The selection itself is the panel model's
//! ([`Panel::toggle`] and its neighbours). The work is [`crate::fileops`]'s, behind [`Run`]. S1-D
//! writes it (S6-B the make-folder and delete dialogs).
//!
//! The browser holds one [`Ops`] and calls it: `open_*` from its commands, [`Ops::rename_field`]
//! in the row being renamed, [`Ops::ui`] after the panels, [`Ops::status`] for the status row,
//! [`Ops::holds_keyboard`] to keep its own keys and its claim on the keyboard back meanwhile
//! (04 §3 rule 4). Every dialog takes the keyboard back to its field on each pass while the focus
//! is outside it, and consumes the Enter and Esc it acts on.
//!
//! S1-0 scaffold: the hook 02 §7 names. Nothing uses it until the integration (S1-E); the
//! `dead_code` allowance below goes then, as a hand-over declared with it (this file is S1-D's).
#![allow(dead_code)]

use super::panel::{self, Panel};
use crate::fileops::{Answer, Clash, Item, ItemOutcome, Progress, Reply, Report};
use crate::gui::{chrome, Status};
use crate::remote::{Entry, EntryKind};
use eframe::egui::{self, Color32, Key, Modifiers, Rect, RichText};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Where F5 and F6 copy or move to (02 §7, 04 §5.2): the other panel's own folder — even when
/// that side shows a preview or the git panel — which the dialog names and prefills in sopen's
/// command-line form (`host:/abs`, a plain path for this machine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsTarget {
    /// `[user@]host` as the browser prints it (an IPv6 host bracketed); `None` on this machine.
    pub host: Option<String>,
    /// The folder (absolute).
    pub dir: String,
}

impl OpsTarget {
    /// sopen's command-line form (01 A1): `host:/abs`, or the plain path on this machine.
    pub fn shown(&self) -> String {
        match &self.host {
            Some(host) => format!("{host}:{}", self.dir),
            None => self.dir.clone(),
        }
    }

    /// What the target field holds, read as sopen reads its command line
    /// ([`crate::target::parse`]): `host:/abs` is that host's folder, a plain absolute path one
    /// on this machine, a relative path is relative to `from` (the source folder, on its host).
    /// A remote folder must be given absolute (nothing is looked up here: a home-relative
    /// `host:dir` is refused with the reason), and so must a folder under `~` (the shell's, not
    /// sopen's). Trailing slashes go; whether the folder exists is the engine's to say (none is
    /// created, 04 §5.2).
    pub fn parse(text: &str, from: &OpsTarget) -> Result<OpsTarget, String> {
        if text.is_empty() {
            return Err("no target folder: give one (host:/abs, or a path here)".to_string());
        }
        let dir = |path: &str| match path.trim_end_matches('/') {
            "" => "/".to_string(),
            kept => kept.to_string(),
        };
        match crate::target::parse(text)? {
            crate::target::Target::Remote(spec) if spec.path.starts_with('/') => Ok(OpsTarget {
                host: Some(spec.label()),
                dir: dir(&spec.path),
            }),
            crate::target::Target::Remote(spec) => Err(format!(
                "{text}: give the folder on {host} as an absolute path ({host}:/…)",
                host = spec.label()
            )),
            crate::target::Target::Local(_) if text.starts_with('/') => Ok(OpsTarget {
                host: None,
                dir: dir(text),
            }),
            crate::target::Target::Local(_) if text.starts_with('~') => Err(format!(
                "{text}: give the folder as an absolute path (~ is the shell's)"
            )),
            crate::target::Target::Local(_) => Ok(OpsTarget {
                host: from.host.clone(),
                dir: dir(&crate::remote::join(&from.dir, text)),
            }),
        }
    }
}

/// The text of a selected entry, on the highlight bar too: yellow, as PC Shell draws it (04 §4).
///
/// Placeholder for the palette's own entry, which S1-A adds to `chrome.rs` in parallel: until
/// then the colour is chrome's CGA yellow, taken here and nowhere else. When S1-A's
/// `Palette::selected` lands, this body becomes `p.selected` (one line, at the merge).
pub fn selected_color(p: &chrome::Palette) -> Color32 {
    let _ = p;
    chrome::cga::YELLOW
}

/// The colour of a panel row's text: a selected entry's is [`selected_color`] (on the bar too);
/// else the bar's text on the active panel's bar, the dim colour for a dot file, the folder
/// colour, the plain text — `panel_ui`'s rule (browser.rs) with the selection first.
pub fn entry_color(
    p: &chrome::Palette,
    entry: &Entry,
    selected: bool,
    on_active_bar: bool,
) -> Color32 {
    if selected {
        selected_color(p)
    } else if on_active_bar {
        p.bar_text
    } else if panel::hidden(entry) {
        p.dim
    } else if entry.kind == EntryKind::Dir {
        p.folder
    } else {
        p.text
    }
}

/// A name the browser may give (04 §5.1): not empty, no `/` (and no NUL), not `.` or `..`, at
/// most 255 bytes of UTF-8 (a Hangul syllable is three).
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        Err("a name cannot be empty".to_string())
    } else if name == "." || name == ".." {
        Err(format!("{name} cannot be a name"))
    } else if name.contains('/') {
        Err(format!("{name}: a name cannot hold a /"))
    } else if name.contains('\0') {
        Err("a name cannot hold a NUL byte".to_string())
    } else if name.len() > 255 {
        Err(format!(
            "{name}: {} bytes; a name has at most 255",
            name.len()
        ))
    } else {
        Ok(())
    }
}

/// How many characters of `name` the rename field selects at first: the name before its
/// extension (`main` of `main.rs`), or all of it for a folder, a dot file and a name without
/// one — the panel's Name column ([`panel::split_name`]).
fn stem_chars(name: &str, folder: bool) -> usize {
    let whole = name.chars().count();
    if folder || name.starts_with('.') {
        return whole;
    }
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => name[..i].chars().count(),
        _ => whole,
    }
}

/// F5 or F6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    Copy,
    Move,
}

impl Transfer {
    /// The dialog's title and button.
    fn word(self) -> &'static str {
        match self {
            Transfer::Copy => "Copy",
            Transfer::Move => "Move",
        }
    }

    /// The status row's.
    fn verb(self) -> &'static str {
        match self {
            Transfer::Copy => "copying",
            Transfer::Move => "moving",
        }
    }

    fn done(self) -> &'static str {
        match self {
            Transfer::Copy => "copied",
            Transfer::Move => "moved",
        }
    }
}

/// What the browser asks the engine to do: the seam with [`crate::fileops`] (S1-C).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// F5 / F6: the entries `names` (in the panel's order) of the folder `from` into the
    /// folder `to`.
    Transfer {
        kind: Transfer,
        from: OpsTarget,
        names: Vec<String>,
        to: OpsTarget,
    },
    /// F2 / Shift+F6: the entry `from` of the folder `dir` becomes `to`.
    Rename {
        dir: OpsTarget,
        from: String,
        to: String,
    },
}

impl Job {
    /// The items, by name, as the report would list them.
    fn names(&self) -> Vec<String> {
        match self {
            Job::Transfer { names, .. } => names.clone(),
            Job::Rename { from, .. } => vec![from.clone()],
        }
    }

    /// Where a name is in the way: the target folder, as the dialog names it.
    fn place(&self) -> String {
        match self {
            Job::Transfer { to, .. } => to.shown(),
            Job::Rename { dir, .. } => dir.shown(),
        }
    }
}

/// Runs a [`Job`] to its end, on the worker [`start`] made for it, and returns what it did.
/// While it runs it tells the window through the [`Link`]: [`Link::progress`],
/// [`Link::ask`] at a name in the way (it blocks until the dialog answers), [`Link::stopped`]
/// after Esc. S1-E builds it around S1-C's engine; tests pass a stand-in.
///
/// "The same for the rest" is applied on this side: once a [`Reply`] carries it, the window
/// answers every later [`Clash`] of the job itself, without the dialog — Skip with Skip, Rename
/// with that clash's own [`Clash::free`] — each reply still carrying the flag. An engine that
/// applies the flag itself never asks again, which comes to the same.
pub type Run = Arc<dyn Fn(Job, &Link) -> Report + Send + Sync>;

/// What the window hears from a running job.
enum Event {
    Progress(Progress),
    Clash(Clash),
    /// The job ended; what it did.
    Done(Report),
}

/// The engine's end of a running job (see [`Run`]). Each call wakes the window.
pub struct Link {
    events: Sender<Event>,
    replies: Receiver<Reply>,
    stop: Arc<AtomicBool>,
    waker: egui::Context,
}

impl Link {
    fn send(&self, event: Event) {
        // The window may be gone: nothing is listening then, and the job goes on to its end.
        let _ = self.events.send(event);
        self.waker.request_repaint();
    }

    /// Where the job is (04 §5.1's status row).
    pub fn progress(&self, progress: Progress) {
        self.send(Event::Progress(progress));
    }

    /// A name in the way: asks the window and waits for its answer. Cancel when the job was
    /// stopped — before the question, or by the time its answer came — or the window is gone.
    pub fn ask(&self, clash: Clash) -> Reply {
        let cancel = Reply {
            answer: Answer::Cancel,
            same_for_rest: false,
        };
        if self.stopped() {
            return cancel;
        }
        self.send(Event::Clash(clash));
        match self.replies.recv() {
            Ok(_) if self.stopped() => cancel,
            Ok(reply) => reply,
            Err(_) => cancel,
        }
    }

    /// Esc asked the job to stop: the engine stops after removing the unfinished file's temp
    /// (04 §5.1).
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// The stop flag itself, for an engine that takes one.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }
}

/// Wakes the window when the worker ends, however it ends (a panic too).
struct Wake(egui::Context);

impl Drop for Wake {
    fn drop(&mut self) {
        self.0.request_repaint();
    }
}

/// The window's end of a running job: what the status row says, the name-in-the-way dialog
/// while one is asked, Esc, and the report when it ends. Dropping it stops the job (at the
/// engine's next check; a later question is answered Cancel); closing the window waits for it
/// instead ([`Running::wait`]).
pub struct Running {
    job: Job,
    events: Receiver<Event>,
    replies: Sender<Reply>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    progress: Option<Progress>,
    clash: Option<ClashDialog>,
    /// "The same for the rest": the answer every later clash gets.
    same: Option<Answer>,
}

/// Starts `job` on a worker of its own, which calls `run`.
pub fn start(run: &Run, job: Job, ctx: &egui::Context) -> Running {
    let (mut running, link) = pair(job, ctx);
    let (run, todo, wake) = (run.clone(), running.job.clone(), Wake(ctx.clone()));
    running.worker = Some(std::thread::spawn(move || {
        let _wake = wake;
        let report = run(todo, &link);
        link.send(Event::Done(report));
    }));
    running
}

/// The two ends of `job`, joined, before any worker runs it.
fn pair(job: Job, ctx: &egui::Context) -> (Running, Link) {
    let (events_tx, events) = mpsc::channel();
    let (replies, replies_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let link = Link {
        events: events_tx,
        replies: replies_rx,
        stop: stop.clone(),
        waker: ctx.clone(),
    };
    let running = Running {
        job,
        events,
        replies,
        stop,
        worker: None,
        progress: None,
        clash: None,
        same: None,
    };
    (running, link)
}

impl Running {
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// Esc: the engine stops at its next check; an open question is answered Cancel.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if self.clash.take().is_some() {
            self.reply(Reply {
                answer: Answer::Cancel,
                same_for_rest: false,
            });
        }
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// The name-in-the-way dialog is open: it has the keyboard.
    pub fn asking(&self) -> bool {
        self.clash.is_some()
    }

    /// The status row while it runs (04 §5.1): the progress line once the engine said where it
    /// is, else what it does.
    pub fn status(&self) -> Status {
        if self.stopping() {
            return Status::info("stopping…");
        }
        if let Some(p) = &self.progress {
            return Status::info(progress_line(p));
        }
        Status::info(match &self.job {
            Job::Transfer { kind, names, .. } => {
                let what = match names.as_slice() {
                    [one] => one.clone(),
                    all => format!("{} items", all.len()),
                };
                format!("{} {what}… · Esc stops", kind.verb())
            }
            Job::Rename { from, to, .. } => format!("renaming {from} → {to}…"),
        })
    }

    fn reply(&self, reply: Reply) {
        // The worker may have died meanwhile: it asks no more.
        let _ = self.replies.send(reply);
    }

    /// Takes the worker's news without drawing: progress, a question (the dialog opens, or "the
    /// same for the rest" answers it), and what the job did once it has ended.
    pub fn poll(&mut self) -> Option<Report> {
        loop {
            match self.events.try_recv() {
                Ok(Event::Progress(p)) => self.progress = Some(p),
                // A stopped job's question is answered Cancel before anything else: Esc wins
                // over "the same for the rest".
                Ok(Event::Clash(_)) if self.stopping() => self.reply(Reply {
                    answer: Answer::Cancel,
                    same_for_rest: false,
                }),
                Ok(Event::Clash(clash)) => match self.same.clone() {
                    Some(answer) => self.reply(Reply {
                        answer: match answer {
                            Answer::Rename(_) => Answer::Rename(clash.free.clone()),
                            other => other,
                        },
                        same_for_rest: true,
                    }),
                    None => self.clash = Some(ClashDialog::open(clash, &self.job.place())),
                },
                Ok(Event::Done(report)) => {
                    self.join();
                    return Some(report);
                }
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    self.join();
                    return Some(self.died());
                }
            }
        }
    }

    /// Each pass: [`Running::poll`], and the name-in-the-way dialog while one is asked (its
    /// answer goes to the engine).
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Report> {
        let report = self.poll();
        if report.is_none() {
            self.dialog(ctx);
        }
        report
    }

    /// The name-in-the-way dialog, while one is asked; its answer goes to the engine.
    fn dialog(&mut self, ctx: &egui::Context) {
        let Some(reply) = self.clash.as_mut().and_then(|d| d.ui(ctx)) else {
            return;
        };
        self.clash = None;
        if reply.same_for_rest {
            self.same = Some(reply.answer.clone());
        }
        self.reply(reply);
    }

    /// The window closes while the job runs (02 §6): waits for it, answering a name in the way
    /// with Cancel (no one is there to ask), and returns what it did.
    pub fn wait(mut self) -> Report {
        let cancel = Reply {
            answer: Answer::Cancel,
            same_for_rest: false,
        };
        if self.clash.take().is_some() {
            self.reply(cancel.clone());
        }
        loop {
            match self.events.recv() {
                Ok(Event::Progress(_)) => {}
                Ok(Event::Clash(_)) => self.reply(cancel.clone()),
                Ok(Event::Done(report)) => {
                    self.join();
                    return report;
                }
                Err(_) => {
                    self.join();
                    return self.died();
                }
            }
        }
    }

    fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    /// The worker ended without a report (it panicked): nothing is known to be done.
    fn died(&self) -> Report {
        Report {
            items: self
                .job
                .names()
                .into_iter()
                .map(|name| Item {
                    name,
                    outcome: ItemOutcome::NotReached,
                })
                .collect(),
            notes: vec![
                "the operation stopped without a report: what it did is not known — \
                 look at both folders"
                    .to_string(),
            ],
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The status row's line for `p` (04 §5.1): `copying 3/12 · name 45% · 12.0 MB / 30.0 MB · Esc
/// stops` for more than one item or a file over 8 MiB, else `copying name… · Esc stops`. Sizes
/// as the browser's fetch line writes them ([`crate::human`]); a file of no bytes has no
/// percentage.
pub fn progress_line(p: &Progress) -> String {
    if p.items <= 1 && p.total <= BIG_FILE {
        return format!("{} {}… · Esc stops", p.verb, p.name);
    }
    let bytes = if p.total > 0 {
        let pct = (u128::from(p.done) * 100 / u128::from(p.total)).min(100);
        format!(
            " {pct}% · {} / {}",
            crate::human(p.done),
            crate::human(p.total)
        )
    } else {
        String::new()
    };
    format!(
        "{} {}/{} · {}{bytes} · Esc stops",
        p.verb, p.item, p.items, p.name
    )
}

/// Above this a single file shows the full progress line (04 §5.1).
pub const BIG_FILE: u64 = 8 * 1024 * 1024;

/// How an ended job is told (04 §5.1 "Reported whole").
pub enum Finished {
    /// A fully successful job on one item: one line on the status row.
    Status(Status),
    /// Anything else — more than one item, a skip, a rename, a failure, a note, a stop: the
    /// report dialog.
    Report(ReportView),
}

/// How `job` is told, now that it did `report`.
pub fn finish(job: &Job, report: Report) -> Finished {
    let one_done = report.notes.is_empty()
        && matches!(report.items.as_slice(), [item] if item.outcome == ItemOutcome::Done);
    match job {
        Job::Transfer { kind, to, .. } if one_done => Finished::Status(Status::info(format!(
            "{} {} to {}",
            kind.done(),
            report.items[0].name,
            to.shown()
        ))),
        Job::Rename { from, to, .. } if one_done => {
            Finished::Status(Status::info(format!("renamed {from} → {to}")))
        }
        Job::Transfer { kind, to, .. } => Finished::Report(ReportView::new(
            kind.word(),
            format!("{} to {}", kind.word(), to.shown()),
            report,
        )),
        Job::Rename { from, to, .. } => Finished::Report(ReportView::new(
            "Rename",
            format!("Rename {from} → {to}"),
            report,
        )),
    }
}

/// The report dialog: what was done, skipped, renamed, failed (with the reason) and not
/// reached, then the notes. Enter, Esc or Close closes it.
pub struct ReportView {
    title: String,
    head: String,
    report: Report,
}

/// The report's word for each outcome, the order its head counts them in.
const OUTCOMES: [&str; 5] = ["done", "renamed", "skipped", "failed", "not reached"];

fn outcome_word(outcome: &ItemOutcome) -> &'static str {
    match outcome {
        ItemOutcome::Done => OUTCOMES[0],
        ItemOutcome::Renamed(_) => OUTCOMES[1],
        ItemOutcome::Skipped => OUTCOMES[2],
        ItemOutcome::Failed(_) => OUTCOMES[3],
        ItemOutcome::NotReached => OUTCOMES[4],
    }
}

/// One row of the report: the outcome in a column of its own, the name, what became of it.
fn item_line(item: &Item) -> String {
    let word = outcome_word(&item.outcome);
    match &item.outcome {
        ItemOutcome::Renamed(to) => format!("{word:<11} {} → {to}", item.name),
        ItemOutcome::Failed(why) => format!("{word:<11} {} — {why}", item.name),
        _ => format!("{word:<11} {}", item.name),
    }
}

impl ReportView {
    fn new(title: &str, what: String, report: Report) -> Self {
        let counts: Vec<String> = OUTCOMES
            .iter()
            .filter_map(|word| {
                let n = report
                    .items
                    .iter()
                    .filter(|i| outcome_word(&i.outcome) == *word)
                    .count();
                (n > 0).then(|| format!("{n} {word}"))
            })
            .collect();
        let head = if counts.is_empty() {
            format!("{what}: nothing to do")
        } else {
            format!("{what}: {}", counts.join(" · "))
        };
        Self {
            title: format!("{title} report"),
            head,
            report,
        }
    }

    /// Its rows as text, the order shown: the head, an item a row, the notes.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![self.head.clone()];
        lines.extend(self.report.items.iter().map(item_line));
        lines.extend(self.report.notes.iter().cloned());
        lines
    }

    /// Shows it; true once it is closed.
    pub fn ui(&mut self, ctx: &egui::Context) -> bool {
        let close_key = ctx.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::Enter)
                || i.consume_key(Modifiers::NONE, Key::Escape)
        });
        let mut closed = false;
        let modal = chrome::dialog(
            ctx,
            egui::Id::unique("sopen-ops-report"),
            &self.title,
            |ui| {
                ui.set_width(dialog_width(ctx, 72.0));
                ui.add(egui::Label::new(&self.head).wrap());
                ui.add_space(chrome::CELL.y / 2.0);
                let (error, weak) = (ui.visuals().error_fg_color, ui.visuals().weak_text_color());
                egui::ScrollArea::vertical()
                    .max_height(chrome::CELL.y * 16.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for item in &self.report.items {
                            let text = RichText::new(item_line(item));
                            let text = match item.outcome {
                                ItemOutcome::Failed(_) => text.color(error),
                                ItemOutcome::NotReached => text.color(weak),
                                _ => text,
                            };
                            ui.add(egui::Label::new(text).wrap());
                        }
                        if !self.report.notes.is_empty() {
                            ui.add_space(chrome::CELL.y / 2.0);
                            for note in &self.report.notes {
                                ui.add(egui::Label::new(note.as_str()).wrap());
                            }
                        }
                    });
                ui.add_space(chrome::CELL.y / 2.0);
                if ui.button("Close").clicked() {
                    closed = true;
                }
            },
        );
        closed || close_key || modal.should_close()
    }
}

/// A dialog's width: `cells` character cells, less on a narrow window.
fn dialog_width(ctx: &egui::Context, cells: f32) -> f32 {
    let room = ctx.content_rect().width() - 8.0 * chrome::CELL.x;
    (cells * chrome::CELL.x)
        .min(room)
        .max(20.0 * chrome::CELL.x)
}

/// A dialog's hold on the keyboard: the widgets it laid out on its last pass. While the focus is
/// on none of them — the list behind it asked for it, or nothing has it — its field takes it
/// back before it is laid out, so the typing of this pass goes there; on the first pass the
/// field's text is selected as asked.
#[derive(Default)]
struct Keyboard {
    ids: Vec<egui::Id>,
    opened: bool,
}

/// The keys a pass of a dialog acts on, taken (consumed) before its widgets see them.
#[derive(Debug, Clone, Copy, Default)]
struct Keys {
    enter: bool,
    escape: bool,
}

impl Keyboard {
    /// Before the dialog's widgets: Esc is taken; Enter is taken unless one of the dialog's
    /// buttons has the focus (Enter clicks it then); the field gets the focus unless such a
    /// button has it — the first time with its first `select` characters selected.
    fn begin(&mut self, ctx: &egui::Context, field: egui::Id, text: &str, select: usize) -> Keys {
        let focused = ctx.memory(|m| m.focused());
        let on_button = focused.is_some_and(|f| f != field && self.ids.contains(&f));
        let escape = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        let enter = !on_button && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        if !std::mem::replace(&mut self.opened, true) {
            let mut state = egui::text_edit::TextEditState::load(ctx, field).unwrap_or_default();
            let end = select.min(text.chars().count());
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(end),
                )));
            state.store(ctx, field);
        }
        if !on_button && focused != Some(field) {
            ctx.memory_mut(|m| m.request_focus(field));
        }
        self.ids.clear();
        Keys { enter, escape }
    }

    /// A widget of the dialog (a button that may keep the focus).
    fn laid(&mut self, response: &egui::Response) {
        self.ids.push(response.id);
    }

    /// Selects the field's first `select` characters again on the next pass (after a refusal,
    /// so typing replaces what was refused).
    fn reselect(&mut self) {
        self.opened = false;
    }
}

/// How the Copy / Move dialog ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyEnd {
    /// Enter or the Copy / Move button, with a target that reads (04 §5.2).
    Start(Job),
    /// Esc, Cancel or a click outside it.
    Cancel,
}

/// F5 / F6's dialog (04 §5.2): what (the bar's name, or `N items`) and the target field,
/// prefilled with the other panel's own folder; Enter goes, Esc cancels. A target that does not
/// read keeps it open with the reason.
pub struct CopyDialog {
    kind: Transfer,
    from: OpsTarget,
    names: Vec<String>,
    /// The target field.
    pub target: String,
    /// What it was prefilled with: while it holds that, the dialog names it.
    other: String,
    error: Option<String>,
    keyboard: Keyboard,
}

fn copy_field_id() -> egui::Id {
    egui::Id::unique("sopen-ops-copy-target")
}

impl CopyDialog {
    /// F5 / F6 in `panel` (its folder on `host`, `None` here): on [`Panel::acted_on`], to `to`.
    /// `Err` is for the status row: there is nothing to act on.
    pub fn open(
        kind: Transfer,
        panel: &Panel,
        host: Option<&str>,
        to: &OpsTarget,
    ) -> Result<Self, String> {
        let names: Vec<String> = panel.acted_on().iter().map(|e| e.name.clone()).collect();
        if names.is_empty() {
            return Err(nothing_to(kind.word(), panel));
        }
        let other = to.shown();
        Ok(Self {
            kind,
            from: OpsTarget {
                host: host.map(str::to_string),
                dir: panel.dir.clone(),
            },
            names,
            target: other.clone(),
            other,
            error: None,
            keyboard: Keyboard::default(),
        })
    }

    /// What it acts on, as the dialog says it: the name, or `N items`.
    pub fn what(&self) -> String {
        match self.names.as_slice() {
            [one] => one.clone(),
            all => format!("{} items", all.len()),
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Option<CopyEnd> {
        let field_id = copy_field_id();
        let all = self.target.chars().count();
        let keys = self.keyboard.begin(ctx, field_id, &self.target, all);
        let word = self.kind.word();
        let (mut go, mut cancel) = (keys.enter, keys.escape);
        let modal = chrome::dialog(ctx, egui::Id::unique("sopen-ops-copy"), word, |ui| {
            ui.set_width(dialog_width(ctx, 60.0));
            ui.label(format!("{word} {}", self.what()));
            ui.add_space(chrome::CELL.y / 2.0);
            ui.horizontal(|ui| {
                ui.label("to");
                ui.add(
                    chrome::field(&mut self.target)
                        .id(field_id)
                        .desired_width(ui.available_width()),
                );
            });
            let hint = if self.target == self.other {
                "the other panel's folder"
            } else {
                "host:/abs on a remote · /abs here · a name: inside this folder"
            };
            ui.label(RichText::new(hint).weak());
            if let Some(error) = &self.error {
                let color = ui.visuals().error_fg_color;
                crate::gui::disclosures(ui, &[(error.clone(), color)]);
            }
            ui.add_space(chrome::CELL.y / 2.0);
            ui.horizontal(|ui| {
                let ok = ui.button(word);
                let no = ui.button("Cancel");
                self.keyboard.laid(&ok);
                self.keyboard.laid(&no);
                go |= ok.clicked();
                cancel |= no.clicked();
            });
        });
        if cancel || modal.backdrop_response.clicked() {
            return Some(CopyEnd::Cancel);
        }
        if !go {
            return None;
        }
        match OpsTarget::parse(&self.target, &self.from) {
            Ok(to) => Some(CopyEnd::Start(Job::Transfer {
                kind: self.kind,
                from: self.from.clone(),
                names: self.names.clone(),
                to,
            })),
            Err(why) => {
                self.error = Some(why);
                None
            }
        }
    }
}

/// Why F5 / F6 (or F2) does nothing here.
fn nothing_to(word: &str, panel: &Panel) -> String {
    match panel.current() {
        Some(e) if e.lossy => format!(
            "{}: the name is not valid UTF-8; sopen cannot {} it",
            e.name,
            word.to_lowercase()
        ),
        _ => format!(
            "nothing to {}: the bar is on .. and nothing is selected",
            word.to_lowercase()
        ),
    }
}

/// How the select-group dialog ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupEnd {
    /// Enter or the button: select (`+`) or unselect (`-`) what matches `pattern`
    /// ([`Panel::select_group`]).
    Apply {
        select: bool,
        pattern: String,
    },
    Cancel,
}

/// `+` and `-`'s one-line dialog (04 §4): a pattern, `*` at first.
pub struct GroupDialog {
    pub select: bool,
    pub pattern: String,
    keyboard: Keyboard,
}

impl GroupDialog {
    pub fn open(select: bool) -> Self {
        Self {
            select,
            pattern: "*".to_string(),
            keyboard: Keyboard::default(),
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Option<GroupEnd> {
        let field_id = egui::Id::unique("sopen-ops-group-pattern");
        let all = self.pattern.chars().count();
        let keys = self.keyboard.begin(ctx, field_id, &self.pattern, all);
        let word = if self.select { "Select" } else { "Unselect" };
        let (mut go, mut cancel) = (keys.enter, keys.escape);
        let modal = chrome::dialog(ctx, egui::Id::unique("sopen-ops-group"), word, |ui| {
            ui.set_width(dialog_width(ctx, 44.0));
            ui.label(format!("{word} the names that match"));
            ui.add(
                chrome::field(&mut self.pattern)
                    .id(field_id)
                    .desired_width(ui.available_width()),
            );
            ui.label(RichText::new("* any characters · ? one · any case").weak());
            ui.add_space(chrome::CELL.y / 2.0);
            ui.horizontal(|ui| {
                let ok = ui.button(word);
                let no = ui.button("Cancel");
                self.keyboard.laid(&ok);
                self.keyboard.laid(&no);
                go |= ok.clicked();
                cancel |= no.clicked();
            });
        });
        if cancel || modal.backdrop_response.clicked() {
            Some(GroupEnd::Cancel)
        } else if go {
            Some(GroupEnd::Apply {
                select: self.select,
                pattern: self.pattern.clone(),
            })
        } else {
            None
        }
    }
}

/// The name-in-the-way dialog (04 §5.2): Skip, Rename (the next free name prefilled, editable),
/// Cancel, and "the same for the rest" (off). Overwrite is never offered. Enter renames, Esc
/// cancels; a click outside it does nothing (the rest of the job hangs on the answer).
pub struct ClashDialog {
    clash: Clash,
    /// Where the name is in the way (the job's target folder), as the dialog names it.
    place: String,
    /// The Rename field.
    pub rename: String,
    pub same_for_rest: bool,
    error: Option<String>,
    keyboard: Keyboard,
}

impl ClashDialog {
    pub fn open(clash: Clash, place: &str) -> Self {
        Self {
            rename: clash.free.clone(),
            clash,
            place: place.to_string(),
            same_for_rest: false,
            error: None,
            keyboard: Keyboard::default(),
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Reply> {
        let field_id = egui::Id::unique("sopen-ops-clash-rename");
        let stem = stem_chars(&self.rename, false);
        let keys = self.keyboard.begin(ctx, field_id, &self.rename, stem);
        let mut answer = None;
        if keys.escape {
            answer = Some(Answer::Cancel);
        }
        let mut rename = keys.enter;
        chrome::dialog(
            ctx,
            egui::Id::unique("sopen-ops-clash"),
            "Name exists",
            |ui| {
                ui.set_width(dialog_width(ctx, 60.0));
                let line = format!("{} is already in {}.", self.clash.name, self.place);
                ui.add(egui::Label::new(line).wrap());
                ui.label("Nothing is ever written over it.");
                ui.add_space(chrome::CELL.y / 2.0);
                ui.horizontal(|ui| {
                    ui.label("Rename to");
                    ui.add(
                        chrome::field(&mut self.rename)
                            .id(field_id)
                            .desired_width(ui.available_width()),
                    );
                });
                if let Some(error) = &self.error {
                    let color = ui.visuals().error_fg_color;
                    crate::gui::disclosures(ui, &[(error.clone(), color)]);
                }
                let rest = ui.checkbox(&mut self.same_for_rest, "the same for the rest");
                self.keyboard.laid(&rest);
                ui.add_space(chrome::CELL.y / 2.0);
                ui.horizontal(|ui| {
                    let skip = ui.button("Skip");
                    let ren = ui.button("Rename");
                    let cancel = ui.button("Cancel");
                    for b in [&skip, &ren, &cancel] {
                        self.keyboard.laid(b);
                    }
                    if skip.clicked() {
                        answer = Some(Answer::Skip);
                    }
                    rename |= ren.clicked();
                    if cancel.clicked() {
                        answer = Some(Answer::Cancel);
                    }
                });
                ui.label(RichText::new("Enter renames · Esc cancels the rest").weak());
            },
        );
        match answer {
            Some(answer) => Some(Reply {
                answer,
                same_for_rest: self.same_for_rest,
            }),
            None if rename => self.answer_rename(),
            None => None,
        }
    }

    /// Rename with the field's name, if it is one the browser may give; else the reason stays
    /// in the dialog and nothing is answered.
    fn answer_rename(&mut self) -> Option<Reply> {
        match check_name(&self.rename) {
            Ok(()) => Some(Reply {
                answer: Answer::Rename(self.rename.clone()),
                same_for_rest: self.same_for_rest,
            }),
            Err(why) => {
                self.error = Some(why);
                self.keyboard.reselect();
                None
            }
        }
    }
}

/// The inline rename field's widget id (one field at a time).
fn rename_id() -> egui::Id {
    egui::Id::unique("sopen-browser-rename")
}

/// How the inline rename field ended (04 §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameEnd {
    /// Enter on a new, valid name: the job to run. The field stays (waiting) until
    /// [`Rename::settle`] hears the engine's answer.
    Commit(Job),
    /// Esc, or Enter on the name unchanged: nothing is renamed.
    Cancel,
}

/// F2 / Shift+F6: the bar's name becomes a field in its row, the name selected and the
/// extension after it; Enter renames, Esc cancels; a name that exists (the engine says so) is an
/// error on the status row and the field stays for another try.
pub struct Rename {
    dir: OpsTarget,
    from: String,
    /// The field.
    pub text: String,
    /// A folder: its whole name is selected.
    folder: bool,
    error: Option<String>,
    /// Committed: the engine is at it (the field shows, takes no typing).
    pending: bool,
    keyboard: Keyboard,
}

impl Rename {
    /// On the bar's entry of `panel` (its folder on `host`). `Err` is for the status row: `..`,
    /// a name that is not UTF-8, or a folder on a remote — SFTP's plain rename, which renaming one
    /// needs, comes with slice S6; until then F2 says so and does nothing (04 §5.3).
    pub fn open(panel: &Panel, host: Option<&str>) -> Result<Self, String> {
        let Some(entry) = panel.current() else {
            return Err("nothing to rename".to_string());
        };
        if entry.name == panel::UP {
            return Err(".. cannot be renamed".to_string());
        }
        if entry.lossy {
            return Err(nothing_to("Rename", panel));
        }
        let folder = entry.kind == EntryKind::Dir && !entry.link;
        if let (Some(host), true) = (host, folder) {
            return Err(format!(
                "{}: sopen cannot rename a folder on {host} yet; nothing changed",
                entry.name
            ));
        }
        Ok(Self {
            dir: OpsTarget {
                host: host.map(str::to_string),
                dir: panel.dir.clone(),
            },
            from: entry.name.clone(),
            text: entry.name.clone(),
            folder,
            error: None,
            pending: false,
            keyboard: Keyboard::default(),
        })
    }

    /// The entry it renames: its row shows the field.
    pub fn name(&self) -> &str {
        &self.from
    }

    /// The last refusal (an invalid name, the engine's reason), for the status row.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn pending(&self) -> bool {
        self.pending
    }

    /// The field over `rect` (the Name and Ext columns of its row). It holds the keyboard while
    /// it is open, and consumes the Enter and Esc it acts on.
    pub fn ui(&mut self, ui: &mut egui::Ui, rect: Rect) -> Option<RenameEnd> {
        fn field(text: &mut String, width: f32) -> egui::TextEdit<'_> {
            chrome::field(text)
                .id(rename_id())
                .lock_focus(true)
                .margin(egui::Margin::ZERO)
                .desired_width(width)
        }
        let field = |text| field(text, rect.width());
        if self.pending {
            ui.put(rect, field(&mut self.text).interactive(false));
            return None;
        }
        let ctx = ui.ctx().clone();
        let stem = stem_chars(&self.text, self.folder);
        let keys = self.keyboard.begin(&ctx, rename_id(), &self.text, stem);
        ui.put(rect, field(&mut self.text));
        if keys.escape {
            return Some(RenameEnd::Cancel);
        }
        if !keys.enter {
            return None;
        }
        if self.text == self.from {
            return Some(RenameEnd::Cancel);
        }
        if let Err(why) = check_name(&self.text) {
            self.error = Some(why);
            self.keyboard.reselect();
            return None;
        }
        self.pending = true;
        self.error = None;
        Some(RenameEnd::Commit(Job::Rename {
            dir: self.dir.clone(),
            from: self.from.clone(),
            to: self.text.clone(),
        }))
    }

    /// The engine's answer to the commit: done, or anything to be read whole (a note: where the
    /// file is now) → `Some` (how to tell it; the field closes); refused (a name that exists,
    /// any failure) → `None`, the field stays with the reason and the name selected again.
    pub fn settle(&mut self, report: &Report) -> Option<Finished> {
        self.pending = false;
        if let ([item], true) = (report.items.as_slice(), report.notes.is_empty()) {
            if let ItemOutcome::Failed(why) = &item.outcome {
                self.error = Some(why.clone());
                self.keyboard.reselect();
                return None;
            }
        }
        let job = Job::Rename {
            dir: self.dir.clone(),
            from: self.from.clone(),
            to: self.text.clone(),
        };
        Some(finish(&job, report.clone()))
    }
}

/// What a finished job leaves for the browser (04 §5.1 "After"): the folders it changed — the
/// panels showing one re-list, the bar kept on the same name, and `BrowserEvent::FilesChanged`
/// carries them (02 §7) — and the line for the status row, if one is to be said.
#[derive(Debug, Clone)]
pub struct Changed {
    pub dirs: Vec<OpsTarget>,
    /// After a rename: the bar goes on the new name, in `dirs[0]`.
    pub bar: Option<String>,
    pub said: Option<Status>,
}

/// The file operations of one browser window, as one piece the browser holds (its
/// file-operation state, 02 §6) and calls: the dialogs, the rename field, the running job, the
/// report.
pub struct Ops {
    run: Run,
    group: Option<GroupDialog>,
    copy: Option<CopyDialog>,
    rename: Option<Rename>,
    running: Option<Running>,
    report: Option<ReportView>,
}

impl Ops {
    pub fn new(run: Run) -> Self {
        Self {
            run,
            group: None,
            copy: None,
            rename: None,
            running: None,
            report: None,
        }
    }

    /// A dialog, the rename field or the report has the keyboard: the list's keys (and its
    /// claim on the keyboard) wait (04 §3 rule 4).
    pub fn holds_keyboard(&self) -> bool {
        self.group.is_some()
            || self.copy.is_some()
            || self.rename.is_some()
            || self.report.is_some()
            || self.running.as_ref().is_some_and(Running::asking)
    }

    /// A job runs (Esc stops it, [`Ops::stop`]).
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// Why nothing new opens while a job runs.
    fn busy_line(&self) -> Option<Status> {
        self.busy()
            .then(|| Status::error("a file operation is running: Esc stops it"))
    }

    /// `+` (`select`) or `-`: the select-group dialog for the active panel.
    pub fn open_group(&mut self, select: bool) {
        self.group = Some(GroupDialog::open(select));
    }

    /// F5 / F6 in `panel` (its folder on `host`) to `to`, the other panel's own folder. `Some`
    /// is for the status row: nothing opened (nothing to act on, a job running).
    pub fn open_copy(
        &mut self,
        kind: Transfer,
        panel: &Panel,
        host: Option<&str>,
        to: &OpsTarget,
    ) -> Option<Status> {
        if let Some(busy) = self.busy_line() {
            return Some(busy);
        }
        match CopyDialog::open(kind, panel, host, to) {
            Ok(dialog) => {
                self.copy = Some(dialog);
                None
            }
            Err(why) => Some(Status::error(why)),
        }
    }

    /// F2 / Shift+F6 on the bar's entry of `panel`. `Some` is for the status row, as for
    /// [`Ops::open_copy`].
    pub fn open_rename(&mut self, panel: &Panel, host: Option<&str>) -> Option<Status> {
        if let Some(busy) = self.busy_line() {
            return Some(busy);
        }
        match Rename::open(panel, host) {
            Ok(rename) => {
                self.rename = Some(rename);
                None
            }
            Err(why) => Some(Status::error(why)),
        }
    }

    /// Esc while a job runs.
    pub fn stop(&mut self) {
        if let Some(running) = &mut self.running {
            running.stop();
        }
    }

    /// What the status row says while the operations have something to say: the running job's
    /// line, else a refused rename's reason.
    pub fn status(&self) -> Option<Status> {
        if let Some(running) = &self.running {
            return Some(running.status());
        }
        let error = self.rename.as_ref().and_then(Rename::error)?;
        Some(Status::error(error))
    }

    /// The entry the rename field is open on, if any (its row draws [`Ops::rename_field`]).
    pub fn renaming(&self) -> Option<&str> {
        self.rename.as_ref().map(Rename::name)
    }

    /// The rename field over `rect` in the row of the entry it renames ([`Ops::renaming`]);
    /// Enter starts the job.
    pub fn rename_field(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let Some(rename) = &mut self.rename else {
            return;
        };
        match rename.ui(ui, rect) {
            Some(RenameEnd::Commit(job)) => self.running = Some(start(&self.run, job, ui.ctx())),
            Some(RenameEnd::Cancel) => self.rename = None,
            None => {}
        }
    }

    /// Each pass, after the panels: the running job's news, then one dialog over them — the
    /// job's own first (its question, then its report), else the select-group or the Copy /
    /// Move dialog, which wait aside, kept as they were, while a job's dialog is up (one modal,
    /// one owner of the keyboard). `active` is the panel the select-group dialog acts on, and the
    /// one a transfer started from (what it did is unselected there: what stays selected is what
    /// is left to do). Returns what a job that just ended changed.
    pub fn ui(&mut self, ctx: &egui::Context, active: &mut Panel) -> Option<Changed> {
        let mut changed = None;
        if let Some(report) = self.running.as_mut().and_then(Running::poll) {
            let job = self.running.take().map(|r| r.job.clone());
            changed = job.and_then(|job| self.ended(job, report, active));
        }
        if let Some(running) = self.running.as_mut().filter(|r| r.asking()) {
            running.dialog(ctx);
        } else if let Some(view) = &mut self.report {
            if view.ui(ctx) {
                self.report = None;
            }
        } else {
            self.owners_dialogs(ctx, active);
        }
        changed
    }

    /// The select-group and Copy / Move dialogs (one is open at a time in practice: each holds
    /// the keyboard the other's key would need).
    fn owners_dialogs(&mut self, ctx: &egui::Context, active: &mut Panel) {
        if let Some(end) = self.group.as_mut().and_then(|d| d.ui(ctx)) {
            if let GroupEnd::Apply { select, pattern } = end {
                active.select_group(&pattern, select);
            }
            self.group = None;
        }
        match self.copy.as_mut().and_then(|d| d.ui(ctx)) {
            Some(CopyEnd::Start(job)) => {
                self.copy = None;
                self.running = Some(start(&self.run, job, ctx));
            }
            Some(CopyEnd::Cancel) => self.copy = None,
            None => {}
        }
    }

    /// `job` ended with `report`: how it is told, what it changed. A refused rename changed
    /// nothing: its field stays.
    fn ended(&mut self, job: Job, report: Report, active: &mut Panel) -> Option<Changed> {
        let (finished, dirs, bar) = match &job {
            Job::Rename { dir, to, .. } => {
                let finished = match self.rename.as_mut() {
                    Some(rename) => rename.settle(&report)?,
                    None => finish(&job, report.clone()),
                };
                self.rename = None;
                let done = report
                    .items
                    .iter()
                    .any(|i| matches!(i.outcome, ItemOutcome::Done | ItemOutcome::Renamed(_)));
                (finished, vec![dir.clone()], done.then(|| to.clone()))
            }
            Job::Transfer { from, to, .. } => {
                if active.dir == from.dir {
                    for item in &report.items {
                        if matches!(item.outcome, ItemOutcome::Done | ItemOutcome::Renamed(_)) {
                            active.unselect(&item.name);
                        }
                    }
                }
                let mut dirs = vec![from.clone()];
                if to != from {
                    dirs.push(to.clone());
                }
                (finish(&job, report), dirs, None)
            }
        };
        let said = match finished {
            Finished::Status(line) => Some(line),
            Finished::Report(view) => {
                self.report = Some(view);
                None
            }
        };
        Some(Changed { dirs, bar, said })
    }

    /// The window closes: waits for a running job (02 §6) and returns what it did.
    pub fn wait(mut self) -> Option<Report> {
        self.running.take().map(Running::wait)
    }
}

#[cfg(test)]
mod tests {
    //! The pieces driven by real egui events through the test kit, in a stand-in host shaped
    //! like browser.rs: a key pass first (Enter as the list's, and Esc stopping a running job —
    //! the dispatch S1-E adds — both held back while [`Ops::holds_keyboard`]), then the list
    //! claiming the keyboard on every pass, ungated, so each piece must hold its own; then rows
    //! drawn with [`entry_color`] and the rename field in its row, and [`Ops::ui`] over them. The
    //! browser's own `panel_ui`, its keys and its bar are S1-E's and not exercised here. The
    //! engine is a stand-in [`Run`].

    use super::*;
    use crate::fileops::Item;
    use crate::gui::testkit::Driver;
    use crate::gui::Window;
    use eframe::egui::{Event, Pos2, Sense, Vec2};
    use std::sync::Mutex;
    use std::time::SystemTime;

    fn entry(name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            name: name.to_string(),
            lossy: false,
            kind,
            link: false,
            size,
            mtime: SystemTime::UNIX_EPOCH,
        }
    }

    /// `/srv/src`: `..`, docs/, .hidden, lib.rs, main.rs, 사진.png.
    fn panel() -> Panel {
        let mut p = Panel::new("/srv/src");
        let entries = vec![
            entry("main.rs", EntryKind::File, 100),
            entry("docs", EntryKind::Dir, 0),
            entry("사진.png", EntryKind::File, 2_000),
            entry("lib.rs", EntryKind::File, 20),
            entry(".hidden", EntryKind::File, 3),
        ];
        p.fill("/srv/src".to_string(), entries, None, None);
        p
    }

    fn at(p: &mut Panel, name: &str) {
        p.highlight = p.entries.iter().position(|e| e.name == name).unwrap();
    }

    fn here(dir: &str) -> OpsTarget {
        OpsTarget {
            host: None,
            dir: dir.to_string(),
        }
    }

    fn on(host: &str, dir: &str) -> OpsTarget {
        OpsTarget {
            host: Some(host.to_string()),
            dir: dir.to_string(),
        }
    }

    fn list_id() -> egui::Id {
        egui::Id::unique("test-host-list")
    }

    /// What the stand-in engine was given and answered, in order.
    #[derive(Default)]
    struct Log {
        jobs: Vec<Job>,
        replies: Vec<(String, Reply)>,
    }

    type Shared = Arc<Mutex<Log>>;

    /// The stand-in engine: a rename to a name starting `taken` is refused as the filesystem
    /// would; every other rename is done. A transfer asks at each name in `clashing` and acts on
    /// the answer (Cancel: the rest is not reached).
    fn engine(log: Shared, clashing: &'static [&'static str]) -> Run {
        Arc::new(move |job: Job, link: &Link| {
            log.lock().unwrap().jobs.push(job.clone());
            let mut report = Report::default();
            match job {
                Job::Rename { from, to, .. } => {
                    let outcome = if to.starts_with("taken") {
                        ItemOutcome::Failed(format!("{to}: a file of that name exists"))
                    } else {
                        ItemOutcome::Done
                    };
                    report.items.push(Item {
                        name: from,
                        outcome,
                    });
                }
                Job::Transfer { names, .. } => {
                    let mut cancelled = false;
                    for name in names {
                        let outcome = if cancelled || link.stopped() {
                            ItemOutcome::NotReached
                        } else if clashing.contains(&name.as_str()) {
                            let free = crate::remote::suffixed(&name, 1);
                            let reply = link.ask(Clash {
                                name: name.clone(),
                                free,
                            });
                            log.lock()
                                .unwrap()
                                .replies
                                .push((name.clone(), reply.clone()));
                            match reply.answer {
                                Answer::Skip => ItemOutcome::Skipped,
                                Answer::Rename(to) => ItemOutcome::Renamed(to),
                                Answer::Cancel => {
                                    cancelled = true;
                                    ItemOutcome::NotReached
                                }
                            }
                        } else {
                            ItemOutcome::Done
                        };
                        report.items.push(Item { name, outcome });
                    }
                }
            }
            report
        })
    }

    struct Host {
        panel: Panel,
        ops: Ops,
        /// The Enter presses the list's own key pass took (it would open the entry).
        list_enters: usize,
        changed: Vec<Changed>,
    }

    fn host(clashing: &'static [&'static str]) -> (Host, Shared) {
        let log = Shared::default();
        let host = Host {
            panel: panel(),
            ops: Ops::new(engine(log.clone(), clashing)),
            list_enters: 0,
            changed: Vec::new(),
        };
        (host, log)
    }

    const ROW: f32 = chrome::CELL.y;

    /// Where the host draws the row of entry `i` (its Name and Ext columns).
    fn row_rect(i: usize) -> Rect {
        Rect::from_min_size(
            Pos2::new(chrome::CELL.x, ROW * (2 + i) as f32),
            Vec2::new(40.0 * chrome::CELL.x, ROW),
        )
    }

    impl Window for Host {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let ctx = ui.ctx().clone();
            // The list's key pass comes first, as `list_keys` before `panels_ui`; it waits while
            // the operations hold the keyboard (the gate S1-E puts on it).
            if !self.ops.holds_keyboard()
                && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
            {
                self.list_enters += 1;
            }
            if self.ops.busy()
                && !self.ops.holds_keyboard()
                && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
            {
                self.ops.stop();
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    // The list claims the keyboard on every pass — ungated, on purpose.
                    let resp = ui.interact(ui.max_rect(), list_id(), Sense::click());
                    if ctx.memory(|m| m.focused()) != Some(list_id()) {
                        resp.request_focus();
                    }
                    let p = chrome::PALETTE;
                    let font = chrome::mono(ui);
                    let mut field = None;
                    for (i, e) in self.panel.entries.iter().enumerate() {
                        let rect = row_rect(i);
                        if i == self.panel.highlight {
                            chrome::bar(ui.painter(), rect, true);
                        }
                        if self.ops.renaming() == Some(e.name.as_str()) {
                            field = Some(rect);
                            continue;
                        }
                        let selected = self.panel.is_selected(e);
                        let color = entry_color(&p, e, selected, i == self.panel.highlight);
                        ui.painter().text(
                            rect.min,
                            egui::Align2::LEFT_TOP,
                            &e.name,
                            font.clone(),
                            color,
                        );
                    }
                    if let Some(rect) = field {
                        self.ops.rename_field(ui, rect);
                    }
                });
            if let Some(changed) = self.ops.ui(&ctx, &mut self.panel) {
                self.changed.push(changed);
            }
        }

        fn ready(&self) -> bool {
            true
        }
    }

    fn type_text(drv: &mut Driver, h: &mut Host, text: &str) {
        drv.frame(h, vec![Event::Text(text.to_string())], Modifiers::NONE);
        drv.idle(h, 1);
    }

    fn selected(p: &Panel) -> Vec<String> {
        p.selection().iter().map(|e| e.name.clone()).collect()
    }

    /// [`entry_color`], as the host draws its rows with it: selected rows read yellow — on the
    /// bar too; the bar's own text, a folder, a dot file and a plain file keep the colours of
    /// `panel_ui`'s rule.
    #[test]
    fn selected_rows_draw_in_the_selection_colour() {
        let (mut h, _) = host(&[]);
        let mut drv = Driver::new(800.0, 500.0);
        let p = chrome::PALETTE;
        assert_eq!(selected_color(&p), chrome::cga::YELLOW);
        h.panel.select_group("*.rs", true);
        at(&mut h.panel, "main.rs");
        drv.idle(&mut h, 2);
        let color = |drv: &Driver, name: &str| drv.colors_of(name).unwrap()[0];
        assert_eq!(
            color(&drv, "main.rs"),
            chrome::cga::YELLOW,
            "selected, on the bar"
        );
        assert_eq!(color(&drv, "lib.rs"), chrome::cga::YELLOW, "selected");
        assert_eq!(color(&drv, "docs"), p.folder);
        assert_eq!(color(&drv, ".hidden"), p.dim);
        assert_eq!(color(&drv, "사진.png"), p.text);
        at(&mut h.panel, "사진.png");
        drv.idle(&mut h, 1);
        assert_eq!(color(&drv, "사진.png"), p.bar_text, "the bar, not selected");
        assert_eq!(color(&drv, "main.rs"), chrome::cga::YELLOW);
        let dir = entry("docs", EntryKind::Dir, 0);
        assert_eq!(entry_color(&p, &dir, false, false), p.folder);
        assert_eq!(entry_color(&p, &dir, true, false), chrome::cga::YELLOW);
    }

    /// [`Ops::open_group`] (what `+` and `-` will call): the dialog opens with `*` selected,
    /// typing replaces it, Enter selects the matches (the host's list never sees that Enter);
    /// unselecting works the same; Esc leaves the selection alone.
    #[test]
    fn the_group_dialog_selects_and_unselects_by_pattern() {
        let (mut h, _) = host(&[]);
        let mut drv = Driver::new(800.0, 500.0);
        h.ops.open_group(true);
        drv.idle(&mut h, 2);
        assert!(h.ops.holds_keyboard());
        assert!(drv.find_text("*").is_some(), "the pattern field holds *");
        assert!(drv.find_text("Select").is_some());
        type_text(&mut drv, &mut h, "*.RS");
        assert!(drv.find_text("*.RS").is_some(), "typing replaced the *");
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        assert_eq!(selected(&h.panel), ["lib.rs", "main.rs"]);
        assert!(!h.ops.holds_keyboard());
        h.ops.open_group(false);
        drv.idle(&mut h, 2);
        type_text(&mut drv, &mut h, "m*");
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        assert_eq!(selected(&h.panel), ["lib.rs"]);
        h.ops.open_group(true);
        drv.idle(&mut h, 2);
        drv.key(&mut h, Key::Escape, Modifiers::NONE);
        assert!(!h.ops.holds_keyboard(), "Esc closed it");
        assert_eq!(selected(&h.panel), ["lib.rs"], "and changed nothing");
        assert_eq!(h.list_enters, 0);
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        assert_eq!(
            h.list_enters, 1,
            "with no dialog, Enter is the list's again"
        );
    }

    /// [`Ops::open_copy`] (what F5 will call): the dialog names what (the selection's count)
    /// and has the other panel's folder in sopen's command-line form; Esc cancels and nothing
    /// runs; Enter starts the copy of the
    /// selection to it.
    #[test]
    fn the_copy_dialog_prefills_the_target_enter_starts_esc_cancels() {
        let (mut h, log) = host(&[]);
        let mut drv = Driver::new(900.0, 500.0);
        h.panel.select_group("*.rs", true);
        let other = on("me@box", "/srv/다른 폴더");
        h.ops
            .open_copy(Transfer::Copy, &h.panel, Some("me@box"), &other);
        drv.idle(&mut h, 2);
        assert!(h.ops.holds_keyboard());
        assert!(drv.find_text("Copy 2 items").is_some());
        assert!(
            drv.find_text("me@box:/srv/다른 폴더").is_some(),
            "prefilled"
        );
        drv.key(&mut h, Key::Escape, Modifiers::NONE);
        assert!(!h.ops.holds_keyboard());
        assert!(drv.find_text("Copy 2 items").is_none(), "closed");
        drv.idle(&mut h, 3);
        assert!(log.lock().unwrap().jobs.is_empty(), "Esc ran nothing");

        h.ops
            .open_copy(Transfer::Copy, &h.panel, Some("me@box"), &other);
        drv.idle(&mut h, 2);
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        drv.until(&mut h, 5, |h| !h.changed.is_empty());
        drv.idle(&mut h, 2);
        assert_eq!(h.list_enters, 0, "the dialog's Enter");
        let want = Job::Transfer {
            kind: Transfer::Copy,
            from: on("me@box", "/srv/src"),
            names: vec!["lib.rs".to_string(), "main.rs".to_string()],
            to: other.clone(),
        };
        assert_eq!(log.lock().unwrap().jobs, [want]);
        let changed = &h.changed[0];
        assert_eq!(changed.dirs, [on("me@box", "/srv/src"), other]);
        assert_eq!(changed.bar, None);
        assert!(changed.said.is_none(), "two items: the report says it");
        assert!(
            drv.find_text("done        lib.rs").is_some(),
            "the report is up"
        );
        assert!(selected(&h.panel).is_empty(), "what was done is unselected");
    }

    /// The target field read as sopen's command line: a relative path is relative to the
    /// source folder; a target that does not read keeps the dialog open with the reason; a Move
    /// of the bar's one entry says its name; `..` alone is nothing to act on.
    #[test]
    fn the_copy_dialog_reads_an_edited_target() {
        let (mut h, log) = host(&[]);
        let mut drv = Driver::new(900.0, 500.0);
        at(&mut h.panel, "사진.png");
        h.ops
            .open_copy(Transfer::Move, &h.panel, None, &here("/tmp/x"));
        drv.idle(&mut h, 2);
        assert!(drv.find_text("Move 사진.png").is_some());
        type_text(&mut drv, &mut h, "box:relative");
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        assert!(h.ops.holds_keyboard(), "still open");
        let error = h.ops.copy.as_ref().unwrap().error().unwrap().to_string();
        assert!(error.contains("absolute"), "{error}");
        assert!(
            drv.find_text(&error).is_some(),
            "the reason shows in the dialog"
        );
        // The field kept the keyboard: what is typed now replaces the whole text (selected).
        drv.key(&mut h, Key::A, crate::gui::testkit::command(false));
        type_text(&mut drv, &mut h, "sub/dir/");
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        drv.until(&mut h, 5, |h| !h.changed.is_empty());
        let jobs = &log.lock().unwrap().jobs;
        assert_eq!(
            jobs[..],
            [Job::Transfer {
                kind: Transfer::Move,
                from: here("/srv/src"),
                names: vec!["사진.png".to_string()],
                to: here("/srv/src/sub/dir"),
            }]
        );
        // Nothing to act on: the bar on `..`, nothing selected.
        h.panel.home();
        let said = h
            .ops
            .open_copy(Transfer::Copy, &h.panel, None, &here("/tmp/x"))
            .expect("refused");
        assert!(!h.ops.holds_keyboard());
        assert!(said.error && said.text.contains(".."), "{said:?}");
    }

    #[test]
    fn targets_read_as_the_command_line() {
        let local = here("/srv/src");
        let remote = on("me@box", "/home/me");
        let ok = |text: &str, from: &OpsTarget| OpsTarget::parse(text, from).unwrap();
        assert_eq!(ok("/tmp/x", &local), here("/tmp/x"));
        assert_eq!(ok("/tmp/x/", &local), here("/tmp/x"));
        assert_eq!(ok("/", &local), here("/"));
        assert_eq!(ok("sub", &local), here("/srv/src/sub"));
        assert_eq!(
            ok("sub", &remote),
            on("me@box", "/home/me/sub"),
            "relative: on its host"
        );
        assert_eq!(
            ok("/tmp/x", &remote),
            here("/tmp/x"),
            "a plain path is this machine"
        );
        assert_eq!(ok("box:/srv/a b", &local), on("box", "/srv/a b"));
        assert_eq!(ok("me@[::1]:/x/", &local), on("me@[::1]", "/x"));
        for bad in ["", "box:", "box:rel", "box:~/x", "@box:/x"] {
            assert!(OpsTarget::parse(bad, &local).is_err(), "{bad:?}");
        }
        assert_eq!(on("me@box", "/srv/x").shown(), "me@box:/srv/x");
        assert_eq!(here("/srv/x").shown(), "/srv/x");
    }

    /// The text field's selection (characters), as egui keeps it.
    fn selection(drv: &Driver, id: egui::Id) -> Option<(usize, usize)> {
        let state = egui::text_edit::TextEditState::load(&drv.ctx, id)?;
        let range = state.cursor.char_range()?;
        let (a, b) = (range.primary.index.0, range.secondary.index.0);
        Some((a.min(b), a.max(b)))
    }

    /// F2 (here [`Ops::open_rename`]): the name becomes a field in the host's row, the name
    /// selected and the extension after it; Esc puts everything back; a name the stand-in engine
    /// finds taken is an error in [`Ops::status`] and the field stays for another try; a free one
    /// closes the field and [`Changed`] names the new name for the bar (moving it is S1-E's).
    /// The host's list key pass never sees the field's Enter.
    #[test]
    fn the_rename_field_commits_reverts_and_stays_on_a_taken_name() {
        let (mut h, log) = host(&[]);
        let mut drv = Driver::new(800.0, 500.0);
        at(&mut h.panel, "main.rs");
        h.ops.open_rename(&h.panel, None);
        drv.idle(&mut h, 2);
        assert_eq!(h.ops.renaming(), Some("main.rs"));
        assert!(h.ops.holds_keyboard());
        assert!(
            drv.find_text("main.rs").is_some(),
            "the field holds the name"
        );
        assert_eq!(
            selection(&drv, rename_id()),
            Some((0, 4)),
            "`main` selected"
        );
        type_text(&mut drv, &mut h, "app");
        assert!(drv.find_text("app.rs").is_some());
        drv.key(&mut h, Key::Escape, Modifiers::NONE);
        assert_eq!(h.ops.renaming(), None, "Esc closed it");
        assert!(
            drv.find_text("main.rs").is_some(),
            "the row reads as before"
        );
        drv.idle(&mut h, 2);
        assert!(log.lock().unwrap().jobs.is_empty(), "nothing renamed");

        h.ops.open_rename(&h.panel, None);
        drv.idle(&mut h, 2);
        type_text(&mut drv, &mut h, "taken");
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        drv.until(&mut h, 5, |h| h.ops.status().is_some_and(|s| s.error));
        drv.idle(&mut h, 2);
        assert_eq!(h.ops.renaming(), Some("main.rs"), "the field stays");
        let said = h.ops.status().unwrap();
        assert!(said.text.contains("exists"), "{said:?}");
        assert!(drv.find_text("taken.rs").is_some(), "with what was typed");
        assert!(h.changed.is_empty(), "nothing changed");
        type_text(&mut drv, &mut h, "fresh");
        assert!(
            drv.find_text("fresh.rs").is_some(),
            "the name selected again"
        );
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        drv.until(&mut h, 5, |h| !h.changed.is_empty());
        assert_eq!(h.ops.renaming(), None);
        let changed = &h.changed[0];
        assert_eq!(changed.dirs, [here("/srv/src")]);
        assert_eq!(changed.bar.as_deref(), Some("fresh.rs"));
        let said = changed.said.as_ref().unwrap();
        assert_eq!(said.text, "renamed main.rs → fresh.rs");
        assert!(
            h.ops.status().is_none(),
            "nothing left to say while nothing runs"
        );
        assert_eq!(h.list_enters, 0);
        let jobs: Vec<String> = log
            .lock()
            .unwrap()
            .jobs
            .iter()
            .map(|j| match j {
                Job::Rename { to, .. } => to.clone(),
                Job::Transfer { .. } => "transfer".to_string(),
            })
            .collect();
        assert_eq!(jobs, ["taken.rs", "fresh.rs"]);
    }

    /// An invalid name is refused in the field (nothing asked of the engine); Enter on the name
    /// unchanged closes it; a change of case only goes to the engine (the filesystem decides,
    /// never a string compare).
    #[test]
    fn the_rename_field_checks_names_itself_and_leaves_case_to_the_engine() {
        let (mut h, log) = host(&[]);
        let mut drv = Driver::new(800.0, 500.0);
        at(&mut h.panel, "main.rs");
        h.ops.open_rename(&h.panel, None);
        drv.idle(&mut h, 2);
        type_text(&mut drv, &mut h, "a/b");
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        drv.idle(&mut h, 2);
        assert_eq!(h.ops.renaming(), Some("main.rs"), "stays");
        assert!(h.ops.status().unwrap().text.contains('/'));
        assert!(log.lock().unwrap().jobs.is_empty());
        drv.key(&mut h, Key::Escape, Modifiers::NONE);

        h.ops.open_rename(&h.panel, None);
        drv.idle(&mut h, 2);
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        drv.idle(&mut h, 2);
        assert_eq!(h.ops.renaming(), None, "unchanged: closed");
        assert!(log.lock().unwrap().jobs.is_empty(), "nothing asked");

        h.ops.open_rename(&h.panel, None);
        drv.idle(&mut h, 2);
        type_text(&mut drv, &mut h, "MAIN");
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        drv.until(&mut h, 5, |h| !h.changed.is_empty());
        assert_eq!(
            log.lock().unwrap().jobs,
            [Job::Rename {
                dir: here("/srv/src"),
                from: "main.rs".to_string(),
                to: "MAIN.rs".to_string(),
            }]
        );
        // `..` has no field; nor (until S6) a folder on a remote — a link to one is a file.
        h.panel.home();
        let said = h.ops.open_rename(&h.panel, None).expect("refused");
        assert_eq!(h.ops.renaming(), None);
        assert!(said.error && said.text.contains(".."), "{said:?}");
        at(&mut h.panel, "docs");
        let said = h.ops.open_rename(&h.panel, Some("box")).expect("refused");
        assert!(said.text.contains("box") && said.text.contains("nothing changed"));
        assert_eq!(h.ops.renaming(), None);
        h.panel.entries[1].link = true;
        assert!(
            h.ops.open_rename(&h.panel, Some("box")).is_none(),
            "a link renames"
        );
        assert_eq!(h.ops.renaming(), Some("docs"));
        drv.idle(&mut h, 2);
        assert_eq!(
            selection(&drv, rename_id()),
            Some((0, 4)),
            "a folder: all of it"
        );
    }

    /// A Hangul name: the stem selected by characters, the limit counted in bytes (04 §5.1).
    #[test]
    fn hangul_names_are_selected_by_character_and_limited_by_byte() {
        let (mut h, _) = host(&[]);
        let mut drv = Driver::new(800.0, 500.0);
        at(&mut h.panel, "사진.png");
        h.ops.open_rename(&h.panel, None);
        drv.idle(&mut h, 2);
        assert_eq!(
            selection(&drv, rename_id()),
            Some((0, 2)),
            "`사진` selected"
        );
        assert!(check_name(&"한".repeat(85)).is_ok(), "255 bytes");
        let long = check_name(&"한".repeat(86)).unwrap_err();
        assert!(long.contains("255"), "{long}");
        for bad in ["", ".", "..", "a/b", "a\0b"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_name("...").is_ok());
        assert!(check_name(" 한글 이름 .txt").is_ok());
    }

    /// The name-in-the-way dialog, each answer: Skip; Rename with the free name (Enter); Rename
    /// with an edited name; Skip with "the same for the rest", which answers the last clash
    /// without the dialog. The report says each.
    #[test]
    fn the_clash_dialog_answers_skip_rename_and_the_same_for_the_rest() {
        let (mut h, log) = host(&["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]);
        let mut drv = Driver::new(900.0, 600.0);
        h.panel.entries.clear();
        let names = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"];
        let entries = names.iter().map(|n| entry(n, EntryKind::File, 1)).collect();
        h.panel.fill("/srv/src".to_string(), entries, None, None);
        h.panel.select_all();
        h.ops
            .open_copy(Transfer::Copy, &h.panel, None, &here("/srv/dst"));
        drv.idle(&mut h, 2);
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        let asked = |drv: &mut Driver, h: &mut Host, name: &str| {
            drv.until(h, 5, |h| {
                h.ops
                    .running
                    .as_ref()
                    .and_then(|r| r.clash.as_ref())
                    .is_some_and(|c| c.clash.name == name)
            });
            drv.idle(h, 2);
        };
        asked(&mut drv, &mut h, "a.txt");
        assert!(h.ops.holds_keyboard());
        assert!(
            drv.find_text("a-1.txt").is_some(),
            "the free name, prefilled"
        );
        assert!(drv.find_text("Overwrite").is_none(), "never offered");
        drv.click_text(&mut h, "Skip");
        asked(&mut drv, &mut h, "b.txt");
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        asked(&mut drv, &mut h, "c.txt");
        drv.key(&mut h, Key::A, crate::gui::testkit::command(false));
        type_text(&mut drv, &mut h, "c 사본.txt");
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        asked(&mut drv, &mut h, "d.txt");
        drv.click_text(&mut h, "the same for the rest");
        drv.click_text(&mut h, "Skip");
        drv.until(&mut h, 5, |h| !h.changed.is_empty());
        drv.idle(&mut h, 2);
        let replies = log.lock().unwrap().replies.clone();
        let skip = |same| Reply {
            answer: Answer::Skip,
            same_for_rest: same,
        };
        let rename = |to: &str| Reply {
            answer: Answer::Rename(to.to_string()),
            same_for_rest: false,
        };
        assert_eq!(
            replies,
            [
                ("a.txt".to_string(), skip(false)),
                ("b.txt".to_string(), rename("b-1.txt")),
                ("c.txt".to_string(), rename("c 사본.txt")),
                ("d.txt".to_string(), skip(true)),
                ("e.txt".to_string(), skip(true)),
            ]
        );
        assert_eq!(h.list_enters, 0);
        assert_eq!(
            selected(&h.panel),
            ["a.txt", "d.txt", "e.txt"],
            "the skipped stay"
        );
        assert!(h.ops.holds_keyboard(), "the report is up");
        for line in [
            "skipped     a.txt",
            "renamed     b.txt → b-1.txt",
            "renamed     c.txt → c 사본.txt",
            "skipped     e.txt",
        ] {
            assert!(drv.find_text(line).is_some(), "{line}");
        }
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        assert!(!h.ops.holds_keyboard(), "Enter closed the report");
    }

    /// Rename with "the same for the rest": each later clash takes its own free name. Cancel
    /// (Esc) stops the rest; the items done stay done.
    #[test]
    fn the_clash_dialog_renames_the_rest_or_cancels_it() {
        let names = ["a.txt", "b.txt", "c.txt"];
        for esc in [false, true] {
            let (mut h, log) = host(&["a.txt", "b.txt", "c.txt"]);
            let mut drv = Driver::new(900.0, 600.0);
            let entries = names.iter().map(|n| entry(n, EntryKind::File, 1)).collect();
            h.panel.fill("/srv/src".to_string(), entries, None, None);
            h.panel.select_all();
            h.ops
                .open_copy(Transfer::Copy, &h.panel, None, &here("/srv/dst"));
            drv.idle(&mut h, 2);
            drv.key(&mut h, Key::Enter, Modifiers::NONE);
            drv.until(&mut h, 5, |h| {
                h.ops.running.as_ref().is_some_and(|r| r.asking())
            });
            drv.idle(&mut h, 2);
            if esc {
                drv.key(&mut h, Key::Escape, Modifiers::NONE);
            } else {
                drv.click_text(&mut h, "the same for the rest");
                drv.click_text(&mut h, "Rename");
            }
            drv.until(&mut h, 5, |h| !h.changed.is_empty());
            drv.idle(&mut h, 2);
            let answers: Vec<Answer> = log
                .lock()
                .unwrap()
                .replies
                .iter()
                .map(|(_, r)| r.answer.clone())
                .collect();
            let want = if esc {
                vec![Answer::Cancel]
            } else {
                ["a-1.txt", "b-1.txt", "c-1.txt"]
                    .map(|n| Answer::Rename(n.to_string()))
                    .to_vec()
            };
            assert_eq!(answers, want, "esc={esc}");
            if esc {
                assert!(drv.find_text("not reached b.txt").is_some());
            }
        }
    }

    /// The dialog's Rename field is checked as names are; a bad one keeps the dialog with the
    /// reason, and nothing is answered.
    #[test]
    fn the_clash_dialog_refuses_a_bad_name() {
        let mut d = ClashDialog::open(
            Clash {
                name: "a.txt".to_string(),
                free: "a-1.txt".to_string(),
            },
            "/srv/dst",
        );
        d.rename = "x/y".to_string();
        assert_eq!(d.answer_rename(), None);
        assert!(d.error.as_deref().is_some_and(|e| e.contains('/')));
        d.rename = "x".to_string();
        assert_eq!(
            d.answer_rename(),
            Some(Reply {
                answer: Answer::Rename("x".to_string()),
                same_for_rest: false
            })
        );
    }

    fn progress(item: usize, items: usize, name: &str, done: u64, total: u64) -> Progress {
        Progress {
            verb: "copying",
            item,
            items,
            name: name.to_string(),
            done,
            total,
        }
    }

    #[test]
    fn progress_lines_read_as_the_status_row_shows_them() {
        let cases = [
            (
                progress(3, 12, "한글 파일.mp4", 13_500_000, 30_000_000),
                "copying 3/12 · 한글 파일.mp4 45% · 13.5 MB / 30.0 MB · Esc stops",
            ),
            (
                progress(1, 1, "big.iso", 4_718_592, 9_437_184),
                "copying 1/1 · big.iso 50% · 4.7 MB / 9.4 MB · Esc stops",
            ),
            (
                progress(1, 1, "small.txt", 10, 20),
                "copying small.txt… · Esc stops",
            ),
            (
                progress(1, 1, "exactly.bin", 0, BIG_FILE),
                "copying exactly.bin… · Esc stops",
            ),
            (
                progress(2, 3, "empty.txt", 0, 0),
                "copying 2/3 · empty.txt · Esc stops",
            ),
        ];
        for (p, want) in cases {
            assert_eq!(progress_line(&p), want);
        }
    }

    fn item(name: &str, outcome: ItemOutcome) -> Item {
        Item {
            name: name.to_string(),
            outcome,
        }
    }

    fn copy_job(names: &[&str]) -> Job {
        Job::Transfer {
            kind: Transfer::Copy,
            from: here("/srv/src"),
            names: names.iter().map(|n| n.to_string()).collect(),
            to: on("me@box", "/srv/받은 편지"),
        }
    }

    /// One item fully done is a status line; anything else is the report dialog, an item a row
    /// with its reason, the notes after — Hangul names included — and Enter closes it.
    #[test]
    fn the_report_lists_every_item_and_one_success_is_a_status_line() {
        let one = Report {
            items: vec![item("사진.png", ItemOutcome::Done)],
            notes: vec![],
        };
        match finish(&copy_job(&["사진.png"]), one) {
            Finished::Status(s) => {
                assert_eq!(s.text, "copied 사진.png to me@box:/srv/받은 편지");
                assert!(!s.error);
            }
            Finished::Report(_) => panic!("one item done is a status line"),
        }
        let report = Report {
            items: vec![
                item("main.rs", ItemOutcome::Done),
                item("사진.png", ItemOutcome::Renamed("사진-1.png".to_string())),
                item("한글 문서.txt", ItemOutcome::Skipped),
                item(
                    "big.iso",
                    ItemOutcome::Failed("Permission denied".to_string()),
                ),
                item("zeta.md", ItemOutcome::NotReached),
            ],
            notes: vec!["the time of 사진-1.png could not be set".to_string()],
        };
        let names = ["main.rs", "사진.png", "한글 문서.txt", "big.iso", "zeta.md"];
        let Finished::Report(mut view) = finish(&copy_job(&names), report) else {
            panic!("five items are a report");
        };
        let lines = [
            "Copy to me@box:/srv/받은 편지: 1 done · 1 renamed · 1 skipped · 1 failed · 1 not reached",
            "done        main.rs",
            "renamed     사진.png → 사진-1.png",
            "skipped     한글 문서.txt",
            "failed      big.iso — Permission denied",
            "not reached zeta.md",
            "the time of 사진-1.png could not be set",
        ];
        assert_eq!(view.lines(), lines);
        struct One<'a>(&'a mut ReportView, bool);
        impl Window for One<'_> {
            fn ui(&mut self, ui: &mut egui::Ui) {
                self.1 |= self.0.ui(ui.ctx());
            }
            fn ready(&self) -> bool {
                true
            }
        }
        let mut drv = Driver::new(900.0, 600.0);
        let mut win = One(&mut view, false);
        drv.idle(&mut win, 2);
        for line in lines {
            assert!(drv.find_text(line).is_some(), "{line}");
        }
        let failed = drv
            .painted_colors_of("failed      big.iso — Permission denied")
            .unwrap();
        assert_eq!(
            failed[0],
            chrome::PALETTE.dialog_error,
            "a failure reads red"
        );
        drv.key(&mut win, Key::Enter, Modifiers::NONE);
        assert!(win.1, "Enter closed it");
        // A one-item failure, a single skip and a note are reports, never a quiet line.
        for outcome in [ItemOutcome::Skipped, ItemOutcome::Failed("x".to_string())] {
            let r = Report {
                items: vec![item("a", outcome)],
                notes: vec![],
            };
            assert!(matches!(finish(&copy_job(&["a"]), r), Finished::Report(_)));
        }
        let noted = Report {
            items: vec![item("a", ItemOutcome::Done)],
            notes: vec!["kept both".to_string()],
        };
        assert!(matches!(
            finish(&copy_job(&["a"]), noted),
            Finished::Report(_)
        ));
    }

    /// While a job runs its status line says what it is doing — the job before the first news,
    /// then the progress line, then `stopping…` after [`Running::stop`] (what Esc calls), which
    /// reaches the engine. Esc as a key is `esc_during_a_transfer_stops_it`.
    #[test]
    fn the_status_line_follows_a_running_job_and_a_stop_reaches_the_engine() {
        let gate = Arc::new(Mutex::new(()));
        let hold = gate.lock().unwrap();
        let g = gate.clone();
        let run: Run = Arc::new(move |_job, link: &Link| {
            link.progress(progress(1, 2, "한글.bin", 50, 100));
            drop(g.lock().unwrap());
            let mut r = Report::default();
            if link.stopped() {
                r.items.push(item("한글.bin", ItemOutcome::NotReached));
            }
            r
        });
        let ctx = egui::Context::default();
        let mut running = start(&run, copy_job(&["한글.bin", "b"]), &ctx);
        let first = running.status().text;
        assert!(
            first == "copying 2 items… · Esc stops"
                || first == "copying 1/2 · 한글.bin 50% · 50 B / 100 B · Esc stops",
            "{first}"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while running.progress.is_none() {
            assert!(std::time::Instant::now() < deadline);
            assert!(running.poll().is_none());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            running.status().text,
            "copying 1/2 · 한글.bin 50% · 50 B / 100 B · Esc stops"
        );
        running.stop();
        assert!(running.stopping());
        assert_eq!(running.status().text, "stopping…");
        drop(hold);
        let report = bounded("wait", move || running.wait());
        assert_eq!(report.items, [item("한글.bin", ItemOutcome::NotReached)]);
    }

    /// The seam's ends: a window gone (the job dropped) or a stop while the dialog asks → Cancel;
    /// the window closing waits and answers Cancel; a worker that dies leaves a report that
    /// says so, never a job that runs on screen for ever; every message wakes the window.
    #[test]
    fn the_seam_ends_well_on_every_path() {
        let asking: Run = Arc::new(|_job, link: &Link| {
            let reply = link.ask(Clash {
                name: "a".to_string(),
                free: "a-1".to_string(),
            });
            Report {
                items: vec![item("a", ItemOutcome::Skipped)],
                notes: vec![format!("{:?}", reply.answer)],
            }
        });
        let ctx = egui::Context::default();
        let wait_asked = |r: &mut Running| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !r.asking() {
                assert!(std::time::Instant::now() < deadline);
                r.poll();
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        // Esc while asking.
        let mut r = start(&asking, copy_job(&["a"]), &ctx);
        wait_asked(&mut r);
        r.stop();
        assert!(!r.asking(), "Esc answered it");
        assert_eq!(bounded("wait", move || r.wait()).notes, ["Cancel"]);
        // The window closes while asking.
        let mut r = start(&asking, copy_job(&["a"]), &ctx);
        wait_asked(&mut r);
        assert_eq!(bounded("wait", move || r.wait()).notes, ["Cancel"]);
        // Dropped while asking: the worker is not left waiting.
        let (tx, rx) = mpsc::channel();
        let told: Run = Arc::new(move |job, link: &Link| {
            let report = asking(job, link);
            tx.send(report.notes.clone()).unwrap();
            report
        });
        let mut r = start(&told, copy_job(&["a"]), &ctx);
        wait_asked(&mut r);
        bounded("drop", move || drop(r));
        let notes = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the worker ended");
        assert_eq!(notes, ["Cancel"]);
        // A worker that dies.
        let dies: Run = Arc::new(|_job, _link: &Link| panic!("the engine broke (a test)"));
        let mut r = start(&dies, copy_job(&["a"]), &ctx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let report = loop {
            if let Some(report) = r.poll() {
                break report;
            }
            assert!(std::time::Instant::now() < deadline, "it never ended");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(
            report.notes.iter().any(|n| n.contains("without a report")),
            "{report:?}"
        );
        // A message wakes the window, and so does the end — each on a context of its own
        // (egui asks the platform once per pass for a repaint already asked for).
        for progress_first in [true, false] {
            let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let w = woken.clone();
            let quiet = egui::Context::default();
            quiet.set_request_repaint_callback(move |_| {
                w.fetch_add(1, Ordering::SeqCst);
            });
            let gate = Arc::new(Mutex::new(()));
            let hold = gate.lock().unwrap();
            let g = gate.clone();
            let chatty: Run = Arc::new(move |_job, link: &Link| {
                if progress_first {
                    link.progress(progress(1, 2, "a", 1, 2));
                }
                drop(g.lock().unwrap());
                Report::default()
            });
            let mut r = start(&chatty, copy_job(&["a", "b"]), &quiet);
            if progress_first {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while r.progress.is_none() {
                    assert!(std::time::Instant::now() < deadline);
                    r.poll();
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                assert_eq!(woken.load(Ordering::SeqCst), 1, "the progress woke it");
            } else {
                std::thread::sleep(std::time::Duration::from_millis(50));
                assert_eq!(woken.load(Ordering::SeqCst), 0, "nothing to say yet");
                drop(hold);
                bounded("wait", move || r.wait());
                assert!(woken.load(Ordering::SeqCst) >= 1, "the end woke it");
                continue;
            }
            drop(hold);
            bounded("wait", move || r.wait());
        }
    }

    /// Runs `f` on a thread of its own and fails unless it ends within 5 s: a shutdown that
    /// blocks fails the test instead of hanging the suite.
    fn bounded<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(value) => value,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("{what} did not end within 5 s"),
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what} panicked"),
        }
    }

    fn clash(name: &str) -> Clash {
        Clash {
            name: name.to_string(),
            free: crate::remote::suffixed(name, 1),
        }
    }

    /// A stop (Esc) wins over "the same for the rest": with a remembered Rename and a question
    /// already waiting (sent before the window polled), it is answered Cancel — never Rename.
    #[test]
    fn a_stop_wins_over_the_remembered_answer_in_the_window() {
        let ctx = egui::Context::default();
        let (mut r, link) = pair(copy_job(&["a.txt", "b.txt"]), &ctx);
        r.same = Some(Answer::Rename("a-1.txt".to_string()));
        link.send(super::Event::Clash(clash("b.txt")));
        r.stop();
        assert!(r.poll().is_none());
        assert!(!r.asking(), "no dialog for a stopped job");
        let reply = link.replies.try_recv().expect("answered");
        assert_eq!(reply.answer, Answer::Cancel);
        assert!(link.replies.try_recv().is_err(), "answered once");
    }

    /// The engine's side of the same race: an answer that arrives after Esc set the flag is
    /// taken as Cancel.
    #[test]
    fn a_stop_wins_over_an_answer_on_its_way() {
        let ctx = egui::Context::default();
        let (r, link) = pair(copy_job(&["a.txt", "b.txt"]), &ctx);
        let window = std::thread::spawn(move || {
            let asked = r.events.recv_timeout(std::time::Duration::from_secs(5));
            assert!(
                matches!(asked, Ok(super::Event::Clash(_))),
                "the question came"
            );
            r.stop.store(true, Ordering::Relaxed);
            r.replies
                .send(Reply {
                    answer: Answer::Rename("b-1.txt".to_string()),
                    same_for_rest: true,
                })
                .unwrap();
            r
        });
        let reply = bounded("ask", move || link.ask(clash("b.txt")));
        assert_eq!(reply.answer, Answer::Cancel);
        drop(window.join().unwrap());
    }

    /// One dialog at a time: a question that arrives while the select-group dialog is open puts
    /// it aside — the question has the keyboard (Esc answers it, not the group), then the
    /// report, and the group comes back as it was once both are gone.
    #[test]
    fn a_question_puts_the_group_dialog_aside() {
        let gate = Arc::new(Mutex::new(()));
        let hold = gate.lock().unwrap();
        let log = Shared::default();
        let inner = engine(log.clone(), &["main.rs"]);
        let g = gate.clone();
        let run: Run = Arc::new(move |job, link: &Link| {
            drop(g.lock().unwrap());
            inner(job, link)
        });
        let mut h = Host {
            panel: panel(),
            ops: Ops::new(run),
            list_enters: 0,
            changed: Vec::new(),
        };
        let mut drv = Driver::new(900.0, 600.0);
        at(&mut h.panel, "main.rs");
        h.ops
            .open_copy(Transfer::Copy, &h.panel, None, &here("/srv/dst"));
        drv.idle(&mut h, 2);
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        assert!(h.ops.busy());
        h.ops.open_group(true);
        drv.idle(&mut h, 2);
        let group = "Select the names that match";
        assert!(drv.find_text(group).is_some());
        drop(hold);
        drv.until(&mut h, 5, |h| {
            h.ops.running.as_ref().is_some_and(|r| r.asking())
        });
        drv.idle(&mut h, 2);
        assert!(drv.find_text("Name exists").is_some());
        assert!(drv.find_text(group).is_none(), "put aside");
        assert!(h.ops.holds_keyboard());
        drv.key(&mut h, Key::Escape, Modifiers::NONE);
        drv.until(&mut h, 5, |h| !h.changed.is_empty());
        drv.idle(&mut h, 2);
        let answers: Vec<Answer> = log
            .lock()
            .unwrap()
            .replies
            .iter()
            .map(|(_, r)| r.answer.clone())
            .collect();
        assert_eq!(answers, [Answer::Cancel], "Esc was the question's");
        assert!(drv.find_text("not reached main.rs").is_some(), "the report");
        assert!(drv.find_text(group).is_none(), "still aside");
        drv.key(&mut h, Key::Escape, Modifiers::NONE);
        drv.idle(&mut h, 2);
        assert!(
            drv.find_text("not reached main.rs").is_none(),
            "the report closed"
        );
        assert!(drv.find_text(group).is_some(), "the group is back");
        assert!(drv.find_text("*").is_some(), "with its pattern");
        drv.key(&mut h, Key::Escape, Modifiers::NONE);
        assert!(!h.ops.holds_keyboard());
        assert_eq!(h.list_enters, 0);
    }

    /// Esc as an event during an ordinary transfer (no dialog up): the host's dispatch reaches
    /// [`Ops::stop`]; the status row goes from the progress line to `stopping…`; the engine
    /// sees it and the report lists what was not reached.
    #[test]
    fn esc_during_a_transfer_stops_it() {
        let run: Run = Arc::new(|job, link: &Link| {
            let Job::Transfer { names, .. } = job else {
                unreachable!()
            };
            link.progress(progress(1, 2, &names[0], 5, 10));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !link.stopped() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Report {
                items: names
                    .into_iter()
                    .map(|name| Item {
                        name,
                        outcome: ItemOutcome::NotReached,
                    })
                    .collect(),
                notes: vec![],
            }
        });
        let mut h = Host {
            panel: panel(),
            ops: Ops::new(run),
            list_enters: 0,
            changed: Vec::new(),
        };
        let mut drv = Driver::new(900.0, 600.0);
        h.panel.select_group("*.rs", true);
        h.ops
            .open_copy(Transfer::Copy, &h.panel, None, &here("/srv/dst"));
        drv.idle(&mut h, 2);
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        let line = "copying 1/2 · lib.rs 50% · 5 B / 10 B · Esc stops";
        drv.until(&mut h, 5, |h| {
            h.ops.status().is_some_and(|s| s.text == line)
        });
        assert!(!h.ops.holds_keyboard(), "no dialog: Esc is the host's");
        drv.frame(&mut h, vec![key_down(Key::Escape)], Modifiers::NONE);
        assert_eq!(h.ops.status().map(|s| s.text).as_deref(), Some("stopping…"));
        drv.until(&mut h, 5, |h| !h.changed.is_empty());
        drv.idle(&mut h, 2);
        assert!(drv.find_text("not reached lib.rs").is_some());
        assert!(drv.find_text("not reached main.rs").is_some());
        assert_eq!(
            selected(&h.panel),
            ["lib.rs", "main.rs"],
            "nothing done: all kept"
        );
    }

    /// A first item done, then Esc at the second's question: the report says done and not
    /// reached; what was done is unselected, what was not stays selected.
    #[test]
    fn a_cancelled_question_keeps_what_was_done() {
        let (mut h, log) = host(&["b.txt"]);
        let mut drv = Driver::new(900.0, 600.0);
        let entries = ["a.txt", "b.txt", "c.txt"]
            .iter()
            .map(|n| entry(n, EntryKind::File, 1))
            .collect();
        h.panel.fill("/srv/src".to_string(), entries, None, None);
        h.panel.select_all();
        h.ops
            .open_copy(Transfer::Copy, &h.panel, None, &here("/srv/dst"));
        drv.idle(&mut h, 2);
        drv.key(&mut h, Key::Enter, Modifiers::NONE);
        drv.until(&mut h, 5, |h| {
            h.ops.running.as_ref().is_some_and(|r| r.asking())
        });
        drv.idle(&mut h, 2);
        drv.key(&mut h, Key::Escape, Modifiers::NONE);
        drv.until(&mut h, 5, |h| !h.changed.is_empty());
        drv.idle(&mut h, 2);
        assert_eq!(log.lock().unwrap().replies.len(), 1);
        for line in [
            "Copy to /srv/dst: 1 done · 2 not reached",
            "done        a.txt",
            "not reached b.txt",
            "not reached c.txt",
        ] {
            assert!(drv.find_text(line).is_some(), "{line}");
        }
        assert_eq!(selected(&h.panel), ["b.txt", "c.txt"]);
    }

    /// A key pressed (not let go): the press is what the key's handlers act on.
    fn key_down(key: Key) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }
}
