//! The browser's one key table (02 §5, 04 §3), written by S1-A from 02 §3.2, 03 §9, 04 §2–§3
//! and 05 §1: each row is a key, its modifiers, the contexts it acts in and its [`Command`]. A
//! row matches only when the pressed modifiers are exactly its own (egui's `consume_key` ignores
//! extra Shift and Alt, so the table does not use it), and the events it matches are consumed;
//! `+`, `-`, `*` and the jump letters come from the frame's typed text; while a text field has
//! the typing, no row acts.
//!
//! S1-0 scaffold: the names the other work units build on, and no rows yet. Nothing calls it
//! until the integration (S1-E); the `dead_code` allowance below goes then, as a hand-over
//! declared with it (this file is S1-A's).
#![allow(dead_code)]

use eframe::egui;

/// Where the keyboard is: a row acts only in the contexts it names (04 §3 rule 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// A file list (a panel with the keyboard).
    List,
    /// A viewer covering both sides (01 §3a).
    Viewer,
    /// The git panel (03 §9).
    Git,
    /// The directory tree (05 §1).
    Tree,
    /// A dialog (04 §5).
    Dialog,
}

/// What a key asks the browser to do; `Browser::run` carries it out (02 §7). S1-A writes the
/// variants; until then there are none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {}

/// The command the frame's key events and typed text ask for in `context`, matched with exact
/// modifiers; the events matched are removed from `input` (04 §3 rule 1). S1-0: no rows, so
/// always `None`, and nothing is consumed.
pub fn lookup(input: &mut egui::InputState, context: Context) -> Option<Command> {
    let _ = (input, context);
    None
}
