//! The git panel (03; 02 §3): the graph of every commit and branch of the active folder's
//! repository on a side of the browser, read-only in S2 — the graph (03 §4.2–§4.4), the details
//! pane and its diffs (§6), find and filters (§4.6–§4.7). Its look is
//! [`crate::gui::chrome::git`]'s. S2-D writes it; S3 adds the menu, dialogs, backups, log and
//! banner, S4 the planner (02 §5).
//!
//! S2-0 scaffold: the `mod` lines of S2-D's files.

mod details;
mod filters;
mod find;
mod graph_view;
