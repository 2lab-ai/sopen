//! F8 and Shift+F8, delete (04 §5.5): to the Trash on this machine where there is one; otherwise,
//! on a remote (SFTP has no Trash) and with Shift+F8, permanently, bottom-up — `REMOVE` for files
//! and links, `RMDIR` for folders once empty. A link is removed as a link, never its target.
//!
//! What the Trash refuses is not deleted another way: the dialog said where it goes, and the
//! item fails with the reason, nothing of it removed. A permanent delete looks at each entry
//! (`lstat`) right before acting on it, so a link — one to a folder too — is removed as a link
//! and never entered; what it cannot remove stays, with the folders that hold it, each such
//! entry named in the report. A name that is not UTF-8 is never acted on (01 §3a): it stays.
//!
//! The walk goes by path, as every SFTP request does (SFTP has no handle to act on a folder's
//! entries through), and locally the same walk keeps the two file layers one engine. The residual:
//! a folder that another process of the account swaps for a link between its `lstat` and its
//! listing is listed through the link, and what is listed there is removed; a descriptor-based
//! local walk (`openat`, `unlinkat`) would close that on this machine, and is not 04 §5.5's.

use super::{Control, Item, ItemOutcome, Progress, STOPPED};
use crate::remote::join;
use crate::remote::ops::FileOps;
use std::sync::atomic::{AtomicBool, Ordering};

/// One item's delete, over the operation's connection.
pub(super) struct Deleting<'a> {
    pub(super) fs: &'a mut dyn FileOps,
    pub(super) ctl: &'a mut dyn Control,
    pub(super) cancel: &'a AtomicBool,
    /// The item, counted from 1, and how many there are.
    pub(super) item: usize,
    pub(super) items: usize,
    /// The lines of the item's entries that stay (`folder/sub/name`).
    pub(super) lines: &'a mut Vec<Item>,
}

/// How a folder's permanent delete went.
#[derive(Default)]
struct Walked {
    /// Entries removed.
    removed: usize,
    /// Entries (folders included) that stay, each with a line.
    kept: usize,
    /// Esc stopped it.
    stopped: bool,
}

impl Deleting<'_> {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn progress(&mut self, shown: &str) {
        self.ctl.progress(&Progress {
            verb: "deleting",
            item: self.item,
            items: self.items,
            name: shown.to_string(),
            done: 0,
            total: 0,
        });
    }

    fn keep(&mut self, shown: String, why: String, walked: &mut Walked) {
        walked.kept += 1;
        self.lines.push(Item {
            name: shown,
            outcome: ItemOutcome::Failed(why),
        });
    }

    /// The item `name` of `dir`, into the Trash (`trash`) or permanently: its outcome, and
    /// whether Esc stopped it.
    pub(super) fn item(&mut self, dir: &str, name: &str, trash: bool) -> (ItemOutcome, bool) {
        self.progress(name);
        if self.stopped() {
            return (stopped("nothing of it was removed"), true);
        }
        let path = join(dir, name);
        let own = match self.fs.lstat_opt(&path) {
            Ok(Some(own)) => own,
            Ok(None) => return (failed(format!("{name}: no such file or folder")), false),
            Err(e) => return (failed(format!("{e:#}")), false),
        };
        if trash {
            let Some(bin) = self.fs.trash() else {
                return (
                    failed(format!(
                        "there is no Trash here; nothing of {name} was removed"
                    )),
                    false,
                );
            };
            return match bin.put(&path) {
                Ok(_) => (ItemOutcome::Done, false),
                Err(e) => (
                    failed(format!(
                        "cannot move {name} to the Trash ({e:#}); nothing of it was removed"
                    )),
                    false,
                ),
            };
        }
        if !own.is_dir || own.is_symlink {
            // A file, a link (as a link), a fifo, a socket or a device: its name goes.
            return match self.fs.remove(&path) {
                Ok(()) => (ItemOutcome::Done, false),
                Err(e) => (failed(format!("{e:#}")), false),
            };
        }
        let mut walked = Walked::default();
        let gone = self.folder(&path, name, &mut walked, true);
        if walked.stopped {
            let what = match walked.removed {
                0 => "nothing of it was removed".to_string(),
                n => format!(
                    "{name} is partly deleted: {n} of its entries were removed, the rest stays"
                ),
            };
            return (stopped(&what), true);
        }
        match gone {
            Ok(()) => (ItemOutcome::Done, false),
            Err(why) if walked.kept == 0 => (failed(why), false),
            Err(_) => (
                failed(format!(
                    "{name} was not deleted whole: {} of its entries stay (listed below), with \
                     the folders that hold them",
                    walked.kept
                )),
                false,
            ),
        }
    }

    /// The folder `path` (shown as `shown`) and everything inside it, bottom-up. `Err`: it stays
    /// (why, for the item itself when it is `top`; inside, its own line says it).
    fn folder(
        &mut self,
        path: &str,
        shown: &str,
        walked: &mut Walked,
        top: bool,
    ) -> Result<(), String> {
        let entries = match self.fs.list_entries(path) {
            Ok(entries) => entries,
            Err(e) => {
                let why = format!("{e:#}");
                if !top {
                    self.keep(shown.to_string(), why.clone(), walked);
                }
                return Err(why);
            }
        };
        let mut whole = true;
        for entry in entries {
            if self.stopped() {
                walked.stopped = true;
                return Err("stopped".into());
            }
            let child = join(path, &entry.name);
            let child_shown = format!("{shown}/{}", entry.name);
            if entry.lossy {
                let why = "its name is not UTF-8, and sopen never acts on such a name".to_string();
                self.keep(child_shown, why, walked);
                whole = false;
                continue;
            }
            self.progress(&child_shown);
            // What it is now, by its own name: a link is removed, never entered.
            let own = match self.fs.lstat_opt(&child) {
                Ok(Some(own)) => own,
                // Gone meanwhile: nothing to remove.
                Ok(None) => continue,
                Err(e) => {
                    self.keep(child_shown, format!("{e:#}"), walked);
                    whole = false;
                    continue;
                }
            };
            if own.is_dir && !own.is_symlink {
                if self.folder(&child, &child_shown, walked, false).is_err() {
                    if walked.stopped {
                        return Err("stopped".into());
                    }
                    whole = false;
                }
                continue;
            }
            match self.fs.remove(&child) {
                Ok(()) => walked.removed += 1,
                Err(e) => {
                    self.keep(child_shown, format!("{e:#}"), walked);
                    whole = false;
                }
            }
        }
        if !whole {
            return Err(format!("{shown} is not empty"));
        }
        if self.stopped() {
            walked.stopped = true;
            return Err("stopped".into());
        }
        match self.fs.remove_dir(path) {
            Ok(()) => {
                walked.removed += 1;
                Ok(())
            }
            Err(e) => {
                let why = format!("{e:#}");
                if !top {
                    self.keep(shown.to_string(), why.clone(), walked);
                }
                Err(why)
            }
        }
    }
}

fn failed(why: String) -> ItemOutcome {
    ItemOutcome::Failed(why)
}

/// The outcome of the item Esc stopped inside: what is left of it.
fn stopped(what: &str) -> ItemOutcome {
    ItemOutcome::Failed(format!("{STOPPED}: {what}"))
}
