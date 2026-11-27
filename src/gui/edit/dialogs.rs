//! The editor's dialogs in cells (`.prd/12-editor.md` §4, §5.1, §5.5, §7.2, §9.2, §9.6, E6,
//! E10; ED-dialogs), driven from [`EditView`] as its screen is: nothing opens the editor yet
//! (ED-layout), so its tests drive the view.
//!
//! - **On the status row** (§4: "Find, Go to, `^K` and `^S` use this row as fields and prompts,
//!   as Turbo C 2.0's Find did"): Find's fields — `Find:`, Tab to `Replace:`, Enter on to
//!   `Options:` (Turbo C 2.0's U W B N G, each read by its physical key or as a 2-set jamo, the
//!   input method off meanwhile, §5.5) and Enter there runs it — Replace's question at each
//!   match (Y replaces it, N skips it, Esc stops: it holds no Save and no Discard, so its letters
//!   answer, as Turbo's did), and Go to line's field. The needle's, the replacement's and the
//!   line's fields are `ops::dialog`'s cell field.
//! - **Boxes** (`ops::dialog`: 10 §6's look, centred between the title and status rows, the DOS
//!   shadow): the close prompt (§9.6: 07 §10's four choices), the conflict dialog (§9.2) and its
//!   Reload's question, and the encoding question (§7.2, read from [`Editor::question`] each
//!   pass). Each opens with the keyboard on `[ Cancel ]` (E6): Enter presses the button that has
//!   it, Esc is Cancel, ← → Tab and Shift+Tab move it — but not in the pass it opens in, nor
//!   when the same box opens again from outside — and a letter answers nothing. One opened from
//!   outside (an exit, a Save's end) disarms a waiting `^K` or `^S`.
//! - **The menus** (F10, or a click on a name in the title row): the bar on a name — ← →, a
//!   name's letter, Enter or ↓ drops its items — then the items as a box under the name (the
//!   kit's, laid on a grid of its own size there), the bar's row with the keyboard (↑ ↓; ← → the
//!   next menu); an item's letter runs it by the physical key or a jamo, so F10 F S saves and
//!   F10 E R redoes in either input source; the status row names the letters (*editor* §2.3).
//!   Esc closes the innermost. Items with no command yet are off: Delete to line end and
//!   Matching bracket have no operation in `edit::ops`, and the Git menu is ED-git's.
//! - **While anything here is up it has every key**, read in the pass's order against the
//!   state each leaves (a field's text before the Enter after it, a menu's letter before the
//!   item's), each stage handing what follows to the next; none goes further, and
//!   [`EditView::press`] takes none but a focus report or a resize (E31's Shift known).
//! - **The binding's contract**: each pass gives its inputs to [`EditView::press`] in their
//!   order until [`EditView::asking`] is true, removes every event it gave from the pass, then
//!   runs [`EditView::dialogs_ui`] — so the dialogs read only the events after those: a dialog a
//!   key opens reads the keys after that key, and no event is acted on twice (an F10 never
//!   closes its own bar, an Enter typed before Ctrl+Q never presses the prompt it brings).
//! - **No edits while a Save runs** (E24): [`EditView::press`], the menus, Replace and the
//!   encoding question's Type refuse them, with §9.1's line; moves and Find still work.
//! - **Exits** (§6.4, §9.6): the binding gives each to [`EditView::leave`]. With unsaved edits
//!   the close prompt asks; a running Save holds the exit until it reports (07 §10). What was
//!   chosen comes back as [`Chosen`] — or, for Save, Save copy and Save As…, as the command
//!   ([`EditView::take_commands`]), whose end [`EditView::landed`], [`EditView::elsewhere`],
//!   [`EditView::not_saved`] or [`EditView::gave_up`] reports, the exit going once a Save lands
//!   (a Save copy or Save As… only while the document is what it wrote).

use super::{EditView, MENUS};
use crate::edit::arm::{self, Armed, Backend, BlockOp, Command};
use crate::edit::buffer::Piece;
use crate::edit::file::Saved;
use crate::edit::ops::{Answer, Block, Options, Said, Search, Sel};
use crate::gui::browser::ops::dialog::{self, Button, Ink, Inputs, Part};
use crate::gui::cells::{self, Buffer, Cells, Grid, Pen, Position};
use crate::gui::chrome::{self, cga, CELL};
use eframe::egui::{self, Key, Modifiers};
use std::ops::Range;
use std::path::PathBuf;

/// An exit of §6.4: where the editor goes once nothing unsaved holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// Esc, F3, `[■]`: back to where F4 was pressed.
    Leave,
    /// F4: the Enter viewer on this file, at the caret.
    View,
    /// Shift+F4: VS Code / Zed.
    External,
    /// F9 then a view key: that view.
    Views(u8),
    /// F3, F4 or Enter on another file in the column.
    Open(String),
    /// Ctrl/⌘+R: the file read again.
    Reread,
    /// Ctrl/⌘+Q, +W, the window's close.
    Close,
    /// F12, Alt+G (10 §7.4).
    Switch,
}

/// What a dialog chose that the binding is to do ([`EditView::take_chosen`], oldest first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chosen {
    /// Take the exit now: a Save it waited for landed.
    Go(Exit),
    /// The close prompt's Discard: the journal goes (`Session::discard`), then the exit.
    Discard(Exit),
    /// The conflict dialog's Reload, confirmed: the edits discarded and the file read as it is
    /// now (`Session::reload`).
    Reload,
    /// The conflict dialog's Overwrite (`Session::overwrite`, E23).
    Overwrite,
}

/// What the prompts say of the file and offer, from the binding's `Session`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileState {
    /// `host:/abs`, or the local path: the close prompt names it.
    pub shown: String,
    /// The server replaces a file in one step: the close prompt offers Save, else Save copy.
    pub can_replace: bool,
    /// The journal's folder, once there is one: Discard's line names it (cells have no hover).
    pub journal: Option<PathBuf>,
    /// A Save runs: an exit waits for it (07 §10).
    pub running: bool,
}

/// The status row's line while an exit waits for a running Save.
pub const WAITS: &str = "saving — the editor goes on when the save is done";

/// The status row's line for an edit refused while a Save runs (E24, §9.1).
pub const SAVING: &str = "saving — editing resumes when the save is done";

/// Replace's question at each match, on the status row.
const REPLACE_ASK: &str = "Replace this one? Y replaces it · N skips it · Esc stops";

/// The close prompt's title and nominal width, the hex window's (07 §10).
const PROMPT_TITLE: &str = "Unsaved changes";
const WIDE: usize = 52;

/// Find's options field, in cells: Turbo's five letters and the caret.
const OPTIONS_CELLS: u16 = 7;

/// The letters the options field takes (TC2-RG p. 418–419; `Options::parse`).
const OPTION_LETTERS: &str = "UWBNG";

/// What is up besides the edit window (the encoding question is the editor's own state).
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) enum Up {
    #[default]
    None,
    /// F10's bar on menu `at`; `open`: its items dropped, the keyboard on `item`.
    Menu {
        at: usize,
        open: bool,
        item: usize,
    },
    Find(Find),
    /// Replace's question at the match `at`, `count` replaced so far; backward, `bound` is where
    /// the earliest replacement starts — a match ending past it would take some of it.
    Replace {
        search: Search,
        text: String,
        at: Range<u64>,
        count: usize,
        bound: Option<u64>,
    },
    GoTo(String),
    /// The close prompt, for this exit.
    Close(Exit),
    /// §9.2's dialog: what changed, where the staged copy is kept, and whether Overwrite is
    /// offered.
    Conflict {
        line: String,
        notes: Vec<String>,
        staged: PathBuf,
        overwrite: bool,
    },
    /// §9.2's Reload, asked first.
    Reload,
}

/// Find's fields as typed.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Find {
    needle: String,
    /// `Some` once Tab moved to `Replace:` (or Replace… opened it): a Replace.
    replace: Option<String>,
    options: String,
    field: Field,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Needle,
    Replace,
    Options,
}

/// A field of the status row: Go to's, or one of Find's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    GoTo,
    Needle,
    Replace,
    Options,
}

fn slot_id(slot: Slot) -> egui::Id {
    id(match slot {
        Slot::GoTo => "goto",
        Slot::Needle => "needle",
        Slot::Replace => "replace",
        Slot::Options => "options",
    })
}

/// Where the status row's labels and fields stand.
struct Row {
    labels: Vec<(u16, &'static str)>,
    fields: Vec<(Slot, Cells)>,
}

/// A key that moves on from a status-row field: Enter, Tab, Shift+Tab or Esc.
fn moves_on(e: &egui::Event) -> bool {
    let r = read(e, &mut false);
    r.is(Key::Enter, false)
        || r.is(Key::Tab, false)
        || r.is(Key::Tab, true)
        || r.is(Key::Escape, false)
}

/// The dialogs' state on the view.
pub(super) struct Dialogs {
    up: Up,
    backend: Backend,
    file: FileState,
    /// The exit a Save from the close prompt, or a Save running when it was asked, waits for.
    pending: Option<Exit>,
    chosen: Vec<Chosen>,
    /// The last Find or Replace: Ctrl+L repeats it, Ctrl+F starts from it.
    last: Option<(Search, Option<String>)>,
    /// The box of the last pass: a new one gives its Cancel the keyboard (E6).
    shown: Option<egui::Id>,
    /// A field opened or moved to: it takes the keyboard, its text selected.
    focus: bool,
    /// Keys a field had no second turn for in their pass: the next pass reads them first.
    held: Vec<egui::Event>,
    /// The document when a Save copy or Save As… (the close prompt's or a menu's) went out,
    /// until that save reports: an edit after it is in neither file, so an exit that waited for
    /// it asks again ([`EditView::elsewhere`]).
    copied: Option<Vec<Piece>>,
}

impl Dialogs {
    pub(super) fn new(name: &str, backend: Backend) -> Dialogs {
        Dialogs {
            up: Up::None,
            backend,
            file: FileState {
                shown: name.to_string(),
                can_replace: true,
                journal: None,
                running: false,
            },
            pending: None,
            chosen: Vec::new(),
            last: None,
            shown: None,
            focus: false,
            held: Vec::new(),
            copied: None,
        }
    }

    /// `up` opened from outside the dialogs' pass (a key, an exit, a Save's end): new, so a box
    /// it shows opens on Cancel even where the same box was up the pass before, and keys held
    /// for a field it replaces are not its.
    fn open(&mut self, up: Up) {
        self.up = up;
        self.shown = None;
        self.held.clear();
    }

    /// A Save runs, as the binding last said.
    pub(super) fn running(&self) -> bool {
        self.file.running
    }

    /// The menu F10's bar is on, while the bar or its items are up (the title row shows it).
    pub(super) fn bar_on(&self) -> Option<usize> {
        match self.up {
            Up::Menu { at, .. } => Some(at),
            _ => None,
        }
    }
}

fn id(what: &str) -> egui::Id {
    egui::Id::unique("sopen-edit-dialog").with(what)
}

fn row_id(menu: usize, item: usize) -> egui::Id {
    id("menu").with(menu).with(item)
}

/// What a menu item does.
#[derive(Debug, Clone, PartialEq)]
enum Act {
    /// A command the binding runs (Save, Save As…, leaving, the clipboard's paste).
    Bind(Command),
    /// An editing command, made here as its key makes it.
    Edit(Command),
    Find,
    Replace,
    FindAgain,
    GoTo,
    /// Options → Autoindent.
    Indent,
    /// Options → Column: as a click on `[↕]`.
    Column,
    /// No command yet.
    Off,
}

struct Item {
    label: &'static str,
    key: String,
    letter: char,
    act: Act,
}

/// Menu `menu`'s items (§4), with the keys they have in `backend` and the letter that runs each.
fn items(menu: usize, backend: Backend) -> Vec<Item> {
    use Act::*;
    use Command as C;
    let terminal = backend == Backend::Terminal;
    let cmd = if !terminal && cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+"
    };
    let it = |label, key: &str, letter, act| Item {
        label,
        key: key.replace("Cmd+", cmd),
        letter,
        act,
    };
    let block = |label, letter: char, op| {
        let key = format!("^K {letter}");
        it(label, &key, letter, Edit(C::Block(op)))
    };
    match menu {
        0 => vec![
            it("Save", "F2", 'S', Bind(C::Save)),
            it("Save as…", "F7", 'A', Bind(C::SaveAs)),
            it("Save copy", "", 'C', Bind(C::SaveCopy)),
            it("Reload", "Cmd+R", 'R', Bind(C::Reread)),
            it("VS Code / Zed", "Shift+F4", 'V', Bind(C::External)),
            it("Exit", "F3", 'X', Bind(C::Leave)),
        ],
        1 => vec![
            it("Undo", "Cmd+Z", 'U', Edit(C::Undo { hint: false })),
            // A legacy terminal sends Ctrl+Shift+Z as Ctrl+Z: Redo is this item there (E7).
            it(
                "Redo",
                if terminal { "" } else { "Shift+Cmd+Z" },
                'R',
                Edit(C::Redo),
            ),
            it("Cut", "Cmd+X", 'T', Edit(C::Cut)),
            it("Copy", "Cmd+C", 'C', Edit(C::Copy)),
            // The window only: a terminal's paste is its own key (E9).
            it(
                "Paste",
                "Cmd+V",
                'P',
                if terminal {
                    Off
                } else {
                    Bind(C::PasteClipboard)
                },
            ),
            it("Select all", "Cmd+A", 'A', Edit(C::SelectAll)),
            it("Delete line", "Ctrl+Y", 'L', Edit(C::DeleteLine)),
            it("Delete word", "Ctrl+T", 'W', Edit(C::DeleteWord)),
            it("Insert line", "Ctrl+N", 'I', Edit(C::InsertLine)),
            it("Delete to line end", "", 'E', Off),
        ],
        2 => vec![
            it("Find…", "Cmd+F", 'F', Find),
            it("Replace…", "", 'R', Replace),
            it("Find again", "Ctrl+L", 'A', FindAgain),
            it("Go to line…", "Cmd+G", 'G', GoTo),
            it("Matching bracket", "", 'M', Off),
        ],
        3 => vec![
            block("Begin", 'B', BlockOp::Begin),
            block("End", 'K', BlockOp::End),
            block("Copy", 'C', BlockOp::Copy),
            block("Move", 'V', BlockOp::Move),
            block("Delete", 'Y', BlockOp::Delete),
            block("Hide", 'H', BlockOp::Hide),
            block("Indent", 'I', BlockOp::Indent),
            block("Unindent", 'U', BlockOp::Unindent),
            block("Mark line", 'L', BlockOp::MarkLine),
            block("Mark word", 'T', BlockOp::MarkWord),
        ],
        4 => vec![
            it("Stage this file", "", 'S', Off),
            it("Unstage", "", 'U', Off),
            it("Commit…", "", 'C', Off),
            it("Diff against the index", "", 'D', Off),
            it("Show in Git", "", 'G', Off),
        ],
        _ => vec![
            it("Insert / Overwrite", "Ins", 'I', Edit(C::ToggleInsert)),
            it("Autoindent", "", 'A', Indent),
            it("Column", "", 'C', Column),
        ],
    }
}

/// The first item of `items` from `from` on, stepping `step`, that has a command.
fn next_on(items: &[Item], from: usize, step: isize) -> usize {
    let n = items.len() as isize;
    let mut i = from as isize;
    for _ in 0..n {
        i += step;
        if (0..n).contains(&i) && items[i as usize].act != Act::Off {
            return i as usize;
        }
    }
    from
}

fn first_on(menu: usize, backend: Backend) -> usize {
    let its = items(menu, backend);
    match its.first().is_some_and(|i| i.act != Act::Off) {
        true => 0,
        false => next_on(&its, 0, 1),
    }
}

/// `key` pressed with no modifier (`shift`: with Shift alone), taken from the pass.
fn take(ctx: &egui::Context, key: Key, shift: bool) -> bool {
    ctx.input_mut(|i| {
        let at = i.events.iter().position(|e| {
            matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. }
                if *k == key && if shift { modifiers.shift_only() } else { modifiers.is_none() })
        });
        at.map(|at| i.events.remove(at)).is_some()
    })
}

/// egui's letter key's letter.
fn key_letter(key: Key) -> Option<char> {
    let mut name = key.name().chars();
    match (name.next(), name.next()) {
        (Some(l), None) if l.is_ascii_uppercase() => Some(l),
        _ => None,
    }
}

/// The pass's keys, text, pastes, clipboard requests and input-method events, taken out of it
/// in their order (the rest stay): what a field's engine is given segment by segment, so one
/// committed composition, one Cut or one Copy reaches one field.
fn take_keys(ctx: &egui::Context) -> Vec<egui::Event> {
    ctx.input_mut(|i| {
        let all = std::mem::take(&mut i.events);
        let (keys, rest) = all.into_iter().partition(|e| {
            use egui::Event as E;
            matches!(
                e,
                E::Key { .. } | E::Text(_) | E::Paste(_) | E::Ime(_) | E::Copy | E::Cut
            )
        });
        i.events = rest;
        keys
    })
}

/// `events` given back to the pass, for what is up after the dialog that left them.
fn put_keys(ctx: &egui::Context, events: Vec<egui::Event>) {
    ctx.input_mut(|i| i.events.extend(events));
}

/// The events after one that was read, handed on: when that was a letter key (`after_key`),
/// the text egui sends with it goes with it — F10 S F opens Find with its needle as it was, not
/// holding `f`.
fn after_its_text(events: impl Iterator<Item = egui::Event>, after_key: bool) -> Vec<egui::Event> {
    let mut rest = events.peekable();
    if after_key && matches!(rest.peek(), Some(egui::Event::Text(_))) {
        rest.next();
    }
    rest.collect()
}

/// One key or text event of the pass as the dialogs read it, in the pass's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    /// A key pressed that types no letter here: Enter, Esc, Tab, an arrow, F10, Backspace, a
    /// letter key with Ctrl, ⌘ or Alt.
    Key(Key, Modifiers),
    /// A letter (§5.5, E10): a letter key with no Ctrl, ⌘ or Alt by its physical key first —
    /// the text egui sends after it then dropped — or typed text that is one letter or a 2-set
    /// jamo (a terminal's, the input method's).
    Letter(char),
    /// A release, other text, a paste.
    Other,
}

impl Read {
    /// `key` with no modifier, or with Shift alone (`shift`).
    fn is(self, key: Key, shift: bool) -> bool {
        match self {
            Read::Key(k, m) => k == key && if shift { m.shift_only() } else { m.is_none() },
            _ => false,
        }
    }
}

/// Reads `e`; `after_key`: the event before it was a letter key, whose typed text this may be.
fn read(e: &egui::Event, after_key: &mut bool) -> Read {
    match e {
        egui::Event::Key {
            key,
            physical_key,
            pressed: true,
            modifiers,
            ..
        } => {
            let held = modifiers.ctrl || modifiers.command || modifiers.mac_cmd || modifiers.alt;
            let letter = (physical_key.and_then(key_letter))
                .or_else(|| key_letter(*key))
                .filter(|_| !held);
            *after_key = letter.is_some();
            letter.map_or(Read::Key(*key, *modifiers), Read::Letter)
        }
        egui::Event::Text(_) if std::mem::take(after_key) => Read::Other,
        egui::Event::Text(text) => arm::letter(text).map_or(Read::Other, Read::Letter),
        _ => Read::Other,
    }
}

/// The options field's letters for `o`.
fn option_letters(o: Options) -> String {
    [
        (o.ignore_case, 'U'),
        (o.whole_words, 'W'),
        (o.backward, 'B'),
        (o.no_ask, 'N'),
        (o.global, 'G'),
    ]
    .into_iter()
    .filter_map(|(on, l)| on.then_some(l))
    .collect()
}

fn not_found(search: &Search) -> Said {
    Said::Refused(format!("not found: {}", search.needle))
}

fn replaced(n: usize) -> Said {
    Said::Warned(format!("replaced {n}"))
}

/// What a button of a box does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Save,
    SaveCopy,
    SaveAs,
    Discard,
    Cancel,
    Reload,
    Overwrite,
    Type,
    Item(usize),
}

/// A box laid out: its id, its layout, where its grid's corner is on the editor's grid, and
/// its buttons in their order — whether each is on, and what it does.
struct Boxed {
    id: egui::Id,
    layout: dialog::Layout,
    at: (u16, u16),
    buttons: Vec<(egui::Id, Choice, bool)>,
    menu: bool,
}

impl EditView {
    /// What the prompts say of the file and offer (the binding's, from its `Session`).
    pub fn set_file(&mut self, file: FileState) {
        self.dialogs.file = file;
    }

    /// A menu, a field or a prompt is up: it has the keys ([`EditView::press`] takes none).
    pub fn asking(&self) -> bool {
        self.dialogs.up != Up::None || self.ed.question().is_some()
    }

    /// What the dialogs chose that the binding is to do, oldest first.
    pub fn take_chosen(&mut self) -> Vec<Chosen> {
        std::mem::take(&mut self.dialogs.chosen)
    }

    /// An exit of §6.4 asked for (the binding's: Esc, F3, F4, Shift+F4, F9 then a view, a file
    /// in the column, Ctrl/⌘+R, the window's close, the switch). `true`: nothing unsaved holds
    /// it, go now. Else it waits — for a running Save, then [`Chosen::Go`] once that lands — or
    /// the close prompt asks, on Cancel.
    pub fn leave(&mut self, exit: Exit) -> bool {
        if self.dialogs.file.running {
            self.dialogs.pending = Some(exit);
            self.said = Some(Said::Warned(WAITS.to_string()));
            return false;
        }
        // An exit asked now replaces any an earlier prompt's Save was still waiting for.
        self.dialogs.pending = None;
        if self.ed.buffer().is_saved() {
            return true;
        }
        self.prompt(Up::Close(exit));
        false
    }

    /// A dialog opened from outside the keys (an exit, a Save's end, a click on a menu's name):
    /// a `^K` or `^S` waiting for its key is disarmed first, so the key after it is not its
    /// letter.
    fn prompt(&mut self, up: Up) {
        if self.keys.armed() != Armed::None {
            // Esc disarms either arming, untyped (`edit::arm::decide`).
            self.keys
                .press(&arm::Press::key(Key::Escape, Modifiers::NONE));
        }
        self.dialogs.open(up);
    }

    /// A Save ended (`Session::save`, `overwrite`): its line on the status row; a conflict opens
    /// §9.2's dialog; a landed one lets the exit waiting for it go — unless it has notes the
    /// owner must read first, as the hex window keeps open for them.
    pub fn landed(&mut self, saved: &Saved) {
        self.dialogs.file.running = false;
        self.dialogs.copied = None;
        let exit = self.dialogs.pending.take();
        let notes = match saved {
            Saved::Conflict {
                line,
                notes,
                staged,
                overwrite,
            } => {
                self.said = Some(Said::Refused(saved.line()));
                self.prompt(Up::Conflict {
                    line: line.clone(),
                    notes: notes.clone(),
                    staged: staged.clone(),
                    overwrite: *overwrite,
                });
                return;
            }
            Saved::CannotReplace(line) => {
                self.said = Some(Said::Refused(line.clone()));
                return;
            }
            Saved::Written { notes, kept, .. } => !notes.is_empty() || kept.is_some(),
            Saved::Found { notes } => !notes.is_empty(),
        };
        self.said = Some(Said::Warned(saved.line()));
        let Some(exit) = exit else { return };
        if !self.ed.buffer().is_saved() {
            self.prompt(Up::Close(exit));
        } else if notes {
            self.said = Some(Said::Warned(format!(
                "not closed: read this first · {}",
                saved.line()
            )));
        } else {
            self.dialogs.chosen.push(Chosen::Go(exit));
        }
    }

    /// A Save copy or a Save As landed, `line` saying where: the exit waiting for it goes, the
    /// edits being in that file — unless the document changed after it went out (E24), when
    /// the close prompt asks again.
    pub fn elsewhere(&mut self, line: String) {
        self.dialogs.file.running = false;
        self.said = Some(Said::Warned(line));
        let copied = self.dialogs.copied.take();
        let Some(exit) = self.dialogs.pending.take() else {
            return;
        };
        let same = copied.is_none_or(|p| p.as_slice() == self.ed.buffer().pieces());
        if same || self.ed.buffer().is_saved() {
            self.dialogs.chosen.push(Chosen::Go(exit));
        } else {
            self.prompt(Up::Close(exit));
        }
    }

    /// A Save failed (`why`): the edit stays, and the exit waiting for it is dropped.
    pub fn not_saved(&mut self, why: String) {
        self.dialogs.file.running = false;
        self.dialogs.pending = None;
        self.dialogs.copied = None;
        self.said = Some(Said::Refused(why));
    }

    /// The Save, Save copy or Save As… an exit waited for was given up (its modal cancelled):
    /// the exit is dropped, so no later Save takes it.
    pub fn gave_up(&mut self) {
        self.dialogs.file.running = false;
        self.dialogs.pending = None;
        self.dialogs.copied = None;
    }

    /// The dialog a command opens: F10's bar, Find's or Go to's field, Find again; and the
    /// encoding question a typed character raised (E37), whose box is the editor's question.
    pub(super) fn open(&mut self, command: &Command) {
        match command {
            Command::Menu => self.dialogs.open(Up::Menu {
                at: 0,
                open: false,
                item: 0,
            }),
            Command::Find => self.find_field(false),
            Command::FindAgain => match self.dialogs.last.clone() {
                Some((search, replace)) => self.search(search, replace, true),
                None => self.find_field(false),
            },
            Command::GoTo => {
                self.dialogs.open(Up::GoTo(String::new()));
                self.dialogs.focus = true;
            }
            _ => {}
        }
        if self.dialogs.up == Up::None && self.ed.question().is_some() {
            self.dialogs.shown = None;
        }
    }

    /// One pass of the dialogs over the editor's `grid` (its title row and status row at
    /// `rows`), into `buf` before it is painted: a click on a menu's name; the status row's
    /// fields and prompts; the box up, the cells under it blanked first; and every key of the
    /// pass taken while anything is up. The caret to paint: a field's while one has the keys,
    /// none while a menu or a box does, else `caret`.
    ///
    /// It reads the pass's events as the binding left them: per the contract, every event the
    /// binding gave to [`EditView::press`] this pass is already removed, so what is here is
    /// the dialogs' alone.
    pub fn dialogs_ui(
        &mut self,
        ui: &mut egui::Ui,
        grid: &Grid,
        buf: &mut Buffer,
        rows: (u16, u16),
        caret: Option<Position>,
    ) -> Option<Position> {
        let ctx = ui.ctx().clone();
        let wide = cells::ambiguous_wide(&ctx);
        if !self.dialogs.held.is_empty() {
            let held = std::mem::take(&mut self.dialogs.held);
            ctx.input_mut(|i| i.events.splice(0..0, held).for_each(drop));
        }
        self.title_clicks(ui, grid, rows.0, wide);
        if !self.asking() {
            self.dialogs.shown = None;
            return caret;
        }
        // The pass's keys in their order, each stage handing what follows to the next: a menu
        // item may open Find or Go to, a field's Enter the Replace question, a Replace the
        // encoding question's box.
        self.menu_keys(&ctx);
        let caret = match self.dialogs.up {
            Up::Find(_) | Up::GoTo(_) => self.fields(ui, grid, buf, rows.1),
            _ => None,
        };
        self.replace_keys(&ctx);
        self.status_words(buf, rows.1, grid.cols, wide);
        self.dialogs.shown = self.boxed(&ctx, grid, buf, rows, wide);
        // Every key of the pass is the dialog's: none reaches the editor or the binding.
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                use egui::Event as E;
                !matches!(
                    e,
                    E::Key { .. } | E::Text(_) | E::Paste(_) | E::Copy | E::Cut
                )
            })
        });
        caret
    }

    /// A click on a menu's name in the title row opens it (not under a prompt: its modal keeps
    /// the pointer).
    fn title_clicks(&mut self, ui: &mut egui::Ui, grid: &Grid, y: u16, wide: bool) {
        let prompt = matches!(
            self.dialogs.up,
            Up::Close(_) | Up::Conflict { .. } | Up::Reload
        );
        if prompt || self.ed.question().is_some() {
            return;
        }
        for (i, x) in self.menu_places(grid.cols, wide).into_iter().enumerate() {
            let w = cells::width(MENUS[i]) as u16;
            let at = grid.rect(Cells::new(x, y, w, 1));
            let name = ui.interact(at, id("menu-name").with(i), egui::Sense::CLICK);
            if name.clicked() {
                // As from outside the keys: a waiting `^K` or `^S` is disarmed.
                let item = first_on(i, self.dialogs.backend);
                self.prompt(Up::Menu {
                    at: i,
                    open: true,
                    item,
                });
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // The status row.

    /// The status row's words while the menus or Replace's question are up (*editor* §2.3);
    /// in an ambiguous-wide terminal (`wide`) its arrows and `·` are §10's ASCII.
    fn status_words(&self, buf: &mut Buffer, y: u16, cols: u16, wide: bool) {
        let backend = self.dialogs.backend;
        let words = match &self.dialogs.up {
            Up::Menu { open: false, .. } => {
                let names: Vec<&str> = MENUS.iter().map(|m| &m[..1]).collect();
                format!(
                    "←→ choose · Enter opens · Esc closes · a letter: {}",
                    names.join(" ")
                )
            }
            Up::Menu { at, open: true, .. } => {
                let letters: Vec<String> = (items(*at, backend).iter())
                    .filter(|i| i.act != Act::Off)
                    .map(|i| i.letter.to_string())
                    .collect();
                format!(
                    "{}  ↑↓ choose · Enter runs · Esc closes · a letter: {}",
                    MENUS[*at],
                    letters.join(" ")
                )
            }
            // While a Save runs Y is refused (E24): the row says so.
            Up::Replace { .. } if self.dialogs.file.running => {
                format!("{SAVING} · N skips it · Esc stops")
            }
            Up::Replace { .. } => REPLACE_ASK.to_string(),
            _ => return,
        };
        let words = match wide {
            true => (words.replace('·', ".").replace("←→", "<>")).replace("↑↓", "^v"),
            false => words,
        };
        let p = chrome::PALETTE;
        let pen = Pen::new(p.text, p.back);
        cells::fill(buf, Cells::new(0, y, cols, 1), pen);
        let room = usize::from(cols.saturating_sub(2));
        cells::put(buf, 1, y, &cells::fit_start(&words, room), room, pen);
    }

    /// Go to's and Find's fields on the status row `y`, the pass's keys read in their order:
    /// what comes before an Enter, a Tab or an Esc goes to the field that has the keyboard (a
    /// text field's engine, or `Options:`'s letters), then that key moves on — so `o` then
    /// Enter in one pass finds with the `o` (a terminal's paste or fast typing comes in one
    /// pass). A text field's engine runs once a pass: keys for its second turn wait for the next
    /// pass. What follows a field that closed is given back for what is up then. The row drawn,
    /// and the caret of the field with the keyboard.
    fn fields(
        &mut self,
        ui: &mut egui::Ui,
        grid: &Grid,
        buf: &mut Buffer,
        y: u16,
    ) -> Option<Position> {
        let ctx = ui.ctx().clone();
        // Tab is the fields' own: egui moves no focus for it.
        ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        let mut events = take_keys(&ctx);
        let mut ran: Vec<Slot> = Vec::new();
        loop {
            let Some(slot) = self.slot() else {
                // The field closed: what follows is for what is up now.
                put_keys(&ctx, events);
                return None;
            };
            let cut = events.iter().position(moves_on).unwrap_or(events.len());
            let after = events.split_off(cut);
            let segment = std::mem::replace(&mut events, after);
            if slot != Slot::Options && ran.contains(&slot) {
                if !segment.is_empty() {
                    // Its engine has run this pass: the rest waits for the next.
                    self.dialogs.held = segment.into_iter().chain(events).collect();
                    ctx.request_repaint();
                    break;
                }
            } else {
                self.feed(ui, grid, y, slot, segment);
                ran.push(slot);
            }
            if events.is_empty() {
                break;
            }
            let key = events.remove(0);
            self.move_on(&key);
        }
        self.fields_cells(&ctx, grid, buf, y)
    }

    /// The field that has the keyboard, while Go to's or Find's are up.
    fn slot(&self) -> Option<Slot> {
        match &self.dialogs.up {
            Up::GoTo(_) => Some(Slot::GoTo),
            Up::Find(f) => Some(match f.field {
                Field::Needle => Slot::Needle,
                Field::Replace => Slot::Replace,
                Field::Options => Slot::Options,
            }),
            _ => None,
        }
    }

    /// A text field's text.
    fn slot_text(&mut self, slot: Slot) -> Option<&mut String> {
        match (&mut self.dialogs.up, slot) {
            (Up::GoTo(text), Slot::GoTo) => Some(text),
            (Up::Find(f), Slot::Needle) => Some(&mut f.needle),
            (Up::Find(f), Slot::Replace) => f.replace.as_mut(),
            _ => None,
        }
    }

    /// Where the status row's labels and fields stand, as what is up lays them on a row `cols`
    /// wide: Go to's one field; Find's `Find:`, `Replace:` once it is a Replace, `Options:` —
    /// the text fields sharing what the row leaves, each at most 30 cells.
    fn row(&self, cols: u16, y: u16) -> Row {
        let mut row = Row {
            labels: Vec::new(),
            fields: Vec::new(),
        };
        let fit = |x: u16, w: u16| Cells::new(x, y, w.min(cols.saturating_sub(x + 1)), 1);
        let mut x = 1u16;
        let label = |row: &mut Row, x: &mut u16, text: &'static str| {
            row.labels.push((*x, text));
            *x += cells::width(text) as u16;
        };
        match &self.dialogs.up {
            Up::GoTo(_) => {
                label(&mut row, &mut x, "Go to line: ");
                row.fields.push((Slot::GoTo, fit(x, 12)));
            }
            Up::Find(f) => {
                let replacing = f.replace.is_some();
                let fixed =
                    2 + 6 + 10 + usize::from(OPTIONS_CELLS) + if replacing { 10 } else { 0 };
                let room = usize::from(cols).saturating_sub(fixed);
                let each = (room / if replacing { 2 } else { 1 }).clamp(4, 30) as u16;
                label(&mut row, &mut x, "Find: ");
                let at = fit(x, each);
                row.fields.push((Slot::Needle, at));
                x += at.width;
                if replacing {
                    label(&mut row, &mut x, " Replace: ");
                    let at = fit(x, each);
                    row.fields.push((Slot::Replace, at));
                    x += at.width;
                }
                label(&mut row, &mut x, " Options: ");
                row.fields.push((Slot::Options, fit(x, OPTIONS_CELLS)));
            }
            _ => {}
        }
        row
    }

    /// `segment` (keys and text, in order) to the field `slot`: `Options:` takes Turbo's letters
    /// by the physical key (E10) and Backspace; a text field's engine (`ops::dialog`'s) has the
    /// keyboard — its text selected when it has just taken it — and reads them. Only that field
    /// holds egui's focus.
    fn feed(
        &mut self,
        ui: &mut egui::Ui,
        grid: &Grid,
        y: u16,
        slot: Slot,
        segment: Vec<egui::Event>,
    ) {
        let ctx = ui.ctx().clone();
        for other in [Slot::GoTo, Slot::Needle, Slot::Replace] {
            let id = slot_id(other);
            if other != slot && ctx.memory(|m| m.has_focus(id)) {
                ctx.memory_mut(|m| m.surrender_focus(id));
            }
        }
        if slot == Slot::Options {
            let Up::Find(f) = &mut self.dialogs.up else {
                return;
            };
            let mut after_key = false;
            for e in &segment {
                match read(e, &mut after_key) {
                    Read::Letter(l) if OPTION_LETTERS.contains(l) && !f.options.contains(l) => {
                        f.options.push(l)
                    }
                    r if r.is(Key::Backspace, false) => {
                        f.options.pop();
                    }
                    _ => {}
                }
            }
            return;
        }
        let at = self
            .row(grid.cols, y)
            .fields
            .into_iter()
            .find(|(s, _)| *s == slot);
        let Some((_, at)) = at else {
            return;
        };
        let id = slot_id(slot);
        let select = std::mem::take(&mut self.dialogs.focus);
        let Some(text) = self.slot_text(slot) else {
            return;
        };
        if select {
            let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
            let n = egui::text::CCursor::new(text.chars().count());
            let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), n);
            state.cursor.set_char_range(Some(all));
            state.store(&ctx, id);
        }
        if !ctx.memory(|m| m.has_focus(id)) {
            ctx.memory_mut(|m| m.request_focus(id));
        }
        if at.width == 0 {
            return;
        }
        put_keys(&ctx, segment);
        dialog::field(ui, grid, at, id, text, |e| e);
        // What the engine read goes no further.
        drop(take_keys(&ctx));
    }

    /// A key that moves on from a field: Enter goes on (Find: to `Replace:` or `Options:`, and
    /// from there runs it; Go to: there), Tab and Shift+Tab move between Find's fields — Tab from
    /// `Find:` making it a Replace — and Esc closes.
    fn move_on(&mut self, key: &egui::Event) {
        let r = read(key, &mut false);
        let (enter, tab, back) = (
            r.is(Key::Enter, false),
            r.is(Key::Tab, false),
            r.is(Key::Tab, true),
        );
        match std::mem::take(&mut self.dialogs.up) {
            _ if r.is(Key::Escape, false) => {}
            Up::GoTo(text) if enter => self.go_to(&text),
            Up::Find(mut f) => {
                let before = f.field;
                if enter {
                    f.field = match f.field {
                        Field::Needle if f.replace.is_some() => Field::Replace,
                        Field::Needle | Field::Replace => Field::Options,
                        Field::Options => return self.run_find(f),
                    };
                } else if tab {
                    f.field = match f.field {
                        Field::Needle => {
                            f.replace.get_or_insert_with(String::new);
                            Field::Replace
                        }
                        Field::Replace => Field::Options,
                        Field::Options => Field::Needle,
                    };
                } else if back {
                    f.field = match f.field {
                        Field::Needle => Field::Options,
                        Field::Replace => Field::Needle,
                        Field::Options if f.replace.is_some() => Field::Replace,
                        Field::Options => Field::Needle,
                    };
                }
                self.dialogs.focus |= f.field != before;
                self.dialogs.up = Up::Find(f);
            }
            other => self.dialogs.up = other,
        }
    }

    /// The status row with Go to's or Find's labels and fields: the text fields as their
    /// engines hold them, `Options:`'s letters; the caret of the field that has the keyboard.
    fn fields_cells(
        &self,
        ctx: &egui::Context,
        grid: &Grid,
        buf: &mut Buffer,
        y: u16,
    ) -> Option<Position> {
        let p = chrome::PALETTE;
        let pen = Pen::new(p.text, p.back);
        cells::fill(buf, Cells::new(0, y, grid.cols, 1), pen);
        let row = self.row(grid.cols, y);
        for (x, text) in row.labels {
            let room = usize::from(grid.cols.saturating_sub(x + 1));
            cells::put(buf, x, y, text, room, pen);
        }
        let active = self.slot();
        let mut caret = None;
        for (slot, at) in row.fields {
            if at.width == 0 {
                continue;
            }
            let text = match (&self.dialogs.up, slot) {
                (Up::GoTo(text), Slot::GoTo) => text,
                (Up::Find(f), Slot::Needle) => &f.needle,
                (Up::Find(f), Slot::Replace) => f.replace.as_ref().expect("a Replace"),
                (Up::Find(f), Slot::Options) => {
                    let field = Pen::new(p.field_text, cga::BLACK);
                    cells::fill(buf, at, field);
                    cells::put(buf, at.x, y, &f.options, usize::from(at.width), field);
                    if active == Some(Slot::Options) {
                        let w = (f.options.len() as u16).min(at.width - 1);
                        caret = Some(Position::new(at.x + w, y));
                    }
                    continue;
                }
                _ => continue,
            };
            let id = slot_id(slot);
            let focused = active == Some(slot) && ctx.memory(|m| m.has_focus(id));
            if let Some(c) = dialog::field_cells(ctx, buf, at, id, text, focused) {
                caret = Some(c);
            }
        }
        caret
    }

    /// Go to line `text` (from 1): the caret to its start; a word that is no line number says
    /// so. Past the last line, the last.
    fn go_to(&mut self, text: &str) {
        let b = self.ed.buffer();
        match text.trim().parse::<u64>() {
            Ok(n) if n >= 1 => {
                let line = (n - 1).min(b.lines().saturating_sub(1));
                let at = b.line_start(line).unwrap_or(b.len());
                self.edit(|ed, _| ed.place(at, false));
                self.follow = true;
            }
            _ => {
                let line = format!("{} is not a line number", text.trim());
                self.said = Some(Said::Refused(line));
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Find and Replace.

    /// Find's fields, from the last search: its needle (selected, so typing replaces it) and
    /// options; `replace`: Replace…, `Replace:` open.
    fn find_field(&mut self, replace: bool) {
        let (needle, options, text) = match &self.dialogs.last {
            Some((s, r)) => (s.needle.clone(), option_letters(s.options), r.clone()),
            None => Default::default(),
        };
        self.dialogs.open(Up::Find(Find {
            needle,
            replace: replace.then(|| text.unwrap_or_default()),
            options,
            field: Field::Needle,
        }));
        self.dialogs.focus = true;
    }

    /// Enter in `Options:`: the search run, and kept for Ctrl+L.
    fn run_find(&mut self, f: Find) {
        if f.needle.is_empty() {
            return;
        }
        let search = Search {
            needle: f.needle,
            options: Options::parse(&f.options),
        };
        self.dialogs.last = Some((search.clone(), f.replace.clone()));
        self.search(search, f.replace, false);
    }

    /// A Find — the match the block, the caret at its end — or a Replace: all at once with N,
    /// else asking at each match; `again`: from the caret (Ctrl+L, the next match). No Replace
    /// while a Save runs (E24).
    fn search(&mut self, search: Search, replace: Option<String>, again: bool) {
        match replace {
            None => {
                let found = self.edit(|ed, _| ed.find(&search, again));
                self.said = found.is_none().then(|| not_found(&search));
            }
            Some(_) if self.saving() => {}
            Some(text) if search.options.no_ask => {
                let (n, said) = self.edit(|ed, _| ed.replace_all(&search, &text));
                self.said = said.or(Some(match n {
                    0 => not_found(&search),
                    n => replaced(n),
                }));
            }
            Some(text) => self.next_match(search, text, 0, again, None),
        }
    }

    /// The next match asked about, or the end said. Backward, a match that ends past `bound` —
    /// where the earliest replacement made starts — would take some of what was written, and
    /// is passed over; where nothing follows, the caret and the block are as before it (no
    /// block left over the replacement).
    fn next_match(
        &mut self,
        search: Search,
        text: String,
        count: usize,
        mut again: bool,
        bound: Option<u64>,
    ) {
        let before = self.ed.sel();
        let mut skipped = false;
        loop {
            match self.edit(|ed, _| ed.find(&search, again)) {
                Some(at) if bound.is_some_and(|b| at.end > b) => (again, skipped) = (true, true),
                Some(at) => {
                    self.dialogs.up = Up::Replace {
                        search,
                        text,
                        at,
                        count,
                        bound,
                    };
                    return;
                }
                None => {
                    if skipped {
                        self.restore(before);
                    }
                    self.said = Some(match count {
                        0 => not_found(&search),
                        n => replaced(n),
                    });
                    return;
                }
            }
        }
    }

    /// Replace's question, the pass's keys in their order: Y replaces the match and asks at the
    /// next, N skips it, Esc stops; another letter answers nothing. Keys after the question
    /// ended are given back (the encoding question's box may be up).
    fn replace_keys(&mut self, ctx: &egui::Context) {
        if !matches!(self.dialogs.up, Up::Replace { .. }) {
            return;
        }
        let mut events = take_keys(ctx).into_iter();
        let mut after_key = false;
        for e in events.by_ref() {
            let Up::Replace {
                search,
                text,
                at,
                count,
                bound,
            } = std::mem::take(&mut self.dialogs.up)
            else {
                break;
            };
            let r = read(&e, &mut after_key);
            if r.is(Key::Escape, false) {
                self.said = (count > 0).then(|| replaced(count));
            } else if r == Read::Letter('Y') && !self.saving() {
                let said = self.edit(|ed, _| ed.replace_at(at.clone(), &text));
                if matches!(said, Some(Said::Refused(_) | Said::Asked(_))) {
                    // Refused, or the encoding question takes over (E37).
                    self.said = said;
                } else {
                    let bound = search.options.backward.then_some(at.start);
                    if bound.is_some() {
                        self.edit(|ed, _| ed.place(at.start, false));
                    }
                    self.next_match(search, text, count + 1, true, bound);
                }
            } else if r == Read::Letter('N') {
                self.next_match(search, text, count, true, bound);
            } else {
                self.dialogs.up = Up::Replace {
                    search,
                    text,
                    at,
                    count,
                    bound,
                };
                continue;
            }
            if !matches!(self.dialogs.up, Up::Replace { .. }) {
                break;
            }
        }
        put_keys(ctx, after_its_text(events, after_key));
    }

    /// The caret and the block put back as `sel` had them, through the editor's own block
    /// markers (Ctrl+K B, K, H): no edit, no undo step. No block becomes an empty one at the
    /// caret, which shows nothing.
    fn restore(&mut self, sel: Sel) {
        self.edit(|ed, _| {
            let b = sel.block.unwrap_or(Block {
                begin: sel.caret,
                end: sel.caret,
                hidden: false,
            });
            ed.place(b.begin, false);
            ed.block(BlockOp::Begin);
            ed.place(b.end, false);
            ed.block(BlockOp::End);
            if b.hidden {
                ed.block(BlockOp::Hide);
            }
            ed.place(sel.caret, false);
        });
    }

    /// While a Save runs no edit is made (E24): `true`, and the status row says why.
    fn saving(&mut self) -> bool {
        if self.dialogs.file.running {
            self.said = Some(Said::Refused(SAVING.to_string()));
        }
        self.dialogs.file.running
    }

    // -----------------------------------------------------------------------------------------
    // The menus.

    /// F10's menus, the pass's keys read in their order against the state each one leaves — a
    /// letter that drops a menu makes the next letter that menu's item:
    /// - the bar: ← → move it, a name's letter, Enter or ↓ drops that menu, Esc or F10 closes;
    /// - a menu: ↑ ↓ move the keyboard over the items that have a command, ← → to the next
    ///   menu, Enter, Space or an item's letter runs it, Esc goes back to the bar, F10 closes.
    ///
    /// Keys after an item ran are given back for what it opened (Find's field).
    fn menu_keys(&mut self, ctx: &egui::Context) {
        let Up::Menu {
            mut at,
            mut open,
            mut item,
        } = self.dialogs.up
        else {
            return;
        };
        let backend = self.dialogs.backend;
        if open {
            // The row a click gave the keyboard.
            let rows = items(at, backend).len();
            if let Some(i) = (0..rows).find(|&i| ctx.memory(|m| m.has_focus(row_id(at, i)))) {
                item = i;
            }
        }
        // The keyboard is the menus' to place: egui's own arrow moves are not made.
        ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        let n = MENUS.len();
        let mut events = take_keys(ctx).into_iter();
        let mut after_key = false;
        while let Some(e) = events.next() {
            let r = read(&e, &mut after_key);
            let its = items(at, backend);
            let on = |i: usize| its.get(i).is_some_and(|it| it.act != Act::Off);
            let mut chosen = None;
            if r.is(Key::F10, false) || (r.is(Key::Escape, false) && !open) {
                self.dialogs.up = Up::None;
                return;
            } else if r.is(Key::Escape, false) {
                (open, item) = (false, 0);
            } else if r.is(Key::ArrowLeft, false) || r.is(Key::ArrowRight, false) {
                let by = if r.is(Key::ArrowLeft, false) {
                    n - 1
                } else {
                    1
                };
                at = (at + by) % n;
                item = first_on(at, backend);
            } else if r.is(Key::ArrowDown, false) && open {
                item = next_on(&its, item, 1);
            } else if r.is(Key::ArrowUp, false) && open {
                item = next_on(&its, item, -1);
            } else if r.is(Key::Enter, false)
                || r.is(Key::Space, false)
                || r.is(Key::ArrowDown, false)
            {
                match open {
                    true if !r.is(Key::ArrowDown, false) => chosen = Some(item).filter(|&i| on(i)),
                    true => {}
                    false => (open, item) = (true, first_on(at, backend)),
                }
            } else if let Read::Letter(l) = r {
                if open {
                    chosen = (0..its.len()).find(|&i| its[i].letter == l && on(i));
                } else if let Some(i) = MENUS.iter().position(|m| m.starts_with(l)) {
                    (at, open, item) = (i, true, first_on(i, backend));
                }
            }
            if let Some(i) = chosen {
                self.choose(at, i);
                put_keys(ctx, after_its_text(events, after_key));
                return;
            }
        }
        self.dialogs.up = Up::Menu { at, open, item };
    }

    /// Item `i` of menu `menu` chosen: the menus close and it runs. An edit is made here as its
    /// key makes it — none while a Save runs (E24) — and Copy and Cut are handed back too, for
    /// the binding to put the text on the clipboard as for their keys.
    fn choose(&mut self, menu: usize, i: usize) {
        self.dialogs.up = Up::None;
        let Some(item) = items(menu, self.dialogs.backend).into_iter().nth(i) else {
            return;
        };
        match item.act {
            Act::Bind(command) => {
                if matches!(command, Command::SaveCopy | Command::SaveAs) {
                    // What the copy holds, until it reports (`elsewhere`).
                    self.dialogs.copied = Some(self.ed.buffer().pieces().to_vec());
                }
                self.commands.push(command);
            }
            Act::Edit(command) if super::changes(&command) && self.saving() => {}
            Act::Edit(command) => {
                self.said = None;
                self.run(&command);
                if matches!(command, Command::Copy | Command::Cut) {
                    self.commands.push(command);
                }
            }
            Act::Find => self.find_field(false),
            Act::Replace => self.find_field(true),
            Act::FindAgain => self.open(&Command::FindAgain),
            Act::GoTo => self.open(&Command::GoTo),
            Act::Indent => {
                let on = !self.ed.indent();
                self.ed.set_indent(on);
            }
            Act::Column => self.column_clicked = true,
            Act::Off => {}
        }
    }

    // -----------------------------------------------------------------------------------------
    // The boxes.

    /// The box up now: a menu's items under its name, or a prompt between the title and status
    /// rows.
    fn box_of(&self, grid: &Grid, (title, status): (u16, u16), wide: bool) -> Option<Boxed> {
        let area = Grid {
            origin: grid.origin + egui::vec2(0.0, f32::from(title + 1) * CELL.y),
            cols: grid.cols,
            rows: status.saturating_sub(title + 1),
        };
        let at = (0, title + 1);
        let prompt = |key: &str, heading: &str, mut parts: Vec<Part>, buttons, after| {
            let id = id(key);
            let buttons: Vec<(&str, Choice)> = buttons;
            let all: Vec<(egui::Id, Choice, bool)> = (buttons.iter())
                .map(|(label, c)| (id.with(*label), *c, true))
                .collect();
            parts.push(Part::Blank);
            parts.push(Part::Buttons(
                (buttons.iter().zip(&all))
                    .map(|((label, _), (b, ..))| Button::new(label, *b))
                    .collect(),
            ));
            parts.extend::<Vec<Part>>(after);
            Boxed {
                id,
                layout: dialog::layout(area, heading, WIDE, &parts),
                at,
                buttons: all,
                menu: false,
            }
        };
        let text = |t: String| Part::Text(t, Ink::Text);
        Some(match &self.dialogs.up {
            Up::Menu {
                at: menu,
                open: true,
                ..
            } => self.menu_box(grid, area, *menu, (title, status), wide),
            Up::Close(exit) => {
                let shown = &self.dialogs.file.shown;
                let question = match exit {
                    Exit::Reread => format!("Save the changes to {shown} before reading it again?"),
                    _ => format!("Save the changes to {shown} before closing?"),
                };
                let save = match self.dialogs.file.can_replace {
                    true => ("Save", Choice::Save),
                    false => ("Save copy", Choice::SaveCopy),
                };
                let buttons = vec![
                    save,
                    ("Save As…", Choice::SaveAs),
                    ("Discard", Choice::Discard),
                    ("Cancel", Choice::Cancel),
                ];
                let after = (self.dialogs.file.journal.as_deref())
                    .map(|dir| {
                        let tilde = crate::gui::binary::tilde(dir);
                        let line = format!("Discard deletes the kept edits at {tilde}");
                        Part::Text(line, Ink::Weak)
                    })
                    .into_iter()
                    .collect();
                prompt("close", PROMPT_TITLE, vec![text(question)], buttons, after)
            }
            Up::Conflict {
                line,
                notes,
                staged,
                overwrite,
            } => {
                let mut parts = vec![text(line.clone())];
                parts.extend(notes.iter().map(|n| Part::Text(n.clone(), Ink::Weak)));
                // Nothing was written over the file: the staged copy is kept, and named.
                let kept = crate::gui::binary::tilde(staged);
                parts.push(Part::Text(
                    format!("your edit is kept at {kept}"),
                    Ink::Weak,
                ));
                let mut buttons = vec![("Reload", Choice::Reload)];
                if *overwrite {
                    buttons.push(("Overwrite", Choice::Overwrite));
                }
                buttons.extend([("Save As…", Choice::SaveAs), ("Cancel", Choice::Cancel)]);
                prompt("conflict", "Changed on disk", parts, buttons, vec![])
            }
            Up::Reload => {
                let ask = format!("Discard your changes and read {} again?", self.name);
                let buttons = vec![("Reload", Choice::Reload), ("Cancel", Choice::Cancel)];
                prompt("reload", "Reload", vec![text(ask)], buttons, vec![])
            }
            Up::None => {
                let ask = self.ed.question()?;
                let buttons = vec![("Type", Choice::Type), ("Cancel", Choice::Cancel)];
                prompt("question", "Encoding", vec![text(ask)], buttons, vec![])
            }
            _ => return None,
        })
    }

    /// Menu `menu`'s items as one-button rows (the encoding list's way), the item's keys at the
    /// right; laid on a grid of the box's own size just under the menu's name, so the kit puts
    /// it there (`area`, centred, where the room is too small).
    fn menu_box(
        &self,
        grid: &Grid,
        area: Grid,
        menu: usize,
        (title, status): (u16, u16),
        wide: bool,
    ) -> Boxed {
        let its = items(menu, self.dialogs.backend);
        let lw = its.iter().map(|i| cells::width(i.label)).max().unwrap_or(0);
        let kw = its.iter().map(|i| cells::width(&i.key)).max().unwrap_or(0);
        let mut buttons = Vec::new();
        let rows: Vec<Part> = (its.iter().enumerate())
            .map(|(i, it)| {
                let pad = " ".repeat(lw - cells::width(it.label) + 2 + kw - cells::width(&it.key));
                let label = format!("{}{pad}{}", it.label, it.key);
                let mut b = Button::new(&label, row_id(menu, i));
                b.on = it.act != Act::Off;
                buttons.push((b.id, Choice::Item(i), b.on));
                Part::Buttons(vec![b])
            })
            .collect();
        // The kit's box: its inside, a border and a margin each side; its shadow two cells
        // right and one down. On a grid that much larger it stands at (1, 0).
        let inside = lw + 2 + kw + 4;
        let (w, h) = ((inside + 8) as u16, (its.len() + 5) as u16);
        let name = (self.menu_places(grid.cols, wide).get(menu).copied()).unwrap_or(0);
        let x = name.saturating_sub(3).min(grid.cols.saturating_sub(w));
        let fits = w <= grid.cols && h <= status.saturating_sub(title + 1);
        let (on, at) = match fits {
            true => {
                let offset = egui::vec2(f32::from(x) * CELL.x, f32::from(title + 1) * CELL.y);
                let sub = Grid {
                    origin: grid.origin + offset,
                    cols: w,
                    rows: h,
                };
                (sub, (x, title + 1))
            }
            false => (area, (0, title + 1)),
        };
        Boxed {
            id: id("menu").with(menu),
            layout: dialog::layout(on, "", 0, &rows),
            at,
            buttons,
            menu: true,
        }
    }

    /// The box up, shown in this pass (`ops::dialog`): the cells under it blanked in `buf`
    /// first, so a wide glyph half under its edge goes as a terminal erases it; the keyboard
    /// placed — a new prompt's on Cancel (E6), ← → moving it — then what its buttons chose.
    /// Its id, while one is up.
    fn boxed(
        &mut self,
        ctx: &egui::Context,
        grid: &Grid,
        buf: &mut Buffer,
        rows: (u16, u16),
        wide: bool,
    ) -> Option<egui::Id> {
        let b = self.box_of(grid, rows, wide)?;
        let body = Pen::new(chrome::PALETTE.text, chrome::PALETTE.back);
        for c in b.layout.covers() {
            let under = Cells::new(c.x + b.at.0, c.y + b.at.1, c.width, c.height);
            cells::fill(buf, under, body);
        }
        let on: Vec<egui::Id> = (b.buttons.iter().filter(|(.., on)| *on))
            .map(|(id, ..)| *id)
            .collect();
        if let (true, Up::Menu { at, item, .. }) = (b.menu, &self.dialogs.up) {
            let row = row_id(*at, *item);
            ctx.memory_mut(|m| m.request_focus(row));
        } else if !on.is_empty() {
            // A new prompt, or one whose buttons lost the keyboard: Cancel (the last) has it.
            // In the pass a prompt opens no key moves the keyboard off Cancel — not ← →, not
            // Tab, which egui applies itself — so Ctrl+R then ← in one read leaves it on Cancel
            // (E6); Enter there presses Cancel.
            let new = self.dialogs.shown != Some(b.id);
            let has = on.iter().position(|id| ctx.memory(|m| m.has_focus(*id)));
            let mut i = match has {
                Some(i) if !new => i,
                _ => on.len() - 1,
            };
            // The box places the keyboard: egui's own arrow move is not made — nor, in its
            // first pass, Tab's.
            let arrows = ctx.input(|inp| {
                inp.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key {
                            key: Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown,
                            pressed: true,
                            ..
                        }
                    )
                })
            });
            if !new {
                if take(ctx, Key::ArrowLeft, false) {
                    i = i.saturating_sub(1);
                }
                if take(ctx, Key::ArrowRight, false) {
                    i = (i + 1).min(on.len() - 1);
                }
            }
            if new || arrows || has != Some(i) {
                ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
            }
            if has != Some(i) {
                ctx.memory_mut(|m| m.request_focus(on[i]));
            }
        }
        // egui lays an area out unseen the first time it shows (its sizing pass), where no
        // control may hold the keyboard: given its size first, the box has its keyboard from the
        // pass it opens in.
        if egui::AreaState::load(ctx, b.id).is_none() {
            let frame = b.layout.covers()[0];
            let size = egui::vec2(
                f32::from(frame.width) * CELL.x,
                f32::from(frame.height) * CELL.y,
            );
            let layer = egui::LayerId::new(egui::Order::Foreground, b.id);
            let state = egui::AreaState {
                size: Some(size),
                interactable: true,
                ..Default::default()
            };
            ctx.memory_mut(|m| m.areas_mut().set_state(layer, state));
        }
        let shown = dialog::show(ctx, b.id, &b.layout, Inputs::default());
        let choice = (b.buttons.iter())
            .find(|(id, ..)| shown.clicked(*id))
            .map(|(_, c, _)| *c);
        let closed = shown.modal.should_close();
        self.answer(choice, closed);
        Some(b.id)
    }

    /// What a box's button (or Esc, or a click outside it: `closed`) chose.
    fn answer(&mut self, choice: Option<Choice>, closed: bool) {
        let up = std::mem::take(&mut self.dialogs.up);
        let question = up == Up::None && self.ed.question().is_some();
        let choice = match choice {
            None if !closed => {
                self.dialogs.up = up;
                return;
            }
            None | Some(Choice::Cancel) => {
                if question {
                    self.edit(|ed, _| ed.answer(Answer::Cancel));
                    self.said = None;
                }
                return;
            }
            Some(choice) => choice,
        };
        let commands = &mut self.commands;
        match (choice, up) {
            (Choice::Save | Choice::SaveCopy | Choice::SaveAs, Up::Close(exit)) => {
                commands.push(match choice {
                    Choice::Save => Command::Save,
                    Choice::SaveCopy => Command::SaveCopy,
                    _ => Command::SaveAs,
                });
                self.dialogs.pending = Some(exit);
                // What the copy holds: an edit after it is in no file (`elsewhere`).
                let pieces = self.ed.buffer().pieces().to_vec();
                self.dialogs.copied = (choice != Choice::Save).then_some(pieces);
            }
            (Choice::Discard, Up::Close(exit)) => self.dialogs.chosen.push(Chosen::Discard(exit)),
            (Choice::Reload, Up::Conflict { .. }) => self.dialogs.up = Up::Reload,
            (Choice::Overwrite, Up::Conflict { .. }) => self.dialogs.chosen.push(Chosen::Overwrite),
            (Choice::SaveAs, Up::Conflict { .. }) => commands.push(Command::SaveAs),
            (Choice::Reload, Up::Reload) => self.dialogs.chosen.push(Chosen::Reload),
            // Refused while a Save runs: the question stays up.
            (Choice::Type, Up::None) if self.saving() => {}
            (Choice::Type, Up::None) => {
                let said = self.edit(|ed, _| ed.answer(Answer::Type));
                self.said = said;
            }
            (Choice::Item(i), Up::Menu { at, .. }) => self.choose(at, i),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
