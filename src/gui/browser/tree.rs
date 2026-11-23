//! The directory tree as a side's content (05 §1, §3, §6.1): in place of the active panel's list
//! (02 §3.1 O4), its rows, the bar, the keys of the tree's context ([`super::keys`]) and its F-key
//! row (`1Help` and `10Tree`), drawn in the look of [`crate::gui::chrome::tree`] from the model of
//! [`crate::tree`]. S5-B writes it.
//!
//! S5-0 scaffold: the hook 02 §7 names. Nothing uses it until the integration (S5-C); the
//! `dead_code` allowance below goes then, as a hand-over declared with it (this file is S5-B's).
#![allow(dead_code)]

/// What the tree hands the browser when it is done (02 §7, 05 §1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeOutcome {
    /// Enter on a row: list this folder in the active panel and close the tree — on a compact
    /// chain its last folder, on a link its target, resolved (absolute, on the panel's host).
    GoTo(String),
}
