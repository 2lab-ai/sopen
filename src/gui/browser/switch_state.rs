//! The switch's state (10 §7.4, T32): what one browser hands another when xtools moves between
//! the terminal and a window — both panels' folders, bars, selections and scroll, the active
//! side, the mode, each side's content (the directory tree open, a pinned git panel), an open
//! dialog with its typed text, caret and refusal, the open viewer (its file, 08's view, place,
//! encoding and wrap) and the host the folders are on. No caches, streams or jobs: the receiver
//! opens them again.
//!
//! - **Taking** ([`Browser::take_state`]) reads the browser and changes nothing.
//! - **The line** ([`SwitchState::line`], [`read`]): one JSON line, `"v"` first. A reader takes
//!   its own `v` — a field the line lacks takes its default — and refuses a newer one with §7.4's
//!   line; a field it does not know is ignored (§7.4). v1 is the first: nothing older exists to
//!   be read (§7.4's older ones once a v2 does), so a lower `v` is refused as one no sopen wrote.
//! - **Paths** go out and come back rewritten by a map the caller gives
//!   ([`SwitchState::rewritten`]), from and to sopen's command-line form (`host:/abs`, or `/abs`
//!   on this machine), as `forward::source_for` rewrites SOURCE (§7.3): the folders, the pinned
//!   repository, the viewer's file and a Copy / Move target typed as an absolute place.
//! - **Giving** ([`Browser::give_state`]) to a browser that has not had a pass yet: each panel
//!   lists its folder with the bar on its entry, as `SOPEN_STATE`'s `open=`, `at=` and `enc=` are
//!   handed on, and the rest follows a step a pass once the display it builds on is committed —
//!   the selection and the scroll, the pin once the active folder's detection has answered, the
//!   tree, the dialog (opened as its key opens it, then its text, caret and refusal), the viewer
//!   (opened at its place and encoding, then its view, then its wrap). The browser has not
//!   settled until the last step ran. A folder that does not list here (gone, unreadable) shows
//!   its nearest parent that does, and the status row says so (§7.3 does not say: X2-state's
//!   choice); the tree, the dialog and the viewer, which were the active folder's, then stay
//!   closed.

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
    /// The encoding its lines are read in (08 §7.1's label).
    #[serde(default)]
    pub encoding: Option<String>,
    /// Whether the text view wraps.
    #[serde(default)]
    pub wrap: Option<bool>,
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
    #[cfg_attr(not(test), allow(dead_code))] // X2-host's and X2-switch's; only tests until then.
    pub(crate) fn line(&self) -> String {
        serde_json::to_string(self).expect("a switch state is always JSON")
    }

    /// The state with every path rewritten by `map` (10 §7.3), which takes and gives sopen's
    /// command-line form — `host:/abs`, or `/abs` on the machine it runs on — as
    /// `forward::source_for` takes SOURCE: out, this machine's `/abs` becomes
    /// `<this machine>:/abs` and HOST's own `HOST:/abs` its `/abs`; back, the other way. The
    /// folders, the pinned repositories and the viewer's file are all on the state's host, so
    /// they must come back on one host, and absolute; a Copy / Move target typed as an absolute
    /// place (it reads as sopen's command line, 04 §5.2) is rewritten too, its caret kept on the
    /// same character.
    #[cfg_attr(not(test), allow(dead_code))] // X2-switch's; only tests until then.
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
        Ok(out)
    }
}

/// A Copy / Move target that reads as an absolute place (`host:/abs`, `/abs`), rewritten by
/// `map`; its caret and selection keep their characters where the change is at its start (a
/// host put before it or taken off), else go to its end. Anything else — a name, a path relative
/// to the folder, a place `map` cannot place (another machine's, on the way back) — is left as
/// typed: it is the owner's text, never a reason to refuse the switch.
fn retarget(d: &mut DialogState, map: impl Fn(&str) -> anyhow::Result<String>) {
    let Some(text) = d.text.clone() else {
        return;
    };
    let absolute = match crate::target::parse(&text) {
        Ok(crate::target::Target::Remote(spec)) => spec.path.starts_with('/'),
        Ok(crate::target::Target::Local(path)) => path.is_absolute(),
        Err(_) => false,
    };
    let Some(to) = absolute.then(|| map(&text).ok()).flatten() else {
        return;
    };
    let (was, now) = (text.chars().count(), to.chars().count());
    let shift = |at: usize| {
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
/// own (§7.4's older versions are read once a v2 exists).
#[cfg_attr(not(test), allow(dead_code))] // X2-host's and X2-switch's; only tests until then.
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
    serde_json::from_value(value).map_err(unread)
}

/// What is still to be put back of a state given to a browser ([`Browser::give_state`]).
pub(crate) struct Given {
    state: SwitchState,
    /// Each panel's folder while its listing is on its way (the one asked for, or a parent).
    listing: [Option<String>; 2],
    /// A panel whose folder did not list here: that folder and why — the panel shows the
    /// nearest parent that lists instead.
    fell_back: [Option<(String, String)>; 2],
    /// The status row's line for the panels that fell back, once their parents listed.
    said: Vec<String>,
    step: Step,
}

/// The steps after the listings, one a pass (each builds on the display the one before it made).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Lists,
    Git,
    GitFocus,
    Tree,
    Dialog,
    Viewer,
    ViewerShown,
}

impl Browser {
    /// The switch's state of this browser (10 §7.4): nothing in it changes. `ctx` is the one
    /// the browser is drawn in (a dialog field's caret is its edit state there).
    #[cfg_attr(not(test), allow(dead_code))] // X2-host's and X2-switch's; only tests until then.
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
        // A diff or a file at a revision is a copy in this machine's cache (03 §6.2): not
        // carried (§7.4: no caches).
        let viewer = self.viewer.as_ref().filter(|v| !v.views.read_only());
        let viewer = viewer.map(|v| {
            let window = &v.embedded.window;
            let set = &v.views.set;
            let on = set.on_screen();
            let slot = Slot::ALL
                .into_iter()
                .find(|&s| on.is_some() && set.slot(s) == on);
            // The text view's encoding, else the one chosen for the file's text views (D21).
            let encoding = (window.encoding().map(|(e, _)| e.label()))
                .or_else(|| set.encoding())
                .map(str::to_string);
            ViewerState {
                file: v.abs.clone(),
                view: slot.map_or(1, Slot::number),
                place: window.byte_place(),
                encoding,
                wrap: window.wrapped(),
            }
        });
        SwitchState {
            v: VERSION,
            host: self.label.clone(),
            mode: self.mode.into(),
            active: self.active,
            panels: [panel(0), panel(1)],
            git_focus: self.git.focus,
            dialog: self.ops.open_dialog(ctx),
            viewer,
        }
    }

    /// Gives `state` to this browser, made on the state's host and not yet drawn: what each
    /// side lists, and the rest once that is listed (see the module's header). `Err`: the state
    /// is another host's (its paths are not this window's).
    #[cfg_attr(not(test), allow(dead_code))] // X2-host's and X2-switch's; only tests until then.
    pub(crate) fn give_state(&mut self, state: SwitchState) -> Result<(), String> {
        if state.host != self.label {
            let host = |h: &Option<String>| h.clone().unwrap_or_else(|| "this machine".into());
            return Err(format!(
                "the switch's state is of {}, this window of {}",
                host(&state.host),
                host(&self.label)
            ));
        }
        self.mode = state.mode.into();
        self.active = state.active.min(1);
        for (i, p) in state.panels.iter().enumerate() {
            self.list(Some(i), p.folder.clone(), p.bar.clone(), None);
        }
        let listing = state.panels.clone().map(|p| Some(p.folder));
        self.given = Some(Given {
            state,
            listing,
            fell_back: [None, None],
            said: Vec::new(),
            step: Step::Lists,
        });
        Ok(())
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
        given.listing[i] = Some(parent.clone());
        self.list(Some(i), parent, Some(name), Some(ctx));
        true
    }

    /// The given state's next step, once the display the one before it made is committed (after
    /// [`Browser::resolve`], every pass).
    pub(super) fn given_pass(&mut self, ctx: &egui::Context) {
        let Some(mut given) = self.given.take() else {
            return;
        };
        // A window drawn only on input still takes each step: the next pass comes at once after a
        // step, soon while one waits for a worker (detection, the viewer), and once more at the
        // end (the page a wrap changed is laid out again).
        match self.given_step(ctx, &given) {
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

    /// What `given`'s step does this pass: the step it is at next (the same one: not yet), or
    /// `None` once the state is back.
    fn given_step(&mut self, ctx: &egui::Context, given: &Given) -> Option<Step> {
        if given.listing.iter().any(Option::is_some) || self.uncommitted {
            return Some(given.step);
        }
        let state = &given.state;
        // The tree, the dialog and the viewer were the active folder's: not where it fell back.
        let moved = given.fell_back[self.active].is_some();
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
                if !moved && state.panels[self.active].tree {
                    self.open_tree(ctx);
                }
                Step::Dialog
            }
            Step::Dialog => {
                if let Some(d) = state.dialog.clone().filter(|_| !moved) {
                    self.run(ctx, command(d.kind));
                    self.ops.give_dialog(&d);
                }
                Step::Viewer
            }
            // The viewer as `SOPEN_STATE` opens one: at its place, in its encoding, then its view.
            Step::Viewer => {
                let v = state.viewer.clone().filter(|_| !moved)?;
                let name = crate::remote::split_parent(&v.file).1.to_string();
                self.open_at = v.place;
                self.open_enc = v
                    .encoding
                    .as_deref()
                    .and_then(crate::encoding::Encoding::from_label);
                self.open_view = (v.view != 1).then_some(v.view);
                self.start_open(ctx, v.file, name);
                Step::ViewerShown
            }
            Step::ViewerShown => {
                if self.opening.is_some() {
                    return Some(Step::ViewerShown);
                }
                // It did not open: the status row says why.
                let viewer = self.viewer.as_mut()?;
                let window = &mut viewer.embedded.window;
                if !window.ready() || !viewer.views.settled() {
                    return Some(Step::ViewerShown);
                }
                let wanted = state.viewer.as_ref();
                // The view chosen has the place its own way (the hex view's cursor follows the
                // text view's top row): on the byte itself.
                if let Some(at) = wanted.and_then(|v| v.place) {
                    if window.byte_place().is_some_and(|now| now != at) {
                        window.go_to_byte(at);
                    }
                }
                // The wrap last: a jump on its way still lands, under it.
                if let Some(wrap) = wanted.and_then(|v| v.wrap) {
                    window.set_wrapped(wrap);
                }
                return None;
            }
        })
    }

    /// The given state is back: what its viewer's hand-overs left untaken goes (a binary's `at=`
    /// would wait for the next text file), and the line for the folders that fell back is said.
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

#[cfg(test)]
mod tests;
