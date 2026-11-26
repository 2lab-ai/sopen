//! Save As (07 §7): the edited result written to a new local file — the modal the hex, image
//! and media windows share (Alt+Cmd/Ctrl+S and F7; the text viewer is read-only and has none),
//! and the writer under it.
//!
//! - **The modal** ([`SaveAs`]): one path field, first `<download folder>/<stem>.edited<ext>`
//!   or the next free `-N` ([`first_path`]); Browse… (the system's save dialog, a
//!   [`Chooser`]); the size and the time expected at the rate the viewer is getting
//!   ([`expected_line`]). While it writes it shows the bytes and the time left
//!   ([`progress_line`]) after a row of `█░` cells; Esc or Cancel stops it. It ends when a save
//!   lands or when it is dismissed ([`Ended`]); a save that does not land leaves it open, saying
//!   why. It is cells (10 §4–§6, X3-questions): an `ops::dialog` box — its field `ops::dialog`'s,
//!   `[ Browse… ]`, `[ Save ]  [ Cancel ]` — on the window's grid ([`SaveAs::show`]) or on one
//!   the window gives, between its title and status rows ([`SaveAs::show_on`]); in a terminal it
//!   is drawn as in the window.
//! - **The writer** ([`write`]): it removes what killed saves left in the chosen folder
//!   (`binary::sweep_dead`, as `save_new` does), then refuses before anything is made when the
//!   folder has less free space than the output ([`room`]). Then it writes through `binary::save_new`'s
//!   routine, unchanged (`save_new_deciding`: the same, with the cancel's [`Decision`]): a
//!   private staging folder, `part` created exclusively, and a hard-link publish that fails if
//!   anything is at the name, never over anything nor through a link. The window's
//!   [`Producer`] fills `part`, reporting progress and stopping on a cancel ([`Ctl`]); its
//!   [`Producer::check`] then may refuse, and nothing is saved.
//! - **What a window does with it** (ED4–ED6, each in the change that wires it): it adds
//!   `chrome::SAVE_AS_KEY` (F7) to its F-key row and its help line — no row lists F7 before a
//!   window answers it. On [`pressed`] — asked before its own Cmd/Ctrl+S checks, which take
//!   Alt+Cmd/Ctrl+S as theirs — it opens a [`SaveAs`] over a [`Job`] and calls
//!   [`SaveAs::show`] every pass while it is open, taking no edits meanwhile. On
//!   [`Ended::Saved`] its edits are somewhere, as after a Save copy (`HexDoc::mark_copied`).
//!   A window closing during a write either calls [`SaveAs::cancel`] and goes on calling
//!   [`SaveAs::show`] until the modal is no longer [`SaveAs::busy`], or drops it, which
//!   cancels and waits.

use super::binary::{self, Chooser, Decision, SaveError};
use super::browser::ops::dialog::{self, Button, Ink, Inputs, Part};
use super::cells::{self, Cells, Grid};
use crate::remote::patch::{about, part_of};
use crate::remote::LocalFs;
use anyhow::{anyhow, Result};
use eframe::egui::{self, Key, Modifiers};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How many bytes the output will have, as the window knows it before writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// Known: a hex Save As (hex never changes the size), an image already encoded.
    Exact(u64),
    /// An estimate: a media render (bitrate × length, 07 §9).
    About(u64),
}

impl Size {
    fn bytes(self) -> u64 {
        match self {
            Size::Exact(n) | Size::About(n) => n,
        }
    }

    /// What the staging lock records for a sweep: only an exact size. A render that ends short
    /// of its estimate would otherwise leave a complete `part` that a later save sweeps as a
    /// dead partial (`binary::sweep_dead`).
    fn exact(self) -> Option<u64> {
        match self {
            Size::Exact(n) => Some(n),
            Size::About(_) => None,
        }
    }
}

/// What a [`Producer`] runs by: where its progress goes, and whether the save was cancelled.
pub struct Ctl<'a> {
    decision: &'a Decision,
    progress: &'a dyn Fn(u64),
}

impl<'a> Ctl<'a> {
    /// `decision` is the save's cancel ([`Decision::cancel`] stops it); `progress` gets the
    /// bytes written so far.
    pub fn new(decision: &'a Decision, progress: &'a dyn Fn(u64)) -> Self {
        Self { decision, progress }
    }

    /// The bytes written into `part` so far.
    pub fn progress(&self, done: u64) {
        (self.progress)(done)
    }

    /// The save was cancelled: stop at the next chunk. Whatever the fill returns then, nothing
    /// is published.
    pub fn cancelled(&self) -> bool {
        self.decision.cancelled()
    }
}

/// What writes the output: the window's edits applied to its file (hex: the source read fresh
/// with the patches applied; image: the encoder's output; media: ffmpeg's render).
pub trait Producer: Send {
    /// Writes the whole output into `part` — a new, empty file in sopen's private staging
    /// folder, open for reading and writing — and returns the bytes written. Progress goes to
    /// [`Ctl::progress`]; [`Ctl::cancelled`] is looked at between chunks. After a save that did
    /// not land (cancelled, refused, the name taken) the same producer is asked again, on a new
    /// empty `part`: every fill starts over.
    fn fill(&mut self, part: &mut File, ctl: &Ctl<'_>) -> Result<u64>;

    /// After the transfer, before anything gets the name: `Err(why)` refuses — nothing is
    /// saved, `part` and the staging folder are removed, and `why` is said (07 §7 step 4: the
    /// source changed meanwhile; a render cut short). `written` is what [`Producer::fill`]
    /// returned. Not asked after a cancel.
    fn check(&mut self, written: u64) -> std::result::Result<(), String> {
        let _ = written;
        Ok(())
    }
}

/// How a Save As ended. Only [`Outcome::Saved`] put anything under a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Complete at `path`. `notes` are what the owner must hear about how.
    Saved {
        path: PathBuf,
        bytes: u64,
        notes: Vec<String>,
    },
    /// The folder has less free space than the output: nothing was made.
    NoRoom { dir: PathBuf, need: u64, free: u64 },
    /// Something is at `path` (from the start, or since): it was not touched. `next` is the
    /// next free name.
    Taken { path: PathBuf, next: PathBuf },
    /// The producer's check refused: nothing was saved, and nothing is left but what `notes`
    /// names.
    Refused { why: String, notes: Vec<String> },
    /// Esc, Cancel or closing: nothing was saved, and nothing is left but what `notes` names.
    Cancelled { notes: Vec<String> },
    /// Anything else, said whole.
    Failed(String),
}

impl Outcome {
    /// The window's line: `saved <path>` (the home folder as `~`), or why nothing was.
    pub fn line(&self) -> String {
        match self {
            Outcome::Saved { path, .. } => format!("saved {}", binary::tilde(path)),
            Outcome::NoRoom { dir, need, free } => format!(
                "needs {} free in {}, has {}; nothing was written",
                crate::human(*need),
                binary::tilde(dir),
                crate::human(*free)
            ),
            Outcome::Taken { path, next } => format!(
                "{} exists — not replaced. Save again saves as {}",
                file_name(path),
                file_name(next)
            ),
            Outcome::Refused { why, .. } => format!("{why}; nothing was saved"),
            Outcome::Cancelled { .. } => binary::SAVE_CANCELLED.to_string(),
            Outcome::Failed(why) => why.clone(),
        }
    }

    /// What cleanup could not remove, beside the line.
    fn notes(&self) -> &[String] {
        match self {
            Outcome::Saved { notes, .. }
            | Outcome::Refused { notes, .. }
            | Outcome::Cancelled { notes } => notes,
            _ => &[],
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// The field's first value: `<folder>/<stem>.edited<ext>` (`model.bin` → `model.edited.bin`),
/// or the next free `-N` and a note saying so (`binary::suggest`). Windows pass
/// `binary::download_dir()`.
pub fn first_path(folder: &Path, name: &str) -> (PathBuf, Option<String>) {
    binary::suggest(folder.join(crate::remote::patch::edited_name(name)))
}

/// A rate worth dividing by.
fn usable(rate: Option<f64>) -> Option<f64> {
    rate.filter(|r| r.is_finite() && *r > 0.0)
}

/// What the modal says before writing: `24.0 GB — about 1 h 50 min at 3.6 MB/s` at `rate`
/// (bytes per second the viewer is getting from the file's host); the size alone when no rate
/// is known (or there is nothing to move); `(estimated)` after an estimate.
pub fn expected_line(size: Size, rate: Option<f64>) -> String {
    let shown = match size {
        Size::Exact(n) => crate::human(n),
        Size::About(n) => format!("{} (estimated)", crate::human(n)),
    };
    match usable(rate).filter(|_| size.bytes() > 0) {
        Some(rate) => format!(
            "{shown} — {} at {}",
            about(size.bytes() as f64 / rate),
            rate_text(rate)
        ),
        None => shown,
    }
}

/// `3.6 MB/s`; a rate under 1 B/s (a stalled link still moving a little) as `less than 1 B/s`,
/// never `0 B/s`.
fn rate_text(rate: f64) -> String {
    if rate < 1.0 {
        "less than 1 B/s".to_string()
    } else {
        format!("{}/s", crate::human(rate as u64))
    }
}

/// The transfer as it runs: `37 % (8.9 of 24.0 GB) — about 1 h 10 min left`. The time left is
/// at the transfer's own rate once it has run a second (`elapsed`), at the viewer's `rate`
/// before that, and not said when neither is known. An estimate that the output runs past says
/// only what is written.
pub fn progress_line(done: u64, size: Size, elapsed: Duration, rate: Option<f64>) -> String {
    let total = size.bytes();
    if done > total || total == 0 {
        return format!("{} written", crate::human(done));
    }
    let percent = (u128::from(done) * 100 / u128::from(total)) as u64;
    let line = format!("{percent} % ({})", part_of(done, total));
    if done == total {
        return line;
    }
    let measured = (elapsed >= Duration::from_secs(1) && done > 0)
        .then(|| done as f64 / elapsed.as_secs_f64());
    match usable(measured.or(rate)) {
        Some(rate) => format!("{line} — {} left", about((total - done) as f64 / rate)),
        None => line,
    }
}

/// The room check (07 §7 step 1): less free space in `dir` than the output refuses. A folder
/// whose free space cannot be read (it does not exist, say) is not refused here; the save says
/// what is wrong with it.
pub fn room(size: Size, free: Option<u64>, dir: &Path) -> Option<Outcome> {
    let need = size.bytes();
    let free = free?;
    (free < need).then(|| Outcome::NoRoom {
        dir: dir.to_path_buf(),
        need,
        free,
    })
}

/// The folder a save into `dest` writes in.
fn folder_of(dest: &Path) -> PathBuf {
    match dest.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// The free space of the local folder `dir` (`statvfs`, by the path's bytes: a name that is not
/// UTF-8 too), when it can be read.
fn free_space(dir: &Path) -> Option<u64> {
    LocalFs::free_space_at(dir)
}

/// A refusal by the producer's check, carried through `save_new`'s error.
#[derive(Debug)]
struct CheckRefused(String);

impl std::fmt::Display for CheckRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CheckRefused {}

/// What a save that did not land says beyond its reason: what its cleanup could not remove.
/// `save_new` puts that on the error as a context of its own, beside its `<dest> was not
/// saved`.
fn left_notes(e: &anyhow::Error, dest: &Path) -> Vec<String> {
    let not_saved = format!("{} was not saved", dest.display());
    e.chain()
        .filter(|c| !c.is::<crate::remote::Cancelled>() && !c.is::<CheckRefused>())
        .map(ToString::to_string)
        .filter(|c| *c != not_saved)
        .collect()
}

/// Writes the producer's output to `dest`, a new local file, or nothing (07 §7): the folder's
/// dead partials swept, then the room check, before anything is made; then `binary::save_new`'s
/// staging and publish, unchanged,
/// with `ctl`'s decision as its cancel. Whatever the fill returns, a cancel decided by then
/// wins, and the check is not asked; a cancel after the publish has begun is refused
/// ([`Decision::cancel`]), and the file is saved. Never over anything, nor through a link.
pub fn write(dest: &Path, size: Size, producer: &mut dyn Producer, ctl: &Ctl<'_>) -> Outcome {
    let seams = Seams {
        free: &mut free_space,
        staging: &mut || {},
    };
    write_with(dest, size, producer, ctl, seams)
}

/// What [`write`] measures, and where its staging begins: the tests' stand-ins (a small disk;
/// whether a refusal ever reached the staging).
struct Seams<'a> {
    /// The folder's free space.
    free: &'a mut dyn FnMut(&Path) -> Option<u64>,
    /// Called right before `save_new` is entered — before which nothing is made.
    staging: &'a mut dyn FnMut(),
}

/// [`write`] over `seams`.
fn write_with(
    dest: &Path,
    size: Size,
    producer: &mut dyn Producer,
    ctl: &Ctl<'_>,
    seams: Seams<'_>,
) -> Outcome {
    let dir = folder_of(dest);
    // What killed saves left here goes first (`save_new` sweeps again: nothing left by then),
    // so a dead partial never counts against the room.
    binary::sweep_dead_in(&dir);
    if let Some(refused) = room(size, (seams.free)(&dir), &dir) {
        return refused;
    }
    (seams.staging)();
    let fill = |part: &mut File| -> Result<u64> {
        let wrote = producer.fill(part, ctl);
        if ctl.cancelled() {
            return Err(anyhow!(crate::remote::Cancelled));
        }
        let n = wrote?;
        producer
            .check(n)
            .map_err(|why| anyhow!(CheckRefused(why)))?;
        Ok(n)
    };
    match binary::save_new_deciding(dest, size.exact(), fill, &mut |_, _| Ok(()), ctl.decision) {
        Ok(saved) => Outcome::Saved {
            path: dest.to_path_buf(),
            bytes: saved.bytes,
            notes: saved.notes,
        },
        Err(SaveError::Exists { next }) => Outcome::Taken {
            path: dest.to_path_buf(),
            next,
        },
        Err(SaveError::Failed(e)) if crate::remote::is_cancelled(&e) => Outcome::Cancelled {
            notes: left_notes(&e, dest),
        },
        Err(SaveError::Failed(e)) => match e.chain().find_map(|c| c.downcast_ref::<CheckRefused>())
        {
            Some(CheckRefused(why)) => Outcome::Refused {
                why: why.clone(),
                notes: left_notes(&e, dest),
            },
            None => Outcome::Failed(format!("{e:#}")),
        },
    }
}

/// Whether Save As was asked for in this pass: Alt+Cmd/Ctrl+S exactly, or F7 alone — F7 not
/// while a text field has the typing (04 §3), the chord as Cmd/Ctrl+S is, any time. Both are
/// taken. egui's own shortcut check (`consume_key`, the windows' `command_key`) ignores an
/// extra Alt, so a window's Cmd/Ctrl+S and Shift+Cmd/Ctrl+S would take Alt+Cmd/Ctrl+S for
/// theirs: a window asks this first.
pub fn pressed(ctx: &egui::Context) -> bool {
    let chord = ctx.input_mut(|i| {
        let at = i.events.iter().position(|e| {
            matches!(
                e,
                egui::Event::Key { key: Key::S, pressed: true, modifiers, .. }
                    if alt_command(*modifiers)
            )
        });
        at.map(|at| i.events.remove(at)).is_some()
    });
    let f7 = super::fkey_alone(ctx, Key::F7);
    chord || f7
}

/// Alt with Cmd (macOS) / Ctrl (elsewhere), nothing else (`browser::keys::Mods`'s reading of
/// egui's flags: ⌘ is `mac_cmd` + `command`, Ctrl is `ctrl` + `command`).
fn alt_command(m: Modifiers) -> bool {
    m.command && !(m.mac_cmd && m.ctrl) && m.alt && !m.shift
}

// ---------------------------------------------------------------------------------------------
// The modal.

/// What a window hands the modal.
pub struct Job {
    /// The file's name (`model.bin`): the field's first value is `<folder>/model.edited.bin`,
    /// and a folder typed in the field gets that name inside it.
    pub name: String,
    pub size: Size,
    /// Bytes per second the viewer is getting from the file's host; `None` when none was
    /// measured, or for a local file.
    pub rate: Option<f64>,
    pub producer: Box<dyn Producer>,
}

/// How the modal ended (it is done; the window drops it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The edits are in a new local file: the window counts them as put somewhere, as after a
    /// Save copy. `line` is its status line, `saved <path>` (also printed on stdout, the path
    /// whole); `notes` are what the owner must read about how.
    Saved {
        path: PathBuf,
        line: String,
        notes: Vec<String>,
    },
    /// Dismissed — Esc or Cancel while no save runs: nothing was saved by it.
    Closed,
}

/// A save's worker hands back the producer with the outcome, for the next try.
type Landed = (Box<dyn Producer>, Outcome);

enum Stage {
    Ask,
    /// The system's save dialog is open.
    Choosing(Receiver<Option<PathBuf>>),
    /// Writing to `dest` on a worker.
    Writing {
        dest: PathBuf,
        done: Arc<AtomicU64>,
        decision: Arc<Decision>,
        began: Instant,
        rx: Receiver<Landed>,
    },
}

/// The worker ended without a result (it panicked): the producer went with it.
const WORKER_DIED: &str =
    "the save stopped without a result (its worker ended); nothing more can be saved from here";

fn field_id() -> egui::Id {
    egui::Id::unique("sopen-save-as-path")
}

fn dialog_id() -> egui::Id {
    egui::Id::unique("sopen-save-as")
}

fn browse_id() -> egui::Id {
    dialog_id().with("browse")
}

fn save_id() -> egui::Id {
    dialog_id().with("save")
}

fn cancel_id() -> egui::Id {
    dialog_id().with("cancel")
}

/// The modal's title in its frame.
const TITLE: &str = "Save As";
/// The cells the box's inside has at least (the egui modal's 64), so a line that comes or goes
/// never changes its width.
const WIDE: usize = 64;

/// The progress row in `room` cells (10 §6): a bar of `█` for what is done and `░` for the rest
/// — `▓` for a cell half done — then, two blanks on, `figures` (the bytes and the time left).
fn progress_row(frac: f64, figures: &str, room: usize) -> String {
    let bar = room.saturating_sub(cells::width(figures) + 2);
    if bar == 0 {
        return figures.to_string();
    }
    let filled = frac.clamp(0.0, 1.0) * bar as f64;
    let full = filled.floor() as usize;
    let half = usize::from(full < bar && filled - full as f64 >= 0.5);
    let rest = bar - full - half;
    format!(
        "{}{}{}  {figures}",
        "█".repeat(full),
        "▓".repeat(half),
        "░".repeat(rest)
    )
}

/// The Save As modal (07 §7, §10), over a window. See the module's words for how a window uses
/// it.
pub struct SaveAs {
    name: String,
    size: Size,
    rate: Option<f64>,
    /// `None` while a save runs (the worker has it), or after its worker died.
    producer: Option<Box<dyn Producer>>,
    chooser: Box<dyn Chooser>,
    /// The path field's text.
    path: String,
    /// What the modal must say whole: (text, is an error).
    notes: Vec<(String, bool)>,
    stage: Stage,
    /// The field takes the keyboard on the next pass (on open, and after a refusal).
    focus: bool,
    /// The modal's buttons as last laid out: one with the focus takes Enter as its click.
    buttons: Vec<egui::Id>,
    /// Where `saved <path>` goes: stdout.
    say: Box<dyn FnMut(&str)>,
}

impl SaveAs {
    /// The modal over `job`, its field at [`first_path`] in `folder` (`binary::download_dir()`;
    /// tests give their own).
    pub fn new(job: Job, folder: &Path, chooser: Box<dyn Chooser>) -> Self {
        let (path, note) = first_path(folder, &job.name);
        Self {
            name: job.name,
            size: job.size,
            rate: job.rate,
            producer: Some(job.producer),
            chooser,
            path: binary::tilde(&path),
            notes: note.map(|n| (n, false)).into_iter().collect(),
            stage: Stage::Ask,
            focus: true,
            buttons: Vec::new(),
            say: Box::new(|line| println!("{line}")),
        }
    }

    /// `saved <path>` goes to `say` instead of stdout (tests).
    #[cfg(test)]
    fn printing(mut self, say: impl FnMut(&str) + 'static) -> Self {
        self.say = Box::new(say);
        self
    }

    /// A save is running.
    pub fn busy(&self) -> bool {
        matches!(self.stage, Stage::Writing { .. })
    }

    /// Stops a running save (the window is closing): whether the cancel won — `false` when
    /// none runs, or when its publish has begun (it is then saved, and said so).
    pub fn cancel(&self) -> bool {
        match &self.stage {
            Stage::Writing { decision, .. } => decision.cancel(),
            _ => false,
        }
    }

    /// Whether Save (and Browse…) can start a save now.
    fn can_save(&self) -> bool {
        matches!(self.stage, Stage::Ask) && self.producer.is_some()
    }

    /// The running save's progress line.
    fn progress_text(&self) -> Option<String> {
        match &self.stage {
            Stage::Writing { done, began, .. } => Some(progress_line(
                done.load(Ordering::Relaxed),
                self.size,
                began.elapsed(),
                self.rate,
            )),
            _ => None,
        }
    }

    /// Saves to the path in the field, exactly as typed (a folder gets the edited name inside
    /// it), on a worker.
    fn start(&mut self, ctx: &egui::Context) {
        if !self.can_save() {
            return;
        }
        if self.path.trim().is_empty() {
            self.notes = vec![("type a path to save to".to_string(), true)];
            self.focus = true;
            return;
        }
        let mut dest = binary::untilde(&self.path);
        if dest.is_dir() {
            dest = dest.join(crate::remote::patch::edited_name(&self.name));
        }
        let Some(mut producer) = self.producer.take() else {
            return;
        };
        self.notes.clear();
        let done = Arc::new(AtomicU64::new(0));
        let decision = Arc::new(Decision::new());
        let (tx, rx) = mpsc::channel();
        let (to, size, done2, decision2, ctx2) = (
            dest.clone(),
            self.size,
            done.clone(),
            decision.clone(),
            ctx.clone(),
        );
        std::thread::spawn(move || {
            let report = |n: u64| {
                done2.store(n, Ordering::Relaxed);
                ctx2.request_repaint();
            };
            let outcome = write(&to, size, producer.as_mut(), &Ctl::new(&decision2, &report));
            let _ = tx.send((producer, outcome));
            ctx2.request_repaint();
        });
        self.stage = Stage::Writing {
            dest,
            done,
            decision,
            began: Instant::now(),
            rx,
        };
    }

    fn browse(&mut self) {
        let suggested = binary::untilde(&self.path);
        let suggested = if suggested.is_dir() {
            suggested.join(crate::remote::patch::edited_name(&self.name))
        } else {
            suggested
        };
        self.notes.clear();
        self.stage = Stage::Choosing(self.chooser.choose(&suggested));
    }

    /// What came back since the last pass: the system dialog's answer, a save's outcome.
    fn poll(&mut self, ctx: &egui::Context) -> Option<Ended> {
        match &self.stage {
            Stage::Choosing(rx) => match rx.try_recv() {
                Ok(Some(path)) => {
                    self.path = binary::tilde(&path);
                    self.stage = Stage::Ask;
                    // The system dialog's own Save button was the confirmation.
                    self.start(ctx);
                }
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.stage = Stage::Ask;
                    self.notes = vec![(binary::BROWSE_NONE.to_string(), false)];
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            },
            Stage::Writing { rx, .. } => match rx.try_recv() {
                Ok((producer, outcome)) => {
                    self.producer = Some(producer);
                    self.stage = Stage::Ask;
                    return self.landed(outcome);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    eprintln!("sopen: {WORKER_DIED}");
                    self.stage = Stage::Ask;
                    self.notes = vec![(WORKER_DIED.to_string(), true)];
                }
                Err(mpsc::TryRecvError::Empty) => {}
            },
            Stage::Ask => {}
        }
        None
    }

    /// A save came back: done when it landed; otherwise the modal says why and asks again.
    fn landed(&mut self, outcome: Outcome) -> Option<Ended> {
        let line = outcome.line();
        let notes = outcome.notes().to_vec();
        for note in &notes {
            eprintln!("sopen: {note}");
        }
        match outcome {
            Outcome::Saved { path, .. } => {
                (self.say)(&format!("saved {}", path.display()));
                return Some(Ended::Saved { path, line, notes });
            }
            Outcome::Taken { next, .. } => {
                self.path = binary::tilde(&next);
                self.notes = vec![(line, true)];
            }
            Outcome::Cancelled { .. } => {
                eprintln!("sopen: {line}");
                self.notes = vec![(line, false)];
            }
            Outcome::NoRoom { .. } | Outcome::Refused { .. } | Outcome::Failed(_) => {
                eprintln!("sopen: {line}");
                self.notes = vec![(line, true)];
            }
        }
        self.notes.extend(notes.into_iter().map(|n| (n, true)));
        self.focus = true;
        None
    }

    /// One pass of the modal on the whole window's grid ([`SaveAs::show_on`]); the image and
    /// media windows show it so.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Ended> {
        self.show_on(ctx, dialog::grid(ctx))
    }

    /// The modal's parts on a grid `cols` wide (10 §6, through `ops::dialog`): the file's line,
    /// the path field — while it can save; else the path a weak text, as an off control is — and
    /// `[ Browse… ]`, the size and the time expected, the hint, what runs (where it writes and
    /// the progress row), the system's dialog awaited, the notes, then `[ Save ]  [ Cancel ]`.
    fn parts(&self, cols: u16) -> Vec<Part> {
        let can = self.can_save();
        let room = dialog::inside_width(cols, WIDE, 0);
        let button = |label: &str, id: egui::Id, on: bool| Button {
            on,
            ..Button::new(label, id)
        };
        let mut parts = vec![
            Part::Text(
                format!("Save {} with your edits to a new local file", self.name),
                Ink::Text,
            ),
            Part::Blank,
            if can {
                Part::Field {
                    label: String::new(),
                    id: field_id(),
                }
            } else {
                Part::Text(cells::fit_left(&self.path, room), Ink::Weak)
            },
            Part::Buttons(vec![button("Browse…", browse_id(), can)]),
            Part::Text(expected_line(self.size, self.rate), Ink::Text),
            Part::Text(
                "never over an existing file · Enter saves · Esc cancels".to_string(),
                Ink::Weak,
            ),
        ];
        if let (Some(progress), Stage::Writing { dest, done, .. }) =
            (self.progress_text(), &self.stage)
        {
            let frac = match self.size.bytes() {
                0 => 0.0,
                total => (done.load(Ordering::Relaxed) as f64 / total as f64).min(1.0),
            };
            parts.extend([
                Part::Blank,
                Part::Text(
                    format!("saving to {}… Esc cancels", binary::tilde(dest)),
                    Ink::Text,
                ),
                Part::Text(progress_row(frac, &progress, room), Ink::Text),
            ]);
        }
        if matches!(self.stage, Stage::Choosing(_)) {
            parts.push(Part::Text(
                "waiting for the system's save dialog…".to_string(),
                Ink::Text,
            ));
        }
        for (text, error) in &self.notes {
            let ink = if *error { Ink::Error } else { Ink::Text };
            parts.extend(text.lines().map(|l| Part::Text(l.to_string(), ink)));
        }
        parts.extend([
            Part::Blank,
            Part::Buttons(vec![
                button("Save", save_id(), can),
                Button::new("Cancel", cancel_id()),
            ]),
        ]);
        parts
    }

    /// The cells of `grid` the modal covers there, its box and its shadow, for the window to
    /// blank before it paints them (cells.rs: one buffer of cells over another).
    pub fn covers(&self, grid: Grid) -> Vec<Cells> {
        dialog::layout(grid, TITLE, WIDE, &self.parts(grid.cols)).covers()
    }

    /// One pass of the modal on `grid` (the window's, between its title and status rows): what
    /// came back, its keys (Enter saves, Esc stops a running save or else closes), then the box
    /// in cells (10 §6, `ops::dialog`). Returns how it ended, once it has.
    pub fn show_on(&mut self, ctx: &egui::Context, grid: Grid) -> Option<Ended> {
        if let Some(ended) = self.poll(ctx) {
            return Some(ended);
        }
        let field = field_id();
        let focused = ctx.memory(|m| m.focused());
        let on_button = focused.is_some_and(|f| f != field && self.buttons.contains(&f));
        let escape = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        let enter = !on_button && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        // The field has the keyboard while the modal asks, unless one of its buttons has it.
        // Asked again every pass: a disabled control gives the focus up.
        let take_back = self.can_save() && !on_button && focused != Some(field);
        if std::mem::take(&mut self.focus) || take_back {
            ctx.memory_mut(|m| m.request_focus(field));
        }
        let can = self.can_save();
        let parts = self.parts(grid.cols);
        let layout = dialog::layout(grid, TITLE, WIDE, &parts);
        let inputs = Inputs {
            field: can.then_some(&mut self.path),
            ..Inputs::default()
        };
        let shown = dialog::show(ctx, dialog_id(), &layout, inputs);
        // Every button laid out: one with the focus keeps Enter as its own click.
        self.buttons = vec![browse_id(), save_id(), cancel_id()];
        let save = enter || shown.clicked(save_id());
        let stop = escape || shown.clicked(cancel_id());
        let browse = shown.clicked(browse_id());
        if stop {
            match &self.stage {
                Stage::Writing { decision, .. } => drop(decision.cancel()),
                // The system dialog is up: its own answer ends it.
                Stage::Choosing(_) => {}
                Stage::Ask => return Some(Ended::Closed),
            }
        }
        if browse && self.can_save() {
            self.browse();
        } else if save {
            self.start(ctx);
        }
        None
    }
}

impl Drop for SaveAs {
    /// The window went away during a save: it is cancelled and waited for, so its staging
    /// folder is gone before sopen is. A publish that had already begun is saved, and said.
    fn drop(&mut self) {
        if let Stage::Writing { decision, rx, .. } = &self.stage {
            decision.cancel();
            match rx.recv() {
                Ok((_, Outcome::Saved { path, notes, .. })) => {
                    (self.say)(&format!("saved {}", path.display()));
                    for note in notes {
                        eprintln!("sopen: {note}");
                    }
                }
                Ok((_, outcome)) => {
                    for note in outcome.notes() {
                        eprintln!("sopen: {note}");
                    }
                }
                Err(_) => eprintln!("sopen: {WORKER_DIED}"),
            }
        }
    }
}

#[cfg(test)]
mod tests;
