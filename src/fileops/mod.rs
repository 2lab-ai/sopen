//! Rename, copy and move of files and folders, on this machine and on a remote, never overwriting
//! (04 §5): plans and runs over both file layers, [`crate::remote::RemoteFs`] and its
//! [`crate::remote::ops::FileOps`]. S1-C writes the engine; S6-A adds make folder and delete.
//!
//! The plain data below is what the engine hands the browser's dialogs (`gui::browser::ops`,
//! S1-D) while it runs and when it ends, so the two are written apart. S1-0 wrote these types
//! and S1-C owns them: S1-C adds fields and variants; renaming or removing one goes through the
//! dispatcher (02 §5, as for the git model's shared types).
//!
//! Running an operation: [`spawn`] a [`Job`] on a worker with its connections ([`Conns`]); it
//! sends [`Event`]s — progress, a name in the way (answered with [`Running::answer`]), and the
//! [`Report`] at the end. [`Running::stop`] is Esc. [`run`] is the same engine on the caller's
//! thread, with a [`Control`] of the caller's own.
//!
//! F7 and F8 (S6) are jobs of the same engine, over one connection (`Hosts::Same`):
//! - F7: `Job { op: Op::MakeDir, src_dir: dir, names: vec![name], dst_dir: dir }`. A name the
//!   dialog may refuse at once is [`valid_name`]'s (a `/` included); the engine refuses it too.
//!   A name that exists ends as [`ItemOutcome::Failed`] (`<name> exists`), nothing made.
//! - F8: the dialog asks [`trash_here`] what a delete in its folder does — [`Deletes::Trash`], or
//!   [`Deletes::Permanent`] with why (a remote: SFTP has no Trash) — and says so; Enter runs
//!   `Job { op: Op::Delete { trash }, src_dir: dir, names, dst_dir: dir }` with `trash` true only
//!   for [`Deletes::Trash`] and not Shift. Every item ends Done or Failed (its reason); inside a
//!   folder deleted permanently, every entry that stays gets a line of its own
//!   (`folder/sub/name`). Esc stops between entries.
//! - A remote folder's F2 and F6 are a plain `SSH_FXP_RENAME` where the session has its second
//!   channel (04 §5.6); a copy to a server without `hardlink@openssh.com` is published the same
//!   way after the server's rename is probed once per connection. Nothing changes for the
//!   caller: the same [`Op::Rename`], [`Op::Move`] and [`Op::Copy`].

mod delete;
mod engine;
mod make_dir;
#[cfg(test)]
mod tests;

pub use engine::{run, valid_name};

use crate::remote::ops::FileOps;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

/// Where a running operation is: the status row's `copying 3/12 · name 45% · 12 MB / 30 MB ·
/// Esc stops` (04 §5.1), shown for more than one item or a file over 8 MiB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// What is being done, as the line starts (`copying`).
    pub verb: &'static str,
    /// The item being worked on, counted from 1, and how many there are.
    pub item: usize,
    pub items: usize,
    /// The name of the file being worked on.
    pub name: String,
    /// Bytes of that file done so far, and its size.
    pub done: u64,
    pub total: u64,
}

/// A name that exists at the target: that item stops and the dialog asks (04 §5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clash {
    /// The item's name, which the target folder already holds.
    pub name: String,
    /// The next free `<stem>-N<ext>`, prefilled (and editable) in the dialog's Rename field.
    pub free: String,
}

/// What the dialog answers to a [`Clash`]. Overwrite is never offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Leave both as they are and go on with the next item.
    Skip,
    /// Put the item under this name instead.
    Rename(String),
    /// Do not do the rest; the items done stay.
    Cancel,
}

/// An [`Answer`], and the dialog's "the same for the rest" (off unless ticked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub answer: Answer,
    pub same_for_rest: bool,
}

/// How one item of an operation ended (04 §5.1 "Reported whole").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemOutcome {
    Done,
    /// Skipped at a [`Clash`], or never acted on (a fifo, a socket, a device).
    Skipped,
    /// Done under this other name (Rename at a [`Clash`]).
    Renamed(String),
    /// Failed, with the reason. Also the item Esc stopped inside (the reason starts with
    /// [`STOPPED`] and says what is left: nothing of an unfinished file is under its target name;
    /// a folder keeps the files already published; a move removed no source, or both copies of
    /// what was not removed yet exist), and a move whose source was kept (the reason says both
    /// copies exist).
    Failed(String),
    /// The operation stopped, was cancelled or failed before it reached this item.
    NotReached,
}

/// How the reason of an item Esc stopped inside begins (`stopped: …`).
pub const STOPPED: &str = "stopped";

/// One line of the report: an item, by its name, and how it ended. Inside a folder, a file that
/// was skipped or failed gets a line of its own, named by its path below the operation's folder
/// (`folder/sub/name`), right after its folder's line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub outcome: ItemOutcome,
}

/// What an operation did, listed in a dialog when it ends — done, stopped or failed (04 §5.1);
/// a fully successful operation on one item is said on the status row instead. An operation
/// refused before anything happened (a folder into itself, an invalid name, a target that is not
/// a folder, a server that cannot publish without replacing, no connection) has every item
/// [`ItemOutcome::NotReached`] and the reason as its first note; one whose worker died (a panic)
/// the same, with that said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub items: Vec<Item>,
    /// What is to be heard besides the items: a time or a mode that could not be set, a folder
    /// left partial, a source kept so both copies exist.
    pub notes: Vec<String>,
}

impl Report {
    /// Every item done (under its name or another) and nothing to note: the status row's case.
    pub fn all_done(&self) -> bool {
        self.notes.is_empty()
            && self
                .items
                .iter()
                .all(|i| matches!(i.outcome, ItemOutcome::Done | ItemOutcome::Renamed(_)))
    }
}

/// What to do (04 §5.2–§5.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Copy,
    Move,
    /// F2: the one item gets this name in its own folder. A name that exists is an error (no
    /// dialog); a change of case or of Unicode form only goes through a free temp name.
    Rename {
        to: String,
    },
    /// F7 (04 §5.4): the one name in `names` is made a folder in `src_dir` — one level (a name
    /// with `/` is refused before anything happens), the default mode, never over anything: a
    /// name that exists is an error (no dialog).
    MakeDir,
    /// F8, Del, ⌘⌫ and Shift+F8, Shift+Del (04 §5.5): `names` in `src_dir` are deleted, after
    /// the dialog asked. `trash`: each goes into this machine's Trash whole — an item the Trash
    /// refuses fails with the reason and nothing of it is removed, and a host without a Trash
    /// (every remote) refuses the operation before anything happens. Without: permanently,
    /// bottom-up (`REMOVE` / `unlink` for files and links, `RMDIR` / `rmdir` for folders once
    /// empty), a link removed as a link, never its target. The dialog sets it — what it said
    /// is what happens ([`trash_here`]).
    Delete {
        trash: bool,
    },
}

/// One operation: `names` (entries of `src_dir`, the selection or the bar's entry) to `dst_dir`
/// (for [`Op::Rename`], [`Op::MakeDir`] and [`Op::Delete`], `src_dir` itself). Folders are
/// absolute paths on their host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub op: Op,
    pub src_dir: String,
    pub names: Vec<String>,
    pub dst_dir: String,
}

/// What F8 does in a folder (04 §5.5), for the delete dialog to say before anything happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deletes {
    /// Into this machine's Trash ([`Op::Delete`] with `trash`).
    Trash,
    /// Permanently — no Trash: why (a remote's `permanent — no Trash on <host>`, or this
    /// machine's `permanent — no Trash here`, with the reason).
    Permanent(String),
}

/// What a delete of entries of the folder `dir` on `fs` does without Shift (04 §5.5): into the
/// Trash where `fs` has one that takes entries of `dir`, else permanently.
pub fn trash_here(fs: &dyn FileOps, dir: &str) -> Deletes {
    match fs.trash() {
        None => Deletes::Permanent("SFTP has no Trash".to_string()),
        Some(trash) => match trash.available(dir) {
            Ok(()) => Deletes::Trash,
            Err(why) => Deletes::Permanent(why),
        },
    }
}

/// The connections an operation runs over, borrowed: one when source and target are on one
/// host (this machine, or one remote) — then a move is a rename where one is possible and a
/// remote copy runs on the server (`copy-data`) — or one per host, the bytes streamed through
/// this machine.
pub enum Hosts<'a> {
    Same(&'a mut dyn FileOps),
    Two {
        src: &'a mut dyn FileOps,
        dst: &'a mut dyn FileOps,
    },
}

/// [`Hosts`], owned by the worker: each operation has connections of its own (04 §5.1).
pub enum Conns {
    Same(Box<dyn FileOps + Send>),
    Two {
        src: Box<dyn FileOps + Send>,
        dst: Box<dyn FileOps + Send>,
    },
}

/// What the engine asks of whoever runs it.
pub trait Control {
    /// Esc: the flag checked between files and between a file's steps.
    fn cancel(&self) -> Arc<AtomicBool>;
    /// Where the operation is.
    fn progress(&mut self, progress: &Progress);
    /// A name in the way: the dialog's answer.
    fn ask(&mut self, clash: &Clash) -> Reply;
}

/// What a running operation sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Progress(Progress),
    /// A name in the way: the worker waits for [`Running::answer`] (or [`Running::stop`]).
    Clash(Clash),
    /// The end; nothing comes after it.
    Done(Report),
}

/// An operation running on its worker.
pub struct Running {
    events: Receiver<Event>,
    replies: Sender<Reply>,
    cancel: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Running {
    /// The next event, if one came.
    pub fn try_event(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// The next event, waiting up to `limit` for it.
    pub fn event_within(&self, limit: Duration) -> Option<Event> {
        self.events.recv_timeout(limit).ok()
    }

    /// Esc: stops after removing the unfinished file's temp (04 §5.1); a pending [`Clash`] is
    /// answered Cancel.
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The dialog's answer to the last [`Event::Clash`].
    pub fn answer(&self, reply: Reply) {
        let _ = self.replies.send(reply);
    }

    /// Whether the worker has ended (its [`Event::Done`] was sent).
    pub fn finished(&self) -> bool {
        self.worker.as_ref().is_none_or(|w| w.is_finished())
    }

    /// Waits for the worker to end (closing the window during an operation waits for it).
    pub fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// How long the worker waits for an answer before it looks at the stop flag again.
const ASK_POLL: Duration = Duration::from_millis(50);

/// A progress event is sent when the item or file changes, when a file ends, and every this
/// many bytes in between.
const PROGRESS_EVERY: u64 = 1024 * 1024;

struct Worker {
    events: Sender<Event>,
    replies: Receiver<Reply>,
    cancel: Arc<AtomicBool>,
    wake: Box<dyn Fn() + Send>,
    last: Option<Progress>,
}

impl Worker {
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
        (self.wake)();
    }
}

impl Control for Worker {
    fn cancel(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    fn progress(&mut self, p: &Progress) {
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
            self.send(Event::Progress(p.clone()));
        }
    }

    fn ask(&mut self, clash: &Clash) -> Reply {
        // An answer left over from an earlier question is not this one's.
        while self.replies.try_recv().is_ok() {}
        self.send(Event::Clash(clash.clone()));
        let cancel = Reply {
            answer: Answer::Cancel,
            same_for_rest: false,
        };
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return cancel;
            }
            match self.replies.recv_timeout(ASK_POLL) {
                Ok(reply) => return reply,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                // Nobody will answer (the window went away).
                Err(mpsc::RecvTimeoutError::Disconnected) => return cancel,
            }
        }
    }
}

/// Runs `job` on a worker thread: `connect` opens the operation's own connections there (a
/// failure refuses it, the reason its note), and `wake` is called after every event (the
/// browser's `request_repaint`).
pub fn spawn(
    job: Job,
    connect: Box<dyn FnOnce() -> Result<Conns> + Send>,
    wake: Box<dyn Fn() + Send>,
) -> Running {
    let (events, rx) = mpsc::channel();
    let (replies, replies_rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut worker = Worker {
        events,
        replies: replies_rx,
        cancel: cancel.clone(),
        wake,
        last: None,
    };
    let names = job.names.clone();
    let handle = std::thread::spawn(move || {
        // A panic still ends with a report, so the dialog never waits for one that cannot come.
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match connect() {
            Ok(mut conns) => {
                let hosts = match &mut conns {
                    Conns::Same(fs) => Hosts::Same(fs.as_mut()),
                    Conns::Two { src, dst } => Hosts::Two {
                        src: src.as_mut(),
                        dst: dst.as_mut(),
                    },
                };
                run(&job, hosts, &mut worker)
            }
            Err(e) => engine::refused(&job.names, format!("cannot connect: {e:#}")),
        }));
        let report = ran.unwrap_or_else(|panic| {
            let what = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "a panic".to_string());
            let mut report = engine::refused(&names, String::new());
            report.notes = vec![format!(
                "the operation ended by an internal error ({what}): what it did is not known — \
                 the panels show what is there"
            )];
            report
        });
        worker.send(Event::Done(report));
    });
    Running {
        events: rx,
        replies,
        cancel,
        worker: Some(handle),
    }
}
