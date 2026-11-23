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
//! never over anything ([`ops`], on [`crate::fileops`]'s engine; 04 §5). F10 (or Alt+F10) shows
//! the directory tree in place of the active list (05; [`crate::tree`] is its model on a worker of
//! its own, [`tree`] its view), the other side frozen as it was (02 §3.1 O4) until Enter lists the
//! tree's folder in the panel or Esc / F10 closes it. Every key is a row of the one key table
//! ([`keys`]).
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

mod git_panel;
pub(crate) mod keys;
mod ops;
mod panel;
mod prepare;
mod preview;
mod tree;
pub(crate) mod view;

use super::chrome::{self, FKey};
use super::{help, Embedded, Hooks, Outcome, Status, Window};
use crate::fileops::{self, Item, ItemOutcome, Report};
use crate::git::detect::{self, Detector};
use crate::git::graph::{Row, RowStream};
use crate::git::history::{
    self, argv::Side as DiffSide, Generation, Git as _, LogOptions, LogStream, Page, PageEnd,
    RunnerGit, StartError,
};
use crate::git::model::{
    Change, ChangedFile, CommitDetails, Detection, FileLetter, Head, Oid, RefLabel, StashRow,
    Status as GitStatus, StatusState, Unavailable,
};
use crate::git::runner::{Class, Ending, GitCommand, GitRunner, Handle, LocalGit, SshGit, StopBy};
use crate::remote::ops::FileOps;
use crate::remote::treefs::TreeFs;
use crate::remote::{EntryKind, LocalFs, RemoteFs, SftpRemote};
use anyhow::{bail, Result};
use eframe::egui::{self, Key, Modifiers, Pos2, Rect, Sense, Vec2};
use git_panel::{
    Details, FileGroup, FileRow, GitAction, GitView, Group, HeadRef, Paging, RowKey, Subject,
    Summary, Tracking,
};
use keys::{Command, Context, Step};
pub use panel::Panel;
use prepare::Prepared;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
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
    /// on this machine. For what runs over the folder's ssh master besides the listing: git
    /// ([`git_connect`] takes it).
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
    /// Makes the git runner for the folder's host (03 §3; [`git_connect`]); `None`: no git
    /// panel in this window.
    pub git: Option<GitConnect>,
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

/// Makes the tree's worker and names the account `~<user>` is (05 §2, §6.4), on a thread of its
/// own: a remote one connects. The flag says the tree closed meanwhile: nothing is built then
/// ([`built_unless_closed`]).
pub(crate) type TreeConnect = Arc<dyn Fn(&AtomicBool) -> Result<TreeWorker> + Send + Sync>;

/// The tree's worker and the account `~<user>` is (`None`: the home is `realpath(".")`).
pub(crate) type TreeWorker = (Box<dyn TreeFs>, Option<String>);

/// The tree's worker as the window's own connection is made: this machine's disk on the
/// watchdog pool (05 §6.2), or the tree's own SFTP session to the host over its ssh master (05
/// §6.4: a deep scan never queues behind the panel's listing), which the worker renews through
/// a fresh one once handles may have been left open on the server.
fn tree_connect(label: Option<String>, state_dir: PathBuf) -> TreeConnect {
    Arc::new(move |closed: &AtomicBool| match label.as_deref() {
        None => Ok((LocalFs::tree(), None)),
        Some(label) => {
            let spec = host_spec(label)?;
            let user = spec.user.clone().or_else(|| ssh_user(&spec.destination()));
            let conn = SftpRemote::connect(&spec, &state_dir);
            let state_dir = state_dir.clone();
            built_unless_closed(closed, conn, move |conn| {
                let renew = Box::new(move || SftpRemote::connect(&spec, &state_dir).ok());
                (Box::new(conn).into_tree_renewing(renew), user)
            })
        }
    })
}

/// The tree's worker built on `conn` once its connect has returned — unless the tree closed
/// while it connected (`closed`; a blocking connect cannot be stopped): then the connection is
/// dropped at once, closing, and nothing is built on it.
fn built_unless_closed<C>(
    closed: &AtomicBool,
    conn: Result<C>,
    build: impl FnOnce(C) -> TreeWorker,
) -> Result<TreeWorker> {
    let conn = conn?;
    if closed.load(Ordering::Acquire) {
        drop(conn);
        bail!("the tree closed while it connected");
    }
    Ok(build(conn))
}

/// How often Enter on a link tries the browser's connection while a panel's listing has it:
/// it is taken only when free, never queued for, so the panel's own work never waits behind a
/// link's resolution.
const LINK_POLL: Duration = Duration::from_millis(20);

/// A link's target, `realpath` on the browser's connection (05 §1, Enter on a link). The
/// connection is tried every [`LINK_POLL`], never waited on, for as long as the request stands;
/// `None` — nothing called — once it does not (`closed`: the tree closed, or a newer Enter
/// replaced it), looked at before each try and again before `realpath`.
fn resolve_link(
    conn: &Mutex<Box<dyn RemoteFs + Send>>,
    path: &str,
    closed: &AtomicBool,
) -> Option<Result<String>> {
    loop {
        if closed.load(Ordering::Acquire) {
            return None;
        }
        let mut fs = match conn.try_lock() {
            Ok(fs) => fs,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(LINK_POLL);
                continue;
            }
        };
        if closed.load(Ordering::Acquire) {
            return None;
        }
        return Some(fs.realpath(path));
    }
}

/// A flag set when it is dropped: the tree's work on other threads (its connect, a link's
/// resolution) is told the tree closed — by Esc, Enter, F10 or the window closing alike.
struct Closed(Arc<AtomicBool>);

impl Closed {
    fn new() -> Self {
        Self(Arc::default())
    }

    fn flag(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}

impl Drop for Closed {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// The account `ssh -G` names for `dest` (05 §6.4: whose home `~<user>` is); `None` when ssh
/// cannot say. Nothing goes over the network.
fn ssh_user(dest: &str) -> Option<String> {
    let out = std::process::Command::new("ssh")
        .arg("-G")
        .arg(dest)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("user "))
        .map(|user| user.trim().to_string())
        .filter(|user| !user.is_empty())
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
            // Hand-over to S6-C, which runs F7 and F8 on S6-A's engine: until then, refused
            // before anything happens.
            job @ (ops::Job::MakeFolder { .. } | ops::Job::Delete { .. }) => {
                return Report {
                    items: job
                        .names()
                        .into_iter()
                        .map(|name| Item {
                            name,
                            outcome: ItemOutcome::NotReached,
                        })
                        .collect(),
                    notes: vec![
                        "sopen cannot make folders or delete yet (comes with S6-C); nothing \
                         was done"
                            .to_string(),
                    ],
                };
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
    /// A diff or a file at a revision (03 §6.2–§6.3): its viewer is read-only.
    read_only: Option<ReadOnly>,
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
    /// A diff or a file at a revision (03 §6.3): the viewer was told before its first pass, and
    /// so is the hex window it gives way to.
    read_only: bool,
}

/// The directory tree on the side that had the keyboard when F10 was pressed (05; 02 §3.1
/// `Content::Tree`): its model on a worker of its own, the view it is drawn from, and what the
/// other side showed then (O4).
struct TreeSide {
    side: usize,
    /// The panel's folder as F10 found it (05 §2 resolves it).
    folder: String,
    /// What the other side showed when the tree opened, with its identity (02 §3.1
    /// `tree_snapshot`): shown, unchanged, until the tree closes.
    snapshot: view::Content,
    model: TreeModel,
    view: tree::TreeView,
    /// The rows have been given to the view once (its first pass with rows centres the bar).
    rows_given: bool,
    /// 05 §5 has run to its end since: the bar was centred again then.
    built_seen: bool,
    /// What the model was last told of the screen (`top`, `height`, the bar's row).
    told: Option<(usize, usize, Option<crate::tree::RowId>)>,
    /// Enter on a link: its row as the status row names it, its target being resolved on the
    /// browser's connection, and that resolution's flag (set when it is dropped: replaced by
    /// another Enter, or the tree closed).
    target: Option<(String, Receiver<Result<String>>, Closed)>,
    /// Set when the tree goes: its connect, if still running, builds nothing.
    closed: Closed,
    /// What the tree says on its status row until the next key (a link whose target does not
    /// resolve).
    said: Option<Status>,
    /// The filesystem figure each boundary row showed last (the status row keeps it once a
    /// scan replaces the size, 05 §6.1).
    fs_seen: HashMap<crate::tree::RowId, u64>,
}

/// The tree's model, once its worker is made (a remote one connects first).
enum TreeModel {
    Connecting(Receiver<Result<TreeWorker>>),
    Open(Box<crate::tree::Tree>),
}

impl TreeSide {
    /// Nothing is on its way that a receipt waits for: the rows are on screen, 05 §5 has run to
    /// its end (or the tree has said why it has none), no link is being resolved. Listings
    /// further away and scans go on (`~/Library` and tag rows never get counts).
    fn settled(&self) -> bool {
        match &self.model {
            TreeModel::Connecting(_) => false,
            TreeModel::Open(model) => {
                self.target.is_none()
                    && (model.error().is_some() || model.built() && self.told.is_some())
            }
        }
    }
}

/// The rows the tree has room for beyond the list's: it has no column heads, rule or foot (its
/// 05 §5 budget E is the rows it shows).
const TREE_ROWS_OVER_LIST: usize = 3;

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
    /// The F-key switch did not take effect: the status row's line for it (04 §6.3), as the
    /// shell left it for this pass ([`super::fkeys_status`]).
    fkeys_line: Option<String>,
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
    /// The directory tree, while it is open (F10, 05).
    tree: Option<TreeSide>,
    /// Makes the tree's worker (05 §6.4).
    tree_connect: TreeConnect,
    /// The tree's host as its per-process cache knows it: empty on this machine, else `label`.
    tree_host: String,
    /// The git panels (03): the host, detection, the panels, the pin.
    git: GitSide,
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
        // The git panels' reads and streams stop with their handles; the `.revisions` folder
        // goes once the viewers that may read it are done, off the window's thread.
        self.git.panels.clear();
        let space = self.git.space.clone();
        leftover.workers.push(std::thread::spawn(move || {
            drop(space.lock().unwrap_or_else(|p| p.into_inner()).take());
        }));
    }
}

fn list_id() -> egui::Id {
    egui::Id::unique("sopen-browser-list")
}

/// Whether a click (`resp.clicked()`) is the second of a double click on the row `row` names,
/// which opens it — in the list and in the tree. egui's count says the click came within its
/// double-click time and distance of the one before; that one must have been on the same row.
/// egui alone counts a click on one row and then a double click on another as a triple click
/// (its count checks the earlier clicks' times, not their places), so its `double_clicked`
/// misses the second row's double click. A click that opened starts no pair.
fn second_click(resp: &egui::Response, row: egui::Id) -> bool {
    let quick = resp.double_clicked() || resp.triple_clicked();
    let last = egui::Id::unique("sopen-browser-last-row-click");
    resp.ctx.data_mut(|d| {
        let opens = quick && d.get_temp::<egui::Id>(last) == Some(row);
        d.insert_temp(last, if opens { egui::Id::NULL } else { row });
        opens
    })
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
        let tree_connect = tree_connect(start.label.clone(), start.state_dir.clone());
        let tree_host = start.label.clone().unwrap_or_default();
        let git = GitSide::new(start.git, start.label.as_deref());
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
            fkeys_line: None,
            visible: 1,
            painted: false,
            open_first: None,
            bar_first: None,
            open_at: None,
            open_loading: false,
            script: VecDeque::new(),
            script_gap: 0,
            tree: None,
            tree_connect,
            tree_host,
            git,
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

    /// Tests: the tree's worker made by `connect`, its per-process cache keyed by `host` (a
    /// test's own, so no other test's home is taken for it).
    #[cfg(test)]
    fn set_tree_connect(&mut self, connect: TreeConnect, host: &str) {
        self.tree_connect = connect;
        self.tree_host = host.to_string();
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
        let (connect, label) = (self.connect.clone(), self.label.clone());
        self.start_open_with(ctx, abs, name, connect, label, None);
    }

    /// [`Browser::start_open`] over `connect` to `label`'s files (a diff or a file at a revision
    /// is this machine's, in the cache's `.revisions`, whatever the folder's host), read-only
    /// when `read_only` says so.
    fn start_open_with(
        &mut self,
        ctx: &egui::Context,
        abs: String,
        name: String,
        connect: Connect,
        label: Option<String>,
        read_only: Option<ReadOnly>,
    ) {
        let (done, total) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let remote = label.is_some();
        let in_place = read_only.is_some();
        let cache = self.cache.clone();
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
            let result = if in_place {
                prepare::prepare_in_place(&req, &stop, &mut progress)
            } else {
                prepare::prepare(&req, &stop, &mut progress)
            };
            let _ = tx.send(result);
            waker.request_repaint();
        });
        self.status = Status::default();
        self.opening = Some(Opening {
            abs,
            name,
            remote,
            done,
            total,
            cancel,
            source: Source::Fresh {
                rx,
                worker: Some(worker),
            },
            read_only,
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
            // Tab (02 §3.2): into the other side's git panel when it shows one (pinned or O6),
            // else into its list.
            Command::OtherSide => match self.shown.as_ref().map(|s| &s[1 - active]) {
                Some(view::Content::Git(_)) => {
                    self.git.focus = true;
                    self.moved_at = None;
                }
                _ => self.active = 1 - active,
            },
            // Cmd/Ctrl+G (02 §3.2): the active folder's repository pinned to the other side;
            // again, unpinned.
            Command::Pin => {
                if self.git.pin.take().is_some() {
                    self.moved_at = None;
                    return;
                }
                let dir = self.panels[active].dir.clone();
                match self.git.detections.get(&dir) {
                    Some(Detection::WorkTree { top, .. }) => {
                        self.git.pin = Some(view::Pin {
                            side: 1 - active,
                            repo: top.clone(),
                        });
                        self.moved_at = None;
                    }
                    found => {
                        let host = self.label.clone().unwrap_or_else(|| "this machine".into());
                        let why = found.and_then(|d| d.reason(&host)).unwrap_or_else(|| {
                            match (found, &self.git.connect) {
                                (None, Some(_)) => {
                                    "git panel: still asking git about this folder".into()
                                }
                                (None, None) => "git panel: no git in this window".into(),
                                _ => "git panel: not in a git work tree".into(),
                            }
                        });
                        self.status = Status::info(format!(
                            "{why} — nothing to pin ({})",
                            show(self.label.as_deref(), &dir)
                        ));
                    }
                }
            }
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
                // 03 §2.2: the folder is detected again — and a host whose git could not be
                // reached (none, too old, its ssh options unread) is asked again.
                self.git.detector.forget(&self.git.host, &dir);
                self.git.detections.remove(&dir);
                self.git.said.remove(&dir);
                // On the frame: only if the lock is free at once (it is held for a store only).
                if let Ok(mut made) = self.git.runner.try_lock() {
                    if made.as_ref().is_some_and(|r| r.is_err()) {
                        *made = None;
                    }
                }
                self.relist(Some(active), dir, name, Some(at), Some(ctx));
            }
            Command::SwapPanels => {
                self.panels.swap(0, 1);
                // The keyboard goes with its list — unless the other side is pinned: the pin
                // belongs to its side, and the keyboard's list stays where it is.
                if self.git.pin.is_none() {
                    self.active = 1 - active;
                }
            }
            Command::Tree => self.open_tree(ctx),
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
    /// the engine stops at its next check and reports what it did (04 §5.1, §5.2) — else, when
    /// a pin exists, unpins it.
    fn stop(&mut self) {
        if let Some(opening) = &self.opening {
            opening.cancel.store(true, Ordering::Relaxed);
        } else if self.ops.busy() {
            self.ops.stop();
        } else if self.git.pin.take().is_some() {
            self.moved_at = None;
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
                // 03 §5: a git panel whose top level holds one soft-refreshes.
                for i in 0..self.git.panels.len() {
                    let top = self.git.panels[i].top.clone();
                    let inside = |d: &String| {
                        *d == top || d.starts_with(&format!("{}/", top.trim_end_matches('/')))
                    };
                    if dirs.iter().any(inside) {
                        self.start_read(ctx, i, true, 0);
                    }
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
        // A listing is not a bar move: what it changes commits at once (02 §3.1). A folder whose
        // detection failed (an answer it could not read, a lost channel) is asked again on this
        // visit (S2-B: such a result is not cached).
        self.moved_at = None;
        if matches!(
            self.git.detections.get(&dir),
            Some(Detection::Unavailable(Unavailable::Failed(_)))
        ) {
            self.git.detections.remove(&dir);
        }
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
        let (abs, name, read_only) = (opening.abs, opening.name, opening.read_only);
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
            Ok(result) => self.opened(ctx, result, abs, name, read_only),
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

    /// The file at `abs` came back ready: its viewer shows, in this window — read-only, titled
    /// and edited as `read_only` says, for a diff or a file at a revision (03 §6.2–§6.3).
    fn opened(
        &mut self,
        ctx: &egui::Context,
        result: Result<Prepared>,
        abs: String,
        name: String,
        read_only: Option<ReadOnly>,
    ) {
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
        let title = match &read_only {
            Some(r) => r.title.clone(),
            None => show(self.label.as_deref(), &abs),
        };
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
        // F4 in a viewer of a revision opens the file's path now, never the cache's copy.
        let (abs, read_only) = match read_only {
            Some(r) => (r.edit, true),
            None => (abs, false),
        };
        self.view(ctx, embedded, hold, abs, switch, read_only);
    }

    /// What `SOPEN_STATE` asks of the browser (a scripted run): the entry to open or to put
    /// the bar on, the mode, and what the text viewer it opens is handed (`at=`, `loading=1`).
    fn scripted(&mut self, state: &super::State) {
        self.open_first = state.open.clone();
        self.open_at = state.at;
        self.open_loading = state.loading;
        self.mode = state.mode;
        self.bar_first = state.bar.clone();
        self.git.script = GitScript {
            pin: state.git_pin,
            select: state.select.clone(),
            details: state.details,
            label: state.label,
        };
    }

    /// A preview handed over (02 §4.4): the live window is the viewer.
    fn handed_over(&mut self, ctx: &egui::Context, viewer: prepare::HandedOver, abs: String) {
        self.status = match viewer.notices.as_slice() {
            [] => Status::default(),
            said => Status::info(said.join(" · ")),
        };
        self.view(ctx, viewer.embedded, viewer.hold, abs, None, false);
    }

    /// `embedded` is the viewer on screen from now on; `read_only` (a diff, a file at a
    /// revision) is told to it here, before its first pass (03 §6.3; S2-E: a flag set later
    /// could let a recovery of an earlier session start).
    fn view(
        &mut self,
        ctx: &egui::Context,
        mut embedded: Embedded,
        hold: Option<crate::cache::Claim>,
        abs: String,
        switch: Option<super::image::HexSwitch>,
        read_only: bool,
    ) {
        if read_only {
            embedded.window.set_read_only(true);
        }
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
            read_only,
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
                            read_only: None,
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

    /// F10, Alt+F10 (05 §1): the tree in place of the active list, its worker made on a thread
    /// (a remote one connects); the other side keeps what it shows (O4). It commits at once.
    fn open_tree(&mut self, ctx: &egui::Context) {
        let side = self.active;
        let folder = self.panels[side].dir.clone();
        let snapshot = self
            .shown
            .as_ref()
            .map_or(view::Content::List, |shown| shown[1 - side].clone());
        let (tx, rx) = mpsc::channel();
        let (connect, waker) = (self.tree_connect.clone(), ctx.clone());
        let closed = Closed::new();
        let flag = closed.flag();
        std::thread::spawn(move || {
            let made = connect(&flag);
            // A tree closed meanwhile gets nothing: what was made goes here, and its connection
            // closes with it.
            if flag.load(Ordering::Acquire) {
                drop(made);
                return;
            }
            let _ = tx.send(made);
            waker.request_repaint();
        });
        let title = show(self.label.as_deref(), &folder);
        self.tree = Some(TreeSide {
            side,
            folder,
            snapshot,
            model: TreeModel::Connecting(rx),
            view: tree::TreeView::new(title, Vec::new(), 0),
            rows_given: false,
            built_seen: false,
            told: None,
            target: None,
            closed,
            said: None,
            fs_seen: HashMap::new(),
        });
        self.status = Status::default();
        self.moved_at = None;
    }

    /// Closes the tree (Esc, F10, Enter): every listing and scan stops, nothing is waited for;
    /// its connect and a link's resolution still running are told (their flags), so neither
    /// builds or calls anything more; the side shows its list as it was and the other side
    /// follows the rules again, at once.
    fn close_tree(&mut self) {
        if let Some(t) = self.tree.take() {
            t.closed.0.store(true, Ordering::Release);
            if let Some((_, _, closed)) = &t.target {
                closed.0.store(true, Ordering::Release);
            }
            if let TreeModel::Open(mut model) = t.model {
                model.close();
            }
        }
        self.moved_at = None;
    }

    /// The tree's pass before the sides are drawn (05 §1): its worker once made, the model's
    /// answers and its rows given to the view, a link's target resolved for Enter, then the
    /// tree's keys — none while a file operation's dialog has the keyboard.
    fn tree_pass(&mut self, ctx: &egui::Context) {
        let Some(t) = self.tree.as_mut() else {
            return;
        };
        let mut failed = None;
        if let TreeModel::Connecting(rx) = &t.model {
            match rx.try_recv() {
                Ok(Ok((fs, user))) => {
                    let params = crate::tree::Params {
                        e: self.visible + TREE_ROWS_OVER_LIST,
                        ..Default::default()
                    };
                    let mut model = crate::tree::Tree::open(
                        fs,
                        crate::tree::Open {
                            host: self.tree_host.clone(),
                            folder: t.folder.clone(),
                            user,
                            params,
                        },
                    );
                    let waker = ctx.clone();
                    model.set_wake(crate::remote::Wake::new(move || waker.request_repaint()));
                    t.model = TreeModel::Open(Box::new(model));
                }
                Ok(Err(e)) => failed = Some(format!("{e:#}")),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    failed = Some("its worker stopped without a result".to_string())
                }
            }
        }
        let mut went = None;
        if let TreeModel::Open(model) = &mut t.model {
            model.poll();
            if let Some(why) = model.error() {
                failed = Some(why.to_string());
            } else if model.ready() {
                let rows = model
                    .rows()
                    .iter()
                    .map(|row| {
                        if let crate::tree::Size::Fs { used_bytes } = row.info.size {
                            t.fs_seen.insert(row.id, used_bytes);
                        }
                        tree::view_row(row, t.fs_seen.get(&row.id).copied())
                    })
                    .collect::<Vec<_>>();
                if t.rows_given {
                    t.view.set_rows(rows);
                } else {
                    // The first rows: the bar on the panel's folder, its row in the middle.
                    let bar = model
                        .current()
                        .and_then(|id| rows.iter().position(|r| r.id == tree::view_id(id)))
                        .unwrap_or(0);
                    let title = model.title().unwrap_or_else(|| t.view.title.clone());
                    t.view = tree::TreeView::new(title, rows, bar);
                    t.rows_given = true;
                }
                if model.built() && !t.built_seen {
                    // The folders 05 §5 opened since the first frame may have put rows above the
                    // bar: its row goes back to the middle (05 §1) — unless the owner moved it.
                    t.built_seen = true;
                    let current = model.current().map(tree::view_id);
                    if t.view.bar_row().map(|r| r.id) == current {
                        t.view.centre();
                    }
                }
            }
            t.view.status = model.status().map(Status::error);
        }
        // Enter on a link: its target, once resolved, is listed; one that does not resolve is
        // said, and the tree stays.
        if let Some((shown, rx, _)) = &t.target {
            match rx.try_recv() {
                Ok(Ok(path)) => went = Some(path),
                Ok(Err(e)) => {
                    t.said = Some(Status::error(format!(
                        "{shown}: the link's target cannot be reached: {e:#}"
                    )));
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    t.said = Some(Status::error(format!(
                        "{shown}: its target was not resolved"
                    )));
                }
            }
            if went.is_some() || t.said.is_some() {
                t.target = None;
            }
        }
        if let Some(why) = failed {
            self.close_tree();
            self.status = Status::error(format!("the tree cannot open: {why}"));
            return;
        }
        if let Some(path) = went {
            self.tree_outcome(ctx, tree::TreeOutcome::GoTo(path));
            return;
        }
        if self.ops.holds_keyboard() {
            return;
        }
        let Some(t) = self.tree.as_mut() else {
            return;
        };
        // Esc first stops an owner's whole listing or Space's scan (05 §6.3); else it closes the
        // tree, as F10 does.
        if let TreeModel::Open(model) = &mut t.model {
            if model.owner_busy() && ctx.input_mut(|i| keys::take(i, Key::Escape, keys::Mods::None))
            {
                model.stop_owner();
                t.said = None;
            }
        }
        let actions = tree::keys(ctx, &mut t.view);
        self.tree_actions(ctx, actions);
    }

    /// Carries out what the tree's keys and clicks ask of the model (05 §1).
    fn tree_actions(&mut self, ctx: &egui::Context, actions: Vec<tree::TreeAction>) {
        use tree::TreeAction as A;
        for action in actions {
            let Some(t) = self.tree.as_mut() else {
                return;
            };
            t.said = None;
            let TreeModel::Open(model) = &mut t.model else {
                // The worker is still being made: only F1 and closing act.
                match action {
                    A::Help => help::open(ctx, help::for_context(Context::Tree, self.mode, &[])),
                    A::Close => self.close_tree(),
                    _ => {}
                }
                continue;
            };
            match action {
                A::Help => help::open(ctx, help::for_context(Context::Tree, self.mode, &[])),
                A::Close => self.close_tree(),
                A::GoTo(id) => self.tree_go_to(ctx, id),
                A::Expand(id) => {
                    model.expand(tree::model_id(id));
                }
                A::Collapse(id) => {
                    if let Some(parent) = model.collapse(tree::model_id(id)) {
                        t.view.put_bar(tree::view_id(parent));
                    }
                }
                A::SizeNow(id) => model.size_now(tree::model_id(id)),
                A::Reread(id) => model.reread(tree::model_id(id)),
                // Done in the view: the bar is on the next row starting with it.
                A::Jump(_) => {}
            }
        }
    }

    /// Enter on a row (05 §1): its folder — a chain's last — is listed in the panel and the tree
    /// closes; a link's target is resolved first, on the browser's connection, and a target that
    /// no longer resolves is said (the model drops that answer).
    fn tree_go_to(&mut self, ctx: &egui::Context, id: usize) {
        let Some(t) = self.tree.as_mut() else {
            return;
        };
        let TreeModel::Open(model) = &mut t.model else {
            return;
        };
        let row = tree::model_id(id);
        let link = t
            .view
            .rows
            .iter()
            .any(|r| r.id == id && r.kind == tree::RowKind::Link);
        if !link {
            if let Some(path) = model.enter(row) {
                self.tree_outcome(ctx, tree::TreeOutcome::GoTo(path));
            }
            return;
        }
        let Some(path) = model
            .rows()
            .into_iter()
            .find(|r| r.id == row)
            .map(|r| r.path)
        else {
            return;
        };
        let waker = ctx.clone();
        let (tx, rx) = mpsc::channel();
        let conn = self.conn.clone();
        let wanted = path.clone();
        let closed = Closed::new();
        let flag = closed.flag();
        std::thread::spawn(move || {
            if let Some(resolved) = resolve_link(&conn, &wanted, &flag) {
                let _ = tx.send(resolved);
                waker.request_repaint();
            }
        });
        // An Enter before it drops the earlier resolution, whose flag stops it.
        t.target = Some((show(self.label.as_deref(), &path), rx, closed));
    }

    /// What the tree hands the browser (02 §7): Enter's folder is listed in the tree's panel,
    /// and the tree closes.
    fn tree_outcome(&mut self, ctx: &egui::Context, outcome: tree::TreeOutcome) {
        let Some(side) = self.tree.as_ref().map(|t| t.side) else {
            return;
        };
        match outcome {
            tree::TreeOutcome::GoTo(path) => {
                self.close_tree();
                self.list(Some(side), path, None, Some(ctx));
            }
        }
    }

    /// The tree's side: the view drawn (its clicks carried out), then the model told what is on
    /// screen when that changed — phase 2 lists around it, phase 3 sizes on it (05 §6.3).
    fn tree_ui(&mut self, ui: &mut egui::Ui, i: usize, rect: Rect) {
        let Some(t) = self.tree.as_mut() else {
            self.panel_ui(ui, i, rect);
            return;
        };
        let mut actions = tree::draw(ui, rect, &mut t.view, true);
        if self.ops.holds_keyboard() {
            // A file operation's dialog is the owner's now, as in the list.
            actions.clear();
        }
        if let TreeModel::Open(model) = &mut t.model {
            if t.rows_given {
                let bar = t.view.bar_row().map(|r| tree::model_id(r.id));
                let now = (t.view.top, t.view.height(), bar);
                if t.told != Some(now) {
                    model.view(now.0, now.1, now.2);
                    t.told = Some(now);
                }
            }
        }
        self.tree_actions(ui.ctx(), actions);
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
        while !self.ops.holds_keyboard()
            && self.viewer.is_none()
            && self.tree.is_none()
            && self.git_focused().is_none()
        {
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
        // The keyboard in a git panel: the one the committed display shows opposite the active
        // list — kept under a viewer it opened (Esc comes back to it); a display that no longer
        // shows one gives the keyboard back.
        let pinned_there = self
            .git
            .pin
            .as_ref()
            .filter(|p| p.side == 1 - self.active)
            .map(|p| p.repo.clone());
        let in_git = match self.shown.as_ref().map(|s| &s[1 - self.active]) {
            _ if !self.git.focus => None,
            // The pinned side shows its panel whatever the display showed (rule 2).
            _ if pinned_there.is_some() && self.viewer.is_none() => {
                self.git.focus_top = pinned_there.clone();
                pinned_there
            }
            Some(view::Content::Git(top)) => {
                self.git.focus_top = Some(top.clone());
                Some(top.clone())
            }
            Some(view::Content::Viewer) => self.git.focus_top.clone(),
            _ => None,
        };
        if self.git.focus && in_git.is_none() && self.shown.is_some() {
            self.git.focus = false;
            self.git.wide = false;
        }
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
        let focus = match (&self.tree, &in_git) {
            (Some(t), _) => t.side,
            (None, Some(_)) => 1 - self.active,
            (None, None) => self.active,
        };
        let resolved = view::resolve(&view::Inputs {
            mode: self.mode,
            focus,
            in_git: in_git.clone(),
            repo: self.git.repo_of(&panel.dir).map(str::to_string),
            pin: self.git.pin.clone(),
            git_wide: self.git.wide,
            tree_snapshot: self.tree.as_ref().map(|t| t.snapshot.clone()),
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
        // preview is still that preparation's worker. While the tree is open the other side is
        // its snapshot (O4): a preview in it is kept, its preparation not cancelled, and the
        // tree's bar prepares nothing.
        let bar = match self.tree.as_ref().map(|t| &t.snapshot) {
            Some(view::Content::Preview(path)) => Some(path.as_str()),
            Some(_) => None,
            None => file
                .as_ref()
                .filter(|_| self.opening.is_none() && self.viewer.is_none() && in_git.is_none())
                .map(|w| w.path.as_str()),
        };
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
            let (abs, read_only) = (viewer.abs.clone(), viewer.read_only);
            self.drop_viewer();
            // A file at a revision stays read-only in the hex window it gives way to.
            let made = super::hex::embed(file, notices);
            self.view(&ctx, made, None, abs, None, read_only);
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
        // Wide (Cmd/Ctrl+Shift+G): the git panel over both sides.
        if let Some([view::Content::Git(a), view::Content::Git(b)]) = self.shown.clone() {
            if a == b {
                self.git_ui(ui, 1 - self.active, area.shrink(2.0), &a);
                return;
            }
        }
        let half = (area.width() / 2.0).floor();
        for i in 0..2 {
            let rect = Rect::from_min_size(
                Pos2::new(area.left() + half * i as f32, area.top()),
                Vec2::new(half, area.height()),
            )
            .shrink(2.0);
            match self.shown.as_ref().map(|s| s[i].clone()) {
                Some(view::Content::Preview(_)) => self.preview.ui(ui, rect),
                Some(view::Content::Tree) => self.tree_ui(ui, i, rect),
                Some(view::Content::Git(top)) => self.git_ui(ui, i, rect, &top),
                _ => self.panel_ui(ui, i, rect),
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
        let active = i == self.active && self.viewer.is_none() && !self.git.focus;
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
            if resp.clicked() {
                let pair = list_id().with((i, &panel.dir, &entry.name));
                clicked = Some((idx, second_click(&resp, pair)));
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
            // While the tree is open the other side is a snapshot (02 §3.1 O4): a click there
            // moves nothing.
            let free = self.listing.is_empty() && self.opening.is_none() && self.tree.is_none();
            if free && self.viewer.is_none() && !self.ops.holds_keyboard() {
                self.active = i;
                // A click in a list gives it the keyboard back from a git panel.
                self.git.focus = false;
                self.git.wide = false;
                self.panels[i].highlight = idx;
                if open {
                    self.enter(&ctx, false);
                }
            }
        }
    }

    /// The status row: a running fetch with its progress, a listing, a running file operation
    /// (or a refused rename's reason), a listing that failed, else the F-key switch's line while
    /// it did not take effect (04 §6.3: said until it does, so no last result covers it), else
    /// the last result, else the keys. Over a viewer: an operation's question, else the switch's
    /// line, else the viewer's result, else its keys.
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
        if let Some(t) = &self.tree {
            if let Some(said) = &t.said {
                return said.clone();
            }
            if let Some((shown, _, _)) = &t.target {
                return Status::info(format!("resolving {shown}…"));
            }
            if let Some(said) = self.ops.status() {
                return said;
            }
            return match &t.model {
                TreeModel::Open(model) if model.ready() => t.view.status_line(),
                _ => Status::info(format!("reading the tree of {}…", t.view.title)),
            };
        }
        if self.viewer.is_some() {
            // The viewer's row: only a question an operation waits with is said over it.
            return match (self.ops.waiting(), &self.fkeys_line) {
                (Some(what), _) => Status::info(format!(
                    "{what} waiting for an answer — F3 back to the list"
                )),
                (None, Some(line)) => Status::error(line.clone()),
                (None, None) if !self.status.text.is_empty() => self.status.clone(),
                (None, None) => Status::info(VIEWER_HINT),
            };
        }
        if let Some(listing) = self.listing.first() {
            return Status::info(format!("listing {}…", listing.dir));
        }
        if let Some(said) = self.ops.status() {
            return said;
        }
        // The git panel has the keyboard: a line of ours (an open refused, …), else its own.
        if let Some(i) = self.git_focused() {
            if !self.status.text.is_empty() {
                return self.status.clone();
            }
            return self.git.panels[i].view.status_line();
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
        if let Some(line) = &self.fkeys_line {
            return Status::error(line.clone());
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
            && self.tree.as_ref().is_none_or(TreeSide::settled)
            && self.git.settled(
                &self.panels[self.active].dir,
                self.shown.as_ref().map_or(&[], |s| s.as_slice()),
            )
    }
}

impl Window for Browser {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Before anything else this pass: did this frame's Esc take the keyboard from a field?
        let esc_was_a_fields = self.focused_before && ctx.memory(|m| m.focused()).is_none();
        self.play(&ctx);
        self.git_stop_on_esc(&ctx);
        self.poll(&ctx);
        self.git_pass(&ctx);
        if self.tree.is_some() {
            self.tree_pass(&ctx);
        } else if self.git_focused().is_some() {
            self.git_keys(&ctx);
        } else {
            self.list_keys(&ctx);
        }
        self.resolve(&ctx);
        self.git_follow(&ctx);
        // The F-key row is the key table's, by the context and the mode (04 §2).
        let in_git = self.git_focused().is_some();
        let keys: Vec<FKey> = match (&self.viewer, &self.tree) {
            (Some(v), _) => chrome::merged(
                &keys::row(Context::Viewer, self.mode),
                v.embedded.window.keys(),
            ),
            (None, Some(_)) => tree::fkeys(),
            (None, None) if in_git => git_panel::fkeys(self.mode),
            (None, None) => keys::row(Context::List, self.mode),
        };
        chrome::title_panel(ui, "sopen-browser-title", &self.title);
        chrome::key_panel(ui, "sopen-browser-keys", &keys);
        self.fkeys_line = super::fkeys_status(&ctx);
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
            || self.ops.busy()
            || self.tree.as_ref().is_some_and(|t| !t.settled())
            || self.git.busy();
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
        // The tree's listings and scans stop; nothing waits for them (05 §6.2).
        self.close_tree();
        true
    }
}

// ---------------------------------------------------------------------------------------------
// The git panel's side of the browser (03; 02 §3, §6; S2-F): the window's git host, detection of
// the active folder, a panel per repository (at most 3 kept), their loads, pages, details,
// history searches, diffs and files at a revision.

/// Makes the window's git runner (02 §6 `GitHost`): on a worker, the first time detection needs
/// it (`ssh -G` for a remote, `xcode-select -p` for the macOS shim); why there is none, else.
pub type GitConnect = Arc<dyn Fn() -> std::result::Result<GitRunner, Unavailable> + Send + Sync>;

/// The git runner as the window's own connection reaches the folder: this machine's git (03
/// §2.4: `SOPEN_GIT`, else `git` on `PATH`, the shim only with the developer tools), or the
/// remote's over the ssh master (03 §3.2), the browser's `connect` bringing a missing master back.
pub fn git_connect(
    destination: Option<String>,
    label: Option<String>,
    state_dir: PathBuf,
    connect: Connect,
) -> GitConnect {
    Arc::new(move || match (&destination, &label) {
        (Some(dest), Some(label)) => {
            let ssh = SshGit::new(dest, label, &state_dir)
                .map_err(|e| Unavailable::Failed(format!("{e:#}")))?;
            let connect = connect.clone();
            let revive: crate::git::runner::Revive = Arc::new(move || connect().map(|_| ()));
            Ok(GitRunner::ssh(ssh).with_revive(revive))
        }
        _ => Ok(GitRunner::local(LocalGit::new(detect::which_git()?))),
    })
}

/// A history read off the window's thread: `rx` gets what it found; `handle` stops it (a newer
/// one replaces it, the panel goes).
struct Pending<T> {
    rx: Receiver<T>,
    handle: Handle,
}

impl<T> Drop for Pending<T> {
    fn drop(&mut self) {
        self.handle.stop();
    }
}

/// The reads a load starts from (03 §4.1): refs, HEAD, stashes, status, and the remotes.
#[derive(Debug, Clone)]
struct Snapshot {
    refs: Vec<RefLabel>,
    head: Head,
    stashes: Vec<StashRow>,
    status: GitStatus,
    remotes: Vec<String>,
}

impl Snapshot {
    /// The same graph: refs, HEAD and stashes unchanged (03 §5's soft refresh keeps the rows).
    fn same_graph(&self, other: &Snapshot) -> bool {
        self.refs == other.refs && self.head == other.head && self.stashes == other.stashes
    }
}

/// A load's snapshot read: a full load (`soft` false) starts a new stream; a soft refresh
/// starts one only when the refs, HEAD or stashes moved (03 §5).
struct SnapshotRead {
    soft: bool,
    /// Pages the new stream reads at once (a reload reads as many as were loaded, 03 §4.5).
    pages: usize,
    pending: Pending<std::result::Result<Snapshot, String>>,
}

/// The log stream of one load on a worker of its own (03 §4.5): asked for pages, it reads them;
/// between asks git waits on the full pipe. Dropping the link stops git (the handle) and ends
/// the worker (the sender).
struct StreamLink {
    generation: u64,
    handle: Handle,
    ask: mpsc::Sender<usize>,
    pages: Receiver<Page>,
    /// Pages asked for and not come back yet.
    asked: usize,
    rows: RowStream,
    /// The next page is the load's first: it replaces the rows.
    first: bool,
    /// The stream has ended: all loaded, an error, or stopped.
    ended: bool,
}

impl Drop for StreamLink {
    fn drop(&mut self) {
        self.handle.stop();
    }
}

/// A history search's stream (03 §4.7), paged like the graph's.
struct SearchLink {
    query: git_panel::SearchQuery,
    link: StreamLink,
}

/// What a file row's Enter or Shift+F3 found (03 §6.2–§6.3).
enum Revealed {
    /// A diff or a blob written under `.revisions`: opened read-only, titled `host:/top/path @
    /// <rev>`; F4 opens the path now.
    File {
        file: PathBuf,
        title: String,
        edit: String,
    },
    /// The working file itself (the uncommitted row's untracked file, a comparison's work-tree
    /// end of one): opened as Enter in the list opens it.
    Working(String),
    /// Nothing to open, and why (`not in this version`, a read that failed).
    Refused(String),
}

/// The git panel of one repository (03), kept while the window lives (at most [`KEPT_PANELS`]).
struct RepoPanel {
    /// Its top level.
    top: String,
    /// Detection said it is a shallow clone: the last page's end is `history ends here`.
    shallow: bool,
    view: GitView,
    generation: Generation,
    snapshot: Option<Snapshot>,
    read: Option<SnapshotRead>,
    stream: Option<StreamLink>,
    /// The stream was ended while the panel was hidden (03 §4.5: after 30 s, or for a newer
    /// panel's): shown again, a new one reads as many pages as were loaded.
    stream_dropped: bool,
    search: Option<SearchLink>,
    /// A history search whose stream was ended while hidden: its next page runs it again.
    search_dropped: Option<git_panel::SearchQuery>,
    details: Option<Pending<Details>>,
    /// The details the pane was last given (an untracked folder's files go into them).
    last_details: Option<Details>,
    reveal: Option<Pending<Revealed>>,
    /// `SOPEN_STATE select=REV` resolved: the commit's id.
    resolving: Option<Pending<std::result::Result<Oid, String>>>,
    /// Shown now (egui time it was last shown; `None` while it shows).
    hidden_since: Option<std::time::Instant>,
    /// When the panel last asked for its refs (a refresh younger than 10 s is not repeated).
    refreshed: Option<std::time::Instant>,
    /// Least recently shown first, by this counter.
    last_shown: u64,
}

/// Panels kept per window (02 §3.1: the least recently shown is dropped).
const KEPT_PANELS: usize = 3;
/// A hidden panel's stream ends after this long (03 §4.5).
const HIDDEN_STREAM: Duration = Duration::from_secs(30);
/// A refresh younger than this is not repeated when the panel is shown again (03 §5).
const REFRESH_AGAIN: Duration = Duration::from_secs(10);
/// Parked log streams per host at most: each holds one of the host's 3 command slots (02 §6),
/// and a read must always find one.
const PARKED_STREAMS: usize = 2;

/// `SOPEN_STATE`'s git keys (02 §9): what is left to do once the panel is there.
#[derive(Debug, Clone, Default, PartialEq)]
struct GitScript {
    pin: bool,
    select: Option<String>,
    details: bool,
    label: Option<usize>,
}

impl GitScript {
    fn done(&self) -> bool {
        *self == GitScript::default()
    }
}

/// The window's git state (02 §6's `GitHost` and its repositories).
struct GitSide {
    connect: Option<GitConnect>,
    /// The host's runner, made by the first detection's worker; why there is none, until the
    /// list's Cmd/Ctrl+R asks again (03 §2.2: a host without git is detected again then).
    runner: Arc<Mutex<Option<std::result::Result<GitRunner, Unavailable>>>>,
    detector: Arc<Detector>,
    /// The host as detection's cache names it: empty on this machine, else the label.
    host: String,
    /// Each folder's detection this window had (03 §2.2), and those still asked.
    detections: HashMap<String, Detection>,
    asking: HashMap<String, Receiver<Detection>>,
    /// Folders whose reason the status row has said (once per folder, 03 §2.3).
    said: std::collections::HashSet<String>,
    panels: Vec<RepoPanel>,
    shown_counter: u64,
    pin: Option<view::Pin>,
    /// The keyboard is in the git panel on the side opposite the active list (the active list
    /// is the origin list then, 02 §3.1 `origin`).
    focus: bool,
    /// That panel's repository, as the display last showed it (a viewer it opened covers it).
    focus_top: Option<String>,
    /// The keyboard's git panel over both sides (Cmd/Ctrl+Shift+G).
    wide: bool,
    /// The window's folder under `.revisions` (03 §6.2), made on the first diff's worker.
    space: Arc<Mutex<Option<crate::cache::RevisionSpace>>>,
    script: GitScript,
    /// The window had the focus at the last pass (a soft refresh when it gains it, 03 §5).
    window_focused: Option<bool>,
    /// How long a hidden panel keeps its stream ([`HIDDEN_STREAM`]; tests shorten it).
    hidden_stream: Duration,
}

impl GitSide {
    fn new(connect: Option<GitConnect>, label: Option<&str>) -> Self {
        Self {
            connect,
            runner: Arc::default(),
            detector: Arc::new(Detector::new()),
            host: label.unwrap_or_default().to_string(),
            detections: HashMap::new(),
            asking: HashMap::new(),
            said: Default::default(),
            panels: Vec::new(),
            shown_counter: 0,
            pin: None,
            focus: false,
            focus_top: None,
            wide: false,
            space: Arc::default(),
            script: GitScript::default(),
            window_focused: None,
            hidden_stream: HIDDEN_STREAM,
        }
    }

    /// The repository of `folder` when detection found a work tree there.
    fn repo_of(&self, folder: &str) -> Option<&str> {
        match self.detections.get(folder) {
            Some(Detection::WorkTree { top, .. }) => Some(top),
            _ => None,
        }
    }

    fn panel(&self, top: &str) -> Option<usize> {
        self.panels.iter().position(|p| p.top == top)
    }

    /// Nothing git is on its way that a receipt waits for.
    fn settled(&self, active_folder: &str, shown: &[view::Content]) -> bool {
        if self.asking.contains_key(active_folder) || !self.script.done() {
            return false;
        }
        self.panels.iter().all(|p| {
            let showing = shown
                .iter()
                .any(|c| matches!(c, view::Content::Git(top) if *top == p.top));
            !showing
                || (p.read.is_none()
                    && p.stream.as_ref().is_none_or(|s| s.asked == 0)
                    && p.details.is_none()
                    && p.reveal.is_none()
                    && p.resolving.is_none()
                    && p.view.selecting().is_none()
                    && p.search.as_ref().is_none_or(|s| s.link.asked == 0))
        })
    }

    fn busy(&self) -> bool {
        !self.asking.is_empty()
            || self.panels.iter().any(|p| {
                p.read.is_some()
                    || p.stream.as_ref().is_some_and(|s| s.asked > 0)
                    || p.details.is_some()
                    || p.reveal.is_some()
                    || p.resolving.is_some()
                    || p.search.as_ref().is_some_and(|s| s.link.asked > 0)
            })
    }
}

/// HEAD as the panel's labels take it.
fn head_ref(head: &Head) -> HeadRef {
    match head {
        Head::Branch { refname, .. } => HeadRef::Branch(refname.clone()),
        Head::Detached(oid) => HeadRef::Detached(oid.clone()),
        Head::Unborn { .. } => HeadRef::Unborn,
    }
}

/// The foot's right end from the status read's branch headers (03 §4.2).
fn tracking(status: &GitStatus) -> Option<Tracking> {
    let b = &status.branch;
    let (Some(branch), Some(upstream), Some(ab)) = (&b.head, &b.upstream, &b.ahead_behind) else {
        return None;
    };
    Some(Tracking {
        branch: branch.clone(),
        upstream: upstream.clone(),
        ahead: ab.ahead,
        behind: ab.behind,
    })
}

/// The log's options from the panel's filters (03 §4.6).
fn log_options(f: &git_panel::Filters) -> LogOptions {
    LogOptions {
        order: match f.order {
            git_panel::Order::Date => history::Order::Date,
            git_panel::Order::AuthorDate => history::Order::AuthorDate,
            git_panel::Order::Topo => history::Order::Topo,
        },
        first_parent: f.first_parent,
        reflog: f.reflog,
    }
}

/// The log's roots for the filters (03 §4.1, §4.6).
fn roots_of(f: &git_panel::Filters, s: &Snapshot) -> Vec<Oid> {
    let revisions = f.revisions(&s.refs, &s.remotes);
    let head = f.head_is_revision().then_some(&s.head);
    history::roots(&revisions, head, f.stash_revisions(&s.stashes))
}

/// The four reads of a load and the remotes (03 §4.1), at once (at most 3 in flight per host).
fn read_snapshot(git: &RunnerGit, top: &str) -> std::result::Result<Snapshot, String> {
    std::thread::scope(|s| {
        let refs = s.spawn(|| history::refs(git, top));
        let head = s.spawn(|| history::head(git, top));
        let stashes = s.spawn(|| history::stashes(git, top));
        let status = s.spawn(|| history::status(git, top));
        let remotes = history::remotes(git, top);
        let said = |what: &str, e: history::ReadError| format!("git panel: the {what} read: {e}");
        Ok(Snapshot {
            refs: joined(refs.join()).map_err(|e| said("refs", e))?,
            head: joined(head.join()).map_err(|e| said("HEAD", e))?,
            stashes: joined(stashes.join()).map_err(|e| said("stashes", e))?,
            status: joined(status.join()).map_err(|e| said("status", e))?,
            remotes: remotes.map_err(|e| said("remotes", e))?,
        })
    })
}

/// A read joined from its thread (a panic in it is a read that stopped).
fn joined<T>(
    r: std::thread::Result<std::result::Result<T, history::ReadError>>,
) -> std::result::Result<T, history::ReadError> {
    r.unwrap_or_else(|_| {
        Err(history::ReadError::Run(history::RunError::Failed(
            "the read stopped".into(),
        )))
    })
}

/// A change letter as the pane draws it (03 §6.1).
fn letter_of(letter: FileLetter) -> char {
    match letter {
        FileLetter::Change(c) => change_char(c),
        FileLetter::Unmerged => 'U',
        FileLetter::Untracked => '?',
    }
}

fn change_char(c: Change) -> char {
    match c {
        Change::Added => 'A',
        Change::Modified => 'M',
        Change::Deleted => 'D',
        Change::Renamed => 'R',
        Change::TypeChanged => 'T',
        Change::Copied => 'C',
    }
}

fn file_row(f: ChangedFile) -> FileRow {
    FileRow {
        letter: letter_of(f.letter),
        path: f.path,
        old_path: f.old_path,
        lines: f.lines,
        lossy: f.lossy,
    }
}

/// `+adds -dels` by path (a binary file has none).
type LineCounts = HashMap<String, (u64, u64)>;

/// The uncommitted row's groups (03 §6.1) from the status read, with their line counts.
fn uncommitted_groups(status: &GitStatus, counts: &[LineCounts; 2]) -> Vec<FileGroup> {
    let mut conflicts = Vec::new();
    let mut staged = Vec::new();
    let mut not_staged = Vec::new();
    let mut untracked = Vec::new();
    for e in &status.entries {
        let row = |letter: char, lines: Option<(u64, u64)>| FileRow {
            letter,
            path: e.path.clone(),
            old_path: e.old_path.clone(),
            lines,
            lossy: e.lossy,
        };
        match &e.state {
            StatusState::Conflict(_) => conflicts.push(row('U', None)),
            StatusState::Untracked => untracked.push(FileRow {
                old_path: None,
                ..row('?', None)
            }),
            StatusState::Changed {
                staged: s,
                unstaged: u,
            } => {
                if let Some(c) = s {
                    staged.push(row(change_char(*c), counts[0].get(&e.path).copied()));
                }
                if let Some(c) = u {
                    not_staged.push(FileRow {
                        old_path: None,
                        ..row(change_char(*c), counts[1].get(&e.path).copied())
                    });
                }
            }
        }
    }
    [
        (Group::Conflicts, conflicts),
        (Group::Staged, staged),
        (Group::NotStaged, not_staged),
        (Group::Untracked, untracked),
    ]
    .into_iter()
    .filter(|(_, files)| !files.is_empty())
    .map(|(group, files)| FileGroup { group, files })
    .collect()
}

/// A commit's summary as the pane takes it.
fn summary(d: &CommitDetails) -> Summary {
    Summary {
        oid: d.oid.clone(),
        parents: d.parents.clone(),
        author_name: d.author_name.clone(),
        author_email: d.author_email.clone(),
        author_time: d.author_time,
        committer_name: d.committer_name.clone(),
        committer_email: d.committer_email.clone(),
        commit_time: d.commit_time,
        message: d.message.clone(),
    }
}

/// What the load knows that a details read needs: the stashes (a stash row's parents) and the
/// status (the uncommitted row's groups).
#[derive(Clone)]
struct DetailsInputs {
    stashes: Vec<StashRow>,
    status: GitStatus,
}

/// The details of `subject` (03 §6.1, §6.4), told through `tell` — the summary and files first,
/// then again with `In` once `for-each-ref --contains` has answered.
fn read_details(
    git: &RunnerGit,
    top: &str,
    subject: &Subject,
    inputs: &DetailsInputs,
    tell: &dyn Fn(Details),
) {
    let mut details = Details {
        subject: subject.clone(),
        summary: None,
        contains: None,
        groups: Vec::new(),
        error: None,
    };
    let fail = |mut d: Details, e: history::ReadError| {
        d.error = Some(e.to_string());
        tell(d)
    };
    let counts = || -> std::result::Result<[LineCounts; 2], history::ReadError> {
        Ok([
            history::uncommitted_counts(git, top, true)?,
            history::uncommitted_counts(git, top, false)?,
        ])
    };
    let files = |rows: Vec<ChangedFile>| FileGroup {
        group: Group::Files,
        files: rows.into_iter().map(file_row).collect(),
    };
    let oid_of = |key: &RowKey| match key {
        RowKey::Commit(oid) | RowKey::Stash(oid) => Some(oid.clone()),
        RowKey::Uncommitted => None,
    };
    match subject {
        Subject::Row(RowKey::Uncommitted) => {
            let counts = match counts() {
                Ok(c) => c,
                Err(e) => return fail(details, e),
            };
            details.groups = uncommitted_groups(&inputs.status, &counts);
            tell(details);
        }
        Subject::Row(RowKey::Stash(oid)) => {
            let d = match history::details(git, top, oid) {
                Ok(d) => d,
                Err(e) => return fail(details, e),
            };
            details.summary = Some(summary(&d));
            let stash = inputs.stashes.iter().find(|s| s.oid == *oid).cloned();
            let stash = stash.unwrap_or_else(|| StashRow {
                oid: oid.clone(),
                base: d.parents.first().cloned().unwrap_or_else(|| oid.clone()),
                index: d.parents.get(1).cloned(),
                untracked: d.parents.get(2).cloned(),
                selector: String::new(),
                commit_time: d.commit_time,
                subject: String::new(),
            });
            match history::stash_files(git, top, &stash) {
                Ok(rows) => {
                    let (untracked, tracked): (Vec<_>, Vec<_>) = rows
                        .into_iter()
                        .partition(|f| f.letter == FileLetter::Untracked);
                    details.groups.push(files(tracked));
                    if !untracked.is_empty() {
                        details.groups.push(FileGroup {
                            group: Group::Untracked,
                            ..files(untracked)
                        });
                    }
                    tell(details);
                }
                Err(e) => fail(details, e),
            }
        }
        Subject::Row(RowKey::Commit(oid)) => {
            let d = match history::details(git, top, oid) {
                Ok(d) => d,
                Err(e) => return fail(details, e),
            };
            details.summary = Some(summary(&d));
            match history::commit_files(git, top, &d) {
                Ok(rows) => details.groups.push(files(rows)),
                Err(e) => return fail(details, e),
            }
            tell(details.clone());
            // A failed read is said as such, never as "in no branch".
            details.contains = Some(
                history::contains(git, top, oid)
                    .unwrap_or_else(|e| vec![format!("unavailable: {e}")]),
            );
            tell(details);
        }
        Subject::Compare { from, to } => {
            let result = match (oid_of(from), oid_of(to)) {
                (Some(a), Some(b)) => {
                    history::files(git, top, &a, &b).map(|rows| vec![files(rows)])
                }
                (Some(a), None) | (None, Some(a)) => {
                    history::worktree_files(git, top, &a).map(|rows| {
                        let mut groups = vec![files(rows)];
                        let untracked: Vec<FileRow> = inputs
                            .status
                            .entries
                            .iter()
                            .filter(|e| e.state == StatusState::Untracked)
                            .map(|e| FileRow {
                                letter: '?',
                                path: e.path.clone(),
                                old_path: None,
                                lines: None,
                                lossy: e.lossy,
                            })
                            .collect();
                        if !untracked.is_empty() {
                            groups.push(FileGroup {
                                group: Group::Untracked,
                                files: untracked,
                            });
                        }
                        groups
                    })
                }
                (None, None) => Ok(Vec::new()),
            };
            match result {
                Ok(groups) => {
                    details.groups = groups;
                    tell(details);
                }
                Err(e) => fail(details, e),
            }
        }
    }
}

/// The two endpoints of a file row (03 §6.2's table); `None` for the working file.
fn endpoints(
    git: &RunnerGit,
    top: &str,
    subject: &Subject,
    group: Group,
) -> std::result::Result<(Option<DiffSide>, Option<DiffSide>, String), history::ReadError> {
    let parents = |oid: &Oid| history::details(git, top, oid).map(|d| d.parents);
    let empty = || history::empty_tree(git, top);
    let short = |oid: &Oid| oid.as_str().chars().take(8).collect::<String>();
    Ok(match subject {
        Subject::Row(RowKey::Commit(c)) => {
            let old = match parents(c)?.first() {
                Some(p) => p.clone(),
                None => empty()?,
            };
            (
                Some(DiffSide::Tree(old)),
                Some(DiffSide::Tree(c.clone())),
                short(c),
            )
        }
        Subject::Row(RowKey::Stash(s)) => {
            let parents = parents(s)?;
            match (group, parents.get(2)) {
                (Group::Untracked, Some(untracked)) => (
                    Some(DiffSide::Tree(empty()?)),
                    Some(DiffSide::Tree(untracked.clone())),
                    format!("{}^3", short(s)),
                ),
                _ => (
                    Some(DiffSide::Tree(parents.first().cloned().unwrap_or(empty()?))),
                    Some(DiffSide::Tree(s.clone())),
                    short(s),
                ),
            }
        }
        Subject::Row(RowKey::Uncommitted) => match group {
            Group::Staged => {
                let head = history::head(git, top)?;
                let old = match head.oid() {
                    Some(oid) => oid.clone(),
                    None => empty()?,
                };
                (
                    Some(DiffSide::Tree(old)),
                    Some(DiffSide::Index),
                    "index".into(),
                )
            }
            Group::Untracked => (None, None, "work tree".into()),
            _ => (
                Some(DiffSide::Index),
                Some(DiffSide::WorkTree),
                "work tree".into(),
            ),
        },
        Subject::Compare { from, to } => {
            let oid = |k: &RowKey| match k {
                RowKey::Commit(o) | RowKey::Stash(o) => Some(o.clone()),
                RowKey::Uncommitted => None,
            };
            match (oid(from), oid(to)) {
                (Some(a), Some(b)) => {
                    let rev = short(&b);
                    (Some(DiffSide::Tree(a)), Some(DiffSide::Tree(b)), rev)
                }
                (Some(a), None) if group == Group::Untracked => {
                    let _ = a;
                    (None, None, "work tree".into())
                }
                (Some(a), None) | (None, Some(a)) => (
                    Some(DiffSide::Tree(a)),
                    Some(DiffSide::WorkTree),
                    "work tree".into(),
                ),
                (None, None) => (None, None, "work tree".into()),
            }
        }
    })
}

/// The most of a diff written (03 §6.2): a longer one is cut there with a last line saying so.
const DIFF_CAP: usize = 256 << 20;

/// Writes `bytes` to a new file named `name` in the window's `.revisions` folder (made here, on
/// this worker, the first time).
fn revision_file(
    space: &Mutex<Option<crate::cache::RevisionSpace>>,
    cache: &std::path::Path,
    name: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    let mut space = space.lock().unwrap_or_else(|p| p.into_inner());
    if space.is_none() {
        *space = Some(crate::cache::RevisionSpace::new(cache)?);
    }
    let Some(space) = space.as_ref() else {
        bail!("no folder for files at a revision");
    };
    let (path, mut file) = space.create(name)?;
    use std::io::Write as _;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(path)
}

/// The file at `path`'s last component, `.diff` added for a diff.
fn file_name(path: &str) -> &str {
    crate::remote::split_parent(path.trim_end_matches('/')).1
}

/// Enter (`diff`) or Shift+F3 on a file row (03 §6.2, §6.3): the diff of its endpoints, or its
/// new endpoint's bytes, written under `.revisions`; the working file itself where that is the
/// endpoint.
#[allow(clippy::too_many_arguments)]
fn reveal(
    git: &RunnerGit,
    top: &str,
    shown_top: &str,
    subject: &Subject,
    group: Group,
    file: &FileRow,
    diff: bool,
    space: &Mutex<Option<crate::cache::RevisionSpace>>,
    cache: &std::path::Path,
) -> Revealed {
    let working = || Revealed::Working(format!("{}/{}", top.trim_end_matches('/'), file.path));
    let edit = format!("{}/{}", top.trim_end_matches('/'), file.path);
    let (old, new, rev) = match endpoints(git, top, subject, group) {
        Ok(e) => e,
        Err(e) => return Revealed::Refused(format!("{}: {e}", file.path)),
    };
    let title = format!("{}/{} @ {rev}", shown_top.trim_end_matches('/'), file.path);
    let written = |name: String, bytes: &[u8]| match revision_file(space, cache, &name, bytes) {
        Ok(path) => Revealed::File {
            file: path,
            title: title.clone(),
            edit: edit.clone(),
        },
        Err(e) => Revealed::Refused(format!("{}: {e:#}", file.path)),
    };
    let read = |args: Vec<String>| {
        let cmd = GitCommand::new(Class::Read, top, args).max_output(DIFF_CAP);
        let done = git.runner.run(&cmd, &git.handle);
        match &done.ending {
            Ending::Exit(0) => Ok((done.stdout, false)),
            Ending::Stopped(s) if matches!(s.by, StopBy::Overflow(_)) => Ok((done.stdout, true)),
            _ => Err(done.entry.status_line()),
        }
    };
    if diff {
        let (Some(old), Some(new)) = (old, new) else {
            // The uncommitted row's untracked file (and a comparison's): the working file.
            return working();
        };
        let paths: Vec<&str> = file
            .old_path
            .iter()
            .map(String::as_str)
            .chain([file.path.as_str()])
            .collect();
        return match read(history::argv::diff_text(&old, &new, &paths)) {
            Ok((mut bytes, cut)) => {
                if cut {
                    bytes.extend_from_slice(b"\n... the diff is cut here: it is over 256 MiB\n");
                }
                written(format!("{}.diff", file_name(&file.path)), &bytes)
            }
            Err(why) => Revealed::Refused(format!("{}: {why}", file.path)),
        };
    }
    // Shift+F3: the new endpoint; none for a deleted file.
    if file.letter == 'D' {
        return Revealed::Refused(format!("{}: not in this version", file.path));
    }
    let rev = match new {
        Some(DiffSide::Tree(oid)) => Some(oid),
        Some(DiffSide::Index) => None,
        Some(DiffSide::WorkTree) | None => return working(),
    };
    match read(history::argv::blob(rev.as_ref(), &file.path)) {
        Ok((bytes, false)) => written(file_name(&file.path).to_string(), &bytes),
        Ok((_, true)) => Revealed::Refused(format!("{}: over 256 MiB", file.path)),
        Err(why) => Revealed::Refused(format!("{}: {why}", file.path)),
    }
}

/// The first page of `folder`'s git panel as text (`SOPEN_LIST=git`, 02 §9; 03 §11 G-D1, G-D2,
/// G-G1): the title line, a line per row — the lanes in ASCII, the labels, the subject, the
/// author, the date, the hash — then the paging line. A folder outside a work tree: the listing
/// and one line saying why.
pub fn git_listing(
    fs: &mut dyn RemoteFs,
    label: Option<&str>,
    abs: &str,
    connect: &GitConnect,
) -> Result<Vec<String>> {
    let host = label.unwrap_or_default();
    let detection = match connect() {
        Ok(runner) => Detector::new().detect(host, abs, &RunnerGit::new(runner, Handle::new())),
        Err(why) => Detection::Unavailable(why),
    };
    let Detection::WorkTree { top, shallow, .. } = &detection else {
        let mut out = listing(fs, label, abs)?;
        let shown = label.unwrap_or("this machine");
        let why = detection
            .reason(shown)
            .unwrap_or_else(|| "git panel: not a work tree".into());
        out.push(why);
        return Ok(out);
    };
    let runner = connect().map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let git = RunnerGit::new(runner, Handle::new());
    let snapshot = read_snapshot(&git, top).map_err(|e| anyhow::anyhow!(e))?;
    let title = format!("{}{}", show(label, top), detection.title_notes());
    let mut view = GitView::new(title.clone());
    view.set_remotes(snapshot.remotes.clone());
    view.set_refs(snapshot.refs.clone(), head_ref(&snapshot.head));
    view.tracking = tracking(&snapshot.status);
    let f = view.filters().clone();
    let mut rows = RowStream::new(
        snapshot.head.oid().cloned(),
        !snapshot.status.entries.is_empty(),
        snapshot.stashes.clone(),
        f.stashes,
    );
    let roots = roots_of(&f, &snapshot);
    let n = snapshot.status.entries.len();
    let current = Generation::new();
    let g = current.bump();
    match LogStream::start(&git, top, &log_options(&f), &roots, &current, g) {
        Ok(mut stream) => {
            let page = stream.page(history::PAGE);
            view.set_rows(rows.page(page.rows), f.first_parent, n);
            view.set_paging(paging_of(&page.end, *shallow));
        }
        Err(StartError::NoRoots) => {
            view.set_rows(rows.page(Vec::new()), f.first_parent, n);
            view.set_paging(Paging::NoCommits);
        }
        Err(e) => bail!("git panel: the log: {e:?}"),
    }
    let mut out = vec![title];
    for i in 0..view.rows().len() {
        out.extend(text_row(&view, i));
    }
    out.push(view.paging_line());
    Ok(out)
}

/// What the paging line says after a page that ended so.
fn paging_of(end: &PageEnd, shallow: bool) -> Paging {
    match end {
        PageEnd::More => Paging::More(history::PAGE),
        PageEnd::AllLoaded if shallow => Paging::Shallow,
        PageEnd::AllLoaded => Paging::AllLoaded,
        PageEnd::Error(e) => Paging::Error(e.clone()),
        PageEnd::Superseded => Paging::Loading,
    }
}

/// Row `i` of `view` as `SOPEN_LIST=git` prints it: the lanes in ASCII (`*` a commit, `@` the
/// checked-out one, `=` a stash, `o` the uncommitted row, `|` a line, `/` `\` a lane change),
/// the labels in brackets, the subject, then the author, the date and the hash.
fn text_row(view: &GitView, i: usize) -> Option<String> {
    use crate::git::graph::{End, Node};
    let row = view.line(i)?;
    let graph = view.graph();
    let width = (graph.width() as usize).max(1);
    let mut cells = vec![' '; width * 2];
    for p in graph.pass_throughs(i..i + 1) {
        if let Some(c) = cells.get_mut(p.slot as usize * 2) {
            *c = '|';
        }
    }
    let rg = graph.row(i);
    let column = rg.column as usize;
    for s in graph.segments(i) {
        let slot = s.slot as usize;
        if slot == column {
            continue;
        }
        let between = slot.min(column) * 2 + 1;
        let up = matches!(s.end, End::Top);
        let glyph = match (up, slot > column) {
            (true, true) | (false, false) => '/',
            _ => '\\',
        };
        if let Some(c) = cells.get_mut(between) {
            *c = glyph;
        }
    }
    if let Some(c) = cells.get_mut(column * 2) {
        *c = match rg.node {
            Node::Commit => '*',
            Node::CheckedOut => '@',
            Node::Stash => '=',
            Node::Uncommitted => 'o',
        };
    }
    let lanes: String = cells.into_iter().collect();
    let labels: String = row.labels.iter().map(|l| format!("[{l}] ")).collect();
    let mut line = format!(
        "{lanes}  {labels}{}  {}  {}  {}",
        row.subject, row.author, row.date, row.commit
    );
    let trimmed = line.trim_end().len();
    line.truncate(trimmed);
    Some(line)
}

impl Browser {
    /// The runner, once the first detection's worker made it (never made on a frame).
    /// Never waits: the lock is held only to read or store the result (the connect runs outside
    /// it), and a moment it is held reads as "not ready yet".
    fn git_runner(&self) -> Option<GitRunner> {
        let made = match self.git.runner.try_lock() {
            Ok(made) => made,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        made.as_ref().and_then(|r| r.as_ref().ok()).cloned()
    }

    /// The git panel the keyboard is in (02 §3.1 `in_git`): the one shown on the side opposite
    /// the active list.
    fn git_focused(&self) -> Option<usize> {
        if !self.git.focus || self.viewer.is_some() {
            return None;
        }
        match self.shown.as_ref().map(|s| &s[1 - self.active]) {
            Some(view::Content::Git(top)) => self.git.panel(top),
            _ => None,
        }
    }

    /// The keyboard back to the origin list (Tab, Esc's unpin, F9 from an automatic panel).
    fn git_leave(&mut self) {
        self.git.focus = false;
        self.git.wide = false;
        self.moved_at = None;
    }

    /// The git pass before the keys (02 §6): detections that answered (a detection answer
    /// commits at once), the active folder's asked on a worker, each panel's reads, pages,
    /// details and files heard, a hidden panel's stream ended after 30 s.
    fn git_pass(&mut self, ctx: &egui::Context) {
        let mut answered = Vec::new();
        self.git.asking.retain(|folder, rx| match rx.try_recv() {
            Ok(d) => {
                answered.push((folder.clone(), d));
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
        });
        for (folder, detection) in answered {
            if folder == self.panels[self.active].dir {
                self.moved_at = None;
            }
            // 03 §2.3: why there is no panel, once per folder.
            if detection.on_status_row() && self.git.said.insert(folder.clone()) {
                let host = self.label.clone().unwrap_or_else(|| "this machine".into());
                if let Some(why) = detection.reason(&host) {
                    self.status = Status::error(why);
                }
            }
            self.git.detections.insert(folder, detection);
        }
        let folder = self.panels[self.active].dir.clone();
        let listing = self.listing.iter().any(|l| l.dir == folder);
        if self.git.connect.is_some()
            && !listing
            && !self.git.detections.contains_key(&folder)
            && !self.git.asking.contains_key(&folder)
        {
            self.ask_detection(ctx, folder);
        }
        for i in 0..self.git.panels.len() {
            self.poll_panel(ctx, i);
        }
        self.git_script(ctx);
        // A window that gains the focus soft-refreshes what it shows (03 §5).
        let focused = ctx.input(|i| i.viewport().focused);
        if let Some(now) = focused {
            if self.git.window_focused == Some(false) && now {
                for i in 0..self.git.panels.len() {
                    if self.git.panels[i].hidden_since.is_none() {
                        self.start_read(ctx, i, true, 0);
                    }
                }
            }
            self.git.window_focused = Some(now);
        }
    }

    /// Detection of `folder` on a worker (03 §2): the runner is made there the first time.
    fn ask_detection(&mut self, ctx: &egui::Context, folder: String) {
        let Some(connect) = self.git.connect.clone() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let (cell, detector, host) = (
            self.git.runner.clone(),
            self.git.detector.clone(),
            self.git.host.clone(),
        );
        let (waker, path) = (ctx.clone(), folder.clone());
        std::thread::spawn(move || {
            // The connect (`ssh -G`, finding git) runs outside the lock; the first result stored
            // wins, a second worker's is dropped.
            let known = cell.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let made = match known {
                Some(made) => made,
                None => {
                    let fresh = connect();
                    let mut cell = cell.lock().unwrap_or_else(|p| p.into_inner());
                    cell.get_or_insert(fresh).clone()
                }
            };
            let detection = match made {
                Ok(runner) => detector.detect(&host, &path, &RunnerGit::new(runner, Handle::new())),
                Err(why) => Detection::Unavailable(why),
            };
            let _ = tx.send(detection);
            waker.request_repaint();
        });
        self.git.asking.insert(folder, rx);
    }

    /// After the sides are resolved: each git panel shown exists (made and loaded the first
    /// time), one shown again is refreshed (03 §5) or its stream started again (03 §4.5); one
    /// hidden for 30 s has its stream ended; at most [`KEPT_PANELS`] are kept.
    fn git_follow(&mut self, ctx: &egui::Context) {
        let shown: Vec<String> = self
            .shown
            .iter()
            .flatten()
            .filter_map(|c| match c {
                view::Content::Git(top) => Some(top.clone()),
                _ => None,
            })
            .collect();
        let now = std::time::Instant::now();
        for top in &shown {
            let i = match self.git.panel(top) {
                Some(i) => i,
                None => self.make_panel(ctx, top.clone()),
            };
            self.git.shown_counter += 1;
            let counter = self.git.shown_counter;
            let p = &mut self.git.panels[i];
            p.last_shown = counter;
            if p.hidden_since.take().is_some() {
                if p.stream_dropped {
                    let pages = p.view.rows().len().div_ceil(history::PAGE).max(1);
                    self.start_read(ctx, i, false, pages);
                } else if p.refreshed.is_none_or(|t| t.elapsed() >= REFRESH_AGAIN) {
                    self.start_read(ctx, i, true, 0);
                }
            }
        }
        let hidden_stream = self.git.hidden_stream;
        for p in self.git.panels.iter_mut() {
            if shown.contains(&p.top) {
                continue;
            }
            let since = *p.hidden_since.get_or_insert(now);
            if since.elapsed() < hidden_stream {
                continue;
            }
            if p.stream.as_ref().is_some_and(|s| !s.ended) {
                p.stream = None;
                p.stream_dropped = true;
            }
            // A history search's stream has the same deadline, whatever the graph's did: its
            // results stay, and ↓ on their last row (or the foot) reads them again.
            if let Some(search) = p.search.take_if(|s| !s.link.ended) {
                let paging = Paging::Error("stopped while hidden; ↓ reads it again".into());
                p.view
                    .append_search_results(&search.query, Vec::new(), paging);
                p.search_dropped = Some(search.query);
            }
        }
        // The least recently shown panel goes when a fourth is kept (its running reads stop).
        while self.git.panels.len() > KEPT_PANELS {
            let pinned = self.git.pin.as_ref().map(|p| p.repo.clone());
            let drop = self
                .git
                .panels
                .iter()
                .enumerate()
                .filter(|(_, p)| !shown.contains(&p.top) && Some(&p.top) != pinned.as_ref())
                .min_by_key(|(_, p)| p.last_shown)
                .map(|(i, _)| i);
            match drop {
                Some(i) => {
                    self.git.panels.remove(i);
                }
                None => break,
            }
        }
        let hidden_any = self.git.panels.iter().any(|p| p.hidden_since.is_some());
        if hidden_any {
            // An idle window still ends a hidden stream on time.
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    /// A new panel for the repository at `top`, its load started; its index.
    fn make_panel(&mut self, ctx: &egui::Context, top: String) -> usize {
        let detection = self
            .git
            .detections
            .values()
            .find(|d| matches!(d, Detection::WorkTree { top: t, .. } if *t == top));
        let notes = detection.map(Detection::title_notes).unwrap_or_default();
        let shallow = matches!(detection, Some(Detection::WorkTree { shallow: true, .. }));
        let title = format!("{}{notes}", show(self.label.as_deref(), &top));
        self.git.panels.push(RepoPanel {
            top,
            shallow,
            view: GitView::new(title),
            generation: Generation::new(),
            snapshot: None,
            read: None,
            stream: None,
            stream_dropped: false,
            search: None,
            search_dropped: None,
            details: None,
            last_details: None,
            reveal: None,
            resolving: None,
            hidden_since: None,
            refreshed: None,
            last_shown: 0,
        });
        let i = self.git.panels.len() - 1;
        self.start_read(ctx, i, false, 1);
        i
    }

    /// The reads of a load (03 §4.1) on a worker: `soft` keeps the rows when the refs, HEAD and
    /// stashes did not move (03 §5); otherwise a new stream reads `pages` pages (at least one).
    fn start_read(&mut self, ctx: &egui::Context, i: usize, soft: bool, pages: usize) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let handle = Handle::new();
        let git = RunnerGit::new(runner, handle.clone());
        let (tx, rx) = mpsc::channel();
        let (top, waker) = (p.top.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(read_snapshot(&git, &top));
            waker.request_repaint();
        });
        // A newer read replaces an older one (its handle stops what it still runs).
        p.read = Some(SnapshotRead {
            soft: soft && p.snapshot.is_some() && p.stream.is_some(),
            pages: pages.max(1),
            pending: Pending { rx, handle },
        });
        p.refreshed = Some(std::time::Instant::now());
        if !soft && p.view.rows().is_empty() {
            p.view.set_paging(Paging::Loading);
        }
    }

    /// Everything panel `i` waits for, heard.
    fn poll_panel(&mut self, ctx: &egui::Context, i: usize) {
        // The load's reads.
        let read = self.git.panels[i]
            .read
            .as_ref()
            .and_then(|r| r.pending.rx.try_recv().ok());
        if let Some(result) = read {
            let Some(read) = self.git.panels[i].read.take() else {
                return;
            };
            match result {
                Ok(snapshot) => self.snapshot_read(ctx, i, snapshot, read.soft, read.pages),
                Err(why) => {
                    let p = &mut self.git.panels[i];
                    p.view.status = Some(Status::error(why.clone()));
                    if p.view.rows().is_empty() {
                        p.view.set_paging(Paging::Error(why));
                    }
                }
            }
        }
        // The stream's pages.
        loop {
            let p = &mut self.git.panels[i];
            let Some(link) = p.stream.as_mut() else {
                break;
            };
            let Ok(page) = link.pages.try_recv() else {
                break;
            };
            link.asked = link.asked.saturating_sub(1);
            if page.generation != link.generation || page.end == PageEnd::Superseded {
                continue;
            }
            let n = p.snapshot.as_ref().map_or(0, |s| s.status.entries.len());
            let rows = link.rows.page(page.rows);
            if std::mem::take(&mut link.first) {
                let first_parent = p.view.filters().first_parent;
                p.view.set_rows(rows, first_parent, n);
            } else {
                p.view.append_rows(rows);
            }
            if page.end != PageEnd::More {
                link.ended = true;
                link.asked = 0;
            }
            p.view.set_paging(paging_of(&page.end, p.shallow));
        }
        // A history search's pages.
        loop {
            let p = &mut self.git.panels[i];
            let Some(search) = p.search.as_mut() else {
                break;
            };
            let Ok(page) = search.link.pages.try_recv() else {
                break;
            };
            let link = &mut search.link;
            link.asked = link.asked.saturating_sub(1);
            if page.end != PageEnd::More {
                link.ended = true;
                link.asked = 0;
            }
            let paging = paging_of(&page.end, false);
            if std::mem::take(&mut link.first) {
                p.view.set_search_results(&search.query, page.rows, paging);
            } else {
                p.view
                    .append_search_results(&search.query, page.rows, paging);
            }
        }
        if self.git.panels[i]
            .search
            .as_ref()
            .is_some_and(|s| self.git.panels[i].view.search() != Some(&s.query))
        {
            // The search was left (Esc back to the graph): its stream goes.
            self.git.panels[i].search = None;
        }
        // Details.
        let p = &mut self.git.panels[i];
        if let Some(pending) = &p.details {
            loop {
                match pending.rx.try_recv() {
                    Ok(details) => {
                        p.last_details = Some(details.clone());
                        p.view.set_details(details);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        p.details = None;
                        break;
                    }
                }
            }
        }
        // A file row's diff or file.
        let revealed = p.reveal.as_ref().and_then(|r| r.rx.try_recv().ok());
        if let Some(revealed) = revealed {
            p.reveal = None;
            self.revealed(ctx, revealed);
        }
        // `select=REV` resolved.
        let p = &mut self.git.panels[i];
        let resolved = p.resolving.as_ref().and_then(|r| r.rx.try_recv().ok());
        if let Some(resolved) = resolved {
            p.resolving = None;
            match resolved {
                Ok(oid) => {
                    p.view.select(RowKey::Commit(oid));
                }
                Err(why) => {
                    p.view.status = Some(Status::error(why));
                    self.git.script = GitScript::default();
                }
            }
        }
    }

    /// The load's refs, HEAD, stashes, status and remotes came: the labels and the foot now; the
    /// rows kept (a soft refresh with the graph unchanged, the uncommitted row put in or taken
    /// out) or a new stream started.
    fn snapshot_read(
        &mut self,
        ctx: &egui::Context,
        i: usize,
        snapshot: Snapshot,
        soft: bool,
        pages: usize,
    ) {
        let p = &mut self.git.panels[i];
        p.view.set_remotes(snapshot.remotes.clone());
        p.view
            .set_refs(snapshot.refs.clone(), head_ref(&snapshot.head));
        p.view.tracking = tracking(&snapshot.status);
        let keep = soft
            && p.stream.is_some()
            && p.snapshot
                .as_ref()
                .is_some_and(|old| old.same_graph(&snapshot));
        let was = p.snapshot.replace(snapshot);
        if !keep {
            // 03 §4.5: a refresh that moved the graph reads as many pages as were loaded.
            let loaded = p.view.rows().len().div_ceil(history::PAGE);
            self.start_stream(ctx, i, pages.max(loaded));
            return;
        }
        // 03 §5: the rows stay; the uncommitted row and its count follow the status.
        let p = &mut self.git.panels[i];
        let Some(now) = p.snapshot.as_ref() else {
            return;
        };
        let n = now.status.entries.len();
        let had = was.as_ref().map_or(0, |w| w.status.entries.len());
        if n != had {
            let mut rows = p.view.rows().to_vec();
            let first_is = matches!(rows.first(), Some(Row::Uncommitted { .. }));
            if n == 0 && first_is {
                rows.remove(0);
            } else if n > 0 && !first_is {
                rows.insert(
                    0,
                    Row::Uncommitted {
                        head: now.head.oid().cloned(),
                    },
                );
            }
            let first_parent = p.view.filters().first_parent;
            p.view.set_rows(rows, first_parent, n);
        }
    }

    /// A new log stream for panel `i` (03 §4.5), reading `pages` pages: the old one ends, and a
    /// hidden panel's parked stream gives way when the host has [`PARKED_STREAMS`] already.
    fn start_stream(&mut self, ctx: &egui::Context, i: usize, pages: usize) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        self.git.panels[i].stream = None;
        self.git.panels[i].stream_dropped = false;
        self.make_room_for_a_stream(i);
        let p = &mut self.git.panels[i];
        let Some(snapshot) = p.snapshot.clone() else {
            return;
        };
        let f = p.view.filters().clone();
        let mut rows = RowStream::new(
            snapshot.head.oid().cloned(),
            !snapshot.status.entries.is_empty(),
            snapshot.stashes.clone(),
            f.stashes,
        );
        let roots = roots_of(&f, &snapshot);
        if roots.is_empty() {
            // 03 §4.1: no id is ever sent — git would take HEAD.
            let n = snapshot.status.entries.len();
            p.view.set_rows(rows.page(Vec::new()), f.first_parent, n);
            let nothing = snapshot.refs.is_empty() && snapshot.head.oid().is_none();
            p.view.set_paging(if nothing {
                Paging::NoCommits
            } else {
                Paging::NoRefsChosen
            });
            return;
        }
        let g = p.generation.bump();
        let handle = Handle::new();
        let link = spawn_stream(
            ctx,
            RunnerGit::new(runner, handle.clone()),
            p.top.clone(),
            history::argv::log(&log_options(&f)),
            roots,
            p.generation.clone(),
            g,
        );
        let mut link = StreamLink {
            generation: g,
            handle,
            ask: link.0,
            pages: link.1,
            asked: 0,
            rows,
            first: true,
            ended: false,
        };
        if link.ask.send(pages).is_ok() {
            link.asked = pages;
        }
        p.stream = Some(link);
        if p.view.rows().is_empty() {
            p.view.set_paging(Paging::Loading);
        }
    }

    /// At most [`PARKED_STREAMS`] streams per host: the least recently shown hidden panel's ends
    /// first (it reads its pages again when it is shown).
    fn make_room_for_a_stream(&mut self, for_panel: usize) {
        loop {
            let live = self
                .git
                .panels
                .iter()
                .map(|p| {
                    usize::from(p.stream.as_ref().is_some_and(|s| !s.ended))
                        + usize::from(p.search.as_ref().is_some_and(|s| !s.link.ended))
                })
                .sum::<usize>();
            if live < PARKED_STREAMS {
                return;
            }
            let victim = self
                .git
                .panels
                .iter()
                .enumerate()
                .filter(|(i, p)| {
                    *i != for_panel
                        && p.hidden_since.is_some()
                        && (p.stream.as_ref().is_some_and(|s| !s.ended) || p.search.is_some())
                })
                .min_by_key(|(_, p)| p.last_shown)
                .map(|(i, _)| i);
            let Some(v) = victim else {
                return;
            };
            let p = &mut self.git.panels[v];
            p.stream = None;
            p.search = None;
            p.stream_dropped = true;
        }
    }

    /// The next page of panel `i`'s stream (03 §4.5); a stream that ended on an error or was
    /// dropped is started again, reading as many pages as were loaded and one more.
    fn more_rows(&mut self, ctx: &egui::Context, i: usize) {
        let p = &mut self.git.panels[i];
        let retry = matches!(p.view.paging(), Paging::Error(_)) || p.stream_dropped;
        match p.stream.as_mut() {
            Some(link) if !link.ended && !retry => {
                if link.asked == 0 && link.ask.send(1).is_ok() {
                    link.asked = 1;
                }
            }
            _ if retry => {
                let pages = p.view.rows().len().div_ceil(history::PAGE) + 1;
                self.start_read(ctx, i, false, pages);
            }
            _ => {}
        }
    }

    /// Esc in the git panel, before anything that arrived is heard (03 §3.4: reads stop on Esc):
    /// what the panel waits for — a page, a history search's page, a load's reads, a file row's
    /// diff or file — is stopped, and a file row's result is dropped unread, so a diff whose git
    /// had already ended opens no viewer. Only with nothing waited for is Esc the panel's chain.
    fn git_stop_on_esc(&mut self, ctx: &egui::Context) {
        if self.ops.holds_keyboard() || self.viewer.is_some() || self.tree.is_some() {
            return;
        }
        let Some(i) = self.git_focused() else {
            return;
        };
        let waiting = git_waiting(&self.git.panels[i]);
        if waiting.is_empty()
            || !ctx.input_mut(|inp| keys::take(inp, Key::Escape, keys::Mods::None))
        {
            return;
        }
        for handle in waiting {
            handle.stop();
        }
        self.git.panels[i].reveal = None;
        self.status = Status::info("stopped");
    }

    /// The git panel's keys (03 §9), through the key table's git context.
    fn git_keys(&mut self, ctx: &egui::Context) {
        if self.ops.holds_keyboard() {
            return;
        }
        let Some(i) = self.git_focused() else {
            return;
        };
        // A key of the owner's: what was said before is old news.
        let pressed = ctx.input(|inp| {
            inp.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key { pressed: true, .. } | egui::Event::Text(_)
                )
            })
        });
        if pressed {
            self.status = Status::default();
        }
        let actions = git_panel::keys(ctx, &mut self.git.panels[i].view);
        self.git_actions(ctx, i, actions);
    }

    /// Carries out what panel `i` asks (S2-D's `GitAction`s).
    fn git_actions(&mut self, ctx: &egui::Context, i: usize, actions: Vec<GitAction>) {
        for action in actions {
            if i >= self.git.panels.len() {
                return;
            }
            match action {
                GitAction::Help => {
                    help::open(ctx, help::for_context(Context::Git, self.mode, &[]));
                }
                GitAction::Details(subject) => self.ask_details(ctx, i, subject),
                GitAction::MoreRows => self.more_rows(ctx, i),
                GitAction::Search(query) => self.start_search(ctx, i, query),
                GitAction::MoreResults => {
                    let p = &mut self.git.panels[i];
                    if p.search.is_none() {
                        // Ended while hidden: run again, as many pages as were shown and one more.
                        if let Some(query) = p.search_dropped.take() {
                            if p.view.search() == Some(&query) {
                                let shown = p.view.search_rows().div_ceil(history::PAGE);
                                self.start_search_pages(ctx, i, query, shown + 1);
                                continue;
                            }
                        }
                    }
                    if let Some(s) = self.git.panels[i].search.as_mut() {
                        let link = &mut s.link;
                        if !link.ended && link.asked == 0 && link.ask.send(1).is_ok() {
                            link.asked = 1;
                        }
                    }
                }
                GitAction::FiltersChanged(_) => {
                    let pages = self.git.panels[i].view.rows().len().div_ceil(history::PAGE);
                    self.start_stream(ctx, i, pages.max(1));
                }
                GitAction::Diff {
                    subject,
                    group,
                    file,
                } => self.ask_reveal(ctx, i, subject, group, file, true),
                GitAction::RevisionFile {
                    subject,
                    group,
                    file,
                } => self.ask_reveal(ctx, i, subject, group, file, false),
                GitAction::ListFolder { subject, dir } => self.list_folder(ctx, i, subject, dir),
                GitAction::Edit(path) => {
                    let top = self.git.panels[i].top.clone();
                    match path {
                        None => self.edit(ctx, top, true),
                        Some(path) => {
                            let abs = format!("{}/{path}", top.trim_end_matches('/'));
                            self.edit(ctx, abs, false)
                        }
                    }
                }
                GitAction::OtherSide => self.git_leave(),
                GitAction::SwitchMode => {
                    // 02 §2: an automatic panel gives the keyboard back first; a pinned one
                    // keeps it (the pin outranks the mode).
                    let pinned = self
                        .git
                        .pin
                        .as_ref()
                        .is_some_and(|p| p.repo == self.git.panels[i].top);
                    if !pinned {
                        self.git_leave();
                    }
                    self.git.wide = false;
                    self.mode = self.mode.other();
                    self.moved_at = None;
                }
                GitAction::Refresh => self.hard_refresh(ctx, i),
                GitAction::Pin => {
                    if self.git.pin.take().is_some() {
                        self.git_leave();
                    } else {
                        self.git.pin = Some(view::Pin {
                            side: 1 - self.active,
                            repo: self.git.panels[i].top.clone(),
                        });
                    }
                    self.moved_at = None;
                }
                GitAction::Unpin => {
                    if self.git.pin.take().is_some() {
                        self.git_leave();
                    } else if self.git.wide {
                        self.git.wide = false;
                    }
                    self.moved_at = None;
                }
                GitAction::Wide => {
                    self.git.wide = !self.git.wide;
                    self.moved_at = None;
                }
            }
        }
    }

    /// Cmd/Ctrl+R in the git panel (03 §5): detection, git version and every read again.
    fn hard_refresh(&mut self, ctx: &egui::Context, i: usize) {
        let top = self.git.panels[i].top.clone();
        let host = self.git.host.clone();
        let folders: Vec<String> = self
            .git
            .detections
            .iter()
            .filter(|(_, d)| matches!(d, Detection::WorkTree { top: t, .. } if *t == top))
            .map(|(f, _)| f.clone())
            .collect();
        self.git.detector.forget_host(&host);
        for folder in folders {
            self.git.detector.forget(&host, &folder);
            self.git.detections.remove(&folder);
            self.git.said.remove(&folder);
        }
        let pages = self.git.panels[i].view.rows().len().div_ceil(history::PAGE);
        self.start_read(ctx, i, false, pages.max(1));
    }

    /// The details of `subject` on a worker (03 §6.1, §6.4); a newer ask stops an older one.
    fn ask_details(&mut self, ctx: &egui::Context, i: usize, subject: Subject) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let inputs = DetailsInputs {
            stashes: p
                .snapshot
                .as_ref()
                .map(|s| s.stashes.clone())
                .unwrap_or_default(),
            status: p
                .snapshot
                .as_ref()
                .map(|s| s.status.clone())
                .unwrap_or_else(|| GitStatus {
                    branch: Default::default(),
                    entries: Vec::new(),
                }),
        };
        let handle = Handle::new();
        let git = RunnerGit::new(runner, handle.clone());
        let (tx, rx) = mpsc::channel();
        let (top, waker) = (p.top.clone(), ctx.clone());
        std::thread::spawn(move || {
            let tell = |d: Details| {
                let _ = tx.send(d);
                waker.request_repaint();
            };
            read_details(&git, &top, &subject, &inputs, &tell);
        });
        p.details = Some(Pending { rx, handle });
    }

    /// → or Enter on an untracked folder's row (03 §6.1): its files, read now, after its row.
    fn list_folder(&mut self, ctx: &egui::Context, i: usize, subject: Subject, dir: String) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let Some(mut details) = p.last_details.clone().filter(|d| d.subject == subject) else {
            return;
        };
        let handle = Handle::new();
        let git = RunnerGit::new(runner, handle.clone());
        let (tx, rx) = mpsc::channel();
        let (top, waker) = (p.top.clone(), ctx.clone());
        std::thread::spawn(move || {
            match history::others(&git, &top, &dir) {
                Ok(files) => {
                    for group in details.groups.iter_mut() {
                        let Some(at) = group.files.iter().position(|f| f.path == dir) else {
                            continue;
                        };
                        let already = group
                            .files
                            .get(at + 1)
                            .is_some_and(|f| f.path.starts_with(dir.as_str()) && f.path != dir);
                        if already {
                            continue;
                        }
                        let rows = files.iter().map(|path| FileRow {
                            letter: '?',
                            path: path.clone(),
                            old_path: None,
                            lines: None,
                            lossy: false,
                        });
                        group.files.splice(at + 1..at + 1, rows);
                    }
                }
                Err(e) => details.error = Some(format!("{dir}: {e}")),
            }
            let _ = tx.send(details);
            waker.request_repaint();
        });
        p.details = Some(Pending { rx, handle });
    }

    /// Enter or Shift+F3 on a file row (03 §6.2, §6.3), on a worker.
    fn ask_reveal(
        &mut self,
        ctx: &egui::Context,
        i: usize,
        subject: Subject,
        group: Group,
        file: FileRow,
        diff: bool,
    ) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let handle = Handle::new();
        let git = RunnerGit::new(runner, handle.clone());
        let (tx, rx) = mpsc::channel();
        let top = p.top.clone();
        let shown_top = show(self.label.as_deref(), &top);
        let (space, cache, waker) = (self.git.space.clone(), self.cache.clone(), ctx.clone());
        let said = format!(
            "{} {}…",
            if diff { "diffing" } else { "reading" },
            file.path
        );
        std::thread::spawn(move || {
            let revealed = reveal(
                &git, &top, &shown_top, &subject, group, &file, diff, &space, &cache,
            );
            let _ = tx.send(revealed);
            waker.request_repaint();
        });
        p.reveal = Some(Pending { rx, handle });
        self.status = Status::info(said);
    }

    /// A file row's diff or file is ready: its viewer, read-only, over both sides (03 §6.2); or
    /// the working file as the list opens it; or why there is nothing.
    fn revealed(&mut self, ctx: &egui::Context, revealed: Revealed) {
        if self.viewer.is_some() || self.opening.is_some() {
            return;
        }
        self.status = Status::default();
        match revealed {
            Revealed::File { file, title, edit } => {
                let name = file_name(&file.to_string_lossy()).to_string();
                let local: Connect =
                    Arc::new(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>));
                self.start_open_with(
                    ctx,
                    file.to_string_lossy().into_owned(),
                    name,
                    local,
                    None,
                    Some(ReadOnly { title, edit }),
                );
            }
            Revealed::Working(abs) => {
                let name = file_name(&abs).to_string();
                let (connect, label) = (self.connect.clone(), self.label.clone());
                self.start_open_with(ctx, abs, name, connect, label, None);
            }
            Revealed::Refused(why) => self.status = Status::error(why),
        }
    }

    /// History search (03 §4.7): `git log` with the query's options and the panel's revisions,
    /// paged like the graph.
    fn start_search(&mut self, ctx: &egui::Context, i: usize, query: git_panel::SearchQuery) {
        self.start_search_pages(ctx, i, query, 1);
    }

    /// [`Browser::start_search`], reading `pages` pages at once.
    fn start_search_pages(
        &mut self,
        ctx: &egui::Context,
        i: usize,
        query: git_panel::SearchQuery,
        pages: usize,
    ) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        self.git.panels[i].search_dropped = None;
        self.git.panels[i].search = None;
        self.make_room_for_a_stream(i);
        let p = &mut self.git.panels[i];
        let Some(snapshot) = p.snapshot.clone() else {
            return;
        };
        let f = p.view.filters().clone();
        let roots = roots_of(&f, &snapshot);
        let q = history::argv::Query {
            message: query.message.clone(),
            author: query.author.clone(),
            text: query.text.clone(),
            path: query.path.clone(),
            since: query.since.clone(),
            until: query.until.clone(),
            case: query.case,
        };
        if roots.is_empty() {
            p.view
                .set_search_results(&query, Vec::new(), Paging::NoRefsChosen);
            return;
        }
        let generation = Generation::new();
        let g = generation.bump();
        let handle = Handle::new();
        let link = spawn_stream(
            ctx,
            RunnerGit::new(runner, handle.clone()),
            p.top.clone(),
            history::argv::search(&log_options(&f), &q),
            roots,
            generation,
            g,
        );
        let mut link = StreamLink {
            generation: g,
            handle,
            ask: link.0,
            pages: link.1,
            asked: 0,
            rows: RowStream::new(None, false, Vec::new(), false),
            first: true,
            ended: false,
        };
        if link.ask.send(pages).is_ok() {
            link.asked = pages;
        }
        p.search = Some(SearchLink { query, link });
    }

    /// `SOPEN_STATE`'s git keys (02 §9), each once its panel is there: pin (the keyboard goes
    /// in), `select=REV` (resolved, then the bar on it once its page came — more pages asked
    /// for until it did), `label=N`, `details=1`.
    fn git_script(&mut self, ctx: &egui::Context) {
        if self.git.script.done() {
            return;
        }
        if self.git.script.pin {
            let folder = self.panels[self.active].dir.clone();
            match self.git.detections.get(&folder) {
                Some(Detection::WorkTree { top, .. }) => {
                    self.git.pin = Some(view::Pin {
                        side: 1 - self.active,
                        repo: top.clone(),
                    });
                    self.git.focus = true;
                    self.git.script.pin = false;
                    self.moved_at = None;
                }
                Some(other) => {
                    let host = self.label.clone().unwrap_or_else(|| "this machine".into());
                    self.status = Status::error(format!(
                        "SOPEN_STATE git=pin: {}",
                        other
                            .reason(&host)
                            .unwrap_or_else(|| "not in a git work tree".into())
                    ));
                    self.git.script = GitScript::default();
                }
                None => {}
            }
            return;
        }
        let Some(i) = self.git.pin.as_ref().and_then(|p| self.git.panel(&p.repo)) else {
            return;
        };
        let loaded = {
            let p = &self.git.panels[i];
            p.read.is_none() && p.stream.as_ref().is_some_and(|s| s.asked == 0)
                || !matches!(p.view.paging(), Paging::Loading)
                    && p.read.is_none()
                    && p.stream.is_none()
        };
        if !loaded {
            return;
        }
        if let Some(rev) = self.git.script.select.take() {
            let Some(runner) = self.git_runner() else {
                return;
            };
            let p = &mut self.git.panels[i];
            let handle = Handle::new();
            let git = RunnerGit::new(runner, handle.clone());
            let (tx, rx) = mpsc::channel();
            let (top, waker) = (p.top.clone(), ctx.clone());
            std::thread::spawn(move || {
                let spec = format!("{rev}^{{commit}}");
                let args = vec!["rev-parse".into(), "-q".into(), "--verify".into(), spec];
                let found = match git.run(&top, &args, b"", history::Bound::Read) {
                    Ok(ran) if ran.code == Some(0) => {
                        Oid::parse(String::from_utf8_lossy(&ran.stdout).trim())
                            .ok_or_else(|| format!("SOPEN_STATE select={rev}: not a commit"))
                    }
                    Ok(_) => Err(format!(
                        "SOPEN_STATE select={rev}: git knows no such commit"
                    )),
                    Err(e) => Err(format!("SOPEN_STATE select={rev}: {e}")),
                };
                let _ = tx.send(found);
                waker.request_repaint();
            });
            p.resolving = Some(Pending { rx, handle });
            return;
        }
        let p = &mut self.git.panels[i];
        if p.resolving.is_some() {
            return;
        }
        if p.view.selecting().is_some() {
            // Not loaded yet: the next page, until it comes or the history ends.
            match p.view.paging() {
                Paging::More(_) => self.more_rows(ctx, i),
                _ => {
                    self.status =
                        Status::error("SOPEN_STATE select: that commit is not in the graph");
                    self.git.script = GitScript::default();
                }
            }
            return;
        }
        if let Some(n) = self.git.script.label.take() {
            p.view.set_label_cursor(n);
        }
        if std::mem::take(&mut self.git.script.details) {
            let actions = p.view.open_details();
            self.git_actions(ctx, i, actions);
        }
    }

    /// The git panel shown on side `side` in `rect` (the keyboard's when `active`), its clicks
    /// carried out; a press on it gives it the keyboard.
    fn git_ui(&mut self, ui: &mut egui::Ui, side: usize, rect: Rect, top: &str) {
        let Some(i) = self.git.panel(top) else {
            return;
        };
        let pressed = ui.input(|inp| {
            inp.pointer.any_pressed()
                && inp
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| rect.contains(pos))
        });
        if pressed && !self.ops.holds_keyboard() && self.tree.is_none() && self.listing.is_empty() {
            if side != 1 - self.active {
                // The other side's list had the keyboard: it is the origin list now.
                self.active = 1 - side;
            }
            self.git.focus = true;
        }
        let active = self.git.focus && side == 1 - self.active && self.viewer.is_none();
        let mut actions = git_panel::draw(ui, rect, &mut self.git.panels[i].view, active);
        if self.ops.holds_keyboard() || self.tree.is_some() {
            actions.clear();
        }
        let ctx = ui.ctx().clone();
        self.git_actions(&ctx, i, actions);
    }
}

/// The handles of what panel `p` waits for on the owner's behalf: a page or a search's page asked
/// and not come, a load's reads, a file row's diff or file. (A details read following the bar is
/// not: the next bar move replaces it.)
fn git_waiting(p: &RepoPanel) -> Vec<Handle> {
    let mut out = Vec::new();
    if let Some(s) = p.stream.as_ref().filter(|s| s.asked > 0) {
        out.push(s.handle.clone());
    }
    if let Some(s) = p.search.as_ref().filter(|s| s.link.asked > 0) {
        out.push(s.link.handle.clone());
    }
    if let Some(r) = &p.read {
        out.push(r.pending.handle.clone());
    }
    if let Some(r) = &p.reveal {
        out.push(r.handle.clone());
    }
    out
}

/// A read-only viewer's title and the file F4 opens (the path now, never the `.revisions` copy).
#[derive(Debug, Clone)]
struct ReadOnly {
    title: String,
    edit: String,
}

/// A log stream on a worker of its own: started with `args` and `roots`, it reads as many pages
/// as each ask says, and ends when the asks' sender goes (git stopped with it).
fn spawn_stream(
    ctx: &egui::Context,
    git: RunnerGit,
    top: String,
    args: Vec<String>,
    roots: Vec<Oid>,
    generation: Generation,
    g: u64,
) -> (mpsc::Sender<usize>, Receiver<Page>) {
    let (ask, asks) = mpsc::channel::<usize>();
    let (tx, pages) = mpsc::channel();
    let waker = ctx.clone();
    std::thread::spawn(move || {
        let mut stream = match LogStream::start_with(&git, &top, args, &roots, &generation, g) {
            Ok(stream) => stream,
            Err(e) => {
                let why = match e {
                    StartError::Run(e) => format!("git log: {e}"),
                    StartError::NoRoots => "no refs chosen".into(),
                    StartError::Superseded => return,
                };
                let _ = tx.send(Page {
                    generation: g,
                    rows: Vec::new(),
                    end: PageEnd::Error(why),
                });
                waker.request_repaint();
                return;
            }
        };
        while let Ok(n) = asks.recv() {
            for _ in 0..n {
                let page = stream.page(history::PAGE);
                let more = page.end == PageEnd::More;
                if tx.send(page).is_err() {
                    return;
                }
                waker.request_repaint();
                if !more {
                    break;
                }
            }
        }
    });
    (ask, pages)
}

#[cfg(test)]
mod tests;
