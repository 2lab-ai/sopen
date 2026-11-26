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
//!   it, Esc is Cancel, ← → Tab and Shift+Tab move it, and a letter answers nothing.
//! - **The menus** (F10, or a click on a name in the title row): the bar on a name — ← →, a
//!   name's letter, Enter or ↓ drops its items — then the items as a box under the name (the
//!   kit's, laid on a grid of its own size there), the bar's row with the keyboard (↑ ↓; ← → the
//!   next menu); an item's letter runs it by the physical key or a jamo, so F10 F S saves and
//!   F10 E R redoes in either input source; the status row names the letters (*editor* §2.3).
//!   Esc closes the innermost. Items with no command yet are off: Delete to line end and
//!   Matching bracket have no operation in `edit::ops`, and the Git menu is ED-git's.
//! - **While anything here is up it has every key**: the pass's keys go to it and no further,
//!   and [`EditView::press`] takes none but a focus report or a resize (E31's Shift known).
//! - **Exits** (§6.4, §9.6): the binding gives each to [`EditView::leave`]. With unsaved edits
//!   the close prompt asks; a running Save holds the exit until it reports (07 §10). What was
//!   chosen comes back as [`Chosen`] — or, for Save, Save copy and Save As…, as the command
//!   ([`EditView::take_commands`]), whose end [`EditView::landed`], [`EditView::elsewhere`],
//!   [`EditView::not_saved`] or [`EditView::gave_up`] reports, the exit going once a Save lands.
//!   The key that opened a dialog is dropped from the pass it opens in.

use super::{EditView, MENUS};
use crate::edit::arm::{self, Backend, BlockOp, Command};
use crate::edit::file::Saved;
use crate::edit::ops::{Answer, Options, Said, Search};
use crate::gui::browser::ops::dialog::{self, Button, Ink, Inputs, Part};
use crate::gui::cells::{self, Buffer, Cells, Grid, Pen, Position};
use crate::gui::chrome::{self, cga, CELL};
use eframe::egui::{self, Key};
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
    /// Replace's question at the match `at`, `count` replaced so far.
    Replace {
        search: Search,
        text: String,
        at: Range<u64>,
        count: usize,
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
    /// The key of the editor's last press, until the dialogs' next pass.
    pressed: Option<Key>,
    /// The key a dialog was opened by (an F10, the Esc that led to an exit): dropped from the
    /// pass it opens in, so the same event never answers it too — an F10 closing its own bar,
    /// an Esc cancelling the prompt it brought — whether the binding gives its keys to
    /// [`EditView::press`] before or after the dialogs' pass.
    opener: Option<Key>,
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
            pressed: None,
            opener: None,
        }
    }

    /// The editor's keys took `press` (its key kept for a dialog it may open).
    pub(super) fn pressed(&mut self, press: &arm::Press) {
        self.pressed = match press {
            arm::Press::Key { key, .. } => Some(*key),
            _ => None,
        };
    }

    /// `up` opened from outside the dialogs' pass (a key, an exit, a Save's end).
    fn open(&mut self, up: Up) {
        self.up = up;
        self.opener = self.pressed;
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

/// This pass's letters, taken (§5.5, E10): a letter key with no Ctrl, ⌘ or Alt by its physical
/// key first — the text egui sends after it dropped — or typed text that is one letter or a
/// 2-set jamo (a terminal's, or the input method's).
fn letters(ctx: &egui::Context) -> Vec<char> {
    ctx.input_mut(|i| {
        let mut out = Vec::new();
        let mut after_key = false;
        i.events.retain(|e| match e {
            egui::Event::Key {
                key,
                physical_key,
                pressed: true,
                modifiers,
                ..
            } => {
                after_key = false;
                if modifiers.ctrl || modifiers.command || modifiers.mac_cmd || modifiers.alt {
                    return true;
                }
                match physical_key
                    .and_then(key_letter)
                    .or_else(|| key_letter(*key))
                {
                    Some(l) => {
                        out.push(l);
                        after_key = true;
                        false
                    }
                    None => true,
                }
            }
            egui::Event::Text(text) => {
                if std::mem::take(&mut after_key) {
                    return false;
                }
                match arm::letter(text) {
                    Some(l) => {
                        out.push(l);
                        false
                    }
                    None => true,
                }
            }
            _ => true,
        });
        out
    })
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
        self.dialogs.open(Up::Close(exit));
        false
    }

    /// A Save ended (`Session::save`, `overwrite`): its line on the status row; a conflict opens
    /// §9.2's dialog; a landed one lets the exit waiting for it go — unless it has notes the
    /// owner must read first, as the hex window keeps open for them.
    pub fn landed(&mut self, saved: &Saved) {
        self.dialogs.file.running = false;
        let exit = self.dialogs.pending.take();
        let notes = match saved {
            Saved::Conflict {
                line,
                notes,
                staged,
                overwrite,
            } => {
                self.said = Some(Said::Refused(saved.line()));
                self.dialogs.open(Up::Conflict {
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
            self.dialogs.open(Up::Close(exit));
        } else if notes {
            self.said = Some(Said::Warned(format!(
                "not closed: read this first · {}",
                saved.line()
            )));
        } else {
            self.dialogs.chosen.push(Chosen::Go(exit));
        }
    }

    /// A Save copy or a Save As landed, `line` saying where: the exit waiting for it goes (the
    /// edits are in that file).
    pub fn elsewhere(&mut self, line: String) {
        self.dialogs.file.running = false;
        self.said = Some(Said::Warned(line));
        if let Some(exit) = self.dialogs.pending.take() {
            self.dialogs.chosen.push(Chosen::Go(exit));
        }
    }

    /// A Save failed (`why`): the edit stays, and the exit waiting for it is dropped.
    pub fn not_saved(&mut self, why: String) {
        self.dialogs.file.running = false;
        self.dialogs.pending = None;
        self.said = Some(Said::Refused(why));
    }

    /// The Save, Save copy or Save As… an exit waited for was given up (its modal cancelled):
    /// the exit is dropped, so no later Save takes it.
    pub fn gave_up(&mut self) {
        self.dialogs.file.running = false;
        self.dialogs.pending = None;
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
            self.dialogs.opener = self.dialogs.pressed;
        }
    }

    /// One pass of the dialogs over the editor's `grid` (its title row and status row at
    /// `rows`), into `buf` before it is painted: a click on a menu's name; the status row's
    /// fields and prompts; the box up, the cells under it blanked first; and every key of the
    /// pass taken while anything is up. The caret to paint: a field's while one has the keys,
    /// none while a menu or a box does, else `caret`.
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
        if let Some(key) = self.dialogs.opener.take() {
            ctx.input_mut(|i| {
                i.events.retain(
                    |e| !matches!(e, egui::Event::Key { key: k, pressed: true, .. } if *k == key),
                )
            });
        }
        self.dialogs.pressed = None;
        self.title_clicks(ui, grid, rows.0, wide);
        if !self.asking() {
            self.dialogs.shown = None;
            return caret;
        }
        let caret = match self.dialogs.up {
            Up::Find(_) | Up::GoTo(_) => self.fields(ui, grid, buf, rows.1),
            Up::Replace { .. } => {
                self.replace_keys(&ctx);
                None
            }
            Up::Menu { open: false, .. } => {
                self.bar_keys(&ctx);
                None
            }
            Up::Menu { open: true, .. } => {
                self.menu_keys(&ctx);
                None
            }
            _ => None,
        };
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
                let item = first_on(i, self.dialogs.backend);
                self.dialogs.up = Up::Menu {
                    at: i,
                    open: true,
                    item,
                };
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

    /// Go to's and Find's fields on the status row `y`: their keys first (Enter, Esc, Tab), then
    /// each field's engine at its cells and the row drawn. The caret of the field that has the
    /// keyboard.
    fn fields(
        &mut self,
        ui: &mut egui::Ui,
        grid: &Grid,
        buf: &mut Buffer,
        y: u16,
    ) -> Option<Position> {
        let ctx = ui.ctx().clone();
        let p = chrome::PALETTE;
        let pen = Pen::new(p.text, p.back);
        cells::fill(buf, Cells::new(0, y, grid.cols, 1), pen);
        let enter = take(&ctx, Key::Enter, false);
        let esc = take(&ctx, Key::Escape, false);
        let tab = take(&ctx, Key::Tab, false);
        let back = take(&ctx, Key::Tab, true);
        if tab || back {
            // Tab is the fields' own: egui moves no focus for it.
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
        let label = |buf: &mut Buffer, x: &mut u16, text: &str| {
            let room = usize::from(grid.cols.saturating_sub(*x + 1));
            *x += cells::put(buf, *x, y, text, room, pen) as u16;
        };
        match std::mem::take(&mut self.dialogs.up) {
            Up::GoTo(mut text) => {
                if esc {
                    return None;
                }
                if enter {
                    self.go_to(&text);
                    return None;
                }
                let mut x = 1;
                label(buf, &mut x, "Go to line: ");
                let at = Cells::new(x, y, 12.min(grid.cols.saturating_sub(x + 1)), 1);
                let caret = self.text_field(ui, grid, buf, at, id("goto"), &mut text, true);
                self.dialogs.up = Up::GoTo(text);
                caret
            }
            Up::Find(mut f) => {
                if esc {
                    return None;
                }
                let before = f.field;
                if enter {
                    f.field = match f.field {
                        Field::Needle if f.replace.is_some() => Field::Replace,
                        Field::Needle | Field::Replace => Field::Options,
                        Field::Options => {
                            self.run_find(f);
                            return None;
                        }
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
                if f.field == Field::Options {
                    // Turbo's letters by the physical key, the input method off (E10).
                    for l in letters(&ctx) {
                        if OPTION_LETTERS.contains(l) && !f.options.contains(l) {
                            f.options.push(l);
                        }
                    }
                    if take(&ctx, Key::Backspace, false) {
                        f.options.pop();
                    }
                }
                let caret = self.find_cells(ui, grid, buf, y, &mut f);
                self.dialogs.up = Up::Find(f);
                caret
            }
            other => {
                self.dialogs.up = other;
                None
            }
        }
    }

    /// Find's row: `Find:` and its field, `Replace:` and its once it is a Replace, `Options:`
    /// and its letters; the fields share what the row leaves, each at most 30 cells.
    fn find_cells(
        &mut self,
        ui: &mut egui::Ui,
        grid: &Grid,
        buf: &mut Buffer,
        y: u16,
        f: &mut Find,
    ) -> Option<Position> {
        let p = chrome::PALETTE;
        let pen = Pen::new(p.text, p.back);
        let labels = ["Find: ", " Replace: ", " Options: "];
        let replacing = f.replace.is_some();
        let fixed = 2 + cells::width(labels[0]) + cells::width(labels[2]) + 7;
        let fixed = fixed
            + if replacing {
                cells::width(labels[1])
            } else {
                0
            };
        let room = usize::from(grid.cols).saturating_sub(fixed);
        let each = (room / if replacing { 2 } else { 1 }).clamp(4, 30) as u16;
        let mut x = 1;
        let put = |buf: &mut Buffer, x: &mut u16, text: &str| {
            let room = usize::from(grid.cols.saturating_sub(*x + 1));
            *x += cells::put(buf, *x, y, text, room, pen) as u16;
        };
        let fit = |x: u16, w: u16| Cells::new(x, y, w.min(grid.cols.saturating_sub(x + 1)), 1);
        put(buf, &mut x, labels[0]);
        let needle_at = fit(x, each);
        x += needle_at.width;
        let mut caret = self.text_field(
            ui,
            grid,
            buf,
            needle_at,
            id("needle"),
            &mut f.needle,
            f.field == Field::Needle,
        );
        if let Some(text) = f.replace.as_mut() {
            put(buf, &mut x, labels[1]);
            let at = fit(x, each);
            x += at.width;
            let chosen = f.field == Field::Replace;
            let c = self.text_field(ui, grid, buf, at, id("replace"), text, chosen);
            caret = caret.or(c);
        }
        put(buf, &mut x, labels[2]);
        let at = fit(x, OPTIONS_CELLS);
        let field = Pen::new(p.field_text, cga::BLACK);
        cells::fill(buf, at, field);
        cells::put(buf, at.x, y, &f.options, usize::from(at.width), field);
        if f.field == Field::Options && at.width > 0 {
            let w = (f.options.len() as u16).min(at.width - 1);
            caret = Some(Position::new(at.x + w, y));
        }
        caret
    }

    /// A text field `id` at cells `at` holding `text` (`ops::dialog`'s): while `chosen` it has
    /// the keyboard — its text selected when it has just taken it — else it lets it go. Its
    /// caret, while it has the keyboard.
    #[allow(clippy::too_many_arguments)]
    fn text_field(
        &mut self,
        ui: &mut egui::Ui,
        grid: &Grid,
        buf: &mut Buffer,
        at: Cells,
        id: egui::Id,
        text: &mut String,
        chosen: bool,
    ) -> Option<Position> {
        let ctx = ui.ctx().clone();
        if chosen {
            if std::mem::take(&mut self.dialogs.focus) {
                let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
                let n = egui::text::CCursor::new(text.chars().count());
                let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), n);
                state.cursor.set_char_range(Some(all));
                state.store(&ctx, id);
            }
            if !ctx.memory(|m| m.has_focus(id)) {
                ctx.memory_mut(|m| m.request_focus(id));
            }
        } else if ctx.memory(|m| m.has_focus(id)) {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        if at.width == 0 {
            return None;
        }
        dialog::field(ui, grid, at, id, text, |e| e);
        let focused = chosen && ctx.memory(|m| m.has_focus(id));
        dialog::field_cells(&ctx, buf, at, id, text, focused)
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
    /// else asking at each match; `again`: from the caret (Ctrl+L, the next match).
    fn search(&mut self, search: Search, replace: Option<String>, again: bool) {
        match replace {
            None => {
                let found = self.edit(|ed, _| ed.find(&search, again));
                self.said = found.is_none().then(|| not_found(&search));
            }
            Some(text) if search.options.no_ask => {
                let (n, said) = self.edit(|ed, _| ed.replace_all(&search, &text));
                self.said = said.or(Some(match n {
                    0 => not_found(&search),
                    n => replaced(n),
                }));
            }
            Some(text) => self.next_match(search, text, 0, again),
        }
    }

    /// The next match asked about, or the end said.
    fn next_match(&mut self, search: Search, text: String, count: usize, again: bool) {
        match self.edit(|ed, _| ed.find(&search, again)) {
            Some(at) => {
                self.dialogs.up = Up::Replace {
                    search,
                    text,
                    at,
                    count,
                }
            }
            None if count == 0 => self.said = Some(not_found(&search)),
            None => self.said = Some(replaced(count)),
        }
    }

    /// Replace's question: Y replaces the match and asks at the next, N skips it, Esc stops;
    /// another letter answers nothing.
    fn replace_keys(&mut self, ctx: &egui::Context) {
        let Up::Replace {
            search,
            text,
            at,
            count,
        } = std::mem::take(&mut self.dialogs.up)
        else {
            return;
        };
        if take(ctx, Key::Escape, false) {
            self.said = (count > 0).then(|| replaced(count));
            return;
        }
        for l in letters(ctx) {
            match l {
                'Y' => {
                    let said = self.edit(|ed, _| ed.replace_at(at.clone(), &text));
                    if matches!(said, Some(Said::Refused(_) | Said::Asked(_))) {
                        // Refused, or the encoding question takes over (E37).
                        self.said = said;
                        return;
                    }
                    if search.options.backward {
                        self.edit(|ed, _| ed.place(at.start, false));
                    }
                    return self.next_match(search, text, count + 1, true);
                }
                'N' => return self.next_match(search, text, count, true),
                _ => {}
            }
        }
        self.dialogs.up = Up::Replace {
            search,
            text,
            at,
            count,
        };
    }

    // -----------------------------------------------------------------------------------------
    // The menus.

    /// F10's bar: ← → move it, a name's letter, Enter or ↓ drops that menu, Esc or F10 closes.
    fn bar_keys(&mut self, ctx: &egui::Context) {
        let Up::Menu { mut at, .. } = self.dialogs.up else {
            return;
        };
        let backend = self.dialogs.backend;
        if take(ctx, Key::Escape, false) || take(ctx, Key::F10, false) {
            self.dialogs.up = Up::None;
            return;
        }
        if take(ctx, Key::ArrowLeft, false) {
            at = (at + MENUS.len() - 1) % MENUS.len();
        }
        if take(ctx, Key::ArrowRight, false) {
            at = (at + 1) % MENUS.len();
        }
        let mut open = take(ctx, Key::Enter, false) || take(ctx, Key::ArrowDown, false);
        for l in letters(ctx) {
            if let Some(i) = MENUS.iter().position(|m| m.starts_with(l)) {
                (at, open) = (i, true);
            }
        }
        let item = first_on(at, backend);
        self.dialogs.up = Up::Menu { at, open, item };
    }

    /// A menu's items: ↑ ↓ move the keyboard over the items that have a command, ← → to the
    /// next menu, Enter or an item's letter runs it, Esc goes back to the bar, F10 closes.
    fn menu_keys(&mut self, ctx: &egui::Context) {
        let Up::Menu {
            mut at, mut item, ..
        } = self.dialogs.up
        else {
            return;
        };
        let backend = self.dialogs.backend;
        if take(ctx, Key::F10, false) {
            self.dialogs.up = Up::None;
            return;
        }
        if take(ctx, Key::Escape, false) {
            let item = 0;
            self.dialogs.up = Up::Menu {
                at,
                open: false,
                item,
            };
            return;
        }
        let mut its = items(at, backend);
        // The row a click or Tab gave the keyboard.
        if let Some(i) = (0..its.len()).find(|&i| ctx.memory(|m| m.has_focus(row_id(at, i)))) {
            item = i;
        }
        while take(ctx, Key::ArrowDown, false) {
            item = next_on(&its, item, 1);
        }
        while take(ctx, Key::ArrowUp, false) {
            item = next_on(&its, item, -1);
        }
        let step = |at: usize, by: usize| (at + by) % MENUS.len();
        let left = take(ctx, Key::ArrowLeft, false);
        let right = take(ctx, Key::ArrowRight, false);
        if left || right {
            at = step(at, if left { MENUS.len() - 1 } else { 1 });
            item = first_on(at, backend);
            its = items(at, backend);
        }
        // The keyboard is the menu's to place: egui's own arrow moves are not.
        ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        if take(ctx, Key::Enter, false) {
            return self.choose(at, item);
        }
        for l in letters(ctx) {
            let on = |i: &Item| i.letter == l && i.act != Act::Off;
            if let Some(i) = its.iter().position(on) {
                return self.choose(at, i);
            }
        }
        self.dialogs.up = Up::Menu {
            at,
            open: true,
            item,
        };
    }

    /// Item `i` of menu `menu` chosen: the menus close and it runs.
    fn choose(&mut self, menu: usize, i: usize) {
        self.dialogs.up = Up::None;
        let Some(item) = items(menu, self.dialogs.backend).into_iter().nth(i) else {
            return;
        };
        match item.act {
            Act::Bind(command) => self.commands.push(command),
            Act::Edit(command) => {
                self.said = None;
                self.run(&command);
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
            let has = on.iter().position(|id| ctx.memory(|m| m.has_focus(*id)));
            let mut i = match has {
                Some(i) if self.dialogs.shown == Some(b.id) => i,
                _ => on.len() - 1,
            };
            let left = take(ctx, Key::ArrowLeft, false);
            let right = take(ctx, Key::ArrowRight, false);
            if left {
                i = i.saturating_sub(1);
            }
            if right {
                i = (i + 1).min(on.len() - 1);
            }
            if left || right {
                // The box places the keyboard: egui's own arrow move is not made.
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
            }
            (Choice::Discard, Up::Close(exit)) => self.dialogs.chosen.push(Chosen::Discard(exit)),
            (Choice::Reload, Up::Conflict { .. }) => self.dialogs.up = Up::Reload,
            (Choice::Overwrite, Up::Conflict { .. }) => self.dialogs.chosen.push(Chosen::Overwrite),
            (Choice::SaveAs, Up::Conflict { .. }) => commands.push(Command::SaveAs),
            (Choice::Reload, Up::Reload) => self.dialogs.chosen.push(Chosen::Reload),
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
