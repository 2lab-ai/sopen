//! xtools's editor without a screen (`.prd/12-editor.md`): the model F4 edits a text file through,
//! in the file's own bytes and encoding. The screen, its keys and its layout are `gui`'s (12 §4–§6);
//! what is here runs in plain unit tests (12 §15).
//!
//! - `buffer` (ED-buf, 12 §7.1, §7.3): a piece table over the bytes as read and the bytes typed,
//!   with the line index and the undo steps.
//! - The encode side of typed and pasted text is [`crate::encoding`]'s (ED-enc, 12 §7.2).
//! - `ops` and `arm` (ED-ops): the commands of 12 §5 over the buffer, and the Ctrl+K and `^S`
//!   armings.
//! - `file` (ED-save, 12 §7.4, §9): the whole read with its baseline, Save, its guard and the
//!   journal.
//!
//! Each unit adds its own `mod` line, so nothing here declares a file that does not exist yet
//! (12 §16).

pub mod buffer;
