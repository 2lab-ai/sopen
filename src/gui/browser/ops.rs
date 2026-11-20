//! The browser's side of the file operations (04 §4–§5): selection (Space, Ins, `+ - *`,
//! Cmd/Ctrl+A), the Copy / Move dialog, the inline rename field, the name-in-the-way dialog, the
//! progress line and the report — and drawing helpers for the rename field and the selection
//! colours, which the browser's `panel_ui` (S1-E) calls. The work itself is
//! [`crate::fileops`]'s. S1-D writes it (S6-B the make-folder and delete dialogs).
//!
//! S1-0 scaffold: the hook 02 §7 names. Nothing uses it until the integration (S1-E); the
//! `dead_code` allowance below goes then, as a hand-over declared with it (this file is S1-D's).
#![allow(dead_code)]

/// Where F5 and F6 copy or move to (02 §7, 04 §5.2): the other panel's own folder — even when
/// that side shows a preview or the git panel — which the dialog names and prefills in sopen's
/// command-line form (`host:/abs`, a plain path for this machine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsTarget {
    /// `[user@]host` as the browser prints it (an IPv6 host bracketed); `None` on this machine.
    pub host: Option<String>,
    /// The folder (absolute).
    pub dir: String,
}
