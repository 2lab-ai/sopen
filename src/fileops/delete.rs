//! F8 and Shift+F8, delete (04 §5.5): to the Trash on this machine where there is one; otherwise,
//! on a remote (SFTP has no Trash) and with Shift+F8, permanently, bottom-up — `REMOVE` for files
//! and links, `RMDIR` for folders once empty. A link is removed as a link, never its target.
//!
//! What the Trash refuses is not deleted another way: the dialog said where it goes, and the
//! item fails with the reason, nothing of it removed. A folder deleted permanently is the file
//! layer's walk ([`FileOps::remove_tree`]): on this machine through open folder descriptors, so
//! a folder swapped for a link meanwhile is never gone through; over SFTP by paths, each request
//! only while the folder it goes through still resolves where it was listed. What it cannot
//! remove stays, with the folders that hold it, each such entry named in the report. A name
//! that is not UTF-8 is never acted on (01 §3a): it stays.
//!
//! Esc is looked at again after every look at an entry and right before every request that
//! removes something (the Trash's move too): once it is set, no removal starts.

use super::{Control, Item, ItemOutcome, Progress, STOPPED};
use crate::remote::join;
use crate::remote::ops::{FileOps, TreeEnd, TreeWatch};
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

/// The status row's line for the entry `name` of the item.
fn progress(ctl: &mut dyn Control, item: usize, items: usize, name: String) {
    ctl.progress(&Progress {
        verb: "deleting",
        item,
        items,
        name,
        done: 0,
        total: 0,
    });
}

/// What a folder's permanent delete hears from the file layer's walk.
struct Watch<'w> {
    ctl: &'w mut dyn Control,
    cancel: &'w AtomicBool,
    item: usize,
    items: usize,
    lines: &'w mut Vec<Item>,
    /// The item's name: its entries are `name/rel`.
    name: &'w str,
    removed: usize,
    kept: usize,
}

impl TreeWatch for Watch<'_> {
    fn stopped(&mut self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn at(&mut self, rel: &str) {
        let shown = format!("{}/{rel}", self.name);
        progress(&mut *self.ctl, self.item, self.items, shown);
    }

    fn removed(&mut self, _rel: &str) {
        self.removed += 1;
    }

    fn kept(&mut self, rel: &str, why: String) {
        self.kept += 1;
        self.lines.push(Item {
            name: format!("{}/{rel}", self.name),
            outcome: ItemOutcome::Failed(why),
        });
    }
}

impl Deleting<'_> {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// The item `name` of `dir`, into the Trash (`trash`) or permanently: its outcome, and
    /// whether Esc stopped it.
    pub(super) fn item(&mut self, dir: &str, name: &str, trash: bool) -> (ItemOutcome, bool) {
        let nothing = || (stopped("nothing of it was removed"), true);
        progress(&mut *self.ctl, self.item, self.items, name.to_string());
        if self.stopped() {
            return nothing();
        }
        let path = join(dir, name);
        let own = self.fs.lstat_opt(&path);
        if self.stopped() {
            return nothing();
        }
        let own = match own {
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
            if self.cancel.load(Ordering::Relaxed) {
                return nothing();
            }
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
            if self.stopped() {
                return nothing();
            }
            return match self.fs.remove(&path) {
                Ok(()) => (ItemOutcome::Done, false),
                Err(e) => (failed(format!("{e:#}")), false),
            };
        }
        let Deleting {
            fs,
            ctl,
            cancel,
            item,
            items,
            lines,
        } = self;
        let mut watch = Watch {
            ctl: &mut **ctl,
            cancel,
            item: *item,
            items: *items,
            lines,
            name,
            removed: 0,
            kept: 0,
        };
        let end = fs.remove_tree(&path, &mut watch);
        let (removed, kept) = (watch.removed, watch.kept);
        match end {
            TreeEnd::Removed => (ItemOutcome::Done, false),
            TreeEnd::Stopped if removed == 0 => nothing(),
            TreeEnd::Stopped => (
                stopped(&format!(
                    "{name} is partly deleted: {removed} of its entries were removed, the rest \
                     stays"
                )),
                true,
            ),
            TreeEnd::Kept(why) if kept == 0 => (failed(why), false),
            TreeEnd::Kept(_) => (
                failed(format!(
                    "{name} was not deleted whole: {kept} of its entries stay (listed below), \
                     each folder that holds one too"
                )),
                false,
            ),
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
