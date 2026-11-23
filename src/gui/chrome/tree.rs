//! The directory tree's look (05 §3, §6.1): the colours of its rows — a dimmed hidden-folders row,
//! the tags, the moving mark of a row being scanned — and its glyphs, `▼` `►` `→` from code page
//! 437, which the VGA font has. The tree never picks a colour itself (AGENTS.md "One look").
//!
//! The colours are the list's where the tree shows what the list shows: a folder's name in the
//! list's folder colour, a hidden one dimmed as the list dims a dot entry (01 §3a), and on the
//! active bar every cell in the bar's text colour, as the list paints its bar's row — the error
//! colour is the bar's own, so a light red tag would vanish there.

use super::{cga, PALETTE};
use eframe::egui::Color32;

/// An open folder (its rows below it), whether the owner or 05 §5 opened it.
pub const OPEN: char = '▼';

/// A closed folder with subfolders, or one whose subfolders are not known yet.
pub const CLOSED: char = '►';

/// A link to a folder: it never opens in place (05 §3).
pub const LINK: char = '→';

/// The start of a folder's one row for its hidden subfolders, `· N hidden folders` (05 §3).
pub const HIDDEN: char = '·';

/// The moving mark after the size of a row being scanned (05 §6.3, `9.8M+ ·`): one cell, these
/// glyphs in turn, so a `+` still growing reads differently from one stopped at its budget.
pub const SCAN_MARKS: [char; 4] = ['·', '∙', '•', '∙'];

/// How long each glyph of [`SCAN_MARKS`] stays, in seconds.
pub const SCAN_STEP: f64 = 0.25;

/// What each part of a tree row is painted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    /// A folder's glyph and name (a link's and the root's too): the list's folder colour.
    pub name: Color32,
    /// A hidden folder's row and the `· N hidden folders` row, all of it: the list's dim.
    pub hidden: Color32,
    /// The counts and the size.
    pub info: Color32,
    /// The tags in place of a size: `<LINK>`, `<SYS>`, `<NET>`, `<AUTO>`, `<SYSVOL>`.
    pub tag: Color32,
    /// `?`: the folder cannot be read (R2).
    pub unreadable: Color32,
    /// `not answering`: a call that has not returned in 2 s (05 §6.2).
    pub stalled: Color32,
    /// The moving mark of a row being scanned.
    pub mark: Color32,
    /// Every cell of the row under the active bar.
    pub on_bar: Color32,
}

/// The tree's colours, from the skin's palette.
pub const LOOK: Look = Look {
    name: PALETTE.folder,
    hidden: PALETTE.dim,
    info: PALETTE.text,
    tag: PALETTE.dim,
    unreadable: PALETTE.error,
    stalled: PALETTE.warn,
    mark: cga::WHITE,
    on_bar: PALETTE.bar_text,
};
