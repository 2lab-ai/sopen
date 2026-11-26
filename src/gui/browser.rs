//! The file browser (spec §3a, owner message 12): a folder opens in sopen's own window, two file
//! panels side by side as in Total Commander, laid out where PC Shell puts things (the program's
//! name on top, the path in the panel's frame, the counts at its foot, the F-keys at the
//! bottom — in [`super::chrome`]'s skin). The lists and the title, status and F-key rows are
//! cells of the window's grid ([`list_cells`], [`super::cells`]; 10 §4), the other panes today's
//! egui at their sides' cells. Enter on a file shows the viewer for its kind in
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

mod git_host;
mod git_panel;
pub(crate) mod keys;
mod list_cells;
pub(crate) mod ops;
#[cfg(test)]
pub(crate) use ops::shown_wrapped;
mod panel;
mod prepare;
mod preview;
pub(crate) mod switch_state;
mod tree;
pub(crate) mod view;
mod view_set;

use super::cells::{self, Grid};
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
use eframe::egui::{self, Key, Modifiers, Pos2, Rect, Sense};
use git_host::*;
pub use git_host::{git_connect, git_listing, GitConnect};
use git_panel::{
    Details, FileGroup, FileRow, GitAction, GitView, Group, HeadRef, Paging, RowKey, Subject,
    Summary, Tracking,
};
use keys::{Command, Context, Step};
pub use panel::Panel;

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
    open_on(start, hooks, |title, browser| {
        super::run_window(title, WINDOW, browser, hooks)
    })
}

/// [`open`] with the browser shown by `run` (its title, the browser) — a window, or the
/// terminal (`super::term`); then the same wait for what is still writing.
pub(crate) fn open_on(
    start: Start,
    hooks: &Hooks,
    run: impl FnOnce(&str, Browser) -> Result<()>,
) -> Result<Outcome> {
    let title = show(start.label.as_deref(), &start.abs);
    let script = match std::env::var("SOPEN_KEYS") {
        Ok(text) => parse_keys(&text).map_err(|e| anyhow::anyhow!("SOPEN_KEYS: {e}"))?,
        Err(_) => Vec::new(),
    };
    let mut browser = Browser::new(start);
    browser.scripted(&hooks.state);
    browser.script = script.into();
    let leftover = browser.leftover.clone();
    let run = run(&title, browser);
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

/// Whether this machine has a Trash that takes entries of the folder it is given (F8, 04 §5.5).
/// Asked on a worker, never on the window's thread: a folder on a slow or stalled mount must not
/// hold the window up.
pub(crate) type TrashCheck = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// S6-A's check of this machine's Trash ([`fileops::trash_here`]): a few `stat`s of the folder,
/// and on macOS Foundation's look-up of the Trash for it.
fn trash_check() -> TrashCheck {
    Arc::new(|dir: &str| {
        matches!(
            fileops::trash_here(&LocalFs::new(), dir),
            fileops::Deletes::Trash
        )
    })
}

/// A Trash check on its worker, for the delete dialog opened with its number.
struct TrashWait {
    check: u64,
    rx: Receiver<bool>,
}

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
            // F7 (04 §5.4): one name, in the folder itself.
            ops::Job::MakeFolder { dir, name } => {
                (fileops::Op::MakeDir, dir.clone(), vec![name], dir)
            }
            // F8 (04 §5.5): what the dialog said is what happens — the Trash only where it
            // said so; the engine fails an item the Trash refuses, never deleting it instead.
            ops::Job::Delete {
                dir,
                names,
                removal,
            } => {
                let trash = removal == ops::Removal::Trash;
                (fileops::Op::Delete { trash }, dir.clone(), names, dir)
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
    /// A key that types, as the keyboard sends it: the key with its physical key, then its typed
    /// text, in one pass (`typed:1`, 08 §12).
    Typed(Key, String),
}

/// `SOPEN_KEYS`, read: steps separated by commas — a key by egui's name (`F5`, `Enter`, `Esc`,
/// `Down`, `Space`, `Tab`, a letter), with `Shift+`, `Alt+` or `Cmd+` / `Ctrl+` before it,
/// `text:…` for typed text, or `typed:1` for a key that types one character as the keyboard
/// sends it — the key and its text together (08 §12: a viewer's `1` chooses view 1 and is not
/// typed). The browser plays them, one per settled window, as the owner's keys (a receipt of a
/// dialog, a selection, a rename, a view).
pub(crate) fn parse_keys(text: &str) -> std::result::Result<Vec<Play>, String> {
    let mut out = Vec::new();
    for step in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(typed) = step.strip_prefix("text:") {
            out.push(Play::Text(typed.to_string()));
            continue;
        }
        if let Some(typed) = step.strip_prefix("typed:") {
            let mut chars = typed.chars();
            let key = match (chars.next(), chars.next()) {
                (Some(c), None) => Key::from_name(&c.to_string()),
                _ => None,
            };
            let key = key.ok_or_else(|| format!("'{step}': typed: takes one key's character"))?;
            out.push(Play::Typed(key, typed.to_string()));
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
    /// How its file is reached: what its views are made over later (08 §6).
    connect: Connect,
    label: Option<String>,
    /// How a page file opens (09 §4–§6), as this window decided when it began.
    pages: prepare::Pages,
}

/// Where an opening's file comes from.
enum Source {
    /// Enter on it in the list: [`prepare::prepare`] on a worker.
    Fresh {
        rx: Receiver<Result<prepare::Opened>>,
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
    /// The file's views (08 §8): `embedded` is the one on screen. A diff or a file at a revision
    /// (03 §6.3): the viewer was told before its first pass, and so is every view made later —
    /// the hex window an image gives way to among them.
    views: view_set::Views,
    /// Whether its text view wraps, as last seen on screen: what the switch's state carries of
    /// it while another view is on screen (10 §7.4).
    text_wrap: Option<bool>,
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
    /// The folder opened (`Start::abs`): a page's root when the page is inside it (09 §3.1).
    opened_on: String,
    /// A page view the engine refused in this window (09 §4): page files open as their source
    /// from then on.
    page_failed: Option<crate::gui::web::Unavailable>,
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
    /// `SOPEN_STATE` `enc=NAME` (08 D34): the encoding the first text viewer opened in this
    /// browser reads in — handed and taken as `at=` is.
    open_enc: Option<crate::encoding::Encoding>,
    /// `SOPEN_STATE` `loading=1` (06 §11): handed to the text viewers this browser opens, as
    /// `at=` is. A viewer on a stream is then shot at its first frame with rows.
    open_loading: bool,
    /// `SOPEN_STATE` `view=N` (08 §8): the first viewer opened in this browser switches to view
    /// N once it is on screen — taken by it, as `at=` is.
    open_view: Option<u8>,
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
    /// How F8 asks whether this machine has a Trash for a folder (tests: a stand-in of their
    /// own), and the check running for the delete dialog that is open.
    trash_check: TrashCheck,
    trash_wait: Option<TrashWait>,
    /// The switch's state this browser was given (10 §7.4), while what it asks for is still
    /// being put back ([`switch_state`]): the browser has not settled until it is.
    given: Option<switch_state::Given>,
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
            let Leftover {
                closing, workers, ..
            } = &mut *leftover;
            viewer.views.close(closing, workers);
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
        // A git action running is waited for and its result printed (02 §6, 03 §3.4); the git
        // panels' reads and streams stop with their handles; the `.revisions` folder goes once
        // the viewers that may read it are done, off the window's thread.
        for p in &mut self.git.panels {
            leftover
                .workers
                .extend(p.running.drain(..).map(git_host::wait_run));
        }
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

/// Whether `p` is on `pane`, a pane's rect on the grid, its right and bottom edges outside it
/// as a cell's are ([`Grid::at`]): a point on the edge two sides share is one side's, the right
/// one's, never both — egui's `Rect::contains` holds both edges (10 §4).
fn on_pane(pane: Rect, p: Pos2) -> bool {
    pane.min.x <= p.x && p.x < pane.max.x && pane.min.y <= p.y && p.y < pane.max.y
}

/// Whether the pointer is over `pane` as [`egui::Ui::rect_contains_pointer`] says (in the clip,
/// on `ui`'s layer), its right and bottom edges outside it ([`on_pane`]).
fn pointer_on_pane(ui: &egui::Ui, pane: Rect) -> bool {
    ui.rect_contains_pointer(pane)
        && ui
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| on_pane(pane, p))
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
        let mut preview = preview::Host::new(
            start.connect.clone(),
            start.label.clone(),
            start.cache.clone(),
        );
        preview.set_opened_on(start.abs.clone());
        let tree_connect = tree_connect(start.label.clone(), start.state_dir.clone());
        let tree_host = start.label.clone().unwrap_or_default();
        let git = GitSide::new(start.git, start.label.as_deref());
        let mut browser = Self {
            title: show(start.label.as_deref(), &start.abs),
            opened_on: start.abs.clone(),
            page_failed: None,
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
            open_enc: None,
            open_loading: false,
            open_view: None,
            script: VecDeque::new(),
            script_gap: 0,
            tree: None,
            tree_connect,
            tree_host,
            git,
            trash_check: trash_check(),
            trash_wait: None,
            given: None,
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
        // A listing a given switch state did not ask for: what it was still putting back goes.
        self.given_overtaken();
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
        let (kept_connect, kept_label) = (connect.clone(), label.clone());
        // 09 §4–§6, on the frame: a page file as its page or its source; its root from the
        // folder opened when that is on the file's host (a revision's is this machine's).
        let opened_on = (label == self.label).then(|| self.opened_on.clone());
        let pages = prepare::Pages::here(ctx, &name, opened_on, self.page_failed.as_ref());
        let kept_pages = pages.clone();
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
            let result = prepare::prepare_paged(&req, in_place, &pages, &stop, &mut progress);
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
            connect: kept_connect,
            label: kept_label,
            pages: kept_pages,
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
            Command::MakeFolder => {
                // The bar goes on the new folder in this panel (04 §5.4), as after a rename.
                self.rename_panel = active;
                let refused = self.ops.open_make_folder(panel, self.label.as_deref());
                self.status = refused.unwrap_or_default();
            }
            Command::Delete => self.delete(ctx, ops::Removal::Trash),
            Command::DeleteNow => self.delete(ctx, ops::Removal::Permanent),
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

    /// F8, Del, ⌘⌫ (`asked` the Trash) or Shift+F8, Shift+Del (permanent): the delete dialog
    /// for the selection or the bar's entry as they are now (04 §5.5). It says where they go
    /// before anything is removed: on a remote, permanently (SFTP has no Trash); with Shift,
    /// permanently; here, to the Trash where this machine has one for the folder, else
    /// permanently. That is asked on a worker ([`TrashCheck`]): the dialog opens at once saying
    /// `checking the Trash…`, Delete off, and the answer fills it in ([`Browser::poll`]).
    fn delete(&mut self, ctx: &egui::Context, asked: ops::Removal) {
        let panel = &self.panels[self.active];
        if self.label.is_some() || asked == ops::Removal::Permanent {
            let refused = self
                .ops
                .open_delete(panel, self.label.as_deref(), asked, false);
            self.status = refused.unwrap_or_default();
            return;
        }
        let check = match self.ops.open_delete_checking(panel) {
            Ok(check) => check,
            Err(refused) => {
                self.status = refused;
                return;
            }
        };
        self.status = Status::default();
        let (tx, rx) = mpsc::channel();
        let (trash_here, dir, waker) = (self.trash_check.clone(), panel.dir.clone(), ctx.clone());
        let spawned = std::thread::Builder::new()
            .name("sopen-trash-check".into())
            .spawn(move || {
                let _ = tx.send(trash_here(&dir));
                waker.request_repaint();
            });
        match spawned {
            Ok(_) => self.trash_wait = Some(TrashWait { check, rx }),
            // No worker: no Trash is known, so the dialog says permanent before anything.
            Err(_) => {
                self.ops.trash_checked(check, false);
            }
        }
    }

    /// The Trash check's answer, once it came: the dialog it was opened for fills in; one that
    /// closed meanwhile, or was replaced by a later F8, takes nothing from it.
    fn poll_trash_check(&mut self) {
        let Some(wait) = &self.trash_wait else {
            return;
        };
        let here = match wait.rx.try_recv() {
            Ok(here) => here,
            Err(TryRecvError::Empty) => return,
            // The check ended without an answer (it panicked): no Trash is known.
            Err(TryRecvError::Disconnected) => false,
        };
        let check = wait.check;
        self.trash_wait = None;
        self.ops.trash_checked(check, here);
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

    /// Results from the workers: listings, a file ready (its viewer shows), F8's Trash check, an
    /// F4 launch; and writes of viewers already gone.
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
        self.poll_trash_check();
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
            // A given state's folder that does not list here: its nearest parent that does.
            Err(e) if self.given_unlisted(ctx, panel, &dir, &e) => return,
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
            self.given_listed(i, &dir);
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
        let (connect, label, pages) = (opening.connect, opening.label, opening.pages);
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
            Ok(result) => {
                let means = view_set::Means {
                    path: abs.clone(),
                    name: name.clone(),
                    title: String::new(),
                    connect,
                    label,
                    in_place: read_only.is_some(),
                };
                self.opened(ctx, result, means, read_only, pages)
            }
            // The preview's preparation, taken over, came to its end: the live preview becomes
            // the viewer — or it is an info card (an error), and the file opens as from the
            // list.
            Err(preview::Ended::Ready(_)) if cancelled => self.status = not_opened(&name, false),
            // Prepared for the other backend (a switch since: 10 T21): opened as here.
            Err(preview::Ended::Ready(p)) if !self.preview.fits(ctx, &name, &p) => {
                self.start_open(ctx, abs, name)
            }
            Err(preview::Ended::Ready(previewed)) => match previewed.show().hand_over() {
                Ok(viewer) => self.handed_over(ctx, viewer, abs, name),
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

    /// The file came back ready: its viewer shows, in this window — read-only, titled and edited
    /// as `read_only` says, for a diff or a file at a revision (03 §6.2–§6.3). `means`: where its
    /// bytes are and how they are reached, for the views made later (08 §6).
    fn opened(
        &mut self,
        ctx: &egui::Context,
        result: Result<prepare::Opened>,
        mut means: view_set::Means,
        read_only: Option<ReadOnly>,
        pages: prepare::Pages,
    ) {
        let opened = match result {
            Ok(opened) => opened,
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
        let abs = means.path.clone();
        let title = match &read_only {
            Some(r) => r.title.clone(),
            None => show(self.label.as_deref(), &abs),
        };
        means.title = title.clone();
        let opened_as = view_set::OpenedAs::of(&opened);
        let page = view_set::PageOpen {
            class: opened.page,
            shown: pages.shown(),
            opened_on: match pages {
                prepare::Pages::Page { opened_on } => opened_on,
                prepare::Pages::Source(_) => None,
            },
        };
        // `at=` and `enc=` are the first text file's: taken by a text viewer, kept past any
        // other.
        let text = match opened.prepared {
            prepare::Prepared::Text { .. } => view_set::TextOpen {
                at: self.open_at.take(),
                loading: self.open_loading,
                enc: self.open_enc.take(),
            },
            _ => view_set::TextOpen {
                loading: self.open_loading,
                ..Default::default()
            },
        };
        let made = view_set::embed(opened.prepared, &self.cache, title, &means.name, text, None);
        let made = match made {
            Ok(made) => made,
            Err(e) => {
                self.status = Status::error(format!("{e:#}"));
                return;
            }
        };
        self.status = Status::default();
        // F4 in a viewer of a revision opens the file's path now, never the cache's copy.
        let (abs, read_only) = match read_only {
            Some(r) => (r.edit, true),
            None => (abs, false),
        };
        let views = view_set::open_paged(ctx, &opened_as, means, self.open_view.take(), page);
        self.view(ctx, made, abs, read_only, views);
    }

    /// What `SOPEN_STATE` asks of the browser (a scripted run): the entry to open or to put
    /// the bar on, the mode, and what the text viewer it opens is handed (`at=`, `loading=1`).
    fn scripted(&mut self, state: &super::State) {
        self.open_first = state.open.clone();
        self.open_at = state.at;
        self.open_enc = state.enc;
        self.open_loading = state.loading;
        self.open_view = state.view;
        self.mode = state.mode;
        self.bar_first = state.bar.clone();
        self.git.script = GitScript {
            pin: state.git_pin,
            select: state.select.clone(),
            details: state.details,
            label: state.label,
            dialog: state.dialog,
        };
    }

    /// A preview handed over (02 §4.4): the live window is the viewer.
    fn handed_over(
        &mut self,
        ctx: &egui::Context,
        viewer: prepare::HandedOver,
        abs: String,
        name: String,
    ) {
        self.status = match viewer.notices.as_slice() {
            [] => Status::default(),
            said => Status::info(said.join(" · ")),
        };
        let means = view_set::Means {
            path: abs.clone(),
            title: show(self.label.as_deref(), &abs),
            name,
            connect: self.connect.clone(),
            label: self.label.clone(),
            in_place: false,
        };
        let opened_as = view_set::OpenedAs {
            kind: viewer.kind,
            // The hand-over keeps no stat: the set reads only whether media is empty (D10),
            // and a preview of empty media is a card, never handed over.
            stat: crate::remote::Stat {
                is_dir: false,
                is_file: true,
                is_symlink: false,
                size: 1,
                mtime: std::time::SystemTime::UNIX_EPOCH,
                mode: 0,
                uid: None,
                gid: None,
                missing: Some("size"),
                sha256: None,
            },
            undecodable: None,
        }
        // The stream and the save session the preview's views share from now on (08 §6).
        .with(viewer.shared);
        let page = view_set::PageOpen {
            class: viewer.page.class,
            shown: viewer.page.shown,
            opened_on: viewer.page.shown.then(|| self.opened_on.clone()),
        };
        let views = view_set::open_paged(ctx, &opened_as, means, self.open_view.take(), page);
        let made = view_set::Made {
            embedded: viewer.embedded,
            hold: viewer.hold,
            switch: None,
        };
        self.view(ctx, made, abs, false, views);
    }

    /// `made` is the viewer on screen from now on, the first of `views`; `read_only` (a diff, a
    /// file at a revision) is told to it here, before its first pass (03 §6.3; S2-E: a flag set
    /// later could let a recovery of an earlier session start), and so is that it is a view of
    /// the viewer (D35: the hex view opens viewing).
    fn view(
        &mut self,
        ctx: &egui::Context,
        made: view_set::Made,
        abs: String,
        read_only: bool,
        views: view_set::Views,
    ) {
        let mut embedded = made.embedded;
        if read_only {
            embedded.window.set_read_only(true);
        }
        embedded.window.set_in_viewer(true);
        // The list gave up the keyboard: the viewer's keys are its own now.
        ctx.memory_mut(|m| m.surrender_focus(list_id()));
        self.exit = Exit::List;
        // One viewer at a time, and never one replaced without its own say (it leaves through
        // `leave` / `close_requested` first): nothing opens while one shows.
        debug_assert!(self.viewer.is_none(), "a viewer was replaced unasked");
        self.viewer = Some(Viewer {
            embedded,
            hold: made.hold,
            abs,
            switch: made.switch,
            views,
            text_wrap: None,
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
            let mut leftover = self.leftover.borrow_mut();
            leftover.closing.push(Closing {
                settle: viewer.embedded.settle,
                hold: viewer.hold,
            });
            let Leftover {
                closing, workers, ..
            } = &mut *leftover;
            viewer.views.close(closing, workers);
        }
        // A viewer closing commits at once (02 §3.1).
        self.moved_at = None;
    }

    /// Where the closed viewer's exit leads: the list as it was, or the window closing.
    fn after_viewer(&mut self, ctx: &egui::Context) {
        // The list's digits and letters are its own again from the next pass (08 §8).
        super::set_view_keys(ctx, None);
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
                    None if self.open_loading || self.open_enc.is_some() => preview::Taken::Nothing,
                    None => self.preview.take(ctx, &path),
                };
                match taken {
                    preview::Taken::Viewer(viewer) => {
                        self.handed_over(ctx, viewer, path, entry.name)
                    }
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
                            connect: self.connect.clone(),
                            label: self.label.clone(),
                            // The preview's own decision goes with what it hands over.
                            pages: prepare::Pages::Source(None),
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
    /// screen when that changed — phase 2 lists around it, phase 3 sizes on it (05 §6.3). Called
    /// only while the tree is open ([`Browser::panels_ui`] lists the side otherwise).
    fn tree_ui(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let Some(t) = self.tree.as_mut() else {
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
        let key = |key, physical_key, pressed, modifiers| egui::Event::Key {
            key,
            physical_key,
            pressed,
            repeat: false,
            modifiers,
        };
        let mut events = match step {
            Play::Text(text) => vec![egui::Event::Text(text)],
            Play::Key(k, modifiers) => [true, false]
                .map(|pressed| key(k, None, pressed, modifiers))
                .to_vec(),
            Play::Typed(k, text) => vec![
                key(k, Some(k), true, Modifiers::NONE),
                egui::Event::Text(text),
                key(k, Some(k), false, Modifiers::NONE),
            ],
        };
        // Played after the shell's input hook: the viewer's view keys are taken here as the
        // hook takes the keyboard's (08 §8).
        super::route_view_keys(ctx, &mut events);
        ctx.input_mut(|i| i.events.extend(events));
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
        // F8's list of encodings, over the view (08 §7.5).
        self.picker_ui(&ctx, ui.max_rect());
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        let ((), asked) = super::host_pass(&ctx, || viewer.embedded.window.ui(ui));
        // 06 §7.3: the image gave way to the hex window (Hex view, a file that does not
        // decode) — on the same stream, in its place, nothing read again: it is the Hex view
        // (08 §8). Unless the owner left in the same pass (its Esc asked the image window to
        // close): then the hex window is not made, and its file — the stream with it — goes;
        // the viewer closes below.
        let given_way = viewer.switch.as_ref().and_then(|s| s.take());
        if let Some((file, notices, failed)) = given_way.filter(|_| !asked) {
            self.views_given_way(&ctx, file, notices, failed);
            self.views_after(&ctx);
            return;
        }
        // 09 §4: the engine refused the page view on screen — the source shows in its place,
        // and page files open as their source in this window from now on.
        if let Some(why) = crate::gui::web::page::take_failure(&ctx).filter(|_| !asked) {
            self.preview.set_page_failed(why.clone());
            self.page_failed = Some(why.clone());
            self.views_page_failed(&ctx, &why);
            self.views_after(&ctx);
            return;
        }
        if asked && self.views_asked(&ctx) {
            // The prompt was up for a switch of views: its answer was the switch's.
            self.views_after(&ctx);
            return;
        }
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
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
        self.views_after(&ctx);
    }

    /// The two sides, side by side: each its list, or — the side opposite the active list, in
    /// preview mode — the preview of the file under the bar ([`view`]). They are the halves of
    /// the area's cell grid, the odd column the right side's, each pane at its side's cells
    /// (10 §4).
    fn panels_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let area = ui.max_rect();
        // The list holds the keyboard: Tab, the arrows and Esc are its keys, not focus moves.
        // A dialog or the rename field holds it instead while it is open.
        let resp = ui.interact(area, list_id(), Sense::click());
        // A git panel's dialog (S3's menu, an action's dialog, …) has the keyboard: its first
        // field takes the typing once nothing has the focus.
        let git_modal = self
            .git_focused()
            .is_some_and(|i| self.git.panels[i].view.dialog_up());
        if git_modal {
            ctx.memory_mut(|m| m.surrender_focus(list_id()));
        } else if !self.ops.holds_keyboard() && ctx.memory(|m| m.focused()) != Some(list_id()) {
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
        let grid = Grid::over(area);
        // Wide (Cmd/Ctrl+Shift+G): the git panel over both sides.
        if let Some([view::Content::Git(a), view::Content::Git(b)]) = self.shown.clone() {
            if a == b {
                self.git_ui(ui, 1 - self.active, grid.rect(grid.all()), &a);
                return;
            }
        }
        for (i, side) in cells::halves(grid.all()).into_iter().enumerate() {
            let rect = grid.rect(side);
            match self.shown.as_ref().map(|s| s[i].clone()) {
                Some(view::Content::Preview(_)) => {
                    self.preview.ui(ui, &grid, side, &self.ops.covered(&grid));
                    // 09 §4: the engine refused the preview's page — the preview shows the
                    // source now, and page files open as their source in this window.
                    if let Some(why) = self.preview.take_refused() {
                        self.status = Status::error(crate::gui::web::page::source_line(&why));
                        self.page_failed = Some(why);
                    }
                }
                Some(view::Content::Tree) if self.tree.is_some() => self.tree_ui(ui, rect),
                Some(view::Content::Git(top)) => self.git_ui(ui, i, rect, &top),
                _ => self.panel_cells(ui, i, &grid, side),
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
                (None, None) => match self.viewer.as_ref().and_then(|v| v.views.status()) {
                    Some(said) => said,
                    None if !self.status.text.is_empty() => self.status.clone(),
                    None => Status::info(VIEWER_HINT),
                },
            };
        }
        if let Some(listing) = self.listing.first() {
            return Status::info(format!("listing {}…", listing.dir));
        }
        if let Some(said) = self.ops.status() {
            return said;
        }
        // The git panel has the keyboard: an action running (03 §3.4), a line of ours (an open
        // refused, …), else its own.
        if let Some(i) = self.git_focused() {
            if let Some(running) = self.git_running_status(i) {
                return running;
            }
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

    /// Nothing is on its way: no switch state still being put back, no listing, no opening, the
    /// display committed and its preview on screen, the viewer ready, no file operation running
    /// unasked (one waiting at its name-in-the-way question has settled: only an answer moves
    /// it).
    fn settled(&self) -> bool {
        self.painted
            && self.given.is_none()
            && self.listing.is_empty()
            && self.opening.is_none()
            && !self.uncommitted
            && match &self.viewer {
                Some(viewer) => viewer.embedded.window.ready() && viewer.views.settled(),
                None => self.preview.settled(),
            }
            // A running operation counts only while it waits at its question — not while a
            // rename's field shows it pending.
            && (!self.ops.busy() || self.ops.waiting().is_some())
            // The delete dialog is settled once it says where things go.
            && !self.ops.checking_trash()
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
        if self.viewer.is_none() {
            // No viewer, no view keys (08 §8).
            super::set_view_keys(&ctx, None);
        }
        // The switch's own pass, before anything of this one: a given state's next step, on
        // the display the last pass committed; the text view's wrap noted.
        self.switch_pass(&ctx);
        self.play(&ctx);
        self.git_stop_on_esc(&ctx);
        self.poll(&ctx);
        // The viewer's views (08 §8): a view key's switch, a view made — before the title row,
        // whose strip shows it.
        self.views_before(&ctx);
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
            (None, None) if in_git => self
                .git_focused()
                .map_or_else(Vec::new, |i| self.git.panels[i].view.fkeys(self.mode)),
            (None, None) => keys::row(Context::List, self.mode),
        };
        // The title, F-key and status rows in cells, a row each (10 §6); over a viewer, its
        // views on the title row (08 §8).
        let strip = self.viewer.as_ref().map(|v| v.views.strip());
        chrome::title_panel_with(ui, "sopen-browser-title", &self.title, strip.as_ref());
        chrome::key_panel_cells(ui, "sopen-browser-keys", &keys);
        self.fkeys_line = super::fkeys_status(&ctx);
        let status = self.status_line();
        chrome::status_panel_cells(ui, "sopen-browser-status", &status);
        // A running file operation is heard on every pass, whatever the window shows — before the
        // panels, so a question or a report that arrives in this pass is a dialog the list
        // blanks its cells under (`Ops::covered`) in this pass too.
        let changed = self.ops.poll(&mut self.panels[self.active]);
        // The area between the title and status rows: the list's grid, where its dialogs stand.
        let mut area = ui.available_rect_before_wrap();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                area = ui.max_rect();
                if self.viewer.is_some() {
                    // The viewer over both sides: at all of the area's cells (10 §4).
                    let grid = Grid::over(ui.max_rect());
                    let rect = grid.rect(grid.all());
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        ui.shrink_clip_rect(rect);
                        self.viewer_ui(ui, esc_was_a_fields);
                    });
                } else {
                    self.panels_ui(ui);
                }
            });
        // The operations' dialogs — a question, the report — are the list's, over the panels.
        if self.viewer.is_none() {
            self.ops.dialogs(&ctx, area, &mut self.panels[self.active]);
        }
        if let Some(changed) = changed {
            self.after_operation(&ctx, changed);
        }
        let working = !self.listing.is_empty()
            || self.opening.is_some()
            || !self.launches.is_empty()
            || self.preview.busy()
            || self.ops.busy()
            || self.trash_wait.is_some()
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

#[cfg(test)]
mod tests;
