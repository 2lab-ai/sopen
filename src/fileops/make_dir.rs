//! F7, make folder (04 §5.4): one level, in the active panel's folder, never over an existing
//! name — `MKDIR` on a remote, `mkdir` on this machine, with the mode they give (not the 0700 a
//! copied folder is filled at). A name with `/` never gets here: the engine refuses it first.

use super::ItemOutcome;
use crate::remote::join;
use crate::remote::ops::{is_taken, FileOps};

/// F7's one item: `name` made a folder in `dir`. A name that exists — a folder, a file, a link —
/// is an error, nothing changed: the filesystem says so (the create fails), never a comparison
/// of names.
pub(super) fn make(fs: &mut dyn FileOps, dir: &str, name: &str) -> ItemOutcome {
    match fs.make_folder(&join(dir, name)) {
        Ok(()) => ItemOutcome::Done,
        Err(e) if is_taken(&e) => ItemOutcome::Failed(format!("{name} exists")),
        Err(e) => ItemOutcome::Failed(format!("{e:#}")),
    }
}
