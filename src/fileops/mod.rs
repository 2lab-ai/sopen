//! Rename, copy and move of files and folders, on this machine and on a remote, never overwriting
//! (04 §5): plans and runs over both file layers, [`crate::remote::RemoteFs`] and its
//! [`crate::remote::ops::FileOps`]. S1-C writes the engine; S6-A adds make folder and delete.
//!
//! S1-0 scaffold: no engine yet — only the plain data the engine hands the browser's dialogs
//! (`gui::browser::ops`, S1-D) while it runs and when it ends, so the two are written apart.
//! S1-0 wrote these types and S1-C owns them: S1-C adds fields and variants; renaming or removing
//! one goes through the dispatcher (02 §5, as for the git model's shared types).

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
    /// Failed, with the reason.
    Failed(String),
    /// The operation stopped, was cancelled or failed before it reached this item.
    NotReached,
}

/// One line of the report: an item, by its name, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub outcome: ItemOutcome,
}

/// What an operation did, listed in a dialog when it ends — done, stopped or failed (04 §5.1);
/// a fully successful operation on one item is said on the status row instead.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub items: Vec<Item>,
    /// What is to be heard besides the items: a time or a mode that could not be set, a folder
    /// left partial, a source kept so both copies exist.
    pub notes: Vec<String>,
}
