//! The git panel's look (03 §4.4): lane colours, label colours and node glyphs, from the skin's
//! palette — every colour the panel draws comes from here (AGENTS.md "One look, one place").
//!
//! The panel paints what the file list paints where it shows what the list shows: its text in the
//! list's text colour, heads and muted rows dimmed as the list dims, marked rows yellow as PC Shell
//! draws selected files (03 §4.2), and on the active bar every text cell in the bar's text colour.
//! The lanes cycle through the bright CGA colours that read on blue and are not the bar's light red
//! (03 §4.4); a label is black on its kind's colour, and the label cursor inverts it. Every glyph is
//! a code page 437 character the VGA font has.

use super::{cga, PALETTE};
use eframe::egui::Color32;

/// A commit's node, in its lane's colour.
pub const COMMIT: char = '■';

/// The checked-out commit's node.
pub const CHECKED_OUT: char = '◙';

/// A stash's node.
pub const STASH: char = '≡';

/// The uncommitted row's node, in the dim colour.
pub const UNCOMMITTED: char = '○';

/// Where lanes beyond the Graph column's third of the panel are cut (03 §4.2).
pub const CUT: char = '»';

/// Before the checked-out branch's name (`►main`).
pub const CURRENT: char = '►';

/// Between a local branch and the remotes that have it on the same commit (`main↔origin`).
pub const BOTH: char = '↔';

/// A file whose diff was opened in this window (03 §6.1).
pub const VIEWED: char = '√';

/// An untracked folder's row in the details pane: → or Enter lists its files (03 §6.1).
pub const FOLDER: char = '►';

/// The foot's ahead and behind (`main ↑2 ↓1 origin/main`, 03 §4.2).
pub const AHEAD: char = '↑';
pub const BEHIND: char = '↓';

/// The lane colours, in turn (03 §4.4): light cyan, yellow, light green, light magenta, white.
pub const LANES: [Color32; 5] = [
    cga::LIGHT_CYAN,
    cga::YELLOW,
    cga::LIGHT_GREEN,
    cga::LIGHT_MAGENTA,
    cga::WHITE,
];

/// The colour of a lane's colour index (the layout's, `git::graph`): the indices cycle.
pub fn lane(colour: u32) -> Color32 {
    LANES[colour as usize % LANES.len()]
}

/// A label's text on its back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Label {
    pub text: Color32,
    pub back: Color32,
}

impl Label {
    /// The label under the label cursor: inverted (03 §4.4).
    pub fn inverted(self) -> Label {
        Label {
            text: self.back,
            back: self.text,
        }
    }
}

/// What each part of the git panel is painted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    /// A row's text: description, date, author, commit; the details pane's values.
    pub text: Color32,
    /// The column heads, a merge commit's text (muted, 03 §4.2), a stash row's text and the
    /// uncommitted row's (03 §4.4), the details pane's field names and group heads.
    pub dim: Color32,
    /// A marked row's text (Space / Ins), under the bar too, as PC Shell's selected files.
    pub marked: Color32,
    /// Every text cell of the row under the active bar (a marked one stays `marked`).
    pub on_bar: Color32,
    /// The uncommitted row's node and its dashed line to HEAD's row (03 §4.4).
    pub uncommitted: Color32,
    /// `»` where lanes are cut.
    pub cut: Color32,
    /// The foot and the find row.
    pub foot: Color32,
    /// A read that failed: the paging line's error, the details pane's.
    pub error: Color32,
    /// The details pane's `√`.
    pub viewed: Color32,
    /// A local branch, the checked-out one too: black on light green.
    pub local: Label,
    /// A remote branch: black on light cyan.
    pub remote: Label,
    /// A tag: black on yellow.
    pub tag: Label,
    /// A detached HEAD: black on white, first.
    pub head: Label,
    /// A stash's `stash@{n}`: black on the dim colour, as its row's text is dim.
    pub stash: Label,
}

/// The git panel's colours, from the skin's palette.
pub const LOOK: Look = Look {
    text: PALETTE.text,
    dim: PALETTE.dim,
    marked: PALETTE.selected,
    on_bar: PALETTE.bar_text,
    uncommitted: PALETTE.dim,
    cut: PALETTE.dim,
    foot: PALETTE.text,
    error: PALETTE.error,
    viewed: cga::LIGHT_GREEN,
    local: Label {
        text: cga::BLACK,
        back: cga::LIGHT_GREEN,
    },
    remote: Label {
        text: cga::BLACK,
        back: cga::LIGHT_CYAN,
    },
    tag: Label {
        text: cga::BLACK,
        back: cga::YELLOW,
    },
    head: Label {
        text: cga::BLACK,
        back: cga::WHITE,
    },
    stash: Label {
        text: cga::BLACK,
        back: PALETTE.dim,
    },
};
