//! The switch's state (10 §7.4, T32): what one browser hands another when xtools moves between
//! the terminal and a window — both panels' folders, bars, selections and scroll, the active
//! side, the mode, each side's content (the directory tree open, a pinned git panel), an open
//! dialog with its typed text, caret and refusal, the open viewer (its file, 08's view, place,
//! encoding and wrap) and the host the folders are on. No caches, streams or jobs: the receiver
//! opens them again.
//!
//! - **Taking** ([`Browser::take_state`]) reads the browser and changes nothing; taken while a
//!   given state is still being put back, the parts not back yet are the given ones.
//! - **The line** ([`SwitchState::line`], [`read`]): one JSON line, `"v"` first. A reader takes
//!   its own `v` — a field the line lacks takes its default — and refuses a newer one with §7.4's
//!   line; a field it does not know is ignored (§7.4). v1 is the first: nothing older exists to
//!   be read (§7.4's older ones once a v2 does), so a lower `v` is refused as one no sopen wrote.
//!   The line comes over ssh: it is checked before anything is done with it ([`SwitchState::check`]:
//!   absolute paths, a host label ssh cannot read as an option or as two words, a side 0 or 1).
//! - **Paths** go out and come back rewritten by a map the caller gives
//!   ([`SwitchState::rewritten`]), from and to sopen's command-line form (`host:/abs`, or `/abs`
//!   on this machine), as `forward::source_for` rewrites SOURCE (§7.3): the folders, the pinned
//!   repository, the viewer's file and a Copy / Move target typed as an absolute place — one the
//!   map cannot place keeps the dialog from opening again, the status row saying why.
//! - **Giving** ([`Browser::give_state`]), once, to a browser that has not had a pass yet: each
//!   panel lists its folder with the bar on its entry, as `SOPEN_STATE`'s `open=`, `at=` and
//!   `enc=` are handed on, and the rest follows a step a pass on the display the pass before
//!   committed — the selection and the scroll, the pin once the active folder's detection has
//!   answered, the tree, the dialog (opened as its key opens it, then its text, caret and
//!   refusal), the viewer (opened at its place, in its encoding and wrap, then its view). The
//!   browser has not settled until the last step ran; a listing it did not ask for ends it. The
//!   tree, the dialog and the viewer are opened only on the side and folder they were taken on,
//!   a dialog that acts on entries only on those entries — else the status row says why. A
//!   folder that does not list here (gone, unreadable) shows its nearest parent that does, and
//!   the status row says so (§7.3 does not say: X2-state's choice).

use super::keys::Command;
use super::{show, view, Browser, Status};
use crate::views::Slot;
use eframe::egui;
use serde::{Deserialize, Serialize};

/// The version this sopen writes, and the newest it reads (10 §7.4: each step that moves a
/// screen adds that screen's fields and bumps it).
pub(crate) const VERSION: u64 = 1;

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
    #[serde(default)]
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
    #[serde(default)]
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

/// The state on `line`, sent by `from` (HOST's name, for the refusal): its own `v` read, a field
/// the line lacks taking its default and one this sopen does not know ignored; a newer `v` is
/// refused with §7.4's line, a lower one — v1 is the first, so none was ever written — with its
/// own (§7.4's older versions are read once a v2 exists); then [`SwitchState::check`]ed.
pub(crate) fn read(line: &str, from: &str) -> Result<SwitchState, String> {
    let unread =
        |e: serde_json::Error| format!("{from} sent a switch state that does not read: {e}");
    let value: serde_json::Value = serde_json::from_str(line).map_err(unread)?;
    let Some(v) = value.get("v").and_then(serde_json::Value::as_u64) else {
        return Err(format!("{from} sent a switch state without its version"));
    };
    if v > VERSION {
        return Err(format!(
            "{from} sent switch state v{v}; this sopen reads up to v{VERSION} — update one of them"
        ));
    }
    if v < VERSION {
        return Err(format!("state version {v} is not one sopen wrote"));
    }
    let state: SwitchState = serde_json::from_value(value).map_err(unread)?;
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
}

/// The steps after the listings, one a pass (each builds on the display the one before it made).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Step {
    Lists,
    Git,
    GitFocus,
    Tree,
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
        self.given = Some(Given {
            state,
            listing: [None, None],
            fell_back: [None, None],
            said: Vec::new(),
            starting: false,
            step: Step::Lists,
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
                Step::Tree
            }
            Step::Tree => {
                if at_source && state.panels[self.active].tree {
                    self.open_tree(ctx);
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

    /// The given state is back, or ended: what its viewer's hand-overs left untaken goes (a
    /// binary's `at=` would wait for the next text file), and its lines are said.
    fn given_done(&mut self, given: Given) {
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
