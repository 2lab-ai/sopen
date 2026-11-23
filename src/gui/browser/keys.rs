//! The browser's one key table (02 §5, 04 §3), written by S1-A from 02 §3.2, 03 §9, 04 §2–§3
//! and 05 §1: each row is a key, its modifiers, the context it acts in and its [`Command`]. A
//! row matches only when the pressed modifiers are exactly its own (egui's `consume_key` ignores
//! extra Shift and Alt, so the table does not use it), and the event it matches is consumed;
//! `+`, `-`, `*` and the jump letters come from the frame's typed text, an input method's
//! committed characters included (a Korean name can be jumped to; what is still being composed
//! never acts).
//!
//! - **Every context's rows are here**, each marked with the slice that lands it (02 §8.1).
//!   [`lookup`], [`row`] and the help screen use only the rows of the slices merged so far
//!   ([`LANDED_SLICES`], the set of landed slices; a slice's integration WU adds its number): a
//!   key whose feature has not landed does nothing and its F-key slot is empty (02 §7). A later
//!   slice changes a row in its scaffold step, as a declared hand-over.
//! - **Text fields own their typing** (04 §3 rule 4): [`lookup`] cannot see the keyboard focus,
//!   so while a field has the typing (the inline rename, a dialog's path, find) the caller asks
//!   with [`Context::Dialog`], whose only rows are Enter and Esc.
//! - **Cmd/Ctrl+1…9, 0 and a click on an F-key slot** are not rows: the shell turns them into the
//!   plain F-key before any window's pass (`gui::FKeyInput`, 04 §6.1), so the F-key's row acts.
//!
//! The browser's passes call it from the integration (S1-E) on: `Browser::list_keys` and
//! `viewer_ui` loop [`lookup`], `Browser::run` carries the command out, the F-key row is [`row`].

use super::view::Mode;
use crate::gui::chrome::{self, FKey};
use eframe::egui::{self, Event, Key, Modifiers};

/// The set of landed slices (02 §8.1: 1 = S1 … 7 = S7); a slice's integration WU adds its
/// number. Rows of a slice not in it do nothing yet. A set, not the highest slice: slices land in
/// any order their "Needs first" allows (02 §8.1 "Schedule"), so S5 landed before S2–S4.
pub const LANDED_SLICES: Slices = Slices::of(&[1, 2, 5]);

/// Every slice, 1 to 7: every row of the table.
#[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
pub const ALL_SLICES: Slices = Slices::of(&[1, 2, 3, 4, 5, 6, 7]);

/// A set of slices by number (1 to 7), one bit each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slices(u8);

impl Slices {
    /// The set of these slice numbers, each 1 to 7 (in a `const`, a number from 8 on does not
    /// compile).
    pub const fn of(slices: &[u8]) -> Slices {
        let mut bits: u8 = 0;
        let mut i = 0;
        while i < slices.len() {
            bits |= 1u8 << slices[i];
            i += 1;
        }
        Slices(bits)
    }

    /// Whether `slice` is in the set (a number outside 1 to 7 never is).
    pub fn has(self, slice: u8) -> bool {
        slice != 0
            && 1u8
                .checked_shl(slice.into())
                .is_some_and(|bit| self.0 & bit != 0)
    }
}

/// Where the keyboard is: a row acts only in the context it names (04 §3 rule 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Context {
    /// A file list (a panel with the keyboard).
    List,
    /// A viewer covering both sides (01 §3a).
    Viewer,
    /// The git panel (03 §9).
    Git,
    /// The directory tree (05 §1).
    Tree,
    /// A dialog, or any text field with the typing (04 §3 rule 4): only Enter and Esc act.
    Dialog,
}

impl Context {
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub const ALL: [Context; 5] = [
        Context::List,
        Context::Viewer,
        Context::Git,
        Context::Tree,
        Context::Dialog,
    ];

    /// How the help screen's title names it.
    pub fn name(self) -> &'static str {
        match self {
            Context::List => "the file list",
            Context::Viewer => "the viewer",
            Context::Git => "the git panel",
            Context::Tree => "the directory tree",
            Context::Dialog => "a dialog",
        }
    }
}

/// A step of the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

/// A side of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

/// What a key asks the browser to do; `Browser::run` carries it out (02 §7). Each names its keys
/// and contexts; [`TABLE`] is the authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    /// F1 (list, viewer, git panel, tree): the key help of where the keyboard is (`gui::help`).
    Help,
    // The list (04 §2, §3.1; 02 §3.2).
    /// F2, Shift+F6: rename the bar's entry in place (04 §5.3).
    Rename,
    /// F3: the file under the bar in its viewer; a folder stays (01 §3a).
    View,
    /// F4 (list, viewer, git panel): VS Code or Zed (01 A2; 03 §9.1).
    Edit,
    /// F5: copy to the other panel's folder (04 §5.2).
    Copy,
    /// F6: move to the other panel's folder (04 §5.2).
    Move,
    /// F7: make a folder (04 §5.4; S6).
    MakeFolder,
    /// F8, Del, ⌘⌫ (macOS): delete, to the Trash where there is one (04 §5.5; S6).
    Delete,
    /// Shift+F8, Shift+Del: delete without the Trash (04 §5.5; S6).
    DeleteNow,
    /// F9 (list, git panel): preview mode ↔ dual mode (02 §2).
    SwitchMode,
    /// F10, Alt+F10: the directory tree on this side (05 §1; S5).
    Tree,
    /// Tab, Shift+Tab (list, git panel): the keyboard to the other side (02 §3.2).
    OtherSide,
    /// Enter: enter a folder, or open a file's viewer (01 §3a).
    Open,
    /// Backspace, Cmd/Ctrl+PgUp: the parent folder (01 §3a, 04 §3.1).
    Up,
    /// ↑ ↓ PgUp PgDn Home End (list, git panel, tree): the bar.
    Bar(Step),
    /// Space, Ins: select or unselect the bar's entry, then step down (04 §4).
    Select,
    /// `+` (typed): select a group by a pattern (04 §4).
    SelectGroup,
    /// `-` (typed): unselect a group by a pattern (04 §4).
    UnselectGroup,
    /// `*` (typed): invert the selection (04 §4).
    InvertSelection,
    /// Cmd/Ctrl+A: select all (04 §3.1).
    SelectAll,
    /// Cmd/Ctrl+R: the list read again (04 §3.1), the git panel's hard refresh (03 §5), the
    /// tree's row listed again (05 §1).
    Reread,
    /// Cmd/Ctrl+U: swap the panels (04 §3.1).
    SwapPanels,
    /// Cmd/Ctrl+← / →: the left / right panel to the active panel's folder (04 §3.1).
    SameFolder(Side),
    /// Cmd/Ctrl+G (list, git panel): pin the git panel; again: unpin (02 §3.2; S2).
    Pin,
    /// A typed letter (list, tree): the bar to the next name starting with it.
    Jump(char),
    /// Esc in the list: stop a fetch or a file operation (02 §3.2; from S2, else unpin).
    Stop,
    // The viewer (01 §3a).
    /// F3, Esc: back to the list (through the viewer's own `close_requested`).
    CloseViewer,
    // The git panel (03 §9; S2–S4).
    /// F2: `reword` on a commit, `rename` on a local branch's label.
    Reword,
    /// F3, Enter: the details of the row (with one marked row: compare).
    Details,
    /// F5: `pick`; on a stash: `stash-apply`.
    Pick,
    /// F6: `rebase` onto it.
    Rebase,
    /// F7: `branch` here; on the uncommitted row: `commit`.
    Branch,
    /// F8: `drop`, `delete-branch`, `delete-tag`, `reset-changes`, `discard`, `stash-drop`.
    Remove,
    /// F10, Shift+F10: the menu of the object under the bar.
    Menu,
    /// Shift+F3: a file at this revision (03 §6.3).
    RevisionFile,
    /// ←: the label cursor to the previous label.
    LabelPrev,
    /// →: the label cursor to the next label.
    LabelNext,
    /// Space, Ins: mark or unmark the row and step down (on the uncommitted row's files: stage).
    Mark,
    /// Esc: the innermost open thing closes; with nothing open, the pin goes.
    Back,
    /// A typed letter: Find, starting with it (03 §4.7).
    FindWith(char),
    /// Cmd/Ctrl+F: Find.
    Find,
    /// Cmd/Ctrl+Shift+F: history search.
    HistorySearch,
    /// Cmd/Ctrl+Shift+R: fetch all (03 §7.7).
    FetchAll,
    /// Cmd/Ctrl+Z: undo the newest backup group (03 §8.3).
    Undo,
    /// Cmd/Ctrl+B: the backups list.
    Backups,
    /// Cmd/Ctrl+L: the command log.
    Log,
    /// Cmd/Ctrl+E: the planner from the bar's commit (03 §7.8.2).
    Planner,
    /// Cmd/Ctrl+J: the bar to HEAD's row.
    ToHead,
    /// Cmd/Ctrl+Shift+G: the git panel over both sides, and back.
    Wide,
    /// Cmd/Ctrl+K: the filters (03 §4.6, §9.2; the F10 menu offers them too from S3).
    Filters,
    // The directory tree (05 §1; S5).
    /// F10, Alt+F10, Esc: close the tree, the panel as it was.
    CloseTree,
    /// Enter: list that folder in the active panel.
    GoTo,
    /// →: open the row's folder.
    Expand,
    /// ←: close the row's folder; on a closed row: the bar to its parent.
    Collapse,
    /// Space: size the row now.
    SizeNow,
    // A dialog, or a text field with the typing (04 §5.1).
    /// Enter: go.
    Accept,
    /// Esc: cancel.
    Cancel,
}

impl Command {
    /// The command of a letter row, with the letter typed.
    fn with_char(self, c: char) -> Command {
        match self {
            Command::Jump(_) => Command::Jump(c),
            Command::FindWith(_) => Command::FindWith(c),
            other => other,
        }
    }
}

/// The modifiers a row asks for, exactly: no more, no fewer (04 §3 rule 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mods {
    None,
    Shift,
    Alt,
    /// egui's COMMAND: ⌘ on macOS, Ctrl elsewhere (04 §3 rule 3).
    Command,
    CommandShift,
    /// ⌘ only, so macOS only (⌘⌫).
    MacCmd,
}

impl Mods {
    /// Whether `pressed` is exactly these modifiers. egui reports ⌘ as `mac_cmd` + `command`,
    /// Ctrl elsewhere as `ctrl` + `command` (and the test kit's Cmd/Ctrl as `command` alone); a
    /// Mac's ⌃ is `ctrl` alone, and ⌘ with ⌃ is one key more than Cmd/Ctrl.
    pub fn matches(self, pressed: Modifiers) -> bool {
        let m = pressed;
        let any_command = m.command || m.mac_cmd || m.ctrl;
        let command_only = m.command && !(m.mac_cmd && m.ctrl);
        match self {
            Mods::None => !m.alt && !m.shift && !any_command,
            Mods::Shift => m.shift && !m.alt && !any_command,
            Mods::Alt => m.alt && !m.shift && !any_command,
            Mods::Command => command_only && !m.alt && !m.shift,
            Mods::CommandShift => command_only && m.shift && !m.alt,
            Mods::MacCmd => m.mac_cmd && !m.ctrl && !m.alt && !m.shift,
        }
    }

    /// Whether a row with these modifiers can be pressed on this machine (⌘ is macOS's).
    pub fn here(self) -> bool {
        self != Mods::MacCmd || cfg!(target_os = "macos")
    }

    /// How the help screen writes them before the key: `Shift+`, `Cmd+` / `Ctrl+`.
    pub fn prefix(self) -> String {
        let command = command_name();
        match self {
            Mods::None => String::new(),
            Mods::Shift => "Shift+".to_string(),
            Mods::Alt => "Alt+".to_string(),
            Mods::Command | Mods::MacCmd => format!("{command}+"),
            Mods::CommandShift => format!("{command}+Shift+"),
        }
    }
}

/// The platform's name of egui's COMMAND: `Cmd` on macOS, `Ctrl` elsewhere.
pub fn command_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    }
}

/// What makes a row act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Trigger {
    /// A key pressed (or repeating) with exactly these modifiers.
    Key(Key, Mods),
    /// One character of the frame's typed text, whatever the layout needed to type it (04 §3
    /// rule 2).
    Char(char),
    /// Any other typed character that is not whitespace (the jump letters).
    Letter,
}

impl Trigger {
    /// How the help screen writes it: `Shift+F6`, `Cmd+A`, `+`, `a letter`.
    pub fn name(self) -> String {
        match self {
            Trigger::Key(key, mods) => format!("{}{}", mods.prefix(), key_name(key)),
            Trigger::Char(c) => c.to_string(),
            Trigger::Letter => "a letter".to_string(),
        }
    }
}

/// A key's name on the help screen.
fn key_name(key: Key) -> &'static str {
    match key {
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        Key::PageUp => "PgUp",
        Key::PageDown => "PgDn",
        Key::Escape => "Esc",
        Key::Insert => "Ins",
        Key::Delete => "Del",
        other => other.name(),
    }
}

/// One binding of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub context: Context,
    pub trigger: Trigger,
    /// For [`Trigger::Letter`] the letter typed replaces the placeholder (`Jump(' ')`).
    pub command: Command,
    /// The F-key row's label: plain F1…F10 only, at most six characters (PC Shell's); empty
    /// elsewhere. F9's is [`switch_label`]'s, by the mode.
    pub label: &'static str,
    /// What the help screen says it does; a context's rows with the same words are one line.
    pub what: &'static str,
    /// The slice that lands it (02 §8.1).
    pub slice: u8,
}

const fn key(
    context: Context,
    key: Key,
    mods: Mods,
    command: Command,
    label: &'static str,
    what: &'static str,
    slice: u8,
) -> Row {
    Row {
        context,
        trigger: Trigger::Key(key, mods),
        command,
        label,
        what,
        slice,
    }
}

const fn text(context: Context, c: char, command: Command, what: &'static str, slice: u8) -> Row {
    Row {
        context,
        trigger: Trigger::Char(c),
        command,
        label: "",
        what,
        slice,
    }
}

const fn letter(context: Context, command: Command, what: &'static str, slice: u8) -> Row {
    Row {
        context,
        trigger: Trigger::Letter,
        command,
        label: "",
        what,
        slice,
    }
}

// Words shared by several rows of one context (one help line each).
const HELP: &str = "This screen";
const BAR: &str = "Move the bar";
const RENAME: &str = "Rename the entry under the bar, in place";
const UP: &str = "The parent folder";
const SELECT: &str = "Select or unselect the entry under the bar, then step down";
const DELETE: &str = "Delete the selection or the entry (asks; locally to the Trash)";
const DELETE_NOW: &str = "Delete without the Trash (asks)";
const TREE: &str = "The directory tree in place of this list";
const SWITCH: &str = "Switch the mode, preview or two lists (the label names the next)";
const OTHER_SIDE: &str = "The keyboard to the other side";
const CLOSE_VIEWER: &str = "Back to the list";
const DETAILS: &str = "The row's details (with one marked row: compare the two)";
const MENU: &str = "The menu of what is under the bar";
const MARK: &str = "Mark or unmark the row, then step down (files: stage, unstage)";
const BACK_TO_LIST: &str = "Back to the list";
const CLOSE_TREE: &str = "Close the tree, the list as it was";

use Command as C;
use Context::{Dialog, Git, List, Tree as TreeSide, Viewer};
use Key as K;
use Mods::{Alt, Command as Cmd, CommandShift as CmdShift, MacCmd, None as Plain, Shift};
use Step::{Down, End, Home, PageDown, PageUp, Up};

/// Every context's rows, landed or not, in the order the help screen lists them. Modifier
/// variants sit before their plain key (03 §9), though with exact matching the order decides
/// nothing.
#[rustfmt::skip]
pub const TABLE: &[Row] = &[
    // The file list (04 §2, §3.1; 02 §3.2).
    key(List, K::F1, Plain, C::Help, "Help", HELP, 1),
    key(List, K::F6, Shift, C::Rename, "", RENAME, 1),
    key(List, K::F2, Plain, C::Rename, "Rename", RENAME, 1),
    key(List, K::F3, Plain, C::View, "View", "The file under the bar in its viewer", 1),
    key(List, K::F4, Plain, C::Edit, "Edit", "The entry under the bar in VS Code or Zed", 1),
    key(List, K::F5, Plain, C::Copy, "Copy", "Copy to the other panel's folder (asks first)", 1),
    key(List, K::F6, Plain, C::Move, "Move", "Move to the other panel's folder (asks first)", 1),
    key(List, K::F7, Plain, C::MakeFolder, "MkDir", "Make a folder (asks its name)", 6),
    key(List, K::F8, Shift, C::DeleteNow, "", DELETE_NOW, 6),
    key(List, K::Delete, Shift, C::DeleteNow, "", DELETE_NOW, 6),
    key(List, K::F8, Plain, C::Delete, "Delete", DELETE, 6),
    key(List, K::Delete, Plain, C::Delete, "", DELETE, 6),
    key(List, K::Backspace, MacCmd, C::Delete, "", DELETE, 6),
    key(List, K::F9, Plain, C::SwitchMode, "Dual", SWITCH, 1),
    key(List, K::F10, Alt, C::Tree, "", TREE, 5),
    key(List, K::F10, Plain, C::Tree, "Tree", TREE, 5),
    key(List, K::Enter, Plain, C::Open, "", "Enter the folder, or open the file's viewer", 1),
    key(List, K::PageUp, Cmd, C::Up, "", UP, 1),
    key(List, K::Backspace, Plain, C::Up, "", UP, 1),
    key(List, K::ArrowUp, Plain, C::Bar(Up), "", BAR, 1),
    key(List, K::ArrowDown, Plain, C::Bar(Down), "", BAR, 1),
    key(List, K::PageUp, Plain, C::Bar(PageUp), "", BAR, 1),
    key(List, K::PageDown, Plain, C::Bar(PageDown), "", BAR, 1),
    key(List, K::Home, Plain, C::Bar(Home), "", BAR, 1),
    key(List, K::End, Plain, C::Bar(End), "", BAR, 1),
    key(List, K::Tab, Shift, C::OtherSide, "", OTHER_SIDE, 1),
    key(List, K::Tab, Plain, C::OtherSide, "", OTHER_SIDE, 1),
    key(List, K::Space, Plain, C::Select, "", SELECT, 1),
    key(List, K::Insert, Plain, C::Select, "", SELECT, 1),
    text(List, '+', C::SelectGroup, "Select the entries matching a pattern", 1),
    text(List, '-', C::UnselectGroup, "Unselect the entries matching a pattern", 1),
    text(List, '*', C::InvertSelection, "Invert the selection", 1),
    key(List, K::A, Cmd, C::SelectAll, "", "Select all", 1),
    key(List, K::R, Cmd, C::Reread, "", "Read the folder again", 1),
    key(List, K::U, Cmd, C::SwapPanels, "", "Swap the panels", 1),
    key(List, K::ArrowLeft, Cmd, C::SameFolder(Side::Left), "", "The left panel to this folder", 1),
    key(List, K::ArrowRight, Cmd, C::SameFolder(Side::Right), "", "The right panel to this folder", 1),
    key(List, K::G, Cmd, C::Pin, "", "Pin the git panel to the other side; again: unpin", 2),
    letter(List, C::Jump(' '), "The next name starting with it", 1),
    key(List, K::Escape, Plain, C::Stop, "", "Stop a fetch or a file operation; else unpin the git panel", 1),
    // A viewer over both sides (01 §3a); its own keys come with its F-key slots.
    key(Viewer, K::F1, Plain, C::Help, "Help", HELP, 1),
    key(Viewer, K::F3, Plain, C::CloseViewer, "Exit", CLOSE_VIEWER, 1),
    key(Viewer, K::Escape, Plain, C::CloseViewer, "", CLOSE_VIEWER, 1),
    key(Viewer, K::F4, Plain, C::Edit, "Edit", "The file in VS Code or Zed", 1),
    // The git panel (03 §9.1, §9.2).
    key(Git, K::F1, Plain, C::Help, "Help", HELP, 2),
    key(Git, K::F2, Plain, C::Reword, "Reword", "Reword the commit; on a branch's label: rename it", 3),
    key(Git, K::F3, Shift, C::RevisionFile, "", "The file at this revision", 2),
    key(Git, K::F3, Plain, C::Details, "View", DETAILS, 2),
    key(Git, K::Enter, Plain, C::Details, "", DETAILS, 2),
    key(Git, K::F4, Plain, C::Edit, "Edit", "The repository (uncommitted row: the file) in the editor", 2),
    key(Git, K::F5, Plain, C::Pick, "Pick", "Cherry-pick it or the marked ones; a stash: apply it", 3),
    key(Git, K::F6, Plain, C::Rebase, "Rebase", "Rebase the current branch onto it", 3),
    key(Git, K::F7, Plain, C::Branch, "Branch", "A new branch here (uncommitted row: commit)", 3),
    key(Git, K::F8, Plain, C::Remove, "Delete", "Drop, delete or discard what is under the bar (asks)", 3),
    key(Git, K::F9, Plain, C::SwitchMode, "Dual", SWITCH, 2),
    key(Git, K::F10, Shift, C::Menu, "", MENU, 3),
    key(Git, K::F10, Plain, C::Menu, "Menu", MENU, 3),
    key(Git, K::ArrowUp, Plain, C::Bar(Up), "", BAR, 2),
    key(Git, K::ArrowDown, Plain, C::Bar(Down), "", BAR, 2),
    key(Git, K::PageUp, Plain, C::Bar(PageUp), "", BAR, 2),
    key(Git, K::PageDown, Plain, C::Bar(PageDown), "", BAR, 2),
    key(Git, K::Home, Plain, C::Bar(Home), "", BAR, 2),
    key(Git, K::End, Plain, C::Bar(End), "", BAR, 2),
    key(Git, K::ArrowLeft, Plain, C::LabelPrev, "", "The label cursor to the previous label", 2),
    key(Git, K::ArrowRight, Plain, C::LabelNext, "", "The label cursor to the next label", 2),
    key(Git, K::Space, Plain, C::Mark, "", MARK, 2),
    key(Git, K::Insert, Plain, C::Mark, "", MARK, 2),
    key(Git, K::Tab, Shift, C::OtherSide, "", BACK_TO_LIST, 2),
    key(Git, K::Tab, Plain, C::OtherSide, "", BACK_TO_LIST, 2),
    key(Git, K::Escape, Plain, C::Back, "", "Close the innermost open thing; with none, unpin", 2),
    letter(Git, C::FindWith(' '), "Find, starting with it", 2),
    key(Git, K::F, CmdShift, C::HistorySearch, "", "History search", 2),
    key(Git, K::F, Cmd, C::Find, "", "Find", 2),
    key(Git, K::R, CmdShift, C::FetchAll, "", "Fetch all", 3),
    key(Git, K::R, Cmd, C::Reread, "", "Refresh (hard)", 2),
    key(Git, K::Z, Cmd, C::Undo, "", "Undo the newest backup group", 3),
    key(Git, K::B, Cmd, C::Backups, "", "The backups", 3),
    key(Git, K::L, Cmd, C::Log, "", "The command log", 3),
    key(Git, K::E, Cmd, C::Planner, "", "Plan a history edit from the commit under the bar", 4),
    key(Git, K::J, Cmd, C::ToHead, "", "The bar to HEAD", 2),
    key(Git, K::G, CmdShift, C::Wide, "", "The git panel over both sides, and back", 2),
    key(Git, K::G, Cmd, C::Pin, "", "Pin this git panel; again: unpin", 2),
    key(Git, K::K, Cmd, C::Filters, "", "The filters", 2),
    // The directory tree (05 §1).
    key(TreeSide, K::F1, Plain, C::Help, "Help", HELP, 5),
    key(TreeSide, K::F10, Alt, C::CloseTree, "", CLOSE_TREE, 5),
    key(TreeSide, K::F10, Plain, C::CloseTree, "Tree", CLOSE_TREE, 5),
    key(TreeSide, K::Escape, Plain, C::CloseTree, "", CLOSE_TREE, 5),
    key(TreeSide, K::Enter, Plain, C::GoTo, "", "List that folder in this panel", 5),
    key(TreeSide, K::ArrowRight, Plain, C::Expand, "", "Open the folder (listed if it never was)", 5),
    key(TreeSide, K::ArrowLeft, Plain, C::Collapse, "", "Close the folder; on a closed one: its parent", 5),
    key(TreeSide, K::ArrowUp, Plain, C::Bar(Up), "", BAR, 5),
    key(TreeSide, K::ArrowDown, Plain, C::Bar(Down), "", BAR, 5),
    key(TreeSide, K::PageUp, Plain, C::Bar(PageUp), "", BAR, 5),
    key(TreeSide, K::PageDown, Plain, C::Bar(PageDown), "", BAR, 5),
    key(TreeSide, K::Home, Plain, C::Bar(Home), "", BAR, 5),
    key(TreeSide, K::End, Plain, C::Bar(End), "", BAR, 5),
    letter(TreeSide, C::Jump(' '), "The next folder starting with it", 5),
    key(TreeSide, K::Space, Plain, C::SizeNow, "", "Size the folder now", 5),
    key(TreeSide, K::R, Cmd, C::Reread, "", "List the folder again", 5),
    // A dialog, or a text field with the typing (04 §5.1, §3 rule 4).
    key(Dialog, K::Enter, Plain, C::Accept, "", "Go", 1),
    key(Dialog, K::Escape, Plain, C::Cancel, "", "Cancel", 1),
];

/// The rows of `context` of the slices in `upto` that can be pressed on this machine, in table
/// order.
pub fn rows(context: Context, upto: impl Into<Slices>) -> impl Iterator<Item = &'static Row> {
    let upto = upto.into();
    TABLE
        .iter()
        .filter(move |r| r.context == context && upto.has(r.slice))
        .filter(|r| match r.trigger {
            Trigger::Key(_, mods) => mods.here(),
            _ => true,
        })
}

/// The command the frame's key events and typed text ask for in `context`, matched with exact
/// modifiers; the event matched is removed from `input` (one character of a typed text), so no
/// widget or later pass acts on it (04 §3 rule 1). One command per call, in the order of the
/// events: call it until `None`.
pub fn lookup(input: &mut egui::InputState, context: Context) -> Option<Command> {
    find(&mut input.events, context, LANDED_SLICES)
}

/// [`lookup`] over `events`, with the rows of the slices in `upto`.
pub(crate) fn find(
    events: &mut Vec<Event>,
    context: Context,
    upto: impl Into<Slices>,
) -> Option<Command> {
    let upto = upto.into();
    for at in 0..events.len() {
        match &mut events[at] {
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let (key, pressed) = (*key, *modifiers);
                let row = rows(context, upto).find(|r| match r.trigger {
                    Trigger::Key(k, mods) => k == key && mods.matches(pressed),
                    _ => false,
                });
                if let Some(row) = row {
                    events.remove(at);
                    return Some(row.command);
                }
            }
            // Typed text, and what an input method commits — egui-winit hands a commit over as
            // `Ime(Commit)` alone (its `on_ime`), so a committed character acts once; what is
            // still being composed (`Ime(Preedit)`) acts never and stays in the frame.
            Event::Text(typed) | Event::Ime(egui::ImeEvent::Commit(typed)) => {
                // The first character of the text that has a row here, taken out of it.
                let hit = typed.char_indices().find_map(|(i, c)| {
                    char_row(context, upto, c).map(|row| (i, c, row.command.with_char(c)))
                });
                if let Some((i, c, command)) = hit {
                    typed.replace_range(i..i + c.len_utf8(), "");
                    if typed.is_empty() {
                        events.remove(at);
                    }
                    return Some(command);
                }
            }
            _ => {}
        }
    }
    None
}

/// The row a typed `c` acts on in `context`: its own (`+ - *`), else the letter row; whitespace
/// is never a letter.
fn char_row(context: Context, upto: Slices, c: char) -> Option<&'static Row> {
    rows(context, upto)
        .find(|r| r.trigger == Trigger::Char(c))
        .or_else(|| {
            (!c.is_whitespace())
                .then(|| rows(context, upto).find(|r| r.trigger == Trigger::Letter))
                .flatten()
        })
}

/// Removes one press (or repeat) of `key` with exactly `mods` from `input`; whether there was
/// one. For keys outside the table that follow its rule (the help screen's, a window's own).
pub fn take(input: &mut egui::InputState, key: Key, mods: Mods) -> bool {
    let at = input.events.iter().position(|e| match e {
        Event::Key {
            key: k,
            pressed: true,
            modifiers,
            ..
        } => *k == key && mods.matches(*modifiers),
        _ => false,
    });
    if let Some(at) = at {
        input.events.remove(at);
    }
    at.is_some()
}

/// What F9's slot says in `mode`: the mode it switches to (02 §2).
pub fn switch_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Preview => "Dual",
        Mode::Dual => "Prevw",
    }
}

/// The F-key row of `context` in `mode`: the landed plain F-keys and their labels, by number.
pub fn row(context: Context, mode: Mode) -> Vec<FKey> {
    let mut keys: Vec<FKey> = rows(context, LANDED_SLICES)
        .filter_map(|r| match r.trigger {
            Trigger::Key(key, Mods::None) if !r.label.is_empty() => {
                chrome::fkey_number(key).map(|n| FKey {
                    n,
                    label: if r.command == Command::SwitchMode {
                        switch_label(mode)
                    } else {
                        r.label
                    },
                })
            }
            _ => None,
        })
        .collect();
    keys.sort_by_key(|k| k.n);
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A key pressed with `mods`.
    fn press(key: Key, mods: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: mods,
        }
    }

    fn release(key: Key, mods: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: mods,
        }
    }

    /// What one press of `key` with `mods` asks for in `context`, every row counted.
    fn one(context: Context, key: Key, mods: Modifiers) -> Option<Command> {
        find(&mut vec![press(key, mods)], context, ALL_SLICES)
    }

    const NONE: Modifiers = Modifiers::NONE;
    const SHIFT: Modifiers = Modifiers::SHIFT;
    const ALT: Modifiers = Modifiers::ALT;

    /// Cmd/Ctrl as this machine's backend reports it: ⌘ (`mac_cmd` + `command`) on macOS, Ctrl
    /// (`ctrl` + `command`) elsewhere.
    fn real_command() -> Modifiers {
        if cfg!(target_os = "macos") {
            Modifiers {
                mac_cmd: true,
                command: true,
                ..NONE
            }
        } else {
            Modifiers {
                ctrl: true,
                command: true,
                ..NONE
            }
        }
    }

    /// 04 §3 rule 1, K-R3: a row matches only its own modifiers — a modifier variant has a row
    /// of its own or does nothing; with a row it is never the plain key's.
    #[test]
    fn exact_modifiers() {
        let cases: &[(Context, Key, Modifiers, Option<Command>)] = &[
            (List, K::F5, NONE, Some(C::Copy)),
            (List, K::F5, SHIFT, None),
            (List, K::F5, ALT, None),
            (List, K::F5, Modifiers::COMMAND, None),
            (List, K::F2, NONE, Some(C::Rename)),
            (List, K::F2, ALT, None),
            (List, K::F2, SHIFT, None),
            (List, K::F6, SHIFT, Some(C::Rename)),
            (List, K::F6, NONE, Some(C::Move)),
            (List, K::F6, ALT, None),
            (List, K::F10, NONE, Some(C::Tree)),
            (List, K::F10, ALT, Some(C::Tree)),
            (List, K::F10, SHIFT, None),
            (List, K::F10, Modifiers::SHIFT | Modifiers::ALT, None),
            (List, K::F8, NONE, Some(C::Delete)),
            (List, K::F8, SHIFT, Some(C::DeleteNow)),
            (List, K::Delete, NONE, Some(C::Delete)),
            (List, K::Delete, SHIFT, Some(C::DeleteNow)),
            (List, K::Tab, SHIFT, Some(C::OtherSide)),
            (List, K::Tab, Modifiers::COMMAND, None),
            (List, K::Backspace, NONE, Some(C::Up)),
            (List, K::PageUp, Modifiers::COMMAND, Some(C::Up)),
            (List, K::PageUp, NONE, Some(C::Bar(Step::PageUp))),
            (List, K::A, Modifiers::COMMAND, Some(C::SelectAll)),
            (List, K::A, real_command(), Some(C::SelectAll)),
            (List, K::A, Modifiers::COMMAND | Modifiers::SHIFT, None),
            (List, K::A, NONE, None),
            (
                List,
                K::ArrowLeft,
                real_command(),
                Some(C::SameFolder(Side::Left)),
            ),
            (
                List,
                K::ArrowRight,
                Modifiers::COMMAND,
                Some(C::SameFolder(Side::Right)),
            ),
            // Viewer: F3 and Esc leave, Shift+F3 does nothing; no list keys.
            (Viewer, K::F3, NONE, Some(C::CloseViewer)),
            (Viewer, K::F3, SHIFT, None),
            (Viewer, K::F5, NONE, None),
            // The git panel: Shift+F10 is its menu as F10 is; Alt+F10 does nothing (03 §9.1).
            (Git, K::F10, NONE, Some(C::Menu)),
            (Git, K::F10, SHIFT, Some(C::Menu)),
            (Git, K::F10, ALT, None),
            (Git, K::F3, SHIFT, Some(C::RevisionFile)),
            (Git, K::F3, NONE, Some(C::Details)),
            (Git, K::R, Modifiers::COMMAND, Some(C::Reread)),
            (
                Git,
                K::R,
                Modifiers::COMMAND | Modifiers::SHIFT,
                Some(C::FetchAll),
            ),
            (Git, K::G, real_command(), Some(C::Pin)),
            (Git, K::G, real_command() | Modifiers::SHIFT, Some(C::Wide)),
            (Git, K::G, Modifiers::COMMAND | Modifiers::ALT, None),
            // The tree: F10 and Alt+F10 close it; Shift+F10 does nothing.
            (TreeSide, K::F10, NONE, Some(C::CloseTree)),
            (TreeSide, K::F10, ALT, Some(C::CloseTree)),
            (TreeSide, K::F10, SHIFT, None),
            // A dialog or a field with the typing: Enter and Esc only.
            (Dialog, K::Enter, NONE, Some(C::Accept)),
            (Dialog, K::Escape, NONE, Some(C::Cancel)),
            (Dialog, K::Enter, SHIFT, None),
            (Dialog, K::F1, NONE, None),
            (Dialog, K::F5, NONE, None),
            (Dialog, K::A, Modifiers::COMMAND, None),
        ];
        for &(context, key, mods, want) in cases {
            assert_eq!(
                one(context, key, mods),
                want,
                "{context:?} {key:?} {mods:?}"
            );
        }
        // ⌘ with ⌃ is not Cmd; a Mac's ⌃ alone is not Cmd either.
        let both = Modifiers {
            mac_cmd: true,
            ctrl: true,
            command: true,
            ..NONE
        };
        assert_eq!(one(List, K::A, both), None);
        assert_eq!(one(List, K::A, Modifiers::CTRL), None);
        // ⌘⌫ deletes on macOS only: Ctrl+Backspace elsewhere has no row.
        let mac_cmd = Modifiers {
            mac_cmd: true,
            command: true,
            ..NONE
        };
        let ctrl = Modifiers {
            ctrl: true,
            command: true,
            ..NONE
        };
        let here = cfg!(target_os = "macos").then_some(C::Delete);
        assert_eq!(one(List, K::Backspace, mac_cmd), here);
        assert_eq!(one(List, K::Backspace, ctrl), None);
        // The order of the events decides, not the order of the rows: both orders.
        for (first, second) in [(SHIFT, NONE), (NONE, SHIFT)] {
            let mut events = vec![press(K::F6, first), press(K::F6, second)];
            let got = [
                find(&mut events, List, ALL_SLICES),
                find(&mut events, List, ALL_SLICES),
            ];
            let name = |m: Modifiers| if m == SHIFT { C::Rename } else { C::Move };
            assert_eq!(got, [Some(name(first)), Some(name(second))]);
            assert!(events.is_empty());
        }
    }

    /// The event a row matched is gone (as `consume_key` removes it), and only that one: a
    /// release, a key with no row and the rest of the frame stay for whoever reads them.
    #[test]
    fn matched_events_are_consumed() {
        let mut events = vec![
            press(K::F5, SHIFT),
            press(K::F5, NONE),
            release(K::F5, NONE),
            press(K::Q, NONE),
            Event::Text("q".into()),
        ];
        assert_eq!(find(&mut events, List, LANDED_SLICES), Some(Command::Copy));
        assert_eq!(
            events,
            vec![
                press(K::F5, SHIFT),
                release(K::F5, NONE),
                press(K::Q, NONE),
                Event::Text("q".into()),
            ]
        );
        assert_eq!(
            find(&mut events, List, LANDED_SLICES),
            Some(Command::Jump('q'))
        );
        assert_eq!(find(&mut events, List, LANDED_SLICES), None);
        assert_eq!(
            events,
            vec![press(K::F5, SHIFT), release(K::F5, NONE), press(K::Q, NONE)]
        );
        // A repeat acts as a press does (holding ↓ moves the bar).
        let mut held = vec![Event::Key {
            key: K::ArrowDown,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: NONE,
        }];
        assert_eq!(
            find(&mut held, List, LANDED_SLICES),
            Some(Command::Bar(Step::Down))
        );
        // Through egui's own input, as the browser asks: `lookup` takes the key from the frame.
        let ctx = egui::Context::default();
        let pass = |events: Vec<Event>, check: &dyn Fn(&mut egui::InputState)| {
            let raw = egui::RawInput {
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| ui.input_mut(|i| check(i)));
            out.textures_delta.clear();
        };
        pass(vec![press(K::F6, SHIFT), press(K::F5, NONE)], &|i| {
            assert_eq!(lookup(i, List), Some(Command::Rename));
            assert!(i.key_pressed(K::F5) && i.events.len() == 1);
            assert_eq!(lookup(i, List), Some(Command::Copy));
            assert!(!i.key_pressed(K::F5) && !i.key_pressed(K::F6));
            assert_eq!(lookup(i, List), None);
        });
        // `take` follows the same rule for keys outside the table.
        pass(vec![press(K::F1, SHIFT), press(K::F1, NONE)], &|i| {
            assert!(take(i, K::F1, Mods::None));
            assert!(!take(i, K::F1, Mods::None));
            assert!(take(i, K::F1, Mods::Shift));
            assert!(i.events.is_empty());
        });
    }

    /// 04 §3 rule 2, K-R3: `+ - *` are read from the typed text, so whatever the layout needed
    /// to type them (Shift on a US layout: the key event is `Shift` + `=` or `8`; a German
    /// layout has a `+` key) the same command comes; the jump letters too, one per call.
    #[test]
    fn typed_plus_minus_star() {
        // US: Shift+= types "+", Shift+8 types "*", - is plain.
        let us_plus = vec![press(K::Plus, SHIFT), Event::Text("+".into())];
        let us_star = vec![press(K::Num8, SHIFT), Event::Text("*".into())];
        let us_minus = vec![press(K::Minus, NONE), Event::Text("-".into())];
        // German: a + key, no Shift.
        let de_plus = vec![press(K::Plus, NONE), Event::Text("+".into())];
        for (events, want) in [
            (us_plus, Command::SelectGroup),
            (us_star, Command::InvertSelection),
            (us_minus, Command::UnselectGroup),
            (de_plus, Command::SelectGroup),
        ] {
            let mut events = events;
            assert_eq!(
                find(&mut events, List, LANDED_SLICES),
                Some(want),
                "{events:?}"
            );
            assert!(
                !events.iter().any(|e| matches!(e, Event::Text(_))),
                "the text is taken: {events:?}"
            );
            assert_eq!(
                find(&mut events, List, LANDED_SLICES),
                None,
                "and nothing else"
            );
        }
        // Several characters in one frame: one command each, in order; spaces are no letters.
        let mut events = vec![Event::Text("a+ b".into())];
        let mut got = Vec::new();
        while let Some(c) = find(&mut events, List, LANDED_SLICES) {
            got.push(c);
        }
        assert_eq!(
            got,
            [Command::Jump('a'), Command::SelectGroup, Command::Jump('b')]
        );
        // Elsewhere they are letters (the tree jumps, the git panel finds); a dialog's field
        // keeps all of its typing.
        let typed = || vec![Event::Text("+".into())];
        assert_eq!(
            find(&mut typed(), TreeSide, ALL_SLICES),
            Some(Command::Jump('+'))
        );
        assert_eq!(
            find(&mut typed(), Git, ALL_SLICES),
            Some(Command::FindWith('+'))
        );
        let mut field = typed();
        assert_eq!(find(&mut field, Dialog, ALL_SLICES), None);
        assert_eq!(field, typed());
        assert_eq!(find(&mut typed(), Viewer, ALL_SLICES), None);
    }

    /// One table: in no context do two rows share a binding; no row binds what the shell turns
    /// into F-keys (Cmd/Ctrl+digits) or F11/F12 (macOS takes F11); `+ - *` are typed, not keys.
    #[test]
    fn no_duplicate_bindings() {
        for context in Context::ALL {
            let mut seen = HashSet::new();
            for row in rows(context, ALL_SLICES) {
                assert!(
                    seen.insert(row.trigger),
                    "{context:?}: {:?} twice",
                    row.trigger
                );
                if let Trigger::Key(key, mods) = row.trigger {
                    let digit = matches!(
                        key,
                        K::Num0
                            | K::Num1
                            | K::Num2
                            | K::Num3
                            | K::Num4
                            | K::Num5
                            | K::Num6
                            | K::Num7
                            | K::Num8
                            | K::Num9
                    );
                    assert!(!(digit && mods == Mods::Command), "{row:?}");
                    assert!(!matches!(key, K::F11 | K::F12), "{row:?}");
                    assert!(!matches!(key, K::Plus | K::Minus), "{row:?}: typed");
                }
                if let Trigger::Char(c) = row.trigger {
                    assert!(!c.is_whitespace(), "{row:?}");
                }
            }
        }
        // Every slice number is one of the seven.
        assert!(TABLE.iter().all(|r| ALL_SLICES.has(r.slice)));
    }

    /// The landed slices are a set (02 §8.1 "Schedule"): S1, S2 and S5 have landed, so exactly
    /// their rows act — F10 opens the tree and Cmd/Ctrl+G pins the git panel, while F7, an S6 row,
    /// and the git panel's F5, an S3 row, still do nothing.
    #[test]
    fn landed_slices_are_a_set() {
        assert_eq!(LANDED_SLICES, Slices::of(&[1, 2, 5]));
        assert!(TABLE
            .iter()
            .all(|r| LANDED_SLICES.has(r.slice) == matches!(r.slice, 1 | 2 | 5)));
        assert!(!ALL_SLICES.has(0) && !ALL_SLICES.has(8) && !ALL_SLICES.has(u8::MAX));
        assert_eq!(
            find(&mut vec![press(K::F10, NONE)], List, LANDED_SLICES),
            Some(Command::Tree)
        );
        let pin = || vec![press(K::G, Modifiers::COMMAND)];
        assert_eq!(find(&mut pin(), List, LANDED_SLICES), Some(Command::Pin));
        assert_eq!(find(&mut pin(), Git, LANDED_SLICES), Some(Command::Pin));
        assert_eq!(
            find(&mut vec![press(K::F7, NONE)], List, LANDED_SLICES),
            None,
            "S6 has not landed"
        );
        assert_eq!(
            find(&mut vec![press(K::F5, NONE)], Git, LANDED_SLICES),
            None,
            "S3 has not landed"
        );
    }

    /// 03 §9.2: Cmd/Ctrl+K is the git panel's filters (from S2, before the F10 menu of S3), and
    /// nothing elsewhere — it is free in the list, a viewer and the tree.
    #[test]
    fn command_k_is_the_git_panels_filters() {
        let k = |m: Modifiers| vec![press(K::K, m)];
        assert_eq!(
            find(&mut k(Modifiers::COMMAND), Git, LANDED_SLICES),
            Some(Command::Filters)
        );
        assert_eq!(
            find(&mut k(real_command()), Git, LANDED_SLICES),
            Some(Command::Filters)
        );
        assert_eq!(
            find(
                &mut k(Modifiers::COMMAND | Modifiers::SHIFT),
                Git,
                ALL_SLICES
            ),
            None
        );
        assert_eq!(find(&mut k(NONE), Git, ALL_SLICES), None);
        for context in [List, Viewer, TreeSide, Dialog] {
            assert_eq!(
                find(&mut k(Modifiers::COMMAND), context, ALL_SLICES),
                None,
                "{context:?}"
            );
        }
    }

    /// PC Shell's labels are six characters at most, and six fit a slot from an 80-column
    /// width (640 points) up; a label sits on a plain F1…F10 and every plain F-key of a context
    /// but a dialog has one (the row lists what works there).
    #[test]
    fn every_label_fits_its_slot() {
        for n in 1..=10 {
            let (_, label) = chrome::key_slot(0.0, 640.0, n);
            assert!(label.width() >= 6.0 * chrome::CELL.x, "slot {n}");
        }
        for row in TABLE {
            let plain_f = match row.trigger {
                Trigger::Key(key, Mods::None) => chrome::fkey_number(key).is_some(),
                _ => false,
            };
            assert!(row.label.chars().count() <= 6, "{row:?}");
            assert_eq!(!row.label.is_empty(), plain_f, "{row:?}");
        }
        for mode in [Mode::Preview, Mode::Dual] {
            assert!(switch_label(mode).chars().count() <= 6);
            for context in Context::ALL {
                for k in row(context, mode) {
                    assert!(k.label.chars().count() <= 6, "{context:?} {k:?}");
                    assert!((1..=10).contains(&k.n));
                }
            }
        }
    }

    /// 02 §7, 04 §2: the rows of the landed slices, S1, S2 and S5 — the list's slots are `1Help
    /// 2Rename 3View 4Edit 5Copy 6Move 9Dual 10Tree` (9 names the mode F9 switches to); the
    /// viewer's `1Help 3Exit 4Edit`; the git panel's `1Help 3View 4Edit 9Dual` (03 §9.1, S2's
    /// slots); the tree's `1Help 10Tree` (05 §1). The keys of the slices not landed do nothing
    /// yet.
    #[test]
    fn the_rows_of_s1() {
        let pairs = |keys: Vec<FKey>| -> Vec<(u8, &str)> {
            keys.into_iter().map(|k| (k.n, k.label)).collect()
        };
        assert_eq!(
            pairs(row(List, Mode::Preview)),
            [
                (1, "Help"),
                (2, "Rename"),
                (3, "View"),
                (4, "Edit"),
                (5, "Copy"),
                (6, "Move"),
                (9, "Dual"),
                (10, "Tree")
            ]
        );
        assert_eq!(pairs(row(List, Mode::Dual))[6], (9, "Prevw"));
        assert_eq!(
            pairs(row(Viewer, Mode::Preview)),
            [(1, "Help"), (3, "Exit"), (4, "Edit")]
        );
        assert_eq!(
            pairs(row(Git, Mode::Preview)),
            [(1, "Help"), (3, "View"), (4, "Edit"), (9, "Dual")],
            "S2"
        );
        assert_eq!(pairs(row(Git, Mode::Dual))[3], (9, "Prevw"));
        assert_eq!(
            pairs(row(TreeSide, Mode::Preview)),
            [(1, "Help"), (10, "Tree")],
            "S5"
        );
        assert!(row(Dialog, Mode::Preview).is_empty());
        // S5: F10 and Alt+F10 open the tree from the list, and close it from the tree.
        for mods in [NONE, ALT] {
            let f10 = || vec![press(K::F10, mods)];
            assert_eq!(find(&mut f10(), List, LANDED_SLICES), Some(Command::Tree));
            assert_eq!(
                find(&mut f10(), TreeSide, LANDED_SLICES),
                Some(Command::CloseTree)
            );
        }
        for (key, mods) in [(K::F7, NONE), (K::F8, NONE)] {
            assert_eq!(
                find(&mut vec![press(key, mods)], List, LANDED_SLICES),
                None,
                "{key:?} {mods:?} lands later"
            );
        }
        assert_eq!(
            find(&mut vec![press(K::F9, NONE)], List, LANDED_SLICES),
            Some(Command::SwitchMode)
        );
    }

    /// What `lookup` returns over a frame of `events` in `context`, one call after another, and
    /// what is left of the frame — through egui's own input, as the browser asks.
    fn looked_up(context: Context, events: Vec<Event>) -> (Vec<Command>, Vec<Event>) {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut got = Vec::new();
        let mut left = Vec::new();
        let mut out = ctx.run_ui(raw, |ui| {
            ui.input_mut(|i| {
                while let Some(c) = lookup(i, context) {
                    got.push(c);
                }
                left = i.events.clone();
            })
        });
        out.textures_delta.clear();
        (got, left)
    }

    fn preedit(text: &str) -> Event {
        Event::Ime(egui::ImeEvent::Preedit {
            text: text.into(),
            active_range_chars: None,
        })
    }

    fn commit(text: &str) -> Event {
        Event::Ime(egui::ImeEvent::Commit(text.into()))
    }

    /// A composing input method (Hangul here): what is still being composed never jumps and
    /// stays in the frame; a committed character jumps as a typed one does — once, since
    /// egui-winit 0.37 hands a commit over as `Ime(Commit)` alone, never also as typed text
    /// (`on_ime`, src/lib.rs:823–827). So a Korean name can be jumped to.
    #[test]
    fn a_committed_character_jumps_once_and_composing_never() {
        let composing = vec![preedit("ㅎ"), preedit("하"), preedit("한")];
        assert_eq!(looked_up(List, composing.clone()), (vec![], composing));
        // 한 committed, 그 started: one jump, the preedits untouched.
        let (got, left) = looked_up(List, vec![preedit("한"), commit("한"), preedit("ㄱ")]);
        assert_eq!(got, [C::Jump('한')]);
        assert_eq!(left, vec![preedit("한"), preedit("ㄱ")]);
        // A commit of several characters: one jump each, in order.
        let (got, left) = looked_up(List, vec![commit("测试")]);
        assert_eq!(got, [C::Jump('测'), C::Jump('试')]);
        assert!(left.is_empty());
        // `+` committed is the select-group key, as typed; an empty commit is nothing.
        assert_eq!(looked_up(List, vec![commit("+")]).0, [C::SelectGroup]);
        assert_eq!(looked_up(List, vec![commit("")]).0, []);
        // The tree jumps (S5 has landed), the git panel finds; a dialog's field keeps its
        // composition and commit.
        assert_eq!(
            looked_up(TreeSide, vec![commit("한")]).0,
            [C::Jump('한')],
            "S5"
        );
        assert_eq!(
            find(&mut vec![commit("한")], Git, ALL_SLICES),
            Some(C::FindWith('한'))
        );
        let field = vec![preedit("한"), commit("한")];
        assert_eq!(looked_up(Dialog, field.clone()), (vec![], field));
        // Shift+letter still jumps (the key event has no row; its typed text does), once.
        let (got, left) = looked_up(List, vec![press(K::A, SHIFT), Event::Text("A".into())]);
        assert_eq!(got, [C::Jump('A')]);
        assert_eq!(left, vec![press(K::A, SHIFT)]);
    }

    /// The help screen's names: the platform's command key, the arrows as glyphs.
    #[test]
    fn trigger_names() {
        let cmd = command_name();
        assert_eq!(Trigger::Key(K::F6, Mods::Shift).name(), "Shift+F6");
        assert_eq!(Trigger::Key(K::A, Mods::Command).name(), format!("{cmd}+A"));
        assert_eq!(
            Trigger::Key(K::F, Mods::CommandShift).name(),
            format!("{cmd}+Shift+F")
        );
        assert_eq!(
            Trigger::Key(K::ArrowLeft, Mods::Command).name(),
            format!("{cmd}+←")
        );
        assert_eq!(Trigger::Key(K::Escape, Mods::None).name(), "Esc");
        assert_eq!(Trigger::Char('*').name(), "*");
        assert_eq!(Trigger::Letter.name(), "a letter");
    }
}
