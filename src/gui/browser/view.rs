//! What each side of the browser shows (02 §2–§3): the mode, and [`resolve`], a pure function of
//! 02 §3.1's inputs (the committed display among them) that the browser asks whenever an input
//! changes. S1-E writes the rules O2, O5 and O7, the 150 ms commit and their table test; S2-F adds
//! O6 and the pin, S5 the tree's O4.
//!
//! S1-0 scaffold: the names, and a [`resolve`] that keeps today's two lists. Nothing calls it
//! until the integration (S1-E), which removes the `dead_code` allowance below.
#![allow(dead_code)]

/// The browser's mode (02 §2). It belongs to the window and is not saved; F9 switches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// The side opposite the active list follows its bar (the default).
    #[default]
    Preview,
    /// Two lists, as the browser shipped with message 12.
    Dual,
}

/// What one side shows (02 §1). S1 has these two; the git panel (S2-F) and the tree (S5) add
/// theirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// The side's own panel.
    List,
    /// The preview of the file under the active list's bar (02 §4).
    Preview,
}

/// 02 §3.1's inputs. S1-E adds the others (`focus`, `bar`, `folder`, `rest`, `shown`,
/// `viewer`, …).
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub mode: Mode,
}

/// What the two sides show, `[left, right]` (02 §3.1). S1-0: both lists, as the browser shows
/// them today.
pub fn resolve(inputs: &Inputs) -> [Content; 2] {
    let _ = inputs;
    [Content::List, Content::List]
}
