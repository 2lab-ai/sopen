//! The switch's state (10 §7.4, T32): what one browser hands another when xtools moves between
//! the terminal and a window — both panels' folders, bars, selections and scroll, the active
//! side, the mode, each side's content (the directory tree open, a pinned git panel), an open
//! dialog with its typed text, caret and refusal, the open viewer (its file, 08's view, place,
//! encoding and wrap) and the host the folders are on; from v2 (X4-state) also the tree's bar and
//! the folders the owner opened or closed in it, and each git panel on screen's part — its bar,
//! marks, label cursor, details pane, filters, Find's field, an open dialog with what is typed in
//! it, the planner's plan (`git_panel::state`); from v3 (X4-state2) what is typed in a history
//! edit's own dialog — a quick edit's (reword, squash, author) and the planner's message and
//! author dialogs. No caches, streams or jobs: the receiver opens them again.
//!
//! - **Taking** ([`Browser::take_state`]) reads the browser and changes nothing; taken while a
//!   given state is still being put back, the parts not back yet are the given ones.
//! - **The line** ([`SwitchState::line`], [`read`]): one JSON line, `"v"` first. A reader takes
//!   its own `v` and every older one — a field the line lacks takes its default — and refuses a
//!   newer one with §7.4's line; a field it does not know is ignored (§7.4); v0 is refused as one
//!   no sopen wrote. The line comes over ssh: it is checked before anything is done with it
//!   ([`SwitchState::check`]: absolute paths, a host label ssh cannot read as an option or as two
//!   words, a side 0 or 1; every commit a full object id, every action, verb and order one sopen
//!   has).
//! - **Paths** go out and come back rewritten by a map the caller gives
//!   ([`SwitchState::rewritten`]), from and to sopen's command-line form (`host:/abs`, or `/abs`
//!   on this machine), as `forward::source_for` rewrites SOURCE (§7.3): the folders, the pinned
//!   repository, the viewer's file, the tree's folders, the git panels' repositories and a Copy /
//!   Move target typed as an absolute place — one the map cannot place keeps the dialog from
//!   opening again, the status row saying why.
//! - **Giving** ([`Browser::give_state`]), once, to a browser that has not had a pass yet: each
//!   panel lists its folder with the bar on its entry, as `SOPEN_STATE`'s `open=`, `at=` and
//!   `enc=` are handed on, and the rest follows a step a pass on the display the pass before
//!   committed — the selection and the scroll, the pin once the active folder's detection has
//!   answered, each git panel's part at its own pace ([`Browser::give_git`]: its commits looked
//!   up on this host first, its dialog or plan opened as their keys open them, on this host's own,
//!   unseen), the tree and its folders, the dialog (opened as its key opens it, then its text,
//!   caret and refusal), the viewer (opened at its place, in its encoding and wrap, then its
//!   view). The browser has not settled until the last step ran; a listing it did not ask for
//!   ends it. The tree, the dialog and the viewer are opened only on the side and folder they
//!   were taken on, a dialog that acts on entries only on those entries — else the status row
//!   says why, as it says what of a git panel's part this host lacks. A folder that does not list
//!   here (gone, unreadable) shows its nearest parent that does, and the status row says so (§7.3
//!   does not say: X2-state's choice).

use super::keys::Command;
use super::{show, view, Browser, GitAction, GitView, Paging, Status};
use crate::git::actions::ActionId;
use crate::git::history::{self, Git as _, RunnerGit};
use crate::git::model::Oid;
use crate::git::rebase::Verb;
use crate::git::runner::Handle;
use crate::views::Slot;
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// The version this sopen writes, and the newest it reads (10 §7.4: each step that moves a
/// screen adds that screen's fields and bumps it). v2 (X4-state) adds the tree's bar and the
/// folders the owner opened or closed ([`TreeState`]) and the git panels' parts ([`GitState`]);
/// v3 (X4-state2) a history edit's own dialogs ([`GitDialog::Reword`] to
/// [`GitDialog::PlanAuthor`]).
pub(crate) const VERSION: u64 = 3;

/// The switch's state, as its line carries it (10 §7.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SwitchState {
    pub v: u64,
    /// The host the folders are on, `[user@]host` as the browser prints it; `None`: the machine
    /// the state was taken on.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub mode: Mode,
    /// The side whose list has the keyboard.
    #[serde(default)]
    pub active: usize,
    pub panels: [PanelState; 2],
    /// The keyboard is in the git panel opposite the active list (02 §3.1 `origin`).
    #[serde(default)]
    pub git_focus: bool,
    #[serde(default)]
    pub dialog: Option<DialogState>,
    #[serde(default)]
    pub viewer: Option<ViewerState>,
    /// v2: the directory tree's own part, on the side whose `tree` is set.
    #[serde(default)]
    pub tree: Option<TreeState>,
    /// v2: each git panel on screen, by its repository.
    #[serde(default, deserialize_with = "capped::list::<capped::Panels, _, _>")]
    pub git: Vec<GitState>,
    /// v2: what the sender left out of its own state so the reader takes the rest (`fit`), as
    /// the reader's status row says it once the state is back.
    #[serde(default, deserialize_with = "capped::list::<capped::Said, _, _>")]
    pub said: Vec<String>,
}

/// The directory tree's part (05, v2): its bar and the folders the owner opened or closed, by
/// path — a row's id lives only as long as its tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TreeState {
    /// The bar's row, by its folder (absolute).
    #[serde(default)]
    pub bar: Option<String>,
    /// The bar is on that folder's `· N hidden folders` row, not on the folder's own.
    #[serde(default)]
    pub hidden: bool,
    /// Where the bar's row is on screen: rows below the first one shown (the rows above it may
    /// differ on another host, or a moment later).
    #[serde(default)]
    pub at: usize,
    /// The folders → or ← opened or closed, in no order.
    #[serde(default, deserialize_with = "capped::list::<capped::Folders, _, _>")]
    pub folders: Vec<TreeFolder>,
}

/// A folder the owner opened (→) or closed (←) in the tree, by path (absolute).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TreeFolder {
    pub path: String,
    pub open: bool,
}

/// A git panel's part (03, v2). Commits go by id, looked up on the receiving host before the
/// panel shows them; what a dialog would run is HOST's own, opened afresh and unseen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitState {
    /// The repository's top level (absolute).
    pub top: String,
    #[serde(default)]
    pub bar: Option<GitRow>,
    /// The label cursor on the bar's row: 0 the commit itself, `n` its `n`th label.
    #[serde(default)]
    pub label: usize,
    #[serde(default, deserialize_with = "capped::list::<capped::Marks, _, _>")]
    pub marks: Vec<GitRow>,
    /// The details pane: what it shows, and whether it has the keyboard.
    #[serde(default)]
    pub details: Option<GitDetails>,
    /// The filters, when one differs from its default.
    #[serde(default)]
    pub filters: Option<GitFilters>,
    /// Find's field, open: its text and its `Aa`.
    #[serde(default)]
    pub find: Option<GitFind>,
    #[serde(default)]
    pub dialog: Option<GitDialog>,
    /// The planner's plan, while it is open.
    #[serde(default)]
    pub plan: Option<GitPlan>,
}

/// A row of the graph by what it draws (the panel's `RowKey`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GitRow {
    Uncommitted,
    Commit(String),
    Stash(String),
}

/// The details pane: what it shows, and whether it has the keyboard (else it follows the bar).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitDetails {
    pub subject: GitSubject,
    pub keyboard: bool,
}

/// Find's open field: its text and its `Aa` (case as typed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitFind {
    pub text: String,
    pub case: bool,
}

/// What the details pane shows (the panel's `Subject`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GitSubject {
    Row(GitRow),
    Compare { from: GitRow, to: GitRow },
}

/// The filters (03 §4.6); `branches` `None`: every branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitFilters {
    #[serde(default, deserialize_with = "capped::some::<capped::Branches, _, _>")]
    pub branches: Option<Vec<String>>,
    #[serde(deserialize_with = "capped::list::<capped::Remotes, _, _>")]
    pub hidden_remotes: Vec<String>,
    pub tags: bool,
    pub stashes: bool,
    pub first_parent: bool,
    pub order: GitOrder,
    pub reflog: bool,
}

/// The log's order (03 §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GitOrder {
    Date,
    AuthorDate,
    Topo,
}

/// An open git dialog with what is typed in it (the menu, the result, the log's entry and the
/// stop question hold nothing typed: they are not carried).
///
/// Externally tagged (`{"action": {…}}`): an internally tagged enum is buffered whole before its
/// tag is read, so its lists' bounds would hold nothing while the line is parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GitDialog {
    /// An action's with options (03 §7), by its id, on what it was opened on: its fields' texts
    /// and its rows of choices (the words chosen), by number.
    Action {
        id: String,
        on: GitTarget,
        #[serde(deserialize_with = "capped::list::<capped::Fields, _, _>")]
        texts: Vec<GitText>,
        #[serde(deserialize_with = "capped::list::<capped::Fields, _, _>")]
        choices: Vec<GitChoice>,
    },
    /// The filters' (03 §4.6): the choices not applied yet and the branches' type-to-filter.
    Filters { filters: GitFilters, typed: String },
    /// The history search's (03 §4.7): its six fields, in the dialog's order, and the case.
    Search { fields: [String; 6], case: bool },
    /// v3: Reword's (03 §7.8.1) on `commit`, its message as typed.
    Reword { commit: String, message: String },
    /// v3: Squash's on `commit`, its message — the two joined — as typed.
    Squash { commit: String, message: String },
    /// v3: Author and date's on `commit`, its fields as typed.
    Author {
        commit: String,
        author: GitAuthorForm,
    },
    /// v3: the planner's own message dialog (03 §7.8.2) on its plan's row `commit`, as typed.
    PlanMessage { commit: String, message: String },
    /// v3: the planner's own author dialog on its plan's row `commit`, its fields as typed.
    PlanAuthor {
        commit: String,
        author: GitAuthorForm,
    },
}

impl GitDialog {
    /// A quick edit's: it opens once its range is read (`edit_open`).
    pub(crate) fn quick(&self) -> bool {
        matches!(
            self,
            GitDialog::Reword { .. } | GitDialog::Squash { .. } | GitDialog::Author { .. }
        )
    }

    /// The planner's own: it stands on its plan, and goes with it.
    pub(crate) fn planners(&self) -> bool {
        matches!(
            self,
            GitDialog::PlanMessage { .. } | GitDialog::PlanAuthor { .. }
        )
    }
}

/// An author's fields as typed (`author`'s dialog, the planner's `A`): the date as its field's
/// text, which may not read yet, the committer date by its word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitAuthorForm {
    pub name: String,
    pub email: String,
    pub date: String,
    pub committer: GitCommitter,
}

/// A text field of an action's dialog, by its number in the form, and what is typed in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitText {
    pub field: usize,
    pub text: String,
}

/// A row of choices of an action's dialog, by its number in the form, and the words chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitChoice {
    pub row: usize,
    #[serde(deserialize_with = "capped::list::<capped::Words, _, _>")]
    pub chosen: Vec<String>,
}

/// What an action's dialog acts on (the menu's `Target`), by name: HOST's own is found again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GitTarget {
    Commit {
        commit: String,
        #[serde(deserialize_with = "capped::list::<capped::Marks, _, _>")]
        marked: Vec<String>,
    },
    Label(String),
    Stash(String),
    Uncommitted,
    Files(#[serde(deserialize_with = "capped::list::<capped::Files, _, _>")] Vec<String>),
    Repository,
}

/// The planner's unsaved plan (03 §7.8.2): its rows newest first, each by its commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitPlan {
    /// The commit the range starts above; `None`: `--root`.
    pub base: Option<String>,
    #[serde(deserialize_with = "capped::list::<capped::PlanRows, _, _>")]
    pub rows: Vec<GitPlanRow>,
    pub autostash: bool,
    pub update_refs: bool,
    /// The pane's bar, by its commit.
    pub bar: Option<String>,
}

/// A planner row: its verb (`pick`, `reword`, …), its typed message, its author line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitPlanRow {
    pub commit: String,
    pub verb: String,
    pub message: Option<String>,
    pub author: Option<GitAuthor>,
}

/// An author line's values (`rebase::Author`): the date in Unix seconds and its zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitAuthor {
    pub name: String,
    pub email: String,
    pub seconds: i64,
    pub offset_minutes: i32,
    pub committer: GitCommitter,
}

/// `author`'s committer date (03 §7.8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GitCommitter {
    Keep,
    Now,
    Author,
}

/// The browser's mode (02 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Mode {
    #[default]
    Preview,
    Dual,
}

/// One side: its list, and what shows in its place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PanelState {
    /// The folder (absolute).
    pub folder: String,
    /// The entry under the bar.
    #[serde(default)]
    pub bar: Option<String>,
    /// The selected entries, in the panel's order (04 §4).
    #[serde(default, deserialize_with = "capped::list::<capped::Entries, _, _>")]
    pub selected: Vec<String>,
    /// The first row on screen.
    #[serde(default)]
    pub top: usize,
    /// The directory tree is open in its place (05).
    #[serde(default)]
    pub tree: bool,
    /// The git panel of this repository (its top level) is pinned here (02 §3.2).
    #[serde(default)]
    pub pin: Option<String>,
}

/// The list's dialog that is open (04 §4–§5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DialogState {
    pub kind: DialogKind,
    /// Its field's text (the delete dialog has none).
    #[serde(default)]
    pub text: Option<String>,
    /// The caret, in characters of `text`, and the other end of the selection (the caret
    /// itself when nothing is selected).
    #[serde(default)]
    pub caret: Option<usize>,
    #[serde(default)]
    pub anchor: Option<usize>,
    /// Its refusal, as it says it.
    #[serde(default)]
    pub error: Option<String>,
    /// The entries it acts on, in the panel's order: the one being renamed, those a Copy, Move
    /// or Delete takes (the selection, else the bar's entry). The receiver opens it again only
    /// on these; the folder-wide dialogs (select-group, make folder) have none.
    #[serde(default, deserialize_with = "capped::list::<capped::Entries, _, _>")]
    pub entries: Vec<String>,
    /// A Copy / Move target the caller's map could not place, and why (`<text>: <why>`): the
    /// dialog is not opened again — on the receiver's own target its Enter would move the files
    /// elsewhere than the owner typed — and the status row says this instead.
    #[serde(default)]
    pub unplaced: Option<String>,
}

/// Which dialog: the key that opens it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DialogKind {
    /// `+`, `-`.
    Select,
    Unselect,
    /// F5, F6.
    Copy,
    Move,
    /// F2: the field in the bar's row.
    Rename,
    /// F7.
    MakeFolder,
    /// F8, Shift+F8.
    Delete,
    DeleteNow,
}

/// The viewer on screen (08).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ViewerState {
    /// The file (absolute).
    pub file: String,
    /// The view on screen: 1, 2 or 3 (08 §3's slots).
    #[serde(default = "view_one")]
    pub view: u8,
    /// Its place (08 §2): the text view's first byte of its top row, the hex view's cursor.
    #[serde(default)]
    pub place: Option<u64>,
    /// The encoding its lines are read in (08 §7.1's label), when the owner chose it: absent, the
    /// receiver picks the file's default again (§7.2), as the sender did.
    #[serde(default)]
    pub encoding: Option<String>,
    /// Whether the text view wraps — the one on screen, or the one kept hidden behind another
    /// view.
    #[serde(default)]
    pub wrap: Option<bool>,
    /// A diff or a file at a revision (03 §6.2–§6.3), `file` the working file it is of: a copy
    /// in the sender's cache, never carried (§7.4) — the receiver says so instead.
    #[serde(default)]
    pub revision: bool,
}

fn view_one() -> u8 {
    1
}

impl From<view::Mode> for Mode {
    fn from(mode: view::Mode) -> Mode {
        match mode {
            view::Mode::Preview => Mode::Preview,
            view::Mode::Dual => Mode::Dual,
        }
    }
}

impl From<Mode> for view::Mode {
    fn from(mode: Mode) -> view::Mode {
        match mode {
            Mode::Preview => view::Mode::Preview,
            Mode::Dual => view::Mode::Dual,
        }
    }
}

impl SwitchState {
    /// The state as one line of JSON, `"v"` first (no newline: one in a name is escaped).
    pub(crate) fn line(&self) -> String {
        serde_json::to_string(self).expect("a switch state is always JSON")
    }

    /// What a state must be before anything is done with it — it came over ssh ([`read`]), out
    /// of a caller's map ([`SwitchState::rewritten`]) or from anywhere ([`Browser::give_state`]):
    /// its folders, pinned repositories and viewer's file absolute, its host a label ssh cannot
    /// read as an option or as more than one word, its active side 0 or 1.
    pub(crate) fn check(&self) -> Result<(), String> {
        if let Some(host) = &self.host {
            let bad = host.is_empty()
                || host.starts_with('-')
                || host.chars().any(|c| c.is_whitespace() || c.is_control());
            if bad {
                return Err(format!(
                    "the switch's state names the host {host:?}, which is not a host ssh is given"
                ));
            }
        }
        if self.active > 1 {
            return Err(format!(
                "the switch's state names side {}; there are two, 0 and 1",
                self.active
            ));
        }
        let absolute = |what: &str, path: &str| {
            if path.starts_with('/') && !path.contains('\0') {
                Ok(())
            } else {
                Err(format!(
                    "the switch's state has {what} {path:?}: not an absolute path"
                ))
            }
        };
        for p in &self.panels {
            absolute("the folder", &p.folder)?;
            if let Some(pin) = &p.pin {
                absolute("the pinned repository", pin)?;
            }
        }
        if let Some(v) = &self.viewer {
            absolute("the viewer's file", &v.file)?;
        }
        if let Some(t) = &self.tree {
            t.check()?;
        }
        within("lines of what was left out", self.said.len(), bound::SAID)?;
        for line in &self.said {
            short("a line of what was left out", line, bound::NAME)?;
        }
        // A git panel a side at most.
        within("git panels", self.git.len(), 2)?;
        for (k, g) in self.git.iter().enumerate() {
            absolute("the git panel's repository", &g.top)?;
            if self.git[..k].iter().any(|o| o.top == g.top) {
                return Err(format!(
                    "the switch's state has {:?}'s git panel twice",
                    g.top
                ));
            }
            // One git dialog at a time (X4-menu); a plan is the planner's pane, not a dialog.
            let open = |g: &GitState| g.dialog.is_some();
            if open(g) && self.git[..k].iter().any(open) {
                return Err("the switch's state has two git panels' dialogs open".to_string());
            }
            g.check()?;
        }
        Ok(())
    }

    /// The state with every path rewritten by `map` (10 §7.3), which takes and gives sopen's
    /// command-line form — `host:/abs`, or `/abs` on the machine it runs on — as
    /// `forward::source_for` takes SOURCE: out, this machine's `/abs` becomes
    /// `<this machine>:/abs` and HOST's own `HOST:/abs` its `/abs`; back, the other way. The
    /// folders, the pinned repositories and the viewer's file are all on the state's host, so
    /// they must come back on one host, and absolute ([`SwitchState::check`]); a Copy / Move
    /// target typed as an absolute place (it reads as sopen's command line, 04 §5.2) is
    /// rewritten too ([`retarget`]).
    pub(crate) fn rewritten(
        &self,
        map: impl Fn(&str) -> anyhow::Result<String>,
    ) -> Result<SwitchState, String> {
        let mut out = self.clone();
        let mut host: Option<Option<String>> = None;
        let mut place = |abs: &mut String| -> Result<(), String> {
            let source = show(self.host.as_deref(), abs);
            let mapped = map(&source).map_err(|e| format!("{source}: {e:#}"))?;
            let (to_host, to_abs) = match crate::target::parse(&mapped)? {
                crate::target::Target::Remote(spec) => (Some(spec.label()), spec.path),
                crate::target::Target::Local(path) => (None, path.to_string_lossy().into_owned()),
            };
            if !to_abs.starts_with('/') {
                return Err(format!("{source} became {mapped}: not an absolute path"));
            }
            match &host {
                Some(was) if *was != to_host => {
                    return Err(format!("{source} became {mapped}: not on the others' host"));
                }
                _ => host = Some(to_host),
            }
            *abs = to_abs;
            Ok(())
        };
        for panel in &mut out.panels {
            place(&mut panel.folder)?;
            if let Some(pin) = &mut panel.pin {
                place(pin)?;
            }
        }
        if let Some(viewer) = &mut out.viewer {
            place(&mut viewer.file)?;
        }
        if let Some(t) = &mut out.tree {
            for path in t
                .bar
                .iter_mut()
                .chain(t.folders.iter_mut().map(|f| &mut f.path))
            {
                place(path)?;
            }
        }
        for g in &mut out.git {
            place(&mut g.top)?;
        }
        out.host = host.flatten();
        if let Some(d) = &mut out.dialog {
            if matches!(d.kind, DialogKind::Copy | DialogKind::Move) {
                retarget(d, &map);
            }
        }
        out.check()?;
        Ok(out)
    }
}

/// What a line may hold of v2's parts (10 §7.4; X4-state's review): enough for any screen an
/// owner makes, so no honest state is refused, and a bound on what HOST walks and keeps of one
/// that is not. The line itself is bounded where it is read (`STATE_LINE_MAX`, 64 MiB).
pub(crate) mod bound {
    /// Marked rows: a mark on each row of twenty pages of the graph (500 a page); Compare takes
    /// one, Pick and Revert a handful.
    pub const MARKS: usize = 10_000;
    /// A plan's rows: a range forty times the graph's page, past what `rebase -i` is run over by
    /// hand.
    pub const PLAN_ROWS: usize = 20_000;
    /// One typed text (a message, a field, Find's text): eight times the 1 MiB message the
    /// dialogs' tests hold to (X4-quick), an eighth of the line.
    pub const TEXT: usize = 8 << 20;
    /// A name — a path, a refname, a choice's word, a stash or commit id's text, an author's name,
    /// email or date: PATH_MAX (Linux 4096; macOS's 1024 under it); a refname is a path.
    pub const NAME: usize = 4096;
    /// Names in one list — a row of choices, the filters' branches and remotes, a target's files,
    /// the tree's folders: five times the 20,000 branches the filters' dialog is held to
    /// (X4-menu).
    pub const NAMES: usize = 100_000;
    /// An action's text fields and rows of choices: its form has at most three and six.
    pub const FIELDS: usize = 64;
    /// v1's entries — a panel's selection, a dialog's — a million: a folder past what a list
    /// shows by hand, and past the line's 64 MiB at a name's usual length.
    pub const ENTRIES: usize = 1_000_000;
    /// The lines of what a sender's `fit` left out: one each for the tree, and for each git
    /// panel's part, dialog and plan, many times over.
    pub const SAID: usize = 64;
}

/// The wire's lists bounded while the line is parsed (X4-state's fix-check): a list past its
/// bound is refused at the element past it, before more is read or kept — a line under its 64
/// MiB can hold millions of short names, each a string of its own.
mod capped {
    use serde::de::{Deserialize, Deserializer, Error as _, SeqAccess, Visitor};
    use std::marker::PhantomData;

    /// A list's bound and its name in the refusal.
    pub trait Cap {
        const MAX: usize;
        const WHAT: &'static str;
    }

    macro_rules! caps {
        ($($name:ident = $max:expr, $what:literal;)*) => {
            $(pub struct $name; impl Cap for $name {
                const MAX: usize = $max;
                const WHAT: &'static str = $what;
            })*
        };
    }

    caps! {
        Panels = 2, "git panels";
        Said = super::bound::SAID, "lines of what was left out";
        Folders = super::bound::NAMES, "tree folders";
        Marks = super::bound::MARKS, "marks";
        Branches = super::bound::NAMES, "branches";
        Remotes = super::bound::NAMES, "remotes";
        Fields = super::bound::FIELDS, "fields or rows of a dialog";
        Words = super::bound::NAMES, "chosen words";
        Files = super::bound::NAMES, "files";
        PlanRows = super::bound::PLAN_ROWS, "plan rows";
        Entries = super::bound::ENTRIES, "entries";
    }

    /// The line past `C`'s bound: what a state holds.
    pub fn past<C: Cap>() -> String {
        format!(
            "the switch's state has more than the {} {} a state holds",
            C::MAX,
            C::WHAT
        )
    }

    struct Capped<C, T>(Vec<T>, PhantomData<C>);

    impl<'de, C: Cap, T: Deserialize<'de>> Deserialize<'de> for Capped<C, T> {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct Seq<C, T>(PhantomData<(C, T)>);
            impl<'de, C: Cap, T: Deserialize<'de>> Visitor<'de> for Seq<C, T> {
                type Value = Capped<C, T>;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    write!(f, "a list of at most {} {}", C::MAX, C::WHAT)
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                    let hint = seq.size_hint().unwrap_or(0).min(C::MAX).min(1024);
                    let mut out = Vec::with_capacity(hint);
                    while let Some(item) = seq.next_element()? {
                        if out.len() == C::MAX {
                            return Err(A::Error::custom(past::<C>()));
                        }
                        out.push(item);
                    }
                    Ok(Capped(out, PhantomData))
                }
            }
            d.deserialize_seq(Seq(PhantomData))
        }
    }

    /// A list of at most `C::MAX`.
    pub fn list<'de, C: Cap, T: Deserialize<'de>, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<T>, D::Error> {
        Capped::<C, T>::deserialize(d).map(|c| c.0)
    }

    /// An optional list of at most `C::MAX`.
    pub fn some<'de, C: Cap, T: Deserialize<'de>, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Vec<T>>, D::Error> {
        Option::<Capped<C, T>>::deserialize(d).map(|o| o.map(|c| c.0))
    }
}

/// `n` of `what` within `max`.
fn within(what: &str, n: usize, max: usize) -> Result<(), String> {
    match n <= max {
        true => Ok(()),
        false => Err(format!(
            "the switch's state has {n} {what}, more than the {max} a state holds"
        )),
    }
}

/// `text`, one of `what`, within `max` bytes.
fn short(what: &str, text: &str, max: usize) -> Result<(), String> {
    within(&format!("bytes in {what}"), text.len(), max)
}

impl TreeState {
    /// Its folders absolute, as many as a state holds, each a path's length at most.
    fn check(&self) -> Result<(), String> {
        within("tree folders", self.folders.len(), bound::NAMES)?;
        for path in self.bar.iter().chain(self.folders.iter().map(|f| &f.path)) {
            short("a tree folder", path, bound::NAME)?;
            if !path.starts_with('/') || path.contains('\0') {
                return Err(format!(
                    "the switch's state has a tree folder {path:?}: not an absolute path"
                ));
            }
        }
        Ok(())
    }
}

impl GitState {
    /// What a git panel's part must be (it came over ssh): every commit a full object id (no
    /// option, no revision expression reaches git), every dialog's id one sopen has, every verb
    /// one of theirs, every count and text within its bound ([`bound`]).
    fn check(&self) -> Result<(), String> {
        self.check_view()?;
        self.check_dialog()?;
        self.check_plan()
    }

    /// `text`, a commit's id.
    fn oid(&self, what: &str, text: &str) -> Result<(), String> {
        match Oid::parse(text) {
            Some(_) => Ok(()),
            None => Err(format!(
                "{}: the switch's state names {what} {text:?}: not a commit id",
                self.top
            )),
        }
    }

    fn bad(&self, what: &str) -> Result<(), String> {
        Err(format!(
            "{}: the switch's state has {what} sopen has not",
            self.top
        ))
    }

    /// The bar, the marks, the details pane, the filters and Find's text.
    fn check_view(&self) -> Result<(), String> {
        let row = |r: &GitRow| match r {
            GitRow::Uncommitted => Ok(()),
            GitRow::Commit(c) | GitRow::Stash(c) => self.oid("a row", c),
        };
        within("marks", self.marks.len(), bound::MARKS)?;
        for r in self.bar.iter().chain(&self.marks) {
            row(r)?;
        }
        match self.details.as_ref().map(|d| &d.subject) {
            Some(GitSubject::Row(r)) => row(r)?,
            Some(GitSubject::Compare { from, to }) => {
                row(from)?;
                row(to)?;
            }
            None => {}
        }
        if let Some(f) = &self.filters {
            check_filters(f)?;
        }
        if let Some(f) = &self.find {
            short("Find's text", &f.text, bound::TEXT)?;
        }
        Ok(())
    }

    /// The open dialog: an action sopen has, on commits by their ids, its texts and choices within
    /// their bounds ([`GitState::check_typed`]); the planner's own on a row of its plan.
    fn check_dialog(&self) -> Result<(), String> {
        if let Some(GitDialog::PlanMessage { commit, .. } | GitDialog::PlanAuthor { commit, .. }) =
            &self.dialog
        {
            self.plan_row(commit)?;
        }
        self.check_typed()
    }

    /// The open dialog itself, its plan aside: what `fit` leaves it out by when it fails. The
    /// question before the switch holds the dialog that would cross to the same bounds of what is
    /// typed in it, in place (`GitView::carried_whole`).
    fn check_typed(&self) -> Result<(), String> {
        match &self.dialog {
            Some(GitDialog::Action {
                id,
                on,
                texts,
                choices,
            }) => {
                if ActionId::parse(id).is_none() {
                    return self.bad("an action");
                }
                match on {
                    GitTarget::Commit { commit, marked } => {
                        within("marked commits", marked.len(), bound::MARKS)?;
                        for c in std::iter::once(commit).chain(marked) {
                            self.oid("a commit", c)?;
                        }
                    }
                    GitTarget::Stash(c) => self.oid("a stash", c)?,
                    GitTarget::Label(name) => short("a refname", name, bound::NAME)?,
                    GitTarget::Files(paths) => {
                        within("files", paths.len(), bound::NAMES)?;
                        for p in paths {
                            short("a file's path", p, bound::NAME)?;
                        }
                    }
                    GitTarget::Uncommitted | GitTarget::Repository => {}
                }
                within("text fields", texts.len(), bound::FIELDS)?;
                for t in texts {
                    short("a text field", &t.text, bound::TEXT)?;
                }
                within("rows of choices", choices.len(), bound::FIELDS)?;
                for c in choices {
                    within("chosen words", c.chosen.len(), bound::NAMES)?;
                    for w in &c.chosen {
                        short("a choice", w, bound::NAME)?;
                    }
                }
                Ok(())
            }
            Some(GitDialog::Filters { filters, typed }) => {
                check_filters(filters)?;
                short("the filters' text", typed, bound::TEXT)
            }
            Some(GitDialog::Search { fields, .. }) => {
                for f in fields {
                    short("a search field", f, bound::TEXT)?;
                }
                Ok(())
            }
            Some(GitDialog::Reword { commit, message } | GitDialog::Squash { commit, message }) => {
                self.oid("a commit", commit)?;
                short("a history edit's message", message, bound::TEXT)
            }
            Some(GitDialog::Author { commit, author }) => {
                self.oid("a commit", commit)?;
                check_form(author)
            }
            Some(GitDialog::PlanMessage { commit, message }) => {
                self.oid("a commit", commit)?;
                short("the planner's message", message, bound::TEXT)
            }
            Some(GitDialog::PlanAuthor { commit, author }) => {
                self.oid("a commit", commit)?;
                check_form(author)
            }
            None => Ok(()),
        }
    }

    /// `commit`, a row of its plan: the planner's own dialog stands on one.
    fn plan_row(&self, commit: &str) -> Result<(), String> {
        self.oid("a commit", commit)?;
        let rows = self.plan.as_ref().map(|p| p.rows.as_slice());
        match rows.unwrap_or_default().iter().any(|r| r.commit == commit) {
            true => Ok(()),
            false => Err(format!(
                "{}: the switch's state has the planner's dialog on a row of no plan it has",
                self.top
            )),
        }
    }

    /// The planner's plan: its commits by their ids, its verbs sopen's, its texts within bounds.
    fn check_plan(&self) -> Result<(), String> {
        let Some(plan) = &self.plan else {
            return Ok(());
        };
        within("plan rows", plan.rows.len(), bound::PLAN_ROWS)?;
        for c in plan.base.iter().chain(&plan.bar) {
            self.oid("a commit", c)?;
        }
        for r in &plan.rows {
            self.oid("a commit", &r.commit)?;
            if !Verb::ALL.iter().any(|v| v.word() == r.verb) {
                return self.bad("a verb");
            }
            if let Some(m) = &r.message {
                short("a plan's message", m, bound::TEXT)?;
            }
            if let Some(a) = &r.author {
                short("an author's name", &a.name, bound::NAME)?;
                short("an author's email", &a.email, bound::NAME)?;
            }
        }
        Ok(())
    }
}

/// An author's typed fields, each a name's length at most, as a plan row's author is — a dialog
/// may read its fields every pass, and one kept goes into the plan.
fn check_form(a: &GitAuthorForm) -> Result<(), String> {
    short("an author's name", &a.name, bound::NAME)?;
    short("an author's email", &a.email, bound::NAME)?;
    short("an author date", &a.date, bound::NAME)
}

/// The filters' names within their bounds.
fn check_filters(f: &GitFilters) -> Result<(), String> {
    let branches = f.branches.as_deref().unwrap_or_default();
    within("branches", branches.len(), bound::NAMES)?;
    within("remotes", f.hidden_remotes.len(), bound::NAMES)?;
    for name in branches.iter().chain(&f.hidden_remotes) {
        short("a branch or remote", name, bound::NAME)?;
    }
    Ok(())
}

impl SwitchState {
    /// This browser's own state made one a reader takes (X4-state's review and fix-check),
    /// `budget` the bytes its line may take: a part that would fail [`SwitchState::check`] — a
    /// bound passed — is left out, the rest kept; then, while the line as it is written (its JSON,
    /// escaping and all) passes `budget`, its plans go — each with its planner's own dialog —
    /// then its dialogs, then the tree, then its git panels, the last first. Each part left out is
    /// said, in [`SwitchState::said`] — the reader's status row says it once the state is back.
    /// v1's own parts stay as they are.
    pub(crate) fn fit(mut self, budget: usize) -> SwitchState {
        let mut said = std::mem::take(&mut self.said);
        if let Some(Err(why)) = self.tree.as_ref().map(TreeState::check) {
            said.push(format!("the tree's folders and bar are left out: {why}"));
            self.tree = None;
        }
        let (mut tops, mut dialog) = (Vec::new(), false);
        self.git.retain_mut(|g| {
            let why = match (g.top.starts_with('/'), tops.contains(&g.top)) {
                (false, _) => Some("its repository is not an absolute path".to_string()),
                (_, true) => Some("it is in the state twice".to_string()),
                _ => g.check_view().err(),
            };
            if let Some(why) = why {
                said.push(format!("the git panel of {} is left out: {why}", g.top));
                return false;
            }
            tops.push(g.top.clone());
            // The plan first: the planner's own dialog stands on it (`check_dialog`).
            if let Err(why) = g.check_plan() {
                said.push(format!(
                    "the git panel of {}: its plan is left out: {why}",
                    g.top
                ));
                g.plan = None;
            }
            if let Err(why) = g.check_dialog() {
                said.push(format!(
                    "the git panel of {}: its dialog is left out: {why}",
                    g.top
                ));
                g.dialog = None;
            }
            if dialog && g.dialog.take().is_some() {
                let why = "another git panel's is up (one at a time)";
                said.push(format!(
                    "the git panel of {}: its dialog is left out: {why}",
                    g.top
                ));
            }
            dialog |= g.dialog.is_some();
            true
        });
        let past = format!("the line would pass {} MiB", (budget + (1 << 20) - 1) >> 20);
        loop {
            said.truncate(bound::SAID);
            for line in &mut said {
                let mut end = line.len().min(bound::NAME);
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                line.truncate(end);
            }
            self.said.clone_from(&said);
            if line_len(&self) <= budget {
                return self;
            }
            if let Some(g) = (self.git.iter_mut().rev()).find(|g| g.plan.is_some()) {
                g.plan = None;
                said.push(format!(
                    "the git panel of {}: its plan is left out: {past}",
                    g.top
                ));
                // The planner's own dialog goes with it.
                if g.dialog.take_if(|d| d.planners()).is_some() {
                    said.push(format!(
                        "the git panel of {}: its dialog is left out: {past}",
                        g.top
                    ));
                }
                continue;
            }
            if let Some(g) = (self.git.iter_mut().rev()).find(|g| g.dialog.is_some()) {
                g.dialog = None;
                said.push(format!(
                    "the git panel of {}: its dialog is left out: {past}",
                    g.top
                ));
                continue;
            }
            if self.tree.take().is_some() {
                said.push(format!("the tree's folders and bar are left out: {past}"));
                continue;
            }
            match self.git.pop() {
                Some(g) => said.push(format!("the git panel of {} is left out: {past}", g.top)),
                None => return self,
            }
        }
    }
}

/// The bytes of `state`'s line as it is written ([`SwitchState::line`]), counted, not kept.
fn line_len(state: &SwitchState) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, state).expect("a switch state is always JSON");
    count.0
}

/// A Copy / Move target that reads as an absolute place (`host:/abs`, `/abs`), rewritten by
/// `map`; its caret and selection — held to the text first — keep their characters where the
/// change is at its start (a host put before it or taken off), else go to its end. One `map`
/// cannot place is [`DialogState::unplaced`]: the receiver does not open the dialog again —
/// neither on the text as typed (another machine's folder there) nor on a target of its own.
/// Anything else (a name, a path relative to the folder) is left as typed.
fn retarget(d: &mut DialogState, map: impl Fn(&str) -> anyhow::Result<String>) {
    let Some(text) = d.text.clone() else {
        return;
    };
    let absolute = match crate::target::parse(&text) {
        Ok(crate::target::Target::Remote(spec)) => spec.path.starts_with('/'),
        Ok(crate::target::Target::Local(path)) => path.is_absolute(),
        Err(_) => false,
    };
    if !absolute {
        return;
    }
    let to = match map(&text) {
        Ok(to) => to,
        Err(e) => {
            d.unplaced = Some(format!("{text}: {e:#}"));
            return;
        }
    };
    let (was, now) = (text.chars().count(), to.chars().count());
    let shift = |at: usize| {
        let at = at.min(was);
        if to.ends_with(&text) {
            at + (now - was)
        } else if text.ends_with(&to) {
            at.saturating_sub(was - now)
        } else {
            now
        }
    };
    d.caret = d.caret.map(shift);
    d.anchor = d.anchor.map(shift);
    d.text = Some(to);
}

/// The state on `line`, sent by `from` (HOST's name, for the refusal): its own `v` and every
/// older one read — a field the line lacks taking its default, one this sopen does not know
/// ignored — and held as this version's; a newer `v` is refused with §7.4's line, v0 — v1 is the
/// first, so none was ever written — with its own; then [`SwitchState::check`]ed.
pub(crate) fn read(line: &str, from: &str) -> Result<SwitchState, String> {
    let unread =
        |e: serde_json::Error| format!("{from} sent a switch state that does not read: {e}");
    // Its version first, nothing else of it kept (no untyped value of the whole line: each list
    // is bounded as the state itself is read, `capped`).
    #[derive(Deserialize)]
    struct Version {
        v: Option<u64>,
    }
    let Version { v } = serde_json::from_str(line).map_err(unread)?;
    let Some(v) = v else {
        return Err(format!("{from} sent a switch state without its version"));
    };
    if v > VERSION {
        return Err(format!(
            "{from} sent switch state v{v}; this sopen reads up to v{VERSION} — update one of them"
        ));
    }
    if v == 0 {
        return Err(format!("state version {v} is not one sopen wrote"));
    }
    let mut state: SwitchState = serde_json::from_str(line).map_err(unread)?;
    state.v = VERSION;
    state.check()?;
    Ok(state)
}

/// What is still to be put back of a state given to a browser ([`Browser::give_state`]).
pub(crate) struct Given {
    state: SwitchState,
    /// Each panel's folder while its listing is on its way (the one asked for, or a parent).
    listing: [Option<String>; 2],
    /// A panel whose folder did not list here: that folder and why — the panel shows the
    /// nearest parent that lists instead.
    fell_back: [Option<(String, String)>; 2],
    /// The status row's lines, said once the state is back: the panels that fell back, a dialog
    /// not opened again, a viewer not carried.
    said: Vec<String>,
    /// The putting back starts a listing of its own: any other ends it ([`Browser::relist`]).
    starting: bool,
    step: Step,
    /// v2: each git panel's part not all given yet ([`Step::GitParts`]).
    git: Vec<Giving>,
    /// v2: the tree's part not given yet ([`Step::TreeRows`]).
    tree: Option<TreeState>,
}

/// A git panel's part being given ([`Browser::give_git`]), and how far.
struct Giving {
    part: GitState,
    stage: Stage,
}

enum Stage {
    /// Its panel is not made yet: its filters go in as soon as it is, before its graph is read.
    Panel,
    /// Its graph's first page and its repository's snapshot are on their way.
    Loading,
    /// Every commit it names is being looked up on this host: those it has.
    LookingUp(Receiver<Result<HashSet<Oid>, String>>),
    /// The bar waits for its row, pages read until it comes, as `select=` reads them.
    Paging(HashSet<Oid>),
    /// Its plan's planner is opening on this host's range: the plan goes in once it is open.
    Planning(HashSet<Oid>),
    /// Its dialog opens once no part's planner is still opening (the planner's read waits while
    /// a dialog is up, `git_waits`).
    Dialog(HashSet<Oid>),
    /// Its dialog is opening: what was typed goes in once it is open.
    Opening,
}

impl Stage {
    /// Its plan is not given yet.
    fn planning(&self) -> bool {
        !matches!(self, Stage::Dialog(_) | Stage::Opening)
    }
}

/// The steps after the listings, one a pass (each builds on the display the one before it made).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Step {
    Lists,
    Git,
    GitFocus,
    /// v2: the git panels' parts, each at its own pace.
    GitParts,
    Tree,
    /// v2: the tree's bar and the folders the owner opened or closed.
    TreeRows,
    Dialog,
    Viewer,
    /// The viewer is being made; once it is, its wrap, before the view it opens on gives way
    /// to the one asked for.
    ViewerMade,
    /// The view asked for is on screen: its place, once it is ready.
    ViewerShown,
}

impl Given {
    /// What of `state` (taken from the browser) is not back yet: the given state's, as it will
    /// be once it is.
    fn not_back_yet(&self, state: &mut SwitchState) {
        let given = &self.state;
        for (i, p) in state.panels.iter_mut().enumerate() {
            if self.listing[i].is_some() {
                *p = given.panels[i].clone();
            }
            if self.step <= Step::Git {
                p.pin.clone_from(&given.panels[i].pin);
            }
            if self.step <= Step::Tree {
                p.tree = given.panels[i].tree;
            }
        }
        if self.step <= Step::GitFocus {
            state.git_focus = given.git_focus;
        }
        if self.step <= Step::GitParts {
            state.git.clone_from(&given.git);
        }
        if self.step <= Step::TreeRows {
            state.tree.clone_from(&given.tree);
        }
        if self.step <= Step::Dialog && given.dialog.is_some() {
            state.dialog.clone_from(&given.dialog);
        }
        // Every step left is the viewer's.
        if given.viewer.is_some() {
            state.viewer.clone_from(&given.viewer);
        }
    }
}

impl Browser {
    /// The switch's state of this browser (10 §7.4): nothing in it changes. `ctx` is the one
    /// the browser is drawn in (a dialog field's caret is its edit state there). While a given
    /// state is still being put back, what is not back yet is the given state's.
    pub(crate) fn take_state(&self, ctx: &egui::Context) -> SwitchState {
        let panel = |i: usize| {
            let p = &self.panels[i];
            PanelState {
                folder: p.dir.clone(),
                bar: p.current().map(|e| e.name.clone()),
                selected: p.selection().iter().map(|e| e.name.clone()).collect(),
                top: p.top,
                tree: self.tree.as_ref().is_some_and(|t| t.side == i),
                pin: (self.git.pin.as_ref())
                    .filter(|pin| pin.side == i)
                    .map(|pin| pin.repo.clone()),
            }
        };
        let viewer = self.viewer.as_ref().map(|v| {
            let window = &v.embedded.window;
            let set = &v.views.set;
            let on = set.on_screen();
            let slot = Slot::ALL
                .into_iter()
                .find(|&s| on.is_some() && set.slot(s) == on);
            // What the owner chose (enc=, F8's list, D21): the text view reads in another than
            // the one its bytes picked, or the set keeps a choice. A picked one is picked again.
            let reads = window
                .encoding()
                .filter(|(enc, pick)| pick.map(|p| p.encoding) != Some(*enc));
            let encoding = (reads.map(|(enc, _)| enc.label()))
                .or_else(|| set.encoding())
                .map(str::to_string);
            ViewerState {
                file: v.abs.clone(),
                view: slot.map_or(1, Slot::number),
                place: window.byte_place(),
                encoding,
                // The text view's own, on screen or kept behind another view.
                wrap: window.wrapped().or(v.text_wrap),
                // A diff or a file at a revision is a copy in this machine's cache (03 §6.2):
                // only said (§7.4: no caches); `file` is the working file it is of.
                revision: v.views.read_only(),
            }
        });
        let mut state = SwitchState {
            v: VERSION,
            host: self.label.clone(),
            mode: self.mode.into(),
            active: self.active,
            panels: [panel(0), panel(1)],
            git_focus: self.git.focus,
            dialog: self.ops.open_dialog(ctx),
            viewer,
            tree: self.tree_part(),
            git: self.git_parts(),
            said: Vec::new(),
        };
        if let Some(given) = &self.given {
            given.not_back_yet(&mut state);
        }
        state
    }

    /// Gives `state` to this browser — made on the state's host, not drawn yet, given nothing
    /// before: what each side lists, and the rest once that is listed (see the module's header).
    /// `Err`, and nothing changes: a state that does not pass [`SwitchState::check`], one of
    /// another host (its paths are not this window's), a browser drawn or given one already.
    pub(crate) fn give_state(&mut self, state: SwitchState) -> Result<(), String> {
        state.check()?;
        if self.painted || self.given.is_some() {
            return Err("a switch state is given once, to a window not drawn yet".to_string());
        }
        if state.host != self.label {
            let host = |h: &Option<String>| h.clone().unwrap_or_else(|| "this machine".into());
            return Err(format!(
                "the switch's state is of {}, this window of {}",
                host(&state.host),
                host(&self.label)
            ));
        }
        self.mode = state.mode.into();
        self.active = state.active;
        let panels = state.panels.clone();
        let git = (state.git.iter().cloned())
            .map(|part| Giving {
                part,
                stage: Stage::Panel,
            })
            .collect();
        let tree = state.tree.clone();
        self.given = Some(Given {
            state,
            listing: [None, None],
            fell_back: [None, None],
            said: Vec::new(),
            starting: false,
            step: Step::Lists,
            git,
            tree,
        });
        for (i, p) in panels.into_iter().enumerate() {
            self.given_list(i, p.folder, p.bar, None);
        }
        Ok(())
    }

    /// The putting back's own listing of `dir` in panel `i`, the bar on `select`.
    fn given_list(
        &mut self,
        i: usize,
        dir: String,
        select: Option<String>,
        ctx: Option<&egui::Context>,
    ) {
        if let Some(given) = self.given.as_mut() {
            given.starting = true;
            given.listing[i] = Some(dir.clone());
        }
        self.list(Some(i), dir, select, ctx);
        if let Some(given) = self.given.as_mut() {
            given.starting = false;
        }
    }

    /// A listing is starting ([`Browser::relist`]): one the putting back did not ask for — a
    /// file operation's or a git write's re-read, the owner's — ends it, so nothing of it comes
    /// later on a list it no longer knows, and no listing it waits for is one that never comes.
    pub(super) fn given_overtaken(&mut self) {
        if self.given.as_ref().is_some_and(|g| !g.starting) {
            if let Some(given) = self.given.take() {
                self.given_done(given);
            }
        }
    }

    /// Panel `i` listed `dir`: if that is the given state's listing, its selection and scroll
    /// (the bar went on its entry with the listing) — or, for a parent shown in place of a
    /// folder that did not list, the line that says so.
    pub(super) fn given_listed(&mut self, i: usize, dir: &str) {
        let Some(given) = self.given.as_mut() else {
            return;
        };
        if given.listing[i].as_deref() != Some(dir) {
            return;
        }
        given.listing[i] = None;
        if let Some((folder, why)) = &given.fell_back[i] {
            let label = self.label.as_deref();
            given.said.push(format!(
                "{}: {why} — showing {}",
                show(label, folder),
                show(label, dir)
            ));
            return;
        }
        let wanted = &given.state.panels[i];
        let p = &mut self.panels[i];
        p.clear_selection();
        let bar = p.highlight;
        for name in &wanted.selected {
            if let Some(at) = p.entries.iter().position(|e| e.name == *name) {
                p.highlight = at;
                p.toggle();
            }
        }
        p.highlight = bar;
        p.top = wanted.top.min(p.entries.len().saturating_sub(1));
    }

    /// The listing of `dir` for `panel` failed with `e`: if it was the given state's, the
    /// panel lists its parent instead (`true`), the bar on the folder it came from — unless
    /// `dir` is the root, which is said as any listing that fails (`false`).
    pub(super) fn given_unlisted(
        &mut self,
        ctx: &egui::Context,
        panel: Option<usize>,
        dir: &str,
        e: &anyhow::Error,
    ) -> bool {
        let (Some(i), Some(given)) = (panel, self.given.as_mut()) else {
            return false;
        };
        if given.listing[i].as_deref() != Some(dir) {
            return false;
        }
        let (parent, name) = crate::remote::split_parent(dir);
        if dir == "/" || parent == dir || !parent.starts_with('/') {
            given.listing[i] = None;
            return false;
        }
        let (parent, name) = (parent.to_string(), name.to_string());
        given.fell_back[i].get_or_insert_with(|| (dir.to_string(), format!("{e:#}")));
        self.given_list(i, parent, Some(name), Some(ctx));
        true
    }

    /// The switch's part of a pass, before anything else of it (`Browser::ui`): the text view's
    /// wrap noted (what the state carries while another view is on screen), and a given state's
    /// next step, on the display the pass before committed.
    pub(super) fn switch_pass(&mut self, ctx: &egui::Context) {
        if let Some(v) = self.viewer.as_mut() {
            if let Some(wrap) = v.embedded.window.wrapped() {
                v.text_wrap = Some(wrap);
            }
        }
        let Some(mut given) = self.given.take() else {
            return;
        };
        // A window drawn only on input still takes each step: the next pass comes at once after a
        // step, soon while one waits for a worker (detection, the viewer), and once more at the
        // end (what the last step changed is laid out).
        match self.given_step(ctx, &mut given) {
            Some(next) => {
                if next == given.step {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                } else {
                    ctx.request_repaint();
                }
                given.step = next;
                self.given = Some(given);
            }
            None => {
                self.given_done(given);
                ctx.request_repaint();
            }
        }
    }

    /// Whether the active list is the one the state was taken in: the same side, on the same
    /// folder (not a parent it fell back to, not one the owner went to meanwhile).
    fn at_source(&self, state: &SwitchState) -> bool {
        self.active == state.active
            && self.panels[self.active].dir == state.panels[state.active].folder
    }

    /// Why `d` is not opened again here, if it is not: its target could not be placed here, the
    /// list is not the one it was taken in, or what it acts on (the bar's entry, the selection)
    /// is not what it acted on there.
    fn dialog_refused(&self, state: &SwitchState, d: &DialogState) -> Option<String> {
        if let Some(why) = &d.unplaced {
            return Some(why.clone());
        }
        let p = &self.panels[self.active];
        if !self.at_source(state) {
            return Some(format!(
                "the list is on {} here",
                show(self.label.as_deref(), &p.dir)
            ));
        }
        let names = |entries: Vec<&crate::remote::Entry>| -> Vec<String> {
            entries.into_iter().map(|e| e.name.clone()).collect()
        };
        let here = match d.kind {
            DialogKind::Rename => names(p.current().into_iter().collect()),
            DialogKind::Copy | DialogKind::Move | DialogKind::Delete | DialogKind::DeleteNow => {
                names(p.acted_on())
            }
            DialogKind::Select | DialogKind::Unselect | DialogKind::MakeFolder => return None,
        };
        if !here.is_empty() && here == d.entries {
            return None;
        }
        let list = |names: &[String]| match names {
            [] => "nothing".to_string(),
            names => names.join(", "),
        };
        Some(format!(
            "it was on {}, here it would be on {}",
            list(&d.entries),
            list(&here)
        ))
    }

    /// What `given`'s step does this pass: the step it is at next (the same one: not yet), or
    /// `None` once the state is back.
    fn given_step(&mut self, ctx: &egui::Context, given: &mut Given) -> Option<Step> {
        if given.listing.iter().any(Option::is_some) || self.uncommitted {
            return Some(given.step);
        }
        let state = &given.state;
        // The tree, the dialog and the viewer were the active folder's, on the active side.
        let at_source = self.at_source(state);
        Some(match given.step {
            Step::Lists => Step::Git,
            Step::Git => {
                let pinned = state.panels.iter().any(|p| p.pin.is_some());
                if (pinned || state.git_focus) && self.git.connect.is_some() {
                    // The panel is made over the runner the first detection makes.
                    let folder = &self.panels[self.active].dir;
                    if !self.git.detections.contains_key(folder) {
                        return Some(Step::Git);
                    }
                    let pin = (state.panels.iter().enumerate())
                        .find_map(|(side, p)| p.pin.clone().map(|repo| view::Pin { side, repo }));
                    if let Some(pin) = pin {
                        self.git.pin = Some(pin);
                        self.moved_at = None;
                    }
                }
                Step::GitFocus
            }
            // The keyboard goes into the git panel the display shows opposite the active list.
            Step::GitFocus => {
                let other = self.shown.as_ref().map(|s| &s[1 - self.active]);
                if state.git_focus && matches!(other, Some(view::Content::Git(_))) {
                    self.git.focus = true;
                    self.moved_at = None;
                }
                Step::GitParts
            }
            Step::GitParts => {
                let mut giving = std::mem::take(&mut given.git);
                let planning = |g: &Giving| g.part.plan.is_some() && g.stage.planning();
                let plans = giving.iter().filter(|g| planning(g)).count();
                giving.retain_mut(|g| {
                    let others = plans - usize::from(planning(g));
                    !self.give_git(ctx, g, others > 0, &mut given.said)
                });
                given.git = giving;
                if !given.git.is_empty() {
                    return Some(Step::GitParts);
                }
                Step::Tree
            }
            Step::Tree => {
                if at_source && state.panels[self.active].tree {
                    self.open_tree(ctx);
                } else {
                    given.tree = None;
                }
                Step::TreeRows
            }
            Step::TreeRows => {
                if let Some(mut part) = given.tree.take() {
                    if !self.give_tree_part(&mut part, &mut given.said) {
                        given.tree = Some(part);
                        return Some(Step::TreeRows);
                    }
                }
                Step::Dialog
            }
            Step::Dialog => {
                if let Some(d) = &state.dialog {
                    match self.dialog_refused(state, d) {
                        None => {
                            self.run(ctx, command(d.kind));
                            self.ops.give_dialog(d);
                            // Not on screen before this pass: its keys were meant for the list
                            // (an Enter would confirm it unseen). The owner's next are its first.
                            drop_typing(ctx);
                        }
                        Some(why) => {
                            let line = format!("{} was not opened again: {why}", word(d.kind));
                            given.said.push(line);
                        }
                    }
                }
                Step::Viewer
            }
            // The viewer as `SOPEN_STATE` opens one: at its place, in its encoding, then its view.
            Step::Viewer => {
                let v = state.viewer.clone().filter(|_| at_source)?;
                if v.revision {
                    given.said.push(format!(
                        "{}: a diff or a file at a revision is not carried across (a copy in the \
                         other machine's cache) — open it again from the git panel",
                        show(self.label.as_deref(), &v.file)
                    ));
                    return None;
                }
                let name = crate::remote::split_parent(&v.file).1.to_string();
                self.open_at = v.place;
                self.open_enc = v
                    .encoding
                    .as_deref()
                    .and_then(crate::encoding::Encoding::from_label);
                self.open_view = (v.view != 1).then_some(v.view);
                self.start_open(ctx, v.file, name);
                Step::ViewerMade
            }
            // Its wrap on the view it opens on — the text view, before `view=` gives it way to
            // another (the set chooses that once this one is ready, later in this pass or after).
            Step::ViewerMade => {
                if self.opening.is_some() {
                    return Some(Step::ViewerMade);
                }
                // It did not open: the status row says why.
                let viewer = self.viewer.as_mut()?;
                if let Some(wrap) = state.viewer.as_ref().and_then(|v| v.wrap) {
                    viewer.embedded.window.set_wrapped(wrap);
                }
                Step::ViewerShown
            }
            Step::ViewerShown => {
                // Left meanwhile: nothing more to put back.
                let viewer = self.viewer.as_mut()?;
                let window = &mut viewer.embedded.window;
                if !window.ready() || !viewer.views.settled() {
                    return Some(Step::ViewerShown);
                }
                // The view chosen has the place its own way (the hex view's cursor follows the
                // text view's top row): on the byte itself.
                if let Some(at) = state.viewer.as_ref().and_then(|v| v.place) {
                    if window.byte_place().is_some_and(|now| now != at) {
                        window.go_to_byte(at);
                    }
                }
                return None;
            }
        })
    }

    /// The git panels a switch carries (10 §7.4, v2): those on screen, by index, once each — the
    /// one rule the state's parts ([`Browser::take_state`]) and the question before the switch
    /// (`Browser::git_dropped`) both go by.
    pub(super) fn git_carried(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for c in self.shown.iter().flatten() {
            let view::Content::Git(top) = c else {
                continue;
            };
            if let Some(i) = self.git.panel(top).filter(|i| !out.contains(i)) {
                out.push(i);
            }
        }
        out
    }

    /// A given state's git panels are still being put back ([`Step::GitParts`]): what a switch now
    /// takes of them is the given state's, as it came (`Given::not_back_yet`).
    pub(super) fn git_giving(&self) -> bool {
        (self.given.as_ref()).is_some_and(|g| g.step <= Step::GitParts && !g.git.is_empty())
    }

    /// Each carried git panel's part (10 §7.4, v2), by its repository.
    fn git_parts(&self) -> Vec<GitState> {
        let panels = self.git_carried().into_iter().map(|i| &self.git.panels[i]);
        panels.map(|p| p.view.part(&p.top)).collect()
    }

    /// One pass of `g`, a git panel's part being given ([`Step::GitParts`]): `true` once it is
    /// all given or given up, what could not be said in `said`. Its filters go in as soon as its
    /// panel is made; once its first page and its snapshot are in, every commit it names is
    /// looked up here, and the bar goes to its row, pages read until it comes; then the rest of
    /// the view, its plan (the planner opened as Cmd/Ctrl+E opens it), and — once no other part's
    /// planner is still opening (`plans`) — its dialog, opened as its key opens it, what was typed
    /// once it is open (a quick edit's once its range is read, the planner's own only over the
    /// plan given). A commit this host lacks is left out — a mark or a pane's end silently but for
    /// the marks' count, the bar said; a dialog or plan on one, or one the owner's own already
    /// stands in the place of, is not given, and said.
    fn give_git(
        &mut self,
        ctx: &egui::Context,
        g: &mut Giving,
        plans: bool,
        said: &mut Vec<String>,
    ) -> bool {
        let top = g.part.top.clone();
        let at = show(self.label.as_deref(), &top);
        let mut say = |why: &str| said.push(format!("the git panel of {at}: {why}"));
        if self.git.connect.is_none() {
            say("this window has no git");
            return true;
        }
        let Some(i) = self.git.panel(&top) else {
            // Not shown here once the active folder's detection has answered: none to give it.
            let shown = (self.shown.iter().flatten())
                .any(|c| matches!(c, view::Content::Git(t) if *t == top));
            let folder = &self.panels[self.active].dir;
            if shown || !self.git.detections.contains_key(folder) {
                return false;
            }
            say("not shown here");
            return true;
        };
        match &mut g.stage {
            Stage::Panel => {
                g.stage = Stage::Loading;
                let p = &mut self.git.panels[i];
                if p.view.give_filters(&g.part) && p.stream.is_some() {
                    let changed = GitAction::FiltersChanged(p.view.filters().clone());
                    self.git_actions(ctx, i, vec![changed]);
                }
                false
            }
            Stage::Loading => {
                // As `select=` waits for its panel (`git_script`), and for the snapshot.
                let p = &self.git.panels[i];
                let idle = p.stream.as_ref().is_some_and(|s| s.asked == 0);
                let ended = !matches!(p.view.paging(), Paging::Loading) && p.stream.is_none();
                if p.read.is_some() || !(idle || ended) {
                    return false;
                }
                if p.view.repo().is_none() {
                    if let Paging::Error(why) = p.view.paging() {
                        say(&why.clone());
                        return true;
                    }
                    return false;
                }
                let Some(runner) = self.git_runner() else {
                    return false;
                };
                let commits = GitView::commits(&g.part);
                let git = RunnerGit::new(runner, Handle::new());
                let (tx, rx) = mpsc::channel();
                let waker = ctx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(look_up(&git, &top, &commits));
                    waker.request_repaint();
                });
                g.stage = Stage::LookingUp(rx);
                false
            }
            Stage::LookingUp(rx) => {
                let known = match rx.try_recv() {
                    Ok(Ok(known)) => known,
                    Err(TryRecvError::Empty) => return false,
                    Ok(Err(why)) => {
                        say(&format!("its commits were not looked up: {why}"));
                        return true;
                    }
                    Err(TryRecvError::Disconnected) => {
                        say("its commits were not looked up");
                        return true;
                    }
                };
                let view = &mut self.git.panels[i].view;
                if !view.give_bar(&g.part, &known) && g.part.bar.is_some() {
                    say("the bar's row is not here");
                }
                g.stage = Stage::Paging(known);
                false
            }
            Stage::Paging(known) => {
                let view = &mut self.git.panels[i].view;
                if view.selecting().is_some() {
                    match view.paging() {
                        Paging::Loading => return false,
                        Paging::More(_) => {
                            self.more_rows(ctx, i);
                            return false;
                        }
                        _ => {
                            view.give_up_bar();
                            say("the bar's row is not in its graph");
                        }
                    }
                }
                let known = std::mem::take(known);
                // The owner's own dialog or planner opened meanwhile: none is put in its place.
                let owners = view.dialog_up() || view.planner_model().is_some();
                let what = match (g.part.dialog.is_some(), g.part.plan.is_some()) {
                    (true, true) => Some("its dialog and plan were"),
                    (true, false) => Some("its dialog was"),
                    (false, true) => Some("its plan was"),
                    (false, false) => None,
                };
                if let Some(what) = what.filter(|_| owners) {
                    (g.part.dialog, g.part.plan) = (None, None);
                    say(&format!("{what} not given: one of yours is open"));
                }
                let (asked, lacking) = view.give_view(&g.part, &known);
                if lacking > 0 {
                    say(&format!("{lacking} of its marks are not here"));
                }
                // Find's field may stand where the pass's keys were meant for the graph.
                drop_typing(ctx);
                self.git_actions(ctx, i, asked);
                match g
                    .part
                    .plan
                    .as_ref()
                    .map(|plan| GitView::open_plan(plan, &known))
                {
                    Some(Ok(open)) => {
                        self.git_actions(ctx, i, vec![open]);
                        g.stage = Stage::Planning(known);
                    }
                    Some(Err(why)) => {
                        say(&format!("its plan was not given: {why}"));
                        g.part.plan = None;
                        g.stage = Stage::Dialog(known);
                    }
                    None => g.stage = Stage::Dialog(known),
                }
                false
            }
            Stage::Planning(known) => {
                let p = &mut self.git.panels[i];
                let given = p.view.give_plan(&g.part, known);
                match &given {
                    None if p.edit_open.is_some() => return false,
                    None => say("its plan was not given: its range did not read here"),
                    Some(Ok(())) => drop_typing(ctx),
                    Some(Err(why)) => {
                        drop_typing(ctx);
                        say(why);
                    }
                }
                // Not given: the planner's own dialog does not open on the planner as read.
                if !matches!(given, Some(Ok(()))) {
                    g.part.plan = None;
                }
                g.stage = Stage::Dialog(std::mem::take(known));
                false
            }
            Stage::Dialog(known) => {
                if g.part.dialog.is_none() {
                    return true;
                }
                if plans {
                    return false;
                }
                // The planner's own stands on its plan, given first.
                let planners = g.part.dialog.as_ref().is_some_and(GitDialog::planners);
                if planners && g.part.plan.is_none() {
                    say("its dialog was not given: its plan was not");
                    return true;
                }
                // The owner's own opened meanwhile (while the plan's planner opened, say): it stays.
                if self.git.panels[i].view.dialog_up() {
                    say("its dialog was not given: one of yours is open");
                    return true;
                }
                // One dialog at a time (X4-menu): another panel's, the list's question or the tree.
                if self.git_waits(i) {
                    say("its dialog was not opened again: another dialog has the keyboard");
                    return true;
                }
                let view = &mut self.git.panels[i].view;
                let opened = view.give_dialog(ctx, &g.part, known);
                drop_typing(ctx);
                match opened {
                    Ok(asked) => self.git_actions(ctx, i, asked),
                    Err(why) => {
                        say(&format!("its dialog was not opened again: {why}"));
                        return true;
                    }
                }
                g.stage = Stage::Opening;
                false
            }
            Stage::Opening => {
                let p = &mut self.git.panels[i];
                match p.view.give_typed(&g.part) {
                    Some(given) => {
                        drop_typing(ctx);
                        if let Err(why) = given {
                            // A quick edit's range still being read for it: it does not open.
                            if g.part.dialog.as_ref().is_some_and(GitDialog::quick) {
                                p.edit_open = None;
                            }
                            say(&why);
                        }
                        true
                    }
                    // What it opens on is still being read (a prefill).
                    None if p.prefill.is_some() || p.edit_open.is_some() => false,
                    None => {
                        say("its dialog was not opened again: what it opens on did not read");
                        true
                    }
                }
            }
        }
    }

    /// The given state is back, or ended: what its viewer's hand-overs left untaken goes (a
    /// binary's `at=` would wait for the next text file), and its lines are said.
    fn given_done(&mut self, mut given: Given) {
        // What the sender left out of its own state (`fit`), said first.
        let mut said = std::mem::take(&mut given.state.said);
        said.append(&mut given.said);
        given.said = said;
        // Ended before these were back (a listing started meanwhile: the owner's, a file
        // operation's or a git write's re-read): said, each, and the reads asked for them stopped
        // (dropping a pending read stops it), so nothing of them opens later.
        let why = "not given: a listing started before it was";
        for g in &given.git {
            let at = show(self.label.as_deref(), &g.part.top);
            given.said.push(format!("the git panel of {at}: {why}"));
            if let Some(i) = self.git.panel(&g.part.top) {
                let p = &mut self.git.panels[i];
                let quick = g.part.dialog.as_ref().is_some_and(GitDialog::quick);
                match g.stage {
                    Stage::Planning(_) => p.edit_open = None,
                    // A quick edit's dialog opens from its range's read, an action's prefilled.
                    Stage::Opening if quick => p.edit_open = None,
                    Stage::Opening => p.prefill = None,
                    _ => {}
                }
            }
        }
        if given.tree.is_some() && given.step <= Step::TreeRows {
            given
                .said
                .push(format!("the tree's folders and bar: {why}"));
        }
        if given.state.viewer.is_some() {
            self.open_at = None;
            self.open_enc = None;
            self.open_view = None;
        }
        if !given.said.is_empty() {
            self.status = Status::error(given.said.join(" · "));
        }
    }
}

/// The pass's keys, typed text, input method and pastes dropped: what a step puts back was not
/// on screen before it, so they were meant for what was (an Enter would confirm it unseen).
fn drop_typing(ctx: &egui::Context) {
    ctx.input_mut(|i| {
        i.events.retain(|e| {
            !matches!(
                e,
                egui::Event::Key { .. }
                    | egui::Event::Text(_)
                    | egui::Event::Ime(_)
                    | egui::Event::Paste(_)
            )
        })
    });
}

/// The commits of `commits` the repository at `top` has (`git cat-file --batch-check`, one
/// read): ids that came over ssh, each a full object id already (`GitState::check`), on stdin.
fn look_up(git: &RunnerGit, top: &str, commits: &[Oid]) -> Result<HashSet<Oid>, String> {
    if commits.is_empty() {
        return Ok(HashSet::new());
    }
    let ids: String = commits
        .iter()
        .map(|c| format!("{}\n", c.as_str()))
        .collect();
    let args = ["cat-file", "--batch-check=%(objecttype)"].map(String::from);
    let ran =
        (git.run(top, &args, ids.as_bytes(), history::Bound::Read)).map_err(|e| e.to_string())?;
    if ran.code != Some(0) {
        return Err(String::from_utf8_lossy(&ran.stderr).trim().to_string());
    }
    let types = String::from_utf8_lossy(&ran.stdout);
    let types = types.lines().map(str::trim);
    Ok((commits.iter().zip(types))
        .filter(|(_, kind)| *kind == "commit")
        .map(|(c, _)| c.clone())
        .collect())
}

/// The command whose key opens `kind`'s dialog.
fn command(kind: DialogKind) -> Command {
    match kind {
        DialogKind::Select => Command::SelectGroup,
        DialogKind::Unselect => Command::UnselectGroup,
        DialogKind::Copy => Command::Copy,
        DialogKind::Move => Command::Move,
        DialogKind::Rename => Command::Rename,
        DialogKind::MakeFolder => Command::MakeFolder,
        DialogKind::Delete => Command::Delete,
        DialogKind::DeleteNow => Command::DeleteNow,
    }
}

/// `kind`'s dialog as the status row names it.
fn word(kind: DialogKind) -> &'static str {
    match kind {
        DialogKind::Select => "select",
        DialogKind::Unselect => "unselect",
        DialogKind::Copy => "copy",
        DialogKind::Move => "move",
        DialogKind::Rename => "rename",
        DialogKind::MakeFolder => "make folder",
        DialogKind::Delete | DialogKind::DeleteNow => "delete",
    }
}

#[cfg(test)]
mod tests;
