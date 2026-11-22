//! The file browser (spec §3a, owner message 12): a folder opens in sopen's own window, two file
//! panels side by side as in Total Commander, laid out where PC Shell puts things (the program's
//! name on top, the path in the panel's frame, the counts at its foot, the F-keys at the
//! bottom — [`super::chrome`] draws all of it). Enter on a file shows the viewer for its kind in
//! the same window: text in the text viewer, an image in the image window, video and audio in
//! the media window, anything else in the hex window. F4 opens the highlighted file or folder in
//! VS Code or Zed.
//!
//! The window has two modes (02 §2, F9): in preview mode, the default, the side opposite the
//! active list previews the file under its bar ([`view`] decides what each side shows,
//! [`preview`] hosts the preview); in dual mode both sides are lists. F2 renames the entry under
//! the bar, F5 copies and F6 moves the selection or that entry to the other panel's folder,
//! never over anything ([`ops`], on [`crate::fileops`]'s engine; 04 §5). Every key is a row of
//! the one key table ([`keys`]).
//!
//! Every way out of a viewer — Esc, F3, closing the window — goes through the viewer's own
//! [`Window::close_requested`], so its unsaved-changes prompt and its running saves are honoured
//! exactly as in its own window: a viewer is dropped only once it said yes, and what it may still
//! be writing is waited for before sopen exits ([`open`]).
//!
//! Nothing slow runs on a frame: listing, opening a connection, fetching, decoding, probing,
//! preparing a preview and file operations run on workers, each feature with its own state and
//! its own rule for the keys it holds back (02 §6): a listing holds the list's keys, an opening
//! all but Esc, a preview none, a file operation none but a second operation. Esc stops a fetch
//! or a file operation.

pub(crate) mod keys;
mod ops;
mod panel;
mod prepare;
mod preview;
pub(crate) mod view;

use super::chrome::{self, FKey};
use super::{help, Embedded, Hooks, Outcome, Status, Window};
use crate::fileops::{self, Item, ItemOutcome, Report};
use crate::remote::ops::FileOps;
use crate::remote::{EntryKind, LocalFs, RemoteFs, SftpRemote};
use anyhow::{bail, Result};
use eframe::egui::{self, Key, Modifiers, Pos2, Rect, Sense, Vec2};
use keys::{Command, Context, Step};
pub use panel::Panel;
use prepare::Prepared;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

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
    /// The host as `ssh` takes it (`user@host`, an IPv6 host unbracketed:
    /// [`RemoteSpec::destination`](crate::target::RemoteSpec::destination), not `label`); `None`
    /// on this machine. For what runs over the folder's ssh master besides the listing (02 §5:
    /// git, from S2); nothing reads it yet.
    pub destination: Option<String>,
    /// The folder (absolute).
    pub abs: String,
    /// sopen's cache root.
    pub cache: PathBuf,
    /// sopen's state folder, where the ssh master's socket is
    /// ([`connection_args`](crate::remote::connection_args)): the file operations' connections
    /// go over it.
    pub state_dir: PathBuf,
    pub launch: Launch,
}

/// What the browser's features tell it (02 §7): [`Browser::event`] handles them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrowserEvent {
    /// Files changed in `dirs` (absolute) on `host` (`[user@]host`; `None`: this machine), after
    /// a file operation (04 §5.1) or, from S2, a git write: the panels showing one of `dirs`
    /// re-list, a git panel whose top level holds one soft-refreshes.
    FilesChanged {
        host: Option<String>,
        dirs: Vec<String>,
    },
}

/// Shows the browser until its window closes; then waits for anything a viewer may still be
/// writing (printed as the viewer's own window prints it) and returns what the viewers did.
pub fn open(start: Start, hooks: &Hooks) -> Result<Outcome> {
    let title = show(start.label.as_deref(), &start.abs);
    let script = match std::env::var("SOPEN_KEYS") {
        Ok(text) => parse_keys(&text).map_err(|e| anyhow::anyhow!("SOPEN_KEYS: {e}"))?,
        Err(_) => Vec::new(),
    };
    let mut browser = Browser::new(start);
    browser.scripted(&hooks.state);
    browser.script = script.into();
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

/// Opens a connection for a file operation on a host (`[user@]host` as the browser prints it;
/// `None`: this machine). Each operation has connections of its own (04 §5.1).
pub(crate) type OpsConnect =
    Arc<dyn Fn(Option<&str>) -> Result<Box<dyn FileOps + Send>> + Send + Sync>;

/// The host `label` names (`[user@]host`, an IPv6 host bracketed), read as sopen reads its
/// command line.
fn host_spec(label: &str) -> Result<crate::target::RemoteSpec> {
    match crate::target::parse(&format!("{label}:/")) {
        Ok(crate::target::Target::Remote(spec)) => Ok(spec),
        Ok(crate::target::Target::Local(_)) => bail!("{label} is not a host"),
        Err(why) => bail!(why),
    }
}

/// File-operation connections as the window's own are made: this machine's file layer, or
/// SFTP to the host over its ssh master (`state_dir` holds the socket).
fn ops_connect(state_dir: PathBuf) -> OpsConnect {
    Arc::new(move |host: Option<&str>| match host {
        None => Ok(Box::new(LocalFs::new()) as Box<dyn FileOps + Send>),
        Some(label) => Ok(Box::new(SftpRemote::connect(
            &host_spec(label)?,
            &state_dir,
        )?)),
    })
}

/// The browser's dialogs on [`fileops`]' engine: an [`ops::Run`] that runs the job on the
/// worker `ops` made for it — over one connection when both folders are on one host (a move is
/// then a rename where one is possible, a remote copy runs on the server), else over one per
/// host — and tells the window through the job's [`ops::Link`]: progress, a name in the way,
/// and Esc through the link's own stop flag.
fn engine(connect: OpsConnect) -> ops::Run {
    engine_seen(connect, None)
}

/// What a test watches the engine's progress with, on the operation's worker (a gate in the
/// middle of a transfer).
type ProgressSeen = Arc<dyn Fn(&fileops::Progress) + Send + Sync>;

/// [`engine`]; `seen` is called with every progress the engine reports, before the window
/// hears of it.
fn engine_seen(connect: OpsConnect, seen: Option<ProgressSeen>) -> ops::Run {
    Arc::new(move |job: ops::Job, link: &ops::Link| -> Report {
        let (op, from, names, to) = match job {
            ops::Job::Transfer {
                kind,
                from,
                names,
                to,
            } => {
                let op = match kind {
                    ops::Transfer::Copy => fileops::Op::Copy,
                    ops::Transfer::Move => fileops::Op::Move,
                };
                (op, from, names, to)
            }
            ops::Job::Rename { dir, from, to } => {
                (fileops::Op::Rename { to }, dir.clone(), vec![from], dir)
            }
        };
        let job = fileops::Job {
            op,
            src_dir: from.dir.clone(),
            names,
            dst_dir: to.dir.clone(),
        };
        // Refused before anything happened: every item not reached, the reason the note.
        let refused = |host: &ops::OpsTarget, e: anyhow::Error| Report {
            items: job
                .names
                .iter()
                .map(|name| Item {
                    name: name.clone(),
                    outcome: ItemOutcome::NotReached,
                })
                .collect(),
            notes: vec![match &host.host {
                Some(host) => format!("cannot connect to {host}: {e:#}"),
                None => format!("cannot open this machine's files: {e:#}"),
            }],
        };
        let mut ctl = Ctl {
            link,
            last: None,
            seen: seen.clone(),
        };
        let mut src = match connect(from.host.as_deref()) {
            Ok(fs) => fs,
            Err(e) => return refused(&from, e),
        };
        if from.host == to.host {
            return fileops::run(&job, fileops::Hosts::Same(src.as_mut()), &mut ctl);
        }
        let mut dst = match connect(to.host.as_deref()) {
            Ok(fs) => fs,
            Err(e) => return refused(&to, e),
        };
        let hosts = fileops::Hosts::Two {
            src: src.as_mut(),
            dst: dst.as_mut(),
        };
        fileops::run(&job, hosts, &mut ctl)
    })
}

/// The engine's [`fileops::Control`] over a job's [`ops::Link`].
struct Ctl<'a> {
    link: &'a ops::Link,
    last: Option<fileops::Progress>,
    seen: Option<ProgressSeen>,
}

/// A progress event goes to the window when the item or file changes, when a file ends, and
/// every this many bytes in between (the engine reports every chunk).
const PROGRESS_EVERY: u64 = 1024 * 1024;

impl fileops::Control for Ctl<'_> {
    /// Esc: the link's own flag, which [`ops::Ops::stop`] sets.
    fn cancel(&self) -> Arc<AtomicBool> {
        self.link.stop_flag()
    }

    fn progress(&mut self, p: &fileops::Progress) {
        if let Some(seen) = &self.seen {
            seen(p);
        }
        let due = match &self.last {
            None => true,
            Some(last) => {
                (last.verb, last.item, &last.name) != (p.verb, p.item, &p.name)
                    || p.done == p.total
                    || p.done.saturating_sub(last.done) >= PROGRESS_EVERY
            }
        };
        if due {
            self.last = Some(p.clone());
            self.link.progress(p.clone());
        }
    }

    fn ask(&mut self, clash: &fileops::Clash) -> fileops::Reply {
        self.link.ask(clash.clone())
    }
}

/// An engine that does nothing: what a browser that is going away holds in place of its own.
fn no_engine() -> ops::Run {
    Arc::new(|_: ops::Job, _: &ops::Link| Report::default())
}

/// One step of `SOPEN_KEYS`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Play {
    Key(Key, Modifiers),
    Text(String),
}

/// `SOPEN_KEYS`, read: steps separated by commas — a key by egui's name (`F5`, `Enter`, `Esc`,
/// `Down`, `Space`, `Tab`, a letter), with `Shift+`, `Alt+` or `Cmd+` / `Ctrl+` before it, or
/// `text:…` for typed text. The browser plays them, one per settled window, as the owner's
/// keys (a receipt of a dialog, a selection, a rename).
pub(crate) fn parse_keys(text: &str) -> std::result::Result<Vec<Play>, String> {
    let mut out = Vec::new();
    for step in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(typed) = step.strip_prefix("text:") {
            out.push(Play::Text(typed.to_string()));
            continue;
        }
        let mut modifiers = Modifiers::NONE;
        let mut parts: Vec<&str> = step.split('+').collect();
        let name = parts.pop().unwrap_or_default();
        for part in parts {
            modifiers |= match part {
                "Shift" => Modifiers::SHIFT,
                "Alt" => Modifiers::ALT,
                "Cmd" | "Ctrl" => Modifiers::COMMAND,
                other => return Err(format!("'{step}': unknown modifier '{other}'")),
            };
        }
        let key = Key::from_name(name).ok_or_else(|| format!("'{step}': unknown key '{name}'"))?;
        out.push(Play::Key(key, modifiers));
    }
    Ok(out)
}

/// A listing running on a worker.
struct Listing {
    /// The panel it is for (`None`: both, at the start).
    panel: Option<usize>,
    dir: String,
    /// The entry to put the bar on; when it is not there, the bar stays on its row (`keep`).
    select: Option<String>,
    keep: Option<usize>,
    rx: Receiver<Result<Listed>>,
}

/// A listing: the entries and the free space.
type Listed = (Vec<crate::remote::Entry>, Option<u64>);

/// A file being made ready for its viewer.
struct Opening {
    abs: String,
    name: String,
    remote: bool,
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    source: Source,
}

/// Where an opening's file comes from.
enum Source {
    /// Enter on it in the list: [`prepare::prepare`] on a worker.
    Fresh {
        rx: Receiver<Result<Prepared>>,
        worker: Option<JoinHandle<()>>,
    },
    /// Enter on a file whose preview was still being prepared (02 §4.4): that preparation,
    /// its budget lifted, goes on as the opening.
    Preview(preview::Prep),
}

/// The viewer on screen.
struct Viewer {
    embedded: Embedded,
    /// The cached copy it shows, held until it is gone.
    hold: Option<crate::cache::Claim>,
    /// The file (for F4).
    abs: String,
    /// An image read through a stream: where it gives way to the hex window (06 §7.3).
    switch: Option<super::image::HexSwitch>,
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

/// What outlives the window: fetches stopping, viewers' writes, a file operation, the viewers'
/// outcome.
#[derive(Default)]
struct Leftover {
    workers: Vec<JoinHandle<()>>,
    closing: Vec<Closing>,
    /// The window's file operations, with a job that may still run.
    ops: Option<ops::Ops>,
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

    /// After the window: stopped fetches end, every write is waited for — a running file
    /// operation too (02 §6), and what it did is printed.
    fn finish(&mut self) -> Outcome {
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        if let Some(report) = self.ops.take().and_then(ops::Ops::wait) {
            for line in report_lines(&report) {
                eprintln!("sopen: {line}");
            }
        }
        self.settle(true);
        self.outcome.clone()
    }
}

/// What a file operation did, for the terminal: it ended after its window closed.
fn report_lines(report: &Report) -> Vec<String> {
    let done = report
        .items
        .iter()
        .filter(|i| matches!(i.outcome, ItemOutcome::Done | ItemOutcome::Renamed(_)))
        .count();
    let mut lines = vec![format!(
        "the file operation ended after the window closed: {done} of {} done",
        report.items.len()
    )];
    for item in &report.items {
        match &item.outcome {
            ItemOutcome::Done => {}
            ItemOutcome::Renamed(to) => lines.push(format!("{} → {to}", item.name)),
            ItemOutcome::Skipped => lines.push(format!("{}: skipped", item.name)),
            ItemOutcome::Failed(why) => lines.push(format!("{}: {why}", item.name)),
            ItemOutcome::NotReached => lines.push(format!("{}: not reached", item.name)),
        }
    }
    lines.extend(report.notes.iter().cloned());
    lines
}

pub(crate) struct Browser {
    /// `host:/abs` of the folder opened.
    title: String,
    label: Option<String>,
    /// The browser's own connection (listing), shared with its listing workers.
    conn: Arc<Mutex<Box<dyn RemoteFs + Send>>>,
    connect: Connect,
    launch: Launch,
    cache: PathBuf,
    panels: [Panel; 2],
    active: usize,
    /// The browser's mode (02 §2): preview (the default) or dual; F9 switches it.
    mode: view::Mode,
    /// The committed display (02 §3.1): what each side shows now.
    shown: Option<[view::Content; 2]>,
    /// The active list's bar as the last pass saw it — the side, its folder, the entry — and
    /// when it last moved within that folder (egui's clock; `None`: the last change was
    /// something else, which commits at once).
    bar_was: (usize, String, Option<String>),
    moved_at: Option<f64>,
    /// The bar has not rested yet: a side keeps what it shows a little longer.
    uncommitted: bool,
    // One job state per feature (02 §6).
    /// Listings running (the panels re-read after a file operation, one each).
    listing: Vec<Listing>,
    /// The file being made ready for its viewer.
    opening: Option<Opening>,
    /// The preview host (02 §4): its own preparation, its own connection.
    preview: preview::Host,
    /// The file operations (04 §4–§5): the dialogs, the rename field, the running job.
    ops: ops::Ops,
    /// The panel F2 was last pressed in: its row shows the rename field, and its bar goes on
    /// the new name.
    rename_panel: usize,
    /// After a rename: the panel it was made in, its folder, and the new name its bar goes on
    /// at the re-read.
    bar_next: Option<(usize, String, String)>,
    /// Each panel's last listing, when it failed: said until that panel lists again.
    list_errors: [Option<String>; 2],
    /// What an operation that ended under a viewer has to say, for when the list shows again.
    told_later: Option<Status>,
    viewer: Option<Viewer>,
    exit: Exit,
    status: Status,
    launches: Vec<Receiver<Result<String>>>,
    leftover: Rc<RefCell<Leftover>>,
    /// A text field had the keyboard at the end of the last pass: an Esc that took it away was
    /// that field's, not a request to leave the viewer. Only a text field: a viewer's own grid
    /// (the hex window's) holds the keyboard too, and egui lets Esc take it from the grid on the
    /// pass after the grid first took it (its lock filter applies from its second focused pass)
    /// — that Esc is the owner's leaving.
    focused_before: bool,
    /// Rows the active panel shows (the last pass's).
    visible: usize,
    painted: bool,
    /// `SOPEN_STATE` `open=NAME`: opened once the folder is first listed (receipts of a viewer).
    open_first: Option<String>,
    /// `SOPEN_STATE` `bar=NAME`: the bar goes on it once the folder is first listed, without
    /// opening it (a preview's receipt; 02 §9).
    bar_first: Option<String>,
    /// `SOPEN_STATE` `at=OFFSET`: where the first text viewer opened in this browser starts (the
    /// row that byte is in at the top) — the one `open=` opens, or else the first the owner
    /// opens; taken by it, so no later viewer gets it.
    open_at: Option<u64>,
    /// `SOPEN_STATE` `loading=1` (06 §11): handed to the text viewers this browser opens, as
    /// `at=` is. A viewer on a stream is then shot at its first frame with rows.
    open_loading: bool,
    /// `SOPEN_KEYS`: the keys still to play, one per settled window, and the passes to let go
    /// by before the next (a field opened by one takes the typing on the pass after).
    script: VecDeque<Play>,
    script_gap: u8,
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
        if let Some(opening) = self.opening.take() {
            opening.cancel.store(true, Ordering::Relaxed);
            leftover.workers.extend(match opening.source {
                Source::Fresh { worker, .. } => worker,
                Source::Preview(prep) => prep.into_worker(),
            });
        }
        // A running file operation is waited for, not stopped (02 §6); a preview's
        // preparation is cancelled and not waited for (02 §4.4: the host's own drop).
        leftover.ops = Some(std::mem::replace(&mut self.ops, ops::Ops::new(no_engine())));
    }
}

fn list_id() -> egui::Id {
    egui::Id::unique("sopen-browser-list")
}

/// The keys the status row names when nothing else is to be said.
const LIST_HINT: &str =
    "Enter open · Tab other panel · Backspace up · a letter jumps to a name · F1 the keys";
const VIEWER_HINT: &str = "Esc or F3 back to the list · F4 VS Code / Zed";

impl Browser {
    pub(crate) fn new(start: Start) -> Self {
        // The preview copies an older sopen left (every preview reads through a stream now, 06
        // §13): swept once, off the window's thread.
        let cache = start.cache.clone();
        let _ = std::thread::Builder::new()
            .name("legacy-previews".into())
            .spawn(move || crate::cache::sweep_legacy_previews(&cache));
        let preview = preview::Host::new(
            start.connect.clone(),
            start.label.clone(),
            start.cache.clone(),
        );
        let mut browser = Self {
            title: show(start.label.as_deref(), &start.abs),
            label: start.label,
            conn: Arc::new(Mutex::new(start.fs)),
            connect: start.connect,
            launch: start.launch,
            cache: start.cache,
            panels: [Panel::new(&start.abs), Panel::new(&start.abs)],
            active: 0,
            mode: view::Mode::default(),
            shown: None,
            bar_was: (0, start.abs.clone(), None),
            moved_at: None,
            uncommitted: false,
            listing: Vec::new(),
            opening: None,
            preview,
            ops: ops::Ops::new(engine(ops_connect(start.state_dir))),
            rename_panel: 0,
            bar_next: None,
            list_errors: [None, None],
            told_later: None,
            viewer: None,
            exit: Exit::List,
            status: Status::default(),
            launches: Vec::new(),
            leftover: Rc::default(),
            focused_before: false,
            visible: 1,
            painted: false,
            open_first: None,
            bar_first: None,
            open_at: None,
            open_loading: false,
            script: VecDeque::new(),
            script_gap: 0,
        };
        browser.list(None, start.abs, None, None);
        browser
    }

    /// Tests: the connections file operations run over.
    #[cfg(test)]
    fn set_ops_connect(&mut self, connect: OpsConnect) {
        self.ops = ops::Ops::new(engine(connect));
    }

    /// Tests: the real engine over `connect`, its progress watched by `seen`.
    #[cfg(test)]
    fn set_ops_seen(&mut self, connect: OpsConnect, seen: ProgressSeen) {
        self.ops = ops::Ops::new(engine_seen(connect, Some(seen)));
    }

    /// Tests: a stand-in engine.
    #[cfg(test)]
    fn set_ops_run(&mut self, run: ops::Run) {
        self.ops = ops::Ops::new(run);
    }

    /// Lists `dir` on a worker for `panel` (`None`: both), the bar then on `select`.
    fn list(
        &mut self,
        panel: Option<usize>,
        dir: String,
        select: Option<String>,
        ctx: Option<&egui::Context>,
    ) {
        self.relist(panel, dir, select, None, ctx);
    }

    /// [`Browser::list`]; where `select` is not listed any more the bar stays on row `keep`.
    fn relist(
        &mut self,
        panel: Option<usize>,
        dir: String,
        select: Option<String>,
        keep: Option<usize>,
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
        // A newer listing for a panel replaces an older one still running for it.
        self.listing
            .retain(|l| l.panel.is_some() && panel.is_some() && l.panel != panel);
        self.listing.push(Listing {
            panel,
            dir,
            select,
            keep,
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
        let began = std::time::Instant::now();
        let worker = std::thread::spawn(move || {
            let repaint = waker.clone();
            let wake: crate::stream::Waker = Arc::new(move || repaint.request_repaint());
            let req = prepare::Request {
                connect: &connect,
                label: label.as_deref(),
                abs: &path,
                cache: &cache,
                wake: &wake,
                began,
            };
            let mut progress = |now, all| {
                d.store(now, Ordering::Relaxed);
                t.store(all, Ordering::Relaxed);
                waker.request_repaint();
            };
            let result = prepare::prepare(&req, &stop, &mut progress);
            let _ = tx.send(result);
            waker.request_repaint();
        });
        self.status = Status::default();
        self.opening = Some(Opening {
            abs,
            name,
            remote: self.label.is_some(),
            done,
            total,
            cancel,
            source: Source::Fresh {
                rx,
                worker: Some(worker),
            },
        });
    }

    /// Carries out a key's command in the list (02 §7): the bar, the sides and the mode
    /// ([`view`]), viewers, the editor, the file operations ([`ops`]). While a file is being
    /// made ready only Esc acts (it stops the fetch); while a folder is listed, none (it takes
    /// a moment).
    fn run(&mut self, ctx: &egui::Context, command: Command) {
        // Esc acts whatever else is on its way: a pending listing never makes a fetch or a
        // file operation unstoppable.
        if command == Command::Stop {
            self.stop();
            return;
        }
        if self.opening.is_some() {
            return;
        }
        if !self.listing.is_empty() {
            return;
        }
        let page = self.visible.saturating_sub(1).max(1) as isize;
        let active = self.active;
        let panel = &mut self.panels[active];
        match command {
            Command::Help => {
                help::open(ctx, help::for_context(Context::List, self.mode, &[]));
            }
            Command::Bar(step) => match step {
                Step::Up => panel.step(-1),
                Step::Down => panel.step(1),
                Step::PageUp => panel.step(-page),
                Step::PageDown => panel.step(page),
                Step::Home => panel.home(),
                Step::End => panel.end(),
            },
            Command::Jump(c) => {
                panel.jump(c);
            }
            Command::Open => self.enter(ctx, false),
            Command::View => self.enter(ctx, true),
            Command::Up => self.up(ctx),
            Command::Edit => match panel.current() {
                Some(entry) if entry.lossy => self.status = Status::error(not_utf8(&entry.name)),
                Some(entry) => {
                    let is_dir = entry.kind == EntryKind::Dir;
                    let path = panel.path_of(entry);
                    self.edit(ctx, path, is_dir);
                }
                None => {}
            },
            Command::OtherSide => self.active = 1 - active,
            Command::SwitchMode => {
                self.mode = self.mode.other();
                // A mode change commits at once (02 §3.1).
                self.moved_at = None;
            }
            Command::Select => panel.toggle(),
            Command::SelectAll => panel.select_all(),
            Command::InvertSelection => panel.invert(),
            Command::SelectGroup => self.ops.open_group(true),
            Command::UnselectGroup => self.ops.open_group(false),
            Command::Rename => {
                self.rename_panel = active;
                let refused = self.ops.open_rename(panel, self.label.as_deref());
                self.status = refused.unwrap_or_default();
            }
            Command::Copy => self.transfer(ops::Transfer::Copy),
            Command::Move => self.transfer(ops::Transfer::Move),
            Command::Reread => {
                let (dir, at) = (panel.dir.clone(), panel.highlight);
                let name = panel.current().map(|e| e.name.clone());
                self.relist(Some(active), dir, name, Some(at), Some(ctx));
            }
            Command::SwapPanels => {
                self.panels.swap(0, 1);
                // The keyboard goes with its list.
                self.active = 1 - active;
            }
            Command::SameFolder(side) => {
                let to = match side {
                    keys::Side::Left => 0,
                    keys::Side::Right => 1,
                };
                if to != active {
                    let dir = panel.dir.clone();
                    self.list(Some(to), dir, None, Some(ctx));
                }
            }
            // Rows of other contexts and of slices that have not landed: nothing to do here.
            _ => {}
        }
    }

    /// Esc in the list (02 §3.2): stops a running fetch, else a running file operation —
    /// the engine stops at its next check and reports what it did (04 §5.1, §5.2).
    fn stop(&mut self) {
        if let Some(opening) = &self.opening {
            opening.cancel.store(true, Ordering::Relaxed);
        } else if self.ops.busy() {
            self.ops.stop();
        }
    }

    /// F5 / F6: the dialog for the selection or the bar's entry, to the other panel's own
    /// folder — whatever that side shows (02 §7 `OpsTarget`).
    fn transfer(&mut self, kind: ops::Transfer) {
        let to = ops::OpsTarget {
            host: self.label.clone(),
            dir: self.panels[1 - self.active].dir.clone(),
        };
        let (panel, host) = (&self.panels[self.active], self.label.as_deref());
        let refused = self.ops.open_copy(kind, panel, host, &to);
        self.status = refused.unwrap_or_default();
    }

    /// What a feature tells the browser (02 §7).
    fn event(&mut self, ctx: &egui::Context, event: BrowserEvent) {
        match event {
            // Files changed in `dirs`: the panels showing one re-list, the bar kept on its name
            // (a rename's new name in the panel it was made in), else on its row.
            BrowserEvent::FilesChanged { host, dirs } => {
                if host != self.label {
                    return;
                }
                for i in 0..2 {
                    let panel = &self.panels[i];
                    if !dirs.contains(&panel.dir) {
                        continue;
                    }
                    let renamed = self
                        .bar_next
                        .take_if(|(origin, dir, _)| i == *origin && *dir == panel.dir)
                        .map(|(_, _, name)| name);
                    let name = renamed.or_else(|| panel.current().map(|e| e.name.clone()));
                    let (dir, at) = (panel.dir.clone(), panel.highlight);
                    self.relist(Some(i), dir, name, Some(at), Some(ctx));
                }
            }
        }
    }

    /// A file operation ended (04 §5.1 "After"): its line on the status row, and
    /// [`BrowserEvent::FilesChanged`] for the folders it changed, a host at a time.
    fn after_operation(&mut self, ctx: &egui::Context, changed: ops::Changed) {
        if let Some(said) = changed.said {
            if self.viewer.is_some() {
                // Told when the list shows again: the viewer's row stays the viewer's.
                self.told_later = Some(said);
            } else {
                self.status = said;
            }
        }
        if let (Some(name), Some(first)) = (changed.bar, changed.dirs.first()) {
            // The panel the rename was made in, whichever has the keyboard now.
            self.bar_next = Some((self.rename_panel, first.dir.clone(), name));
        }
        let mut hosts: Vec<Option<String>> = Vec::new();
        for dir in &changed.dirs {
            if !hosts.contains(&dir.host) {
                hosts.push(dir.host.clone());
            }
        }
        for host in hosts {
            let dirs = changed
                .dirs
                .iter()
                .filter(|d| d.host == host)
                .map(|d| d.dir.clone())
                .collect();
            self.event(ctx, BrowserEvent::FilesChanged { host, dirs });
        }
        self.bar_next = None;
    }

    /// Results from the workers: listings, a file ready (its viewer shows), an F4 launch; and
    /// writes of viewers already gone.
    fn poll(&mut self, ctx: &egui::Context) {
        let mut at = 0;
        while at < self.listing.len() {
            let result = match self.listing[at].rx.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => {
                    at += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => {
                    Err(anyhow::anyhow!("the listing stopped without a result"))
                }
            };
            let listing = self.listing.remove(at);
            self.listed(ctx, listing, result);
        }
        self.poll_opening(ctx);
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

    /// A listing came back: its panels show it.
    fn listed(&mut self, ctx: &egui::Context, listing: Listing, result: Result<Listed>) {
        let Listing {
            panel,
            dir,
            select,
            keep,
            ..
        } = listing;
        let panels = panel.map_or(vec![0, 1], |p| vec![p]);
        let (entries, free) = match result {
            Ok(listed) => listed,
            Err(e) => {
                for i in panels {
                    self.list_errors[i] = Some(format!("{e:#}"));
                }
                return;
            }
        };
        for i in panels {
            // This panel's earlier failure is over; the other panel's is its own.
            self.list_errors[i] = None;
            let panel = &mut self.panels[i];
            // The entry under the bar, as it was listed before: changed (size or time) under
            // an unmoved bar, its preview is prepared again.
            let was = panel
                .current()
                .filter(|_| panel.dir == dir)
                .map(|e| (e.name.clone(), e.size, e.mtime));
            panel.fill(dir.clone(), entries.clone(), free, select.as_deref());
            if let (Some((name, size, mtime)), Some(now)) = (was, panel.current()) {
                if now.name == name && (now.size, now.mtime) != (size, mtime) {
                    self.preview.refresh(&panel.path_of(now));
                }
            }
            let found = select
                .as_deref()
                .is_some_and(|name| panel.current().is_some_and(|e| e.name == name));
            if let (false, Some(row)) = (found, keep) {
                // The entry is gone (moved away, renamed elsewhere): the bar stays on its row.
                panel.highlight = row.min(panel.entries.len().saturating_sub(1));
            }
        }
        // A listing is not a bar move: what it changes commits at once (02 §3.1).
        self.moved_at = None;
        let first = self.open_first.take().map(|name| (name, true));
        let first = first.or_else(|| self.bar_first.take().map(|name| (name, false)));
        if let Some((name, open)) = first {
            let panel = &mut self.panels[self.active];
            match panel.entries.iter().position(|e| e.name == name) {
                Some(i) => {
                    panel.highlight = i;
                    if open {
                        self.enter(ctx, false);
                    }
                }
                None => {
                    let key = if open { "open" } else { "bar" };
                    self.status = Status::error(format!("SOPEN_STATE {key}: no {name} in {dir}"));
                }
            }
        }
    }

    /// The opening task's news: a file ready, by Enter's own preparation or by a preview's
    /// that Enter took over.
    fn poll_opening(&mut self, ctx: &egui::Context) {
        let ended = match self.opening.as_mut().map(|o| &mut o.source) {
            None => return,
            Some(Source::Fresh { rx, .. }) => match rx.try_recv() {
                Ok(result) => Ok(result),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    Ok(Err(anyhow::anyhow!("opening stopped without a result")))
                }
            },
            Some(Source::Preview(prep)) => match prep.poll() {
                None => return,
                Some(ended) => Err(ended),
            },
        };
        // The task is over: its worker is done (it sent its result, or the preparation was
        // joined).
        let Some(opening) = self.opening.take() else {
            return;
        };
        let cancelled = opening.cancel.load(Ordering::Relaxed);
        let (abs, name) = (opening.abs, opening.name);
        if let Source::Fresh {
            worker: Some(worker),
            ..
        } = opening.source
        {
            let _ = worker.join();
        }
        let not_opened = |name: &str, kept: bool| {
            let kept = if kept {
                " (its copy is in sopen's cache)"
            } else {
                ""
            };
            Status::info(format!("cancelled; {name} was not opened{kept}"))
        };
        match ended {
            Ok(result) if cancelled && result.is_ok() => {
                // Esc came after the last byte (or there was none to fetch): the file is ready
                // but was not wanted any more. Dropping it releases its copy and its connection.
                drop(result);
                self.status = not_opened(&name, self.label.is_some());
            }
            Ok(result) => self.opened(ctx, result, abs, name),
            // The preview's preparation, taken over, came to its end: the live preview becomes
            // the viewer — or it is an info card (an error), and the file opens as from the
            // list.
            Err(preview::Ended::Ready(_)) if cancelled => self.status = not_opened(&name, false),
            Err(preview::Ended::Ready(previewed)) => match previewed.show().hand_over() {
                Ok(viewer) => self.handed_over(ctx, viewer, abs),
                Err(_card) => self.start_open(ctx, abs, name),
            },
            Err(preview::Ended::Cancelled) => {
                self.status = Status::info("cancelled; nothing was kept")
            }
            Err(preview::Ended::Died) => {
                self.status = Status::error("opening stopped without a result")
            }
        }
    }

    /// The file at `abs` came back ready: its viewer shows, in this window.
    fn opened(&mut self, ctx: &egui::Context, result: Result<Prepared>, abs: String, name: String) {
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(e) if crate::remote::is_cancelled(&e) => {
                self.status = Status::info("cancelled; nothing was kept");
                return;
            }
            // 06 §6: a session sshd refused is said in its own words; the list is as it was.
            Err(e) => {
                self.status = Status::error(crate::stream::explain(&e));
                return;
            }
        };
        let title = show(self.label.as_deref(), &abs);
        let mut switch = None;
        let (embedded, hold) = match prepared {
            Prepared::Image {
                file,
                doc,
                notices,
                claim,
            } => (super::image::embed(file, doc, notices), claim),
            Prepared::ImageAsking {
                file,
                path,
                notices,
                claim,
            } => {
                // 06 §7.3: the memory line, with Hex view on the file read in place.
                let gives_way = super::image::HexSwitch::local(path);
                match super::image::embed_asking(file, notices, Some(gives_way.clone())) {
                    Ok(embedded) => {
                        switch = Some(gives_way);
                        (embedded, claim)
                    }
                    Err(e) => {
                        self.status = Status::error(format!("{e:#}"));
                        return;
                    }
                }
            }
            Prepared::ImageStream {
                origin,
                source,
                repaint,
                notices,
            } => {
                // 06 §7.3: the window reads on a worker of its own, and gives way to the hex
                // window on the same stream (Hex view, a file that does not decode).
                let gives_way = super::image::HexSwitch::new(repaint);
                let made = super::image::embed_stream(
                    origin,
                    source,
                    &self.cache,
                    notices,
                    Some(gives_way.clone()),
                );
                match made {
                    Ok(embedded) => {
                        switch = Some(gives_way);
                        (embedded, None)
                    }
                    Err(e) => {
                        self.status = Status::error(format!("{e:#}"));
                        return;
                    }
                }
            }
            Prepared::Media {
                origin,
                media,
                notices,
                claim,
            } => (
                super::media::embed(origin, *media, &self.cache, notices),
                claim,
            ),
            // Probed on the worker: nothing slow is left for this frame (06 §7.4).
            Prepared::MediaStream {
                origin,
                media,
                notices,
            } => (
                super::media::embed_streamed(origin, *media, &self.cache, notices),
                None,
            ),
            Prepared::Hex { file, notices } => (super::hex::embed(file, notices), None),
            Prepared::Text {
                doc,
                notices,
                claim,
            } => {
                // A file read in place (no fetched copy, no stream) is followed at its end.
                let live = claim.is_none() && !doc.is_stream();
                let at = self.open_at.take();
                let loading = self.open_loading;
                (
                    super::text::embed_scripted(doc, title, &name, notices, live, at, loading),
                    claim,
                )
            }
        };
        self.status = Status::default();
        self.view(ctx, embedded, hold, abs, switch);
    }

    /// What `SOPEN_STATE` asks of the browser (a scripted run): the entry to open or to put
    /// the bar on, the mode, and what the text viewer it opens is handed (`at=`, `loading=1`).
    fn scripted(&mut self, state: &super::State) {
        self.open_first = state.open.clone();
        self.open_at = state.at;
        self.open_loading = state.loading;
        self.mode = state.mode;
        self.bar_first = state.bar.clone();
    }

    /// A preview handed over (02 §4.4): the live window is the viewer.
    fn handed_over(&mut self, ctx: &egui::Context, viewer: prepare::HandedOver, abs: String) {
        self.status = match viewer.notices.as_slice() {
            [] => Status::default(),
            said => Status::info(said.join(" · ")),
        };
        self.view(ctx, viewer.embedded, viewer.hold, abs, None);
    }

    /// `embedded` is the viewer on screen from now on.
    fn view(
        &mut self,
        ctx: &egui::Context,
        embedded: Embedded,
        hold: Option<crate::cache::Claim>,
        abs: String,
        switch: Option<super::image::HexSwitch>,
    ) {
        // The list gave up the keyboard: the viewer's keys are its own now.
        ctx.memory_mut(|m| m.surrender_focus(list_id()));
        self.exit = Exit::List;
        // One viewer at a time, and never one replaced without its own say (it leaves through
        // `leave` / `close_requested` first): nothing opens while one shows.
        debug_assert!(self.viewer.is_none(), "a viewer was replaced unasked");
        self.viewer = Some(Viewer {
            embedded,
            hold,
            abs,
            switch,
        });
        self.moved_at = None;
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
        // A viewer closing commits at once (02 §3.1).
        self.moved_at = None;
    }

    /// Where the closed viewer's exit leads: the list as it was, or the window closing.
    fn after_viewer(&mut self, ctx: &egui::Context) {
        self.status = self.told_later.take().unwrap_or_default();
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
    /// in its viewer (`files_only`: F3 leaves folders alone) — what the preview host has of it
    /// is taken over (02 §4.4), else it is made ready from the start.
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
            EntryKind::File | EntryKind::Unknown => {
                // `at=OFFSET` is for a text file opened from its start; `loading=1` for a
                // viewer that is told so when it is made (a preview handed over was not).
                let taken = match self.open_at {
                    Some(_) => preview::Taken::Nothing,
                    None if self.open_loading => preview::Taken::Nothing,
                    None => self.preview.take(&path),
                };
                match taken {
                    preview::Taken::Viewer(viewer) => self.handed_over(ctx, viewer, path),
                    preview::Taken::Card => self.start_open(ctx, path, entry.name),
                    preview::Taken::Running(prep) => {
                        self.status = Status::default();
                        self.opening = Some(Opening {
                            abs: path,
                            name: entry.name,
                            remote: self.label.is_some(),
                            done: prep.done.clone(),
                            total: prep.total.clone(),
                            cancel: prep.cancel.clone(),
                            source: Source::Preview(prep),
                        });
                    }
                    preview::Taken::Nothing => self.start_open(ctx, path, entry.name),
                }
            }
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

    /// `SOPEN_KEYS`: the next step, once the window has settled — as the owner's own key or
    /// typing in this pass.
    fn play(&mut self, ctx: &egui::Context) {
        if self.script_gap > 0 {
            self.script_gap -= 1;
            ctx.request_repaint();
            return;
        }
        if self.script.is_empty() || !self.settled() {
            return;
        }
        let Some(step) = self.script.pop_front() else {
            return;
        };
        self.script_gap = 2;
        ctx.input_mut(|i| match step {
            Play::Text(text) => i.events.push(egui::Event::Text(text)),
            Play::Key(key, modifiers) => {
                for pressed in [true, false] {
                    i.events.push(egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    });
                }
            }
        });
        ctx.request_repaint();
    }

    /// The list's keys, through the key table (04 §3): each command the frame's key events and
    /// typed text ask for, in order. Not while a dialog or the rename field has the keyboard
    /// (04 §3 rule 4: its typing and its Enter and Esc are its own).
    fn list_keys(&mut self, ctx: &egui::Context) {
        while !self.ops.holds_keyboard() && self.viewer.is_none() {
            let Some(command) = ctx.input_mut(|i| keys::lookup(i, Context::List)) else {
                break;
            };
            self.run(ctx, command);
        }
    }

    /// What each side shows now (02 §3.1): [`view::resolve`] over the state, the bar's rest
    /// measured on egui's clock, and the preview host told what the committed display is.
    fn resolve(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let panel = &self.panels[self.active];
        let entry = panel.current();
        let bar_is = (
            self.active,
            panel.dir.clone(),
            entry.map(|e| e.name.clone()),
        );
        if (bar_is.0, &bar_is.1) != (self.bar_was.0, &self.bar_was.1) {
            // Another side or another folder: at once.
            self.moved_at = None;
        } else if bar_is.2 != self.bar_was.2 {
            self.moved_at = Some(now);
        }
        self.bar_was = bar_is;
        // O5: a regular file or a link to one (an entry of unknown kind is tried as a file, as
        // Enter tries it).
        let file = entry
            .filter(|e| {
                !e.lossy
                    && e.name != panel::UP
                    && matches!(e.kind, EntryKind::File | EntryKind::Unknown)
            })
            .map(|e| preview::Want {
                path: panel.path_of(e),
                name: e.name.clone(),
            });
        let bar = match &file {
            Some(want) => view::Bar::File(want.path.clone()),
            None => view::Bar::Other,
        };
        let resolved = view::resolve(&view::Inputs {
            mode: self.mode,
            focus: self.active,
            bar,
            rest: self
                .moved_at
                .map(|at| Duration::from_secs_f64((now - at).max(0.0))),
            shown: self.shown.take(),
            viewer: self.viewer.is_some(),
        });
        self.uncommitted = resolved.pending.is_some();
        if let Some(wait) = resolved.pending {
            // An idle window commits all the same.
            ctx.request_repaint_after(wait);
        }
        // The preview the display shows: the file under the bar when the bar has rested on it,
        // else the one before it, by its path.
        let display = resolved.show.iter().find_map(|c| match c {
            view::Content::Preview(path) => Some(match &file {
                Some(want) if want.path == *path => want.clone(),
                _ => preview::Want {
                    path: path.clone(),
                    name: crate::remote::split_parent(path).1.to_string(),
                },
            }),
            _ => None,
        });
        // While a file is being opened nothing new is prepared: an opening taken over from the
        // preview is still that preparation's worker.
        let bar = file
            .as_ref()
            .filter(|_| self.opening.is_none() && self.viewer.is_none())
            .map(|w| w.path.as_str());
        self.preview.sync(ctx, display, bar);
        self.shown = Some(resolved.show);
    }

    /// The viewer's pass, then the browser's keys it left unused, through the key table: Esc
    /// and F3 leave it, F4 opens its file in the editor, F1 lists its keys. `esc_was_a_fields`:
    /// this frame's Esc took the keyboard from a text field (the viewer's Go to field): it does
    /// not leave.
    fn viewer_ui(&mut self, ui: &mut egui::Ui, esc_was_a_fields: bool) {
        let ctx = ui.ctx().clone();
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        let ((), asked) = super::host_pass(&ctx, || viewer.embedded.window.ui(ui));
        // 06 §7.3: the image gave way to the hex window (Hex view, a file that does not
        // decode) — on the same stream, in its place, nothing read again. Unless the owner left
        // in the same pass (its Esc asked the image window to close): then the hex window is
        // not made, and its file — the stream with it — goes; the viewer closes below.
        let given_way = viewer.switch.as_ref().and_then(|s| s.take());
        if let Some((file, notices)) = given_way.filter(|_| !asked) {
            let abs = viewer.abs.clone();
            self.drop_viewer();
            self.view(&ctx, super::hex::embed(file, notices), None, abs, None);
            ctx.request_repaint();
            return;
        }
        if asked {
            // Its prompt was answered, or a write it waited for landed — or it found it cannot
            // show its file after all: ask it again.
            if viewer.embedded.window.close_requested() {
                let failed = viewer.embedded.window.failure();
                self.drop_viewer();
                self.after_viewer(&ctx);
                // The open failed after all: said as a failed open is, in the list.
                if let Some(why) = failed {
                    self.status = Status::error(why);
                }
            }
            return;
        }
        // A text field with the typing (the hex window's Go to) is the owner's: no key of the
        // table acts from there but Esc (04 §3 rule 4) — and not an Esc that was the field's.
        let typing = ctx.text_edit_focused();
        let withheld: Vec<egui::Event> = ctx.input_mut(|i| {
            let (held, kept) = std::mem::take(&mut i.events).into_iter().partition(|e| {
                matches!(e, egui::Event::Key { key, pressed: true, .. }
                    if (*key == Key::Escape && esc_was_a_fields)
                        || (*key != Key::Escape && typing))
            });
            i.events = kept;
            held
        });
        while let Some(command) = ctx.input_mut(|i| keys::lookup(i, Context::Viewer)) {
            let Some(viewer) = self.viewer.as_ref() else {
                break;
            };
            match command {
                Command::Edit => {
                    let abs = viewer.abs.clone();
                    self.edit(&ctx, abs, false);
                }
                Command::Help => {
                    let own = viewer.embedded.window.keys();
                    help::open(&ctx, help::for_context(Context::Viewer, self.mode, own));
                }
                Command::CloseViewer => self.leave(&ctx, Exit::List),
                _ => {}
            }
        }
        ctx.input_mut(|i| i.events.extend(withheld));
    }

    /// The two sides, side by side: each its list, or — the side opposite the active list, in
    /// preview mode — the preview of the file under the bar ([`view`]).
    fn panels_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let area = ui.max_rect();
        // The list holds the keyboard: Tab, the arrows and Esc are its keys, not focus moves.
        // A dialog or the rename field holds it instead while it is open.
        let resp = ui.interact(area, list_id(), Sense::click());
        if !self.ops.holds_keyboard() && ctx.memory(|m| m.focused()) != Some(list_id()) {
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
            let preview = matches!(
                self.shown.as_ref().map(|s| &s[i]),
                Some(view::Content::Preview(_))
            );
            if preview {
                self.preview.ui(ui, rect);
            } else {
                self.panel_ui(ui, i, rect);
            }
        }
    }

    /// One panel: its frame with the path in the top border, the column heads, the rows — a
    /// selected entry in the selection's colour, the entry being renamed as a field in its row
    /// (S1-D's helpers, [`ops::entry_color`] and [`ops::Ops::rename_field`]) — and the footer. A
    /// click puts the bar on a row (and makes the panel active); a double click opens it.
    fn panel_ui(&mut self, ui: &mut egui::Ui, i: usize, rect: Rect) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter_at(rect);
        let active = i == self.active && self.viewer.is_none();
        let title = show(self.label.as_deref(), &self.panels[i].dir);
        let inner = chrome::panel_frame(&painter, rect, &title, active);
        let p = chrome::Palette::of(ui.visuals());
        let font = chrome::mono(ui);
        let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
        // Rows touch, a cell apart, as on a text screen.
        let row_h = text_h.ceil();
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
        // The foot: a row with the line across the window, then the counts.
        let foot_y = inner.bottom() - row_h;
        let rule_y = foot_y - row_h;
        let visible = (((rule_y - rows_top) / row_h).floor() as usize).max(1);
        if i == self.active {
            self.visible = visible;
        }
        // The entry being renamed (F2), in the panel it was pressed in: its row is the field.
        let renaming = (i == self.rename_panel)
            .then(|| self.ops.renaming().map(str::to_string))
            .flatten();
        let mut field = None;
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
            let row_rect =
                Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), row_h));
            if idx == panel.highlight {
                chrome::bar(&painter, row_rect, active);
            }
            let on_bar = idx == panel.highlight && active;
            let color = ops::entry_color(&p, entry, panel.is_selected(entry), on_bar);
            // Each cell fits its column at a glyph boundary, "…" where it is cut: never a
            // sliver of a glyph (a wide Hangul one included). A size too long for its column
            // is said in a larger unit, never cut.
            let (name, ext) = panel::split_name(entry);
            let name_rect =
                Rect::from_min_max(row_rect.min, Pos2::new(x_ext - cw, row_rect.bottom()));
            if idx == panel.highlight && renaming.as_deref() == Some(entry.name.as_str()) {
                // The name is a field over the Name and Ext columns, drawn after the rows.
                field = Some(Rect::from_min_max(
                    row_rect.min,
                    Pos2::new(x_ext + cw * ext_w as f32, row_rect.bottom()),
                ));
            } else {
                let name = chrome::fit_start(&ctx, name, &font, name_rect.width());
                let ext = chrome::fit_start(&ctx, ext, &font, cw * ext_w as f32);
                cell(
                    &painter.with_clip_rect(name_rect),
                    inner.left(),
                    y,
                    &name,
                    color,
                );
                cell(&painter, x_ext, y, &ext, color);
            }
            painter.text(
                Pos2::new(x_size_end, y),
                egui::Align2::RIGHT_TOP,
                panel::size_in(entry, size_w),
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
        if let Some(rect) = field {
            self.ops.rename_field(ui, rect);
        }
        // The scroll bar on the right border, beside the rows.
        let max_top = panel.entries.len().saturating_sub(visible);
        let column = Rect::from_min_max(
            Pos2::new(inner.right(), inner.top()),
            Pos2::new(inner.right() + chrome::CELL.x, rule_y),
        );
        chrome::scroll_bar(
            &painter,
            column,
            (max_top > 0).then(|| panel.top as f32 / max_top as f32),
        );
        let half = chrome::CELL / 2.0;
        chrome::rule(
            &painter,
            inner.left() - half.x,
            inner.right() + half.x,
            rule_y + half.y,
            active,
        );
        // The foot, centred when it fits inside the frame; else without the free space; else
        // the count from the left, its end cut — the count is never lost.
        let (listed, free) = panel.foot_parts();
        let width = |t: &str| {
            ctx.fonts_mut(|f| {
                f.layout_no_wrap(t.to_string(), font.clone(), p.text)
                    .size()
                    .x
            })
        };
        let whole = match &free {
            Some(free) => format!("{listed} · {free}"),
            None => listed.clone(),
        };
        let room = inner.width();
        let (foot, centred) = if width(&whole) <= room {
            (whole, true)
        } else if width(&listed) <= room {
            (listed, true)
        } else {
            (chrome::fit_start(&ctx, &listed, &font, room), false)
        };
        let (at, align) = if centred {
            (inner.center().x.round(), egui::Align2::CENTER_TOP)
        } else {
            (inner.left(), egui::Align2::LEFT_TOP)
        };
        painter.with_clip_rect(inner).text(
            Pos2::new(at, foot_y),
            align,
            foot,
            font.clone(),
            p.text,
        );
        if let Some((idx, open)) = clicked {
            let free = self.listing.is_empty() && self.opening.is_none();
            if free && self.viewer.is_none() && !self.ops.holds_keyboard() {
                self.active = i;
                self.panels[i].highlight = idx;
                if open {
                    self.enter(&ctx, false);
                }
            }
        }
    }

    /// The status row: a running fetch with its progress, a listing, a running file operation
    /// (or a refused rename's reason), else the last result, else the keys.
    fn status_line(&self) -> Status {
        if let Some(opening) = &self.opening {
            let name = &opening.name;
            if opening.cancel.load(Ordering::Relaxed) {
                return Status::info(format!("cancelling {name}…"));
            }
            let (d, t) = (
                opening.done.load(Ordering::Relaxed),
                opening.total.load(Ordering::Relaxed),
            );
            return if opening.remote && t > 0 {
                let pct = (d * 100).checked_div(t).unwrap_or(100);
                Status::info(format!(
                    "fetching {name} {pct}% · {} / {} · Esc cancels",
                    crate::human(d),
                    crate::human(t)
                ))
            } else {
                Status::info(format!("opening {name}…"))
            };
        }
        if self.viewer.is_some() {
            // The viewer's row: only a question an operation waits with is said over it.
            return match self.ops.waiting() {
                Some(what) => Status::info(format!(
                    "{what} waiting for an answer — F3 back to the list"
                )),
                None if !self.status.text.is_empty() => self.status.clone(),
                None => Status::info(VIEWER_HINT),
            };
        }
        if let Some(listing) = self.listing.first() {
            return Status::info(format!("listing {}…", listing.dir));
        }
        if let Some(said) = self.ops.status() {
            return said;
        }
        match &self.list_errors {
            [None, None] => {}
            [Some(left), Some(right)] if left == right => return Status::error(left.clone()),
            [left, right] => {
                let said: Vec<String> = [("left panel", left), ("right panel", right)]
                    .into_iter()
                    .filter_map(|(side, e)| e.as_ref().map(|e| format!("{side}: {e}")))
                    .collect();
                return Status::error(said.join(" · "));
            }
        }
        if !self.status.text.is_empty() {
            return self.status.clone();
        }
        Status::info(LIST_HINT)
    }

    /// Nothing is on its way: no listing, no opening, the display committed and its preview
    /// on screen, the viewer ready, no file operation running unasked (one waiting at its
    /// name-in-the-way question has settled: only an answer moves it).
    fn settled(&self) -> bool {
        self.painted
            && self.listing.is_empty()
            && self.opening.is_none()
            && !self.uncommitted
            && match &self.viewer {
                Some(viewer) => viewer.embedded.window.ready(),
                None => self.preview.settled(),
            }
            && (!self.ops.busy() || self.ops.holds_keyboard())
    }
}

impl Window for Browser {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Before anything else this pass: did this frame's Esc take the keyboard from a field?
        let esc_was_a_fields = self.focused_before && ctx.memory(|m| m.focused()).is_none();
        self.play(&ctx);
        self.poll(&ctx);
        self.list_keys(&ctx);
        self.resolve(&ctx);
        // The F-key row is the key table's, by the context and the mode (04 §2).
        let keys: Vec<FKey> = match &self.viewer {
            Some(v) => chrome::merged(
                &keys::row(Context::Viewer, self.mode),
                v.embedded.window.keys(),
            ),
            None => keys::row(Context::List, self.mode),
        };
        chrome::title_panel(ui, "sopen-browser-title", &self.title);
        chrome::key_panel(ui, "sopen-browser-keys", &keys);
        let status = self.status_line();
        chrome::bar_panel(egui::Panel::bottom("sopen-browser-status"))
            .show(ui, |ui| chrome::status_row(ui, &status));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                if self.viewer.is_some() {
                    self.viewer_ui(ui, esc_was_a_fields);
                } else {
                    self.panels_ui(ui);
                }
            });
        // A running file operation is heard on every pass, whatever the window shows; its
        // dialogs — a question, the report — are the list's, over the panels.
        let active = &mut self.panels[self.active];
        let changed = self.ops.poll(active);
        if self.viewer.is_none() {
            self.ops.dialogs(&ctx, active);
        }
        if let Some(changed) = changed {
            self.after_operation(&ctx, changed);
        }
        let working = !self.listing.is_empty()
            || self.opening.is_some()
            || !self.launches.is_empty()
            || self.preview.busy()
            || self.ops.busy();
        if working {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.focused_before = ctx.text_edit_focused();
        self.painted = true;
    }

    fn ready(&self) -> bool {
        self.settled() && self.script.is_empty() && self.script_gap == 0
    }

    /// Closing the window: a viewer on screen is asked first (its prompt, its running save);
    /// a fetch is stopped, a preview's preparation cancelled (nothing waits for it); a running
    /// file operation is waited for after the window ([`open`]).
    fn close_requested(&mut self) -> bool {
        if let Some(viewer) = self.viewer.as_mut() {
            self.exit = Exit::Quit;
            if !viewer.embedded.window.close_requested() {
                return false;
            }
            self.exit = Exit::List;
            self.drop_viewer();
        }
        if let Some(opening) = &self.opening {
            opening.cancel.store(true, Ordering::Relaxed);
        }
        self.preview.cancel();
        true
    }
}

#[cfg(test)]
mod tests;
