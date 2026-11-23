//! The git panel's reading side (03): what a folder is to git, running the host's own git — on
//! this machine, or on a remote over sopen's ssh master with nothing uploaded (03 §3) — what it
//! prints, parsed, the log stream and the lane layout. The panel itself is
//! `gui::browser::git_panel`; its look is `gui::chrome::git`.
//!
//! - [`model`]: the types every part shares (S2-0 wrote them, S2-B owns them).
//! - [`runner`]: `GitRunner` and its transports `LocalGit`, `SshGit` and, for tests, `ShellGit`
//!   (03 §3; S2-A).
//! - [`detect`]: detection and its cache (03 §2; S2-B).
//! - [`parse`]: the parsers of every read (03 §4.1, §6.1; S2-B).
//! - [`history`]: the log stream and its pages (03 §4.5; S2-B).
//! - [`graph`]: the lane layout (03 §4.3; S2-C).
//! - [`actions`]: every action of 03 §7.1–§7.7 as a pure plan, and the in-progress banner (§8.5;
//!   S3-A).
//! - [`backup`]: backups, manifests, restore and undo (03 §8.2–§8.3; S3-A).
//!
//! S2-0 scaffold: the `mod` lines, the tests' among them, so no unit edits this file for its
//! tests. S3's scaffold added `actions` and `backup`; S4's adds `rebase` (02 §5).

pub mod actions;
pub mod backup;
pub mod detect;
pub mod graph;
pub mod history;
pub mod model;
pub mod parse;
pub mod runner;

/// Each unit's tests, in a file of its own (02 §5): `src/git/tests/<module>.rs`.
#[cfg(test)]
mod tests {
    mod actions;
    mod backup;
    mod detect;
    mod graph;
    mod history;
    mod model;
    mod parse;
    mod runner;
}
