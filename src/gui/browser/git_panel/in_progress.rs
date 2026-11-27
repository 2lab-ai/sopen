//! The in-progress banner (03 §8.5): the git panel's top row, in the warning colour, while git
//! has an operation in progress — `git::actions::Banner`'s text and its Continue, Skip and Abort
//! (bisect: Reset).
//!
//! [`BannerView`] is what the banner shows: its text, one row a line (bisect has a line of its
//! own beside any operation), and the steps it offers, in order — made from a
//! `git::actions::Banner` by [`BannerView::of`], or from plain text in a test. [`draw`] paints
//! it in the top rows the panel gives it, the offered steps as buttons at the right of its first
//! row. Its keys (03 §9.2) are Cmd/Ctrl+Shift+C, S, A and B — Continue, Skip, Abort, Bisect Reset
//! — read by [`keys`] where the git context's rows are read, never while a field has the typing.
//! A key or a click gives [`Pressed::Proceed`] with `Action::Proceed`, which goes through its
//! dialog (`ActionDialog::confirm`); a step the state does not offer gives
//! [`Pressed::NotOffered`], the status row's words, and does nothing (Skip in a merge).

use super::graph_view::printable;
use crate::git::actions::{Action, Banner, Proceed};
use crate::git::model::Oid;
use crate::git::model::{InProgress, Status};
use crate::git::rebase::Edit;
use crate::gui::browser::keys::{self, Mods};
use crate::gui::cells::{self, Buffer, Cells, Grid, Pen};
use crate::gui::chrome::PALETTE;
use eframe::egui::{self, Key, Sense};

/// What the banner shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerView {
    /// Its text, a row a line (`REBASE 3/7 · stopped at abc12345 fix · 2 conflicts`,
    /// `BISECT · Reset`).
    lines: Vec<String>,
    /// The steps the state offers, in order.
    offers: Vec<Proceed>,
}

/// What a banner key or button asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pressed {
    /// The step, through its dialog (03 §9.2).
    Proceed(Action),
    /// A step the state does not offer: nothing happens, and the status row says this.
    NotOffered(String),
}

/// The banner's steps, in their keys' order.
const STEPS: [Proceed; 4] = [
    Proceed::Continue,
    Proceed::Skip,
    Proceed::Abort,
    Proceed::BisectReset,
];

/// What a step's button says.
pub fn word(p: Proceed) -> &'static str {
    match p {
        Proceed::Continue => "Continue",
        Proceed::Skip => "Skip",
        Proceed::Abort => "Abort",
        Proceed::BisectReset => "Reset",
    }
}

/// A step's key, with Cmd/Ctrl+Shift (03 §9.2).
pub fn key(p: Proceed) -> Key {
    match p {
        Proceed::Continue => Key::C,
        Proceed::Skip => Key::S,
        Proceed::Abort => Key::A,
        Proceed::BisectReset => Key::B,
    }
}

/// A step's key as the help writes it: `Cmd+Shift+C`.
pub fn key_text(p: Proceed) -> String {
    format!("{}{}", Mods::CommandShift.prefix(), key(p).name())
}

impl BannerView {
    /// A banner of `text` (its lines, the rows) offering `offers`.
    pub fn new(text: impl Into<String>, offers: Vec<Proceed>) -> BannerView {
        let text = text.into();
        BannerView {
            lines: text.lines().map(str::to_string).collect(),
            offers,
        }
    }

    /// What `Banner` says: its text and its offers.
    pub fn of(banner: &Banner) -> BannerView {
        BannerView::new(banner.text(), banner.offers())
    }

    /// The banner of a refresh (03 §5, §8.5): the probe's state and the status read's
    /// conflicts, through `Banner::of`; `None` while nothing is in progress.
    pub fn probe(in_progress: &InProgress, status: &Status) -> Option<BannerView> {
        Banner::of(in_progress, status).map(|b| BannerView::of(&b))
    }

    /// The rows it takes at the panel's top.
    pub fn rows(&self) -> usize {
        self.lines.len().max(1)
    }

    pub fn offers(&self) -> &[Proceed] {
        &self.offers
    }

    /// Step `p`, by its key or its button.
    pub fn press(&self, p: Proceed) -> Pressed {
        if self.offers.contains(&p) {
            return Pressed::Proceed(Action::Proceed(p));
        }
        let state = self.lines.first().map_or("", String::as_str);
        Pressed::NotOffered(format!("{}: not offered while {state}", word(p)))
    }
}

/// The banner's keys this frame (03 §9.2), with exact modifiers: Cmd/Ctrl+Shift+C, S, A, B.
/// Read only where the git context's rows are read (no field has the typing).
pub fn keys(ctx: &egui::Context, banner: &BannerView) -> Option<Pressed> {
    STEPS.into_iter().find_map(|p| {
        ctx.input_mut(|i| keys::take(i, key(p), Mods::CommandShift))
            .then(|| banner.press(p))
    })
}

/// Whether [`draw`] and [`draw_stop`] paint the banner's cells (10 §17, X4-graph's seam): they do
/// (X4-pane), so the panel leaves none of them to egui.
pub(super) const PAINTS_CELLS: bool = true;

/// The banner at cells `area` of `grid` (the panel's top rows, [`BannerView::rows`] of them),
/// written into `buf`, the panel's buffer (10 §4, X4-pane): its lines in the warning colour, the
/// offered steps as `[ Continue ]`-style buttons at the right of the first row ([`lay`]) — a
/// click is the step's key.
pub fn draw(
    ui: &mut egui::Ui,
    grid: &Grid,
    area: Cells,
    buf: &mut Buffer,
    banner: &BannerView,
) -> Option<Pressed> {
    let words: Vec<&str> = banner.offers.iter().map(|&p| word(p)).collect();
    let k = lay(ui, grid, area, buf, &banner.lines, &words)?;
    Some(banner.press(banner.offers[k]))
}

/// The banner's `lines` at cells `area` of `grid`, written into `buf`: a line a row in the warning
/// colour on the panel's back, cut by cells (`…`); `buttons` as `[ word ]` at the right of the
/// first row, right-aligned, in the dialog's colours as the hex view's buttons outside a dialog
/// (10 §6) — never focused, the banner's keys being Cmd/Ctrl+Shift's — a blank cell of the panel's
/// back apart and from the state: a dialog's buttons need two (§6: each its own text, on the
/// dialog's own colour), these stand apart in their colour with one, which leaves an edit stop's
/// `stopped to edit` beside its four buttons on a 1000-point window's side (61 cells; its commit
/// id cut there, shown on HEAD's row of the graph). The buttons
/// come first, as egui's did: each that fits whole is laid, from the right, those that do not
/// going from the left (Amend first), and the state is cut to what is left before them. Each is
/// an `interact` rect at its cells, its id salted with the banner's cells, since two
/// repositories' panels draw into one `ui` side by side (review round 1: with one id egui kept
/// the right panel's rects for both, so the left buttons were dead and a click was both panels');
/// the one clicked, by its place in `buttons`. They sense clicks alone (`Sense::CLICK`), never the
/// focus: egui's `Sense::click()` is focusable, and Tab, then Space or Enter, would press a step.
/// The banner's first row is one more such rect, under the buttons, so a click on the state or on
/// the blank between two buttons presses none in any host (egui gives a click a few points off a
/// widget to it when nothing is under the pointer). The buttons have no hover (a cell has no
/// place for it): the status row names their keys while the banner shows
/// (`GitView::status_line`).
fn lay(
    ui: &mut egui::Ui,
    grid: &Grid,
    area: Cells,
    buf: &mut Buffer,
    lines: &[String],
    buttons: &[&str],
) -> Option<usize> {
    let p = PALETTE;
    let warn = Pen::new(p.warn, p.back);
    cells::fill(buf, area, warn);
    if area.height == 0 {
        return None;
    }
    let width = usize::from(area.width);
    // Cell `c` of the banner's row, on the grid (every `c` here is below `width`).
    let x = |c: usize| area.x.saturating_add(u16::try_from(c).unwrap_or(u16::MAX));
    // The buttons, right to left from the first row's end: where each starts.
    let mut laid: Vec<(usize, usize)> = Vec::new();
    let mut right = width;
    for (k, label) in buttons.iter().enumerate().rev() {
        let w = cells::width(label) + 4;
        let gap = usize::from(!laid.is_empty());
        let Some(at) = right.checked_sub(gap + w) else {
            break;
        };
        laid.push((k, at));
        right = at;
    }
    let state_end = laid.last().map_or(width, |&(_, at)| at.saturating_sub(1));
    let id = |part: &str| {
        ui.scope_id()
            .with(("sopen-git-banner", area.x, area.y, part))
    };
    let row = grid.rect(Cells::new(area.x, area.y, area.width, 1));
    ui.interact(row, id(""), Sense::CLICK);
    let mut pressed = None;
    let button = Pen::new(p.dialog_text, p.dialog_back);
    for &(k, at) in &laid {
        let label = format!("[ {} ]", buttons[k]);
        let w = cells::width(&label);
        cells::put(buf, x(at), area.y, &label, w, button);
        let rect = grid.rect(Cells::new(x(at), area.y, w as u16, 1));
        if ui.interact(rect, id(buttons[k]), Sense::CLICK).clicked() {
            pressed = Some(k);
        }
    }
    for (y, (k, line)) in (area.top()..area.bottom()).zip(lines.iter().enumerate()) {
        let room = if k == 0 { state_end } else { width };
        if room > 0 {
            let shown = cells::fit_start(&printable(line), room);
            cells::put(buf, area.x, y, &shown, room, warn);
        }
    }
    pressed
}

// ---------------------------------------------------------------------------------------------
// An edit stop's Amend and Split (03 §8.5; S4-B). S3-B's banner above is unchanged: these
// items stand beside it, and S3-B's steps come back as S3-B's `Pressed`. S4-C wires them
// (`GitView`'s banner and the key table's Cmd/Ctrl+Shift+M and P).

/// An edit stop's own steps (03 §8.5: "with Amend, Split, Continue, Abort"), beside the banner's
/// Continue and Abort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopStep {
    /// §7.6's `amend`: the staged changes into the stopped commit.
    Amend,
    /// §7.8.1's `split`: the stopped commit's paths recorded, then `reset HEAD^`.
    Split,
}

/// The edit stop's steps, in their keys' order.
pub const STOP_STEPS: [StopStep; 2] = [StopStep::Amend, StopStep::Split];

/// What a step's button says.
pub fn stop_word(s: StopStep) -> &'static str {
    match s {
        StopStep::Amend => "Amend",
        StopStep::Split => "Split",
    }
}

/// A step's key, with Cmd/Ctrl+Shift as the banner's others (S4-B's proposal for 03 §9.2:
/// aMend, sPlit — C, S, A and B are the banner's already).
pub fn stop_key(s: StopStep) -> Key {
    match s {
        StopStep::Amend => Key::M,
        StopStep::Split => Key::P,
    }
}

/// A step's key as the help writes it: `Cmd+Shift+M`.
pub fn stop_key_text(s: StopStep) -> String {
    format!("{}{}", Mods::CommandShift.prefix(), stop_key(s).name())
}

/// An edit stop (03 §8.5: `stopped to edit abc12345`): the commit it stopped at, as the banner
/// names it, and in full when the probe read it so — `stopped-sha`, else the second word of
/// `done`'s last line (`edit <c> …`), each taken only when it is a whole object id that starts
/// with the banner's. Without the full id Split is not offered (`Edit::SplitStop` names a whole
/// id); Amend needs none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditStop {
    short: String,
    commit: Option<Oid>,
}

/// What an edit stop's key or button asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopPressed {
    /// Amend: §7.6's `amend` dialog on the stopped commit (`ActionDialog::open(ActionId::Amend,
    /// &Target::Uncommitted, …)`, its message prefilled from the commit), so "keep its message"
    /// or "edit it" stays the owner's choice.
    Amend,
    /// Split: this edit through its dialog (`planner::QuickDialog::confirm`).
    Split(Edit),
    /// A step the state does not offer: nothing happens, and the status row says this.
    NotOffered(String),
}

/// What the banner of an edit stop gives: S3-B's steps as S3-B's [`Pressed`], or Amend / Split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopEnd {
    Banner(Pressed),
    Stop(StopPressed),
}

impl EditStop {
    /// The edit stop `banner` shows (`Banner::edit_stop`), or `None` when it shows none, its
    /// commit as git names it (`stopped-sha`, `done`): the commit the todo named, which is not
    /// HEAD once an earlier commit was rewritten (S4-C round 3). The panel's Split takes HEAD
    /// ([`EditStop::probe`]); S4-B's tests read git's name here.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn of(banner: &Banner) -> Option<EditStop> {
        let short = banner.edit_stop()?.to_string();
        let rebase = banner.state.rebase.as_ref()?;
        let stopped = rebase.stopped_sha.as_deref().map(str::trim);
        let done = rebase
            .done_last
            .as_deref()
            .and_then(|l| l.split_whitespace().nth(1));
        let commit = [stopped, done]
            .into_iter()
            .flatten()
            .filter_map(Oid::parse)
            .find(|o| o.as_str().starts_with(&short));
        Some(EditStop { short, commit })
    }

    /// The edit stop of a refresh (03 §5, §8.5), through `Banner::of`; `None` while there is none.
    /// Its Split splits `head`, the commit checked out, as git's own split recipe does: git's
    /// `stopped-sha` and `done` name the commit the todo named, and once an earlier commit was
    /// rewritten the stopped one is replayed, so that id is not HEAD (*measured, git 2.56.1*:
    /// `stopped-sha` and `done` held `9c6719af…` after an earlier reword, HEAD `fe7c9e26…`; S4-C
    /// round 3). The banner keeps git's name.
    pub fn probe(
        in_progress: &InProgress,
        status: &Status,
        head: Option<&Oid>,
    ) -> Option<EditStop> {
        let short = Banner::of(in_progress, status)?.edit_stop()?.to_string();
        Some(EditStop::new(short, head.cloned()))
    }

    /// An edit stop at `short`, its full id `commit` when known (S4-C may resolve it itself).
    pub fn new(short: impl Into<String>, commit: Option<Oid>) -> EditStop {
        EditStop {
            short: short.into(),
            commit,
        }
    }

    /// The commit it stopped at, in full, when known.
    pub fn commit(&self) -> Option<&Oid> {
        self.commit.as_ref()
    }

    /// The steps it offers: Amend, and Split when the commit is known in full.
    pub fn offers(&self) -> Vec<StopStep> {
        STOP_STEPS
            .into_iter()
            .filter(|s| *s == StopStep::Amend || self.commit.is_some())
            .collect()
    }

    /// Step `s`, by its key or its button.
    pub fn press(&self, s: StopStep) -> StopPressed {
        match (s, &self.commit) {
            (StopStep::Amend, _) => StopPressed::Amend,
            (StopStep::Split, Some(commit)) => StopPressed::Split(Edit::SplitStop {
                commit: commit.clone(),
            }),
            (StopStep::Split, None) => StopPressed::NotOffered(format!(
                "Split: the stopped commit {} was not read in full",
                self.short
            )),
        }
    }
}

/// Step `s` while `banner` shows: the edit stop's ([`EditStop::press`]), or without one
/// (`stop` `None`) nothing, the status row saying so, as for a step the state does not offer.
/// The key table's Cmd/Ctrl+Shift+M and P rows (03 §9.2, S4-C) come here.
pub fn stop_step(banner: &BannerView, stop: Option<&EditStop>, s: StopStep) -> StopPressed {
    match stop {
        Some(stop) => stop.press(s),
        None => {
            let state = banner.lines.first().map_or("", String::as_str);
            StopPressed::NotOffered(format!("{}: not offered while {state}", stop_word(s)))
        }
    }
}

/// An edit stop's keys this frame, with exact modifiers: Cmd/Ctrl+Shift+M (Amend) and
/// Cmd/Ctrl+Shift+P (Split) (03 §9.2), each [`stop_step`]. The panel reads them through the key
/// table's rows instead (S4-C, so F1 lists them); S4-B's tests read them here.
#[cfg_attr(not(test), allow(dead_code))] // The key table's rows are the panel's.
pub fn stop_keys(
    ctx: &egui::Context,
    banner: &BannerView,
    stop: Option<&EditStop>,
) -> Option<StopPressed> {
    STOP_STEPS.into_iter().find_map(|s| {
        ctx.input_mut(|i| keys::take(i, stop_key(s), Mods::CommandShift))
            .then(|| stop_step(banner, stop, s))
    })
}

/// The banner of an edit stop at cells `area` of `grid`, as [`draw`] draws a banner, with Amend
/// and Split before S3-B's steps at the right of its first row (03 §8.5's order: Amend, Split,
/// Continue, Abort); a click is the step's key.
pub fn draw_stop(
    ui: &mut egui::Ui,
    grid: &Grid,
    area: Cells,
    buf: &mut Buffer,
    banner: &BannerView,
    stop: &EditStop,
) -> Option<StopEnd> {
    let steps = stop.offers();
    let words: Vec<&str> = (steps.iter().map(|&s| stop_word(s)))
        .chain(banner.offers.iter().map(|&p| word(p)))
        .collect();
    let k = lay(ui, grid, area, buf, &banner.lines, &words)?;
    Some(match steps.get(k) {
        Some(&s) => StopEnd::Stop(stop.press(s)),
        None => StopEnd::Banner(banner.press(banner.offers[k - steps.len()])),
    })
}
