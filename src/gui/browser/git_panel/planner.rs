//! The history planner (03 §7.8.2) and the quick edits' dialogs (§7.8.1): a pane over the git
//! panel listing the range's commits, newest at the top as in the graph, with their verbs, its
//! keys, and the run's confirmation — `git::rebase`'s model and plans. S4-B writes it.
//!
//! - **Seams.** Everything that is `git::rebase`'s logic comes through function pointers given
//!   at each call, never stored: [`EditPlanner`] (`rebase::plan`, [`PLAN`]) for the dialogs and
//!   [`Seams`] (`Model::apply`, `Model::changed`, `Model::problems` as text lines and
//!   `rebase::plan`, [`SEAMS`]) for the pane, as S3-B's dialogs take `actions::plan`. The tests
//!   give fakes; the integration gives these.
//! - **A quick edit's dialog** ([`QuickDialog`], §7.8.1, §8.1) starts from the [`Commit`] it edits
//!   (for `squash`, also the commit below): reword's message, squash's two messages joined
//!   (`rebase::squash_message`, the todo's own rule), author's name, email and dates. It holds the edit as its options make
//!   it, plans it again whenever that changes, and shows the plan's confirmation: the line of what
//!   is lost first, the options, what changes, the backups, the exact command lines, the notes, or
//!   the refusal. Enter or Run returns the edit ([`QuickEnd::Run`]) only when the plan shown is
//!   the plan of exactly that edit and nothing refuses it; the dialog runs nothing. A message
//!   that is not valid UTF-8 is never put into an editor (03 §3.3): reword and squash then refuse
//!   with the reason and never plan. `plan` is not a dialog: it opens the planner
//!   ([`QuickOpened::Planner`]). [`QuickDialog::confirm`] is the dialog of an edit built
//!   elsewhere: an edit stop's Split (`Edit::SplitStop`), a split's Continue.
//! - **In cells** (X4-quick, 10 §17): each is `ops::dialog`'s box on the panels' grid, as an
//!   action's dialog is (`dialogs.rs`) — the lost line, the message field (six rows, Shift+Enter
//!   a new line) or author's Name, Email and Author date fields and its committer date (a row of
//!   one of several), the plan in the kit's rows that scroll (`dialogs::plan_part`), `[ Run ]`
//!   (off until the plan is in, nothing refuses it and the grid holds all of the box), `[ Cancel ]`
//!   and the hint — and [`QuickDialog::covers`] says its cells. Its fields open afresh: the
//!   message's caret at its start, a one-line field's at its end. Its keys are an action's
//!   dialog's: each Enter of a pass goes to what has the keyboard at that point, one egui cannot
//!   follow is dropped, the buttons take the kit's presses, and only what the screen showed runs
//!   (`cells::OnScreen`, of a revision of what the box shows, never a copy of the draft), else
//!   `dialogs::CHANGED` says so; an Enter that commits an input method's composition runs nothing.
//! - **The planner** ([`PlannerView`], [`draw`]) lists the range newest first — verb, hash,
//!   subject, author, date, and `A` where the author or a date changed — with a bar of its own,
//!   in cells over the panel's body (10 §17, X4-planner): the columns two blanks apart, cut by
//!   cells, the scroll bar on the border beside the rows, and at its foot the checks that fail,
//!   the note, `[ Run… ]`, `[ Close ]` and the keys' hint; its own dialogs are still egui's.
//!   Its keys, read first while it has the keyboard, a press at a time in the pass's order: ↑ ↓
//!   PgUp PgDn Home End the bar; typed `P R E S F D` the bar's verb, `A` its author and dates,
//!   every other typed character nothing (no typed letter starts Find); Cmd/Ctrl+↑/↓ move the
//!   row; Enter the message of a `reword` or `squash` row (a squash row's is its group's);
//!   Cmd/Ctrl+Enter the run's confirmation, with its options (`--autostash`, `--update-refs`)
//!   and the checks that fail; Esc closes, asking first when something changed. Each key becomes
//!   an [`Op`] the model applies or refuses; a refusal is the pane's note, in the error colour.

use super::dialogs::{
    plan_part, plan_rows, plan_wants, remote_warning, step_line, width, PlanRow, CHANGED, TOO_SHORT,
};
use super::graph_view::{self, printable};
use super::menu;
use super::Scroll;
use crate::git::actions::{Plan, Refused, Repo};
use crate::git::model::Oid;
use crate::git::rebase::{
    self, Author, Base, Commit, CommitterDate, Date, Edit, EditId, Model, Op, Problem, Quick,
    Toward, Verb,
};
use crate::gui::browser::keys::{self, Mods, Step};
use crate::gui::browser::ops::dialog::{self as kit, Button, Ink, Input, Inputs, Part};
use crate::gui::cells::{self, Buffer, Cells, Grid, Pen};
use crate::gui::chrome::git::LOOK;
use crate::gui::chrome::{self, PALETTE};
use crate::gui::term;
use eframe::egui::{self, Color32, Event, Key, RichText, Sense};

// ---------------------------------------------------------------------------------------------
// The seams.

/// What plans a history edit for a dialog: `rebase::plan`, pure (it reads and runs nothing).
pub type EditPlanner = fn(&Edit, &Repo) -> Result<Plan, Refused>;

/// The real planner, for the integration.
pub const PLAN: EditPlanner = rebase::plan;

/// What the planner pane asks of `git::rebase`'s model, given at each pass.
#[derive(Debug, Clone, Copy)]
pub struct Seams {
    /// `Model::apply`: an op applied, or refused with the reason.
    pub apply: fn(&mut Model, Op) -> Result<(), Refused>,
    /// `Model::changed`: Esc asks first (03 §7.8.2).
    pub changed: fn(&Model) -> bool,
    /// `Model::problems`, as the lines the pane shows (`Problem::text`).
    pub problems: fn(&Model, &Repo) -> Vec<String>,
    /// `rebase::plan`, for Cmd/Ctrl+Enter.
    pub plan: EditPlanner,
}

/// The real seams, for the integration.
pub const SEAMS: Seams = Seams {
    apply: Model::apply,
    changed: Model::changed,
    problems: problem_lines,
    plan: rebase::plan,
};

/// `Model::problems` as the pane shows them (S4-B does not match on the variants).
fn problem_lines(model: &Model, repo: &Repo) -> Vec<String> {
    model.problems(repo).iter().map(Problem::text).collect()
}

// ---------------------------------------------------------------------------------------------
// Words.

/// The run's `--autostash` (03 §7.8.2), in the words of S3-B's rebase dialog.
pub const AUTOSTASH: &str = "Stash my changes for it";
/// The run's `--update-refs`, offered on git ≥ 2.38 (03 §7.8.2).
pub const UPDATE_REFS: &str = "Move other branches along";
/// The author dialog's button that puts the commit's own author and dates back.
pub const OWN_AUTHOR: &str = "The commit's own";
/// The author dialog's refusal of a date that does not read.
pub const BAD_DATE: &str = "the author date is not YYYY-MM-DD HH:MM:SS ±hhmm (or @<unix> ±hhmm)";
/// What Enter says on a row without a message to edit.
pub const ENTER_HINT: &str = "Enter edits the message of a reword or squash row";
/// What Esc asks when something changed (03 §7.8.2).
pub const ASK_CLOSE: &str =
    "Close the planner and lose its changes? Nothing in the repository has changed.";
/// The pane of an empty range.
pub const NO_ROWS: &str = "no commits in the range";

/// `author`'s committer date (03 §7.8.1): keep (the default), now, the author date.
const COMMITTER: [(CommitterDate, &str); 3] = [
    (CommitterDate::Keep, "keep"),
    (CommitterDate::Now, "now"),
    (CommitterDate::Author, "same as the author date"),
];

/// Why a commit's message is not edited (03 §3.3: never prefilled into an editor).
fn lossy_text(commit: &Oid) -> String {
    format!(
        "{}: its message is not valid UTF-8; editing it would change its bytes",
        commit.short()
    )
}

// ---------------------------------------------------------------------------------------------
// Dates: the author dialog's date field.

/// A zone offset as git writes it, `+0900`; `None` past ±23:59, which no zone has.
fn zone_text(offset_minutes: i32) -> Option<String> {
    let a = offset_minutes.unsigned_abs();
    let (h, m) = (a / 60, a % 60);
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    (h <= 23).then(|| format!("{sign}{h:02}{m:02}"))
}

/// `+0900` as minutes; `None` for anything else.
fn parse_zone(text: &str) -> Option<i32> {
    let b = text.as_bytes();
    if b.len() != 5 || !b[1..].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let sign = match b[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let h = i32::from(b[1] - b'0') * 10 + i32::from(b[2] - b'0');
    let m = i32::from(b[3] - b'0') * 10 + i32::from(b[4] - b'0');
    (h <= 23 && m <= 59).then_some(sign * (h * 60 + m))
}

/// The civil date of `days` since 1970-01-01 (H. Hinnant's algorithm); `None` outside years 0 to
/// 9999.
fn civil(days: i64) -> Option<(i64, i64, i64)> {
    // About ±10 000 years: nothing below can overflow.
    if days.unsigned_abs() > 4_000_000 {
        return None;
    }
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (0..=9999).contains(&year).then_some((year, month, day))
}

/// Days since 1970-01-01 of a civil date (H. Hinnant's algorithm).
fn days_of(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn month_days(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// A date as the author dialog's field shows it: `YYYY-MM-DD HH:MM:SS ±hhmm` in the date's own
/// zone; past the calendar's four-digit years, git's own `@<unix> ±hhmm` (03 §7.8.2's form). An
/// offset no zone has (past ±23:59) is shown as minutes, which [`parse_date`] refuses.
pub fn date_text(d: &Date) -> String {
    let Some(zone) = zone_text(d.offset_minutes) else {
        return format!("@{} {}min", d.seconds, d.offset_minutes);
    };
    let local = d.seconds.checked_add(i64::from(d.offset_minutes) * 60);
    let civil_form = local.and_then(|local| {
        let (y, mo, da) = civil(local.div_euclid(86_400))?;
        let s = local.rem_euclid(86_400);
        Some(format!(
            "{y:04}-{mo:02}-{da:02} {:02}:{:02}:{:02} {zone}",
            s / 3_600,
            s % 3_600 / 60,
            s % 60
        ))
    });
    civil_form.unwrap_or_else(|| format!("@{} {zone}", d.seconds))
}

/// `n` ASCII digits as a number.
fn digits(text: &str, n: usize) -> Option<i64> {
    (text.len() == n && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// The date the field's `text` names: `YYYY-MM-DD HH:MM[:SS] ±hhmm` in that zone, or
/// `@<unix> ±hhmm`; `None` for anything else.
pub fn parse_date(text: &str) -> Option<Date> {
    let words: Vec<&str> = text.split_whitespace().collect();
    match words[..] {
        [raw, zone] if raw.starts_with('@') => Some(Date {
            seconds: raw[1..].parse().ok()?,
            offset_minutes: parse_zone(zone)?,
        }),
        [day, time, zone] => {
            let offset_minutes = parse_zone(zone)?;
            let d: Vec<&str> = day.split('-').collect();
            let [y, mo, da] = d[..] else { return None };
            let (y, mo, da) = (digits(y, 4)?, digits(mo, 2)?, digits(da, 2)?);
            if !(1..=12).contains(&mo) || !(1..=month_days(y, mo)).contains(&da) {
                return None;
            }
            let t: Vec<&str> = time.split(':').collect();
            let (h, mi, s) = match t[..] {
                [h, mi] => (digits(h, 2)?, digits(mi, 2)?, 0),
                [h, mi, s] => (digits(h, 2)?, digits(mi, 2)?, digits(s, 2)?),
                _ => return None,
            };
            if h > 23 || mi > 59 || s > 59 {
                return None;
            }
            let local = days_of(y, mo, da) * 86_400 + h * 3_600 + mi * 60 + s;
            Some(Date {
                seconds: local - i64::from(offset_minutes) * 60,
                offset_minutes,
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Form parts, in the dialog look (S3-B's).

/// A one-line text field after its heading.
fn line_field(ui: &mut egui::Ui, heading: &str, text: &mut String, id: egui::Id) {
    ui.horizontal(|ui| {
        ui.label(heading);
        ui.add(
            chrome::field(text)
                .id(id)
                .desired_width(ui.available_width()),
        );
    });
}

/// A message field: several lines, Shift+Enter a new line (Enter runs the dialog).
fn message_field(ui: &mut egui::Ui, heading: &str, text: &mut String, id: egui::Id) {
    ui.label(heading);
    ui.add(
        egui::TextEdit::multiline(text)
            .id(id)
            .text_color(PALETTE.field_text)
            .desired_rows(6)
            .desired_width(ui.available_width())
            .return_key(egui::KeyboardShortcut::new(
                egui::Modifiers::SHIFT,
                Key::Enter,
            )),
    );
}

/// An option on or off: whether it changed.
fn toggle(ui: &mut egui::Ui, on: &mut bool, word: &str) -> bool {
    let clicked = chrome::choice(ui, *on, word).clicked();
    if clicked {
        *on = !*on;
    }
    clicked
}

/// A heading and its lines, each a line of its own (nothing when there are none).
fn lines(ui: &mut egui::Ui, heading: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    ui.label(RichText::new(heading).weak());
    ui.indent(heading, |ui| {
        for line in lines {
            ui.add(egui::Label::new(line).wrap());
        }
    });
}

/// A refusal, in the dialog's error colour.
fn refusal(ui: &mut egui::Ui, why: &str) {
    let error = ui.visuals().error_fg_color;
    ui.add(egui::Label::new(RichText::new(why).color(error)).wrap());
}

/// The confirmation of a plan (03 §8.1, §8.4) after the options — what changes, the backups,
/// the command lines, the notes, each overwrite on a remote — or the refusal: the dialog's own
/// (`own`) first, else the plan's.
fn plan_ui(ui: &mut egui::Ui, own: Option<&str>, planned: Option<&Result<Plan, Refused>>) {
    if let Some(why) = own {
        refusal(ui, why);
        return;
    }
    let plan = match planned {
        None => return,
        Some(Err(Refused(why))) => {
            refusal(ui, why);
            return;
        }
        Some(Ok(plan)) => plan,
    };
    match &plan.confirm {
        Some(c) => {
            lines(ui, "Changes", &c.changes);
            lines(
                ui,
                "Backed up first, under refs/sopen-backup/<stamp>/",
                &c.backups,
            );
            lines(ui, "Runs", &c.commands);
            for note in &c.notes {
                ui.add(egui::Label::new(note).wrap());
            }
        }
        None => {
            let steps: Vec<String> = plan.steps.iter().map(step_line).collect();
            lines(ui, "Runs", &steps);
        }
    }
    for change in plan.remote_changes() {
        refusal(ui, &remote_warning(change));
    }
}

/// The plan's line of what is lost (03 §8.1: first in the dialog).
fn loses(planned: Option<&Result<Plan, Refused>>) -> Option<String> {
    match planned {
        Some(Ok(plan)) => plan.confirm.as_ref()?.loses.clone(),
        _ => None,
    }
}

/// The plan's title, if it has one.
fn plan_title(planned: Option<&Result<Plan, Refused>>) -> Option<String> {
    match planned {
        Some(Ok(plan)) => plan
            .confirm
            .as_ref()
            .map(|c| c.title.clone())
            .filter(|t| !t.is_empty()),
        _ => None,
    }
}

/// Run and Cancel, and the keys' hint: whether Run and Cancel were clicked, and Cancel's id
/// (Enter while it has the focus cancels).
fn run_buttons(ui: &mut egui::Ui, ready: bool, run: &str) -> (bool, bool, egui::Id) {
    let (mut go, mut no, mut id) = (false, false, egui::Id::NULL);
    ui.horizontal(|ui| {
        go = ui.add_enabled(ready, egui::Button::new(run)).clicked();
        let cancel = ui.add(egui::Button::new("Cancel"));
        id = cancel.id;
        no = cancel.clicked();
    });
    ui.label(RichText::new("Enter runs · Esc cancels").weak());
    (go, no, id)
}

/// `author`'s options (03 §7.8.1), and the planner's `A`: as typed, the date as its field's text.
#[derive(Debug, Clone)]
struct AuthorForm {
    name: String,
    email: String,
    date: String,
    committer: CommitterDate,
}

impl AuthorForm {
    fn of(a: &Author) -> AuthorForm {
        AuthorForm {
            name: a.name.clone(),
            email: a.email.clone(),
            date: date_text(&a.date),
            committer: a.committer,
        }
    }

    /// The author as the fields say it, or why not (a date that does not read). The name and
    /// email go as typed: the plan refuses what git would.
    fn author(&self) -> Result<Author, &'static str> {
        Ok(Author {
            name: self.name.clone(),
            email: self.email.clone(),
            date: parse_date(&self.date).ok_or(BAD_DATE)?,
            committer: self.committer,
        })
    }

    /// Its fields; the first (`id(0)`) has the typing.
    fn ui(&mut self, ui: &mut egui::Ui, id: impl Fn(usize) -> egui::Id) {
        line_field(ui, "Name", &mut self.name, id(0));
        line_field(ui, "Email", &mut self.email, id(1));
        line_field(ui, "Author date", &mut self.date, id(2));
        ui.horizontal_wrapped(|ui| {
            ui.label("Committer date");
            for (value, word) in COMMITTER {
                if chrome::choice(ui, self.committer == value, word).clicked() {
                    self.committer = value;
                }
            }
        });
    }
}

/// Gives `id` the typing when nothing has it.
fn type_into(ctx: &egui::Context, id: egui::Id) {
    if ctx.memory(|m| m.focused()).is_none() {
        ctx.memory_mut(|m| m.request_focus(id));
    }
}

// ---------------------------------------------------------------------------------------------
// The quick edits' dialogs (03 §7.8.1, §8.1).

/// What opening a quick edit gave.
#[derive(Debug)]
pub enum QuickOpened {
    /// The dialog: draw it with [`QuickDialog::ui`] until it ends.
    Dialog(Box<QuickDialog>),
    /// No options and a plan with no confirmation (03 §8.1): run it now. A history rewrite's
    /// plan has one (its backup), so this is S3-B's rule kept, not a case S4 expects.
    Run(Edit),
    /// `plan` (Edit history from here…): open the planner on this base, `rebase::open`'s.
    Planner(Base),
}

/// How a quick edit's dialog ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickEnd {
    /// Enter or Run on the plan shown: the edit as chosen, for the caller to plan and run.
    Run(Edit),
    /// Esc, Cancel or a click outside it.
    Cancel,
}

/// A quick edit's dialog, or the confirmation of an edit built elsewhere, open (see the module's
/// notes).
#[derive(Debug)]
pub struct QuickDialog {
    /// What it is called until a plan names it.
    label: String,
    /// The edit as the options make it now.
    draft: Draft,
    /// reword's and squash's message field.
    message: Option<String>,
    /// author's fields.
    author: Option<AuthorForm>,
    /// Its own refusal no option lifts: a message that is not UTF-8, no commit below.
    refused: Option<String>,
    repo: Repo,
    /// The draft's revision: one more each time its fields change it.
    drafted: u64,
    /// The draft's revision last planned and its plan.
    planned: Option<(u64, Result<Plan, Refused>)>,
    /// What the box shows besides its fields, as the draft, its plan and its refusal made it
    /// last ([`QuickDialog::update`]).
    shows: Shows,
    /// What the box shows, as a number: one more each time the draft changes or `shows` does, so
    /// a pass's painting is this, never a copy of the draft (X4-quick round 1: a 1 MiB message).
    revision: u64,
    /// The field in an input method's composition, as the last pass left it.
    composing: Option<egui::Id>,
    /// Its first field took the keyboard last pass (on its first, not yet).
    typing: bool,
    /// The first of the plan's rows shown.
    plan_top: usize,
    /// A Run or an Enter came when what would run was not what the screen showed, and was not
    /// acted on: the hint says the plan changed and Enter runs it.
    changed: bool,
    /// What the screen shows of the dialog — the painting of the last pass that reached it — and
    /// the last pass's painting; nothing until a pass has painted where it can be seen (egui lays
    /// a new modal out unseen the first time).
    screen: cells::OnScreen<Painting>,
    /// A pass has laid the box out (the next one paints where it can be seen).
    laid_once: bool,
    /// What each pass was told the screen shows (the tests: one answer a frame).
    #[cfg(test)]
    answers: Vec<Option<Painting>>,
}

/// What a pass of a quick edit's dialog painted: the revision of what it shows — the draft, its
/// title, its line of what is lost, its plan's rows — and whether its Run was on. A Run or an
/// Enter runs only what the screen shows ([`cells::OnScreen`]), equal to what would run at its
/// pass's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Painting {
    revision: u64,
    runs: bool,
}

/// What a quick edit's dialog shows besides its fields: its own refusal (the plan's rows say it),
/// its title, its line of what is lost and its plan's rows ([`plan_rows`]).
#[derive(Debug, Clone, Default, PartialEq)]
struct Shows {
    own: Option<String>,
    title: String,
    lost: Option<String>,
    plan: Vec<PlanRow>,
}

/// The bytes of a message a quick edit's dialog copied: counted in a test build ([`copied`]),
/// whose bound is a pass that copies none (X4-quick round 1: a 1 MiB message).
#[cfg(not(test))]
#[inline(always)]
fn copying(_: usize) {}

#[cfg(test)]
fn copying(bytes: usize) {
    COPIED.with(|c| c.set(c.get() + bytes));
}

#[cfg(test)]
thread_local! {
    static COPIED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The bytes of messages the quick edits' dialogs copied on this thread since the last call.
#[cfg(test)]
pub(super) fn copied() -> usize {
    COPIED.with(|c| c.replace(0))
}

/// `edit`, copied: its message's bytes counted ([`copying`]).
fn copy_of(edit: &Edit) -> Edit {
    if let Edit::Quick(Quick::Reword { message, .. } | Quick::Squash { message, .. }) = edit {
        copying(message.len());
    }
    edit.clone()
}

/// A quick edit's dialog's edit. Every copy of it counts its message's bytes ([`copy_of`]),
/// wherever it is made: a `clone()` of the draft that comes back in a pass is seen (X4-quick
/// fix-check), not only the copies the dialog says it makes.
#[derive(Debug)]
struct Draft(Edit);

impl Clone for Draft {
    fn clone(&self) -> Self {
        Draft(copy_of(&self.0))
    }
}

impl std::ops::Deref for Draft {
    type Target = Edit;

    fn deref(&self) -> &Edit {
        &self.0
    }
}

impl std::ops::DerefMut for Draft {
    fn deref_mut(&mut self) -> &mut Edit {
        &mut self.0
    }
}

/// A quick edit's dialog's nominal width in cells, as egui's was (10 §6's rule widens it to its
/// widest text, at most 76 cells inside).
const QUICK_WIDTH: usize = 72;

/// The rows a quick edit's message field shows, as egui's did.
const MESSAGE_ROWS: u16 = 6;

/// What a quick edit's dialog says Enter and Esc do (03 §8.1), as an action's dialog does.
const RUN_HINT: &str = "Enter runs · Esc cancels";

fn quick_id() -> egui::Id {
    egui::Id::unique("sopen-git-quick")
}

/// The id of a quick edit's dialog's `n`th text field; the first has the typing.
pub(super) fn quick_field(n: usize) -> egui::Id {
    quick_id().with(("field", n))
}

/// In cells: the box's `[ Run ]`.
pub(super) fn quick_run_id() -> egui::Id {
    quick_id().with("run")
}

/// In cells: the box's `[ Cancel ]`.
pub(super) fn quick_cancel_id() -> egui::Id {
    quick_id().with("cancel")
}

/// In cells: `author`'s row of committer dates (03 §7.8.1).
pub(super) fn committer_id() -> egui::Id {
    quick_id().with("committer")
}

impl QuickDialog {
    /// The dialog of history edit `id` on `commit` (03 §7.8.1) — the menu's choice, or F2's and
    /// F8's — prefilled from the commit as S4-C read it, `below` its parent (squash's older
    /// message), and planned once. `plan` gives [`QuickOpened::Planner`].
    pub fn open(
        id: EditId,
        commit: &Commit,
        below: Option<&Commit>,
        repo: &Repo,
        planner: EditPlanner,
    ) -> QuickOpened {
        let c = commit.row.oid.clone();
        let mut message = None;
        let mut author = None;
        let mut refused = None;
        let quick = match id {
            EditId::Plan => return QuickOpened::Planner(Base::Commit(c)),
            EditId::Reword => {
                match commit.message_lossy {
                    true => refused = Some(lossy_text(&c)),
                    false => message = Some(commit.message.clone()),
                }
                Quick::Reword {
                    commit: c,
                    message: message.clone().unwrap_or_default(),
                }
            }
            EditId::Squash => {
                match below {
                    _ if commit.message_lossy => refused = Some(lossy_text(&c)),
                    None => refused = Some(format!("{}: there is no commit below it", c.short())),
                    Some(b) if b.message_lossy => refused = Some(lossy_text(&b.row.oid)),
                    Some(b) => message = Some(rebase::squash_message(&b.message, &commit.message)),
                }
                Quick::Squash {
                    commit: c,
                    message: message.clone().unwrap_or_default(),
                }
            }
            EditId::Author => {
                author = Some(AuthorForm::of(&commit.author));
                Quick::Author {
                    commit: c,
                    author: commit.author.clone(),
                }
            }
            EditId::Fixup => Quick::Fixup { commit: c },
            EditId::MoveUp => Quick::Move {
                commit: c,
                toward: Toward::Up,
            },
            EditId::MoveDown => Quick::Move {
                commit: c,
                toward: Toward::Down,
            },
            EditId::Drop => Quick::Drop { commit: c },
            EditId::Edit => Quick::Edit { commit: c },
            EditId::Split => Quick::Split { commit: c },
            EditId::Fold => Quick::Fold { into: c },
        };
        let label = menu::history_label(id).to_string();
        let dialog = QuickDialog::with(label, Edit::Quick(quick), message, author, refused, repo);
        dialog.start(planner)
    }

    /// The dialog of an edit built elsewhere, with no options (03 §8.1): an edit stop's Split
    /// (`Edit::SplitStop`), a split's Continue (`Edit::ContinueSplit`).
    pub fn confirm(edit: Edit, repo: &Repo, planner: EditPlanner) -> QuickOpened {
        let label = match &edit {
            Edit::Quick(q) => menu::history_label(q.id()),
            Edit::Plan(_) => "Run the plan",
            Edit::SplitStop { .. } => "Split",
            Edit::ContinueSplit(_) => "Continue",
        };
        let dialog = QuickDialog::with(label.to_string(), edit, None, None, None, repo);
        dialog.start(planner)
    }

    fn with(
        label: String,
        draft: Edit,
        message: Option<String>,
        author: Option<AuthorForm>,
        refused: Option<String>,
        repo: &Repo,
    ) -> QuickDialog {
        QuickDialog {
            label,
            draft: Draft(draft),
            message,
            author,
            refused,
            repo: repo.clone(),
            drafted: 0,
            planned: None,
            shows: Shows::default(),
            revision: 0,
            composing: None,
            typing: false,
            plan_top: 0,
            changed: false,
            screen: cells::OnScreen::default(),
            laid_once: false,
            #[cfg(test)]
            answers: Vec::new(),
        }
    }

    /// What each pass was told the screen shows, as `(revision, runs)` (the tests).
    #[cfg(test)]
    pub(super) fn answers(&self) -> Vec<Option<(u64, bool)>> {
        let pair = |p: &Painting| (p.revision, p.runs);
        self.answers.iter().map(|a| a.as_ref().map(pair)).collect()
    }

    fn start(mut self, planner: EditPlanner) -> QuickOpened {
        self.update(planner);
        let options = self.message.is_some() || self.author.is_some();
        if !options && matches!(&self.planned, Some((_, Ok(Plan { confirm: None, .. })))) {
            return QuickOpened::Run(self.draft.0);
        }
        QuickOpened::Dialog(Box::new(self))
    }

    /// The edit as the options make it now (while the dialog refuses it, as it last was).
    pub fn draft(&self) -> &Edit {
        &self.draft
    }

    /// The plan of the draft, as last planned.
    pub fn planned(&self) -> Option<&Result<Plan, Refused>> {
        self.planned.as_ref().map(|(_, p)| p)
    }

    /// A refresh came while the dialog is open: the draft is planned again against `repo`.
    pub fn set_repo(&mut self, repo: Repo) {
        self.repo = repo;
        self.planned = None;
    }

    /// Its own refusal: one no option lifts, else a date that does not read.
    fn own_refusal(&self) -> Option<String> {
        self.refused.clone().or_else(|| {
            self.author
                .as_ref()
                .and_then(|f| f.author().err())
                .map(str::to_string)
        })
    }

    /// The draft from the fields (a date that does not read leaves it as it was); whether it
    /// changed, its revision one more. A message is compared, and copied only when it changed.
    fn sync(&mut self) -> bool {
        let changed = match &mut *self.draft {
            Edit::Quick(Quick::Reword { message, .. } | Quick::Squash { message, .. }) => {
                match &self.message {
                    Some(m) if m != message => {
                        copying(m.len());
                        message.clone_from(m);
                        true
                    }
                    _ => false,
                }
            }
            Edit::Quick(Quick::Author { author, .. }) => {
                match self.author.as_ref().map(AuthorForm::author) {
                    Some(Ok(a)) if a != *author => {
                        *author = a;
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        };
        self.drafted += u64::from(changed);
        changed
    }

    /// The draft planned again when it changed since the last plan; whether it was. Never while
    /// the dialog refuses it.
    fn replan(&mut self, planner: EditPlanner, own: bool) -> bool {
        let stale = self
            .planned
            .as_ref()
            .is_none_or(|(at, _)| *at != self.drafted);
        if own || !stale {
            return false;
        }
        let plan = planner(&self.draft, &self.repo);
        self.planned = Some((self.drafted, plan));
        true
    }

    /// The draft from its fields, planned again when it changed, and what the box shows made
    /// again when either did or the dialog's own refusal changed: its revision one more when the
    /// draft or what it shows changed — a changed draft always — and whether it did.
    fn update(&mut self, planner: EditPlanner) -> bool {
        let drafted = self.sync();
        let own = self.own_refusal();
        let planned = self.replan(planner, own.is_some());
        if !drafted && !planned && own == self.shows.own {
            return false;
        }
        let shown = match own {
            Some(_) => None,
            None => self.planned(),
        };
        let shows = Shows {
            title: plan_title(shown).unwrap_or_else(|| self.label.trim_end_matches('…').into()),
            lost: loses(shown),
            plan: plan_rows(own.clone(), shown),
            own,
        };
        let changed = drafted || shows != self.shows;
        self.shows = shows;
        self.revision += u64::from(changed);
        changed
    }

    /// The plan shown is the draft's, it is not refused, and the dialog refuses nothing.
    fn ready(&self) -> bool {
        self.shows.own.is_none()
            && matches!(&self.planned, Some((at, Ok(_))) if *at == self.drafted)
    }

    /// The ids of its text fields, in order: the message, or author's name, email and date.
    fn fields(&self) -> Vec<egui::Id> {
        let n = match (&self.message, &self.author) {
            (Some(_), _) => 1,
            (None, Some(_)) => 3,
            (None, None) => 0,
        };
        (0..n).map(quick_field).collect()
    }

    /// The box on `grid`, and whether it stands whole there: if not, it says so first
    /// ([`TOO_SHORT`]) and its Run is off.
    fn layout(&self, grid: Grid) -> (kit::Layout, bool) {
        let whole = self.laid(grid, true);
        if whole.fits() {
            return (whole, true);
        }
        (self.laid(grid, false), false)
    }

    /// The box on `grid`: the lost line first, then the options — the message, or author's
    /// fields and its committer date — the plan in rows that scroll ([`plan_part`]), the buttons
    /// and the hint; as wide as its widest text by 10 §6's rule. Not `fits`: [`TOO_SHORT`] before
    /// all of it, Run off.
    fn laid(&self, grid: Grid, fits: bool) -> kit::Layout {
        let Shows {
            title, lost, plan, ..
        } = &self.shows;
        let wanted = (plan_wants(plan))
            .max(lost.as_deref().map_or(0, cells::width))
            .max(cells::width(title) + 2);
        let width = kit::inside_width(grid.cols, QUICK_WIDTH, wanted);
        let mut parts = Vec::new();
        if !fits {
            parts.push(Part::Text(TOO_SHORT.to_string(), Ink::Error));
        }
        if let Some(lost) = lost {
            parts.extend([Part::Text(lost.clone(), Ink::Text), Part::Blank]);
        }
        let field = |label: &str, n| Part::Field {
            label: label.to_string(),
            id: quick_field(n),
        };
        if self.message.is_some() {
            parts.push(Part::Message {
                label: "Message".to_string(),
                id: quick_field(0),
                rows: MESSAGE_ROWS,
            });
            parts.push(Part::Blank);
        } else if self.author.is_some() {
            parts.extend([field("Name", 0), field("Email", 1), field("Author date", 2)]);
            parts.push(Part::Choices {
                label: "Committer date".to_string(),
                id: committer_id(),
                choices: COMMITTER.iter().map(|(_, w)| w.to_string()).collect(),
                many: false,
            });
            parts.push(Part::Blank);
        }
        if let Some(part) = plan_part(plan.clone(), grid, width, self.plan_top) {
            parts.extend([part, Part::Blank]);
        }
        let mut run = Button::new("Run", quick_run_id());
        run.on = fits && self.ready();
        parts.push(Part::Buttons(vec![
            run,
            Button::new("Cancel", quick_cancel_id()),
        ]));
        // Said only while what shows can run.
        parts.push(match self.changed && fits && self.ready() {
            true => Part::Text(CHANGED.to_string(), Ink::Text),
            false => Part::Text(RUN_HINT.to_string(), Ink::Weak),
        });
        kit::layout(grid, title, QUICK_WIDTH, &parts)
    }

    /// What a pass paints as the dialog stands now, the box `fits` its grid or not.
    fn painted(&self, fits: bool) -> Painting {
        Painting {
            revision: self.revision,
            runs: fits && self.ready(),
        }
    }

    /// The dialog has just opened: its fields start afresh, whatever an earlier dialog left
    /// under the same ids — the message with its caret at its start (the kit's rule for a
    /// message field), each one-line field with its caret at its end, as egui puts a new one's.
    fn fresh_carets(&self, ctx: &egui::Context) {
        use egui::text::{CCursor, CCursorRange};
        use egui::text_edit::TextEditState;
        if self.message.is_some() {
            TextEditState::default().store(ctx, quick_field(0));
        }
        if let Some(f) = &self.author {
            for (n, text) in [&f.name, &f.email, &f.date].into_iter().enumerate() {
                let mut state = TextEditState::default();
                let end = CCursor::new(text.chars().count());
                state.cursor.set_char_range(Some(CCursorRange::one(end)));
                state.store(ctx, quick_field(n));
            }
        }
    }

    /// The cells of `grid` (the panels') the dialog covers, its box and its shadow, for the panel
    /// and the other side to blank (10 §17, X4-graph's seam), as the dialog stands now.
    pub fn covers(&self, grid: &Grid) -> Vec<Cells> {
        self.layout(*grid).0.covers()
    }

    /// One pass on `grid` (the panels'), in cells through the kit (see the module's notes): Esc
    /// and Enter are taken before its fields see them (Shift+Enter is the message's new line);
    /// `planner` plans the draft again whenever an option changed it.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        grid: Grid,
        planner: EditPlanner,
    ) -> Option<QuickEnd> {
        if self.update(planner) {
            ctx.request_repaint();
        }
        let escape = ctx.input_mut(|i| keys::take(i, Key::Escape, Mods::None));
        let began = ctx.memory(|m| m.focused());
        let fields = self.fields();
        // egui moves the keyboard by an arrow unless a field has it (its caret's).
        let in_field = fields.iter().any(|id| began == Some(*id));
        let enters = take_enters(ctx, began.is_some() && !in_field, in_field);
        // An Enter that commits an input method's composition — in a pass of its composition, or
        // while it composes — is the composition's (an integration that passes it on sends it
        // with the commit), never the dialog's: it runs nothing. A composition is its field's,
        // and one may end with no event at all (egui-winit drops winit's `Disabled`): a pointer
        // press ends it — a click interrupts the input method — and so does the keyboard on
        // another control than the field that composes (X4-quick fix-check).
        let pressed = ctx.input(|i| i.pointer.any_pressed());
        let composing = (self.composing).filter(|id| began == Some(*id) && !pressed);
        let composed = ctx.input(|i| composition(&i.events, composing.is_some()));
        let enters = match composing.is_some() || composed.is_some() {
            true => Vec::new(),
            false => enters,
        };
        self.composing = match composed {
            Some(true) => began.filter(|id| fields.contains(id)),
            Some(false) => None,
            None => composing,
        };
        let (layout, fits) = self.layout(grid);
        // What the owner saw — the painting of the last pass that reached the screen (the
        // terminal prints none while input waits) — and what this pass paints.
        let seen = self.screen.start(ctx).copied();
        #[cfg(test)]
        self.answers.push(seen);
        if std::mem::replace(&mut self.laid_once, true) {
            let painting = self.painted(fits);
            self.screen.painted(painting);
        } else {
            self.fresh_carets(ctx);
        }
        // The first field takes the keyboard as the dialog opens, and while nothing has it.
        if !fields.is_empty() && (!self.typing || began.is_none()) {
            ctx.memory_mut(|m| m.request_focus(quick_field(0)));
        }
        self.typing = !fields.is_empty();
        let (run, cancel) = (quick_run_id(), quick_cancel_id());
        let mut committer = (self.author.as_ref())
            .and_then(|f| COMMITTER.iter().position(|(c, _)| *c == f.committer));
        let mut by_id = Vec::new();
        if let Some(text) = &mut self.message {
            by_id.push((quick_field(0), Input::Text(text)));
        }
        if let Some(AuthorForm {
            name, email, date, ..
        }) = &mut self.author
        {
            for (n, text) in [name, email, date].into_iter().enumerate() {
                by_id.push((quick_field(n), Input::Text(text)));
            }
            by_id.push((committer_id(), Input::One(&mut committer)));
        }
        // The buttons take the kit's presses, in the pass's order (F-kit-tab).
        by_id.extend([run, cancel].map(|b| (b, Input::Button)));
        let inputs = Inputs {
            top: Some(&mut self.plan_top),
            by_id,
            ..Inputs::default()
        };
        let pass = kit::show(ctx, quick_id(), &layout, inputs);
        let now = ctx.memory(|m| m.focused());
        if let (Some(f), Some(k)) = (&mut self.author, committer) {
            f.committer = COMMITTER[k].0;
        }
        // The options as this pass left them, planned: the next pass shows it.
        if self.update(planner) {
            ctx.request_repaint();
        }
        let mut cancelled = escape || pass.clicked(cancel) || pass.modal.should_close();
        let mut ran = pass.clicked(run);
        for after_tab in enters {
            match if after_tab { now } else { began } {
                Some(id) if id == cancel => cancelled = true,
                _ => ran = true,
            }
        }
        if cancelled {
            return Some(QuickEnd::Cancel);
        }
        if !ran {
            return None;
        }
        // Only what the screen showed runs: what would run now — this pass's typing and options,
        // a refresh's plan — is what the last pass painted, its Run on; else the screen shows it
        // now and the next Run or Enter runs it.
        let (_, fits_now) = self.layout(grid);
        let end = self.painted(fits_now);
        if seen != Some(end) {
            self.changed |= seen.is_some();
            ctx.request_repaint();
            return None;
        }
        end.runs.then(|| QuickEnd::Run(self.draft.clone().0))
    }
}

/// Whether `events` leave an input method composing (its last preedit not empty), or `None`
/// when they hold no composition at all (egui's `ImeEvent`: a preedit, then its commit), from
/// `composing`. An empty preedit or commit with nothing composing is none, as egui ignores it
/// (Wayland sends them as the IME is allowed or its cursor area set).
fn composition(events: &[Event], composing: bool) -> Option<bool> {
    let (mut now, mut seen) = (composing, false);
    for e in events {
        let text = match e {
            Event::Ime(egui::ImeEvent::Preedit { text, .. } | egui::ImeEvent::Commit(text)) => text,
            _ => continue,
        };
        if text.is_empty() && !now {
            continue;
        }
        now = matches!(e, Event::Ime(egui::ImeEvent::Preedit { .. })) && !text.is_empty();
        seen = true;
    }
    seen.then_some(now)
}

/// The pass's presses of Enter with no modifiers, taken out of its input in order, each saying
/// whether it came after the pass's first move of the keyboard — `dialogs::take_enters`, an
/// action's dialog's, word for word (X4-dialogs; private there while X4-dialogs2 holds
/// `dialogs.rs`). Where egui cannot follow the moves the owner made before an Enter — after a
/// move in a pass with a Shift+Tab or an arrow that moves the keyboard (egui moves it later), or
/// after a second move (egui moves it once a pass) — that Enter is dropped, and a Space there
/// too, with its text, unless the pass began on a text field (`on_text`), which types it.
fn take_enters(ctx: &egui::Context, arrows_move: bool, on_text: bool) -> Vec<bool> {
    let arrow = |key: &Key| {
        matches!(
            key,
            Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight
        )
    };
    // Whether a key moves the keyboard as egui reads it (`None`: not), and whether egui makes
    // that move only later (`true`: a Shift+Tab, an arrow) rather than in the pass (a Tab).
    let moves = |e: &Event| match e {
        Event::Key {
            key: Key::Tab,
            pressed: true,
            modifiers,
            ..
        } if !modifiers.any() || modifiers.shift_only() => Some(modifiers.shift),
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } if arrows_move && arrow(key) && !modifiers.any() => Some(true),
        _ => None,
    };
    ctx.input_mut(|i| {
        let later = i.events.iter().any(|e| moves(e) == Some(true));
        let (mut moved, mut out, mut typed) = (0usize, Vec::new(), false);
        // A press here goes where egui cannot follow the owner's moves.
        let lost = |moved: usize| moved >= 2 || (moved == 1 && later);
        i.events.retain(|e| match (e, std::mem::take(&mut typed)) {
            (Event::Text(text), true) if text == " " => false,
            (
                Event::Key {
                    key: Key::Enter,
                    pressed: true,
                    modifiers,
                    ..
                },
                _,
            ) if Mods::None.matches(*modifiers) => {
                if !lost(moved) {
                    out.push(moved > 0);
                }
                false
            }
            (
                Event::Key {
                    key: Key::Space,
                    pressed: true,
                    modifiers,
                    ..
                },
                _,
            ) if !on_text && lost(moved) && Mods::None.matches(*modifiers) => {
                typed = true;
                false
            }
            (e, _) => {
                moved += usize::from(moves(e).is_some());
                true
            }
        });
        out
    })
}

// ---------------------------------------------------------------------------------------------
// The planner (03 §7.8.2).

/// How the planner ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannerEnd {
    /// Enter on the run's confirmation: `Edit::Plan` of the model shown, for the caller to plan
    /// and run.
    Run(Edit),
    /// Esc with nothing changed, or after the question; the pane closes.
    Close,
}

/// The pane's note: an op's refusal (error colour), or what a key does.
#[derive(Debug, Clone)]
enum Note {
    Refused(String),
    Hint(&'static str),
}

/// A dialog over the pane; while one is up the pane reads no key. Each keeps the ids its
/// buttons had last pass: Enter while one of them has the focus is that button's click, as in
/// S3-B's dialogs (Enter on a focused Cancel cancels).
#[derive(Debug)]
enum Inner {
    /// Enter on a `reword` or `squash` row: its message (a squash row's: its group's).
    Message {
        commit: Oid,
        title: String,
        text: String,
        cancel: Option<egui::Id>,
    },
    /// `A`: the row's author and dates; `own` the commit's.
    Author {
        commit: Oid,
        title: String,
        own: Author,
        form: AuthorForm,
        /// The Cancel and "The commit's own" buttons.
        cancel: Option<egui::Id>,
        own_button: Option<egui::Id>,
    },
    /// Esc with something changed; `keep` the "Keep planning" button.
    Ask { keep: Option<egui::Id> },
    /// Cmd/Ctrl+Enter: the run's options and confirmation, the model last planned and its plan.
    Run {
        planned: Option<(Model, Result<Plan, Refused>)>,
        cancel: Option<egui::Id>,
    },
}

/// The planner, open over the git panel (see the module's notes).
#[derive(Debug)]
pub struct PlannerView {
    model: Model,
    repo: Repo,
    scroll: Scroll,
    note: Option<Note>,
    inner: Option<Inner>,
    /// The edit and plan the run dialog showed when Enter returned [`PlannerEnd::Run`] (S4-C:
    /// what runs is the plan shown, [`PlannerView::take_shown`]).
    shown: Option<(Edit, Plan)>,
}

fn inner_id(kind: &str) -> egui::Id {
    egui::Id::unique(("sopen-git-planner", kind))
}

/// The message dialog's field: one id for every row's.
fn message_field_id() -> egui::Id {
    inner_id("message").with("field")
}

/// The author dialog's field `n` (the name, the email, the author date): one id for every row's.
fn author_field_id(n: usize) -> egui::Id {
    inner_id("author").with(("field", n))
}

/// The first line of `text`, at most `n` characters, control characters shown.
fn first_line(text: &str, n: usize) -> String {
    let line = text.lines().next().unwrap_or("");
    printable(&line.chars().take(n).collect::<String>()).into_owned()
}

/// The message of the squash group row `k` is in (03 §7.8.2): the group's own when one of its
/// squash rows has it (each of them holds the same), else the group's messages joined, the
/// oldest first (`rebase::squash_message`, the rule the todo writes): its first row's (a
/// reword's own), then each squash row's; a fixup row's is left out. The group is the nearest
/// row below that is neither squash, fixup nor dropped, and the squash, fixup and dropped rows
/// above it. A message that is not UTF-8 is refused, as reword's is.
fn group_message(model: &Model, k: usize) -> Result<String, String> {
    let rows = &model.rows;
    let member = |v: Verb| matches!(v, Verb::Squash | Verb::Fixup | Verb::Drop);
    let mut top = k;
    while top > 0 && member(rows[top - 1].verb) {
        top -= 1;
    }
    let mut first = k + 1;
    while first < rows.len() && member(rows[first].verb) {
        first += 1;
    }
    let end = first.min(rows.len() - 1);
    let group = &rows[top..=end];
    if let Some(m) = group
        .iter()
        .filter(|r| r.verb == Verb::Squash)
        .find_map(|r| r.message.clone())
    {
        return Ok(m);
    }
    let mut parts: Vec<&str> = Vec::new();
    if let Some(r) = rows.get(first) {
        match &r.message {
            Some(m) if r.verb == Verb::Reword => parts.push(m),
            _ if r.commit.message_lossy => return Err(lossy_text(&r.commit.row.oid)),
            _ => parts.push(&r.commit.message),
        }
    }
    for r in rows[top..first].iter().rev() {
        if r.verb == Verb::Squash {
            if r.commit.message_lossy {
                return Err(lossy_text(&r.commit.row.oid));
            }
            parts.push(&r.commit.message);
        }
    }
    let mut parts = parts.into_iter();
    let joined = parts.next().unwrap_or_default().to_string();
    Ok(parts.fold(joined, |older, newer| rebase::squash_message(&older, newer)))
}

/// One of the pane's presses (03 §7.8.2), as [`PlannerView::keys`] reads them.
#[derive(Debug)]
enum PaneKey {
    /// Cmd/Ctrl+↑/↓: the bar's row moved.
    Move(Toward),
    /// Cmd/Ctrl+Enter: the run's confirmation.
    Run,
    /// Esc.
    Close,
    /// Enter: the message of a `reword` or `squash` row.
    Message,
    /// ↑ ↓ PgUp PgDn Home End: the bar.
    Step(Step),
    /// Typed text, or an input method's commit.
    Typed(String),
}

impl PaneKey {
    /// Takes the earliest of `events` that is one of the pane's: a press (or a repeat) of its
    /// keys with exactly their modifiers, or typed text. The others stay where they are. With
    /// it, where it stood, so what is left of a text can be put back there.
    fn take(events: &mut Vec<Event>) -> Option<(usize, PaneKey)> {
        let (at, pressed) =
            (events.iter().enumerate()).find_map(|(at, e)| Some((at, PaneKey::of(e)?)))?;
        events.remove(at);
        Some((at, pressed))
    }

    fn of(e: &Event) -> Option<PaneKey> {
        let (key, modifiers) = match e {
            Event::Text(t) | Event::Ime(egui::ImeEvent::Commit(t)) => {
                return Some(PaneKey::Typed(t.clone()))
            }
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => (*key, *modifiers),
            _ => return None,
        };
        let with = |mods: Mods| mods.matches(modifiers);
        Some(match key {
            Key::ArrowUp if with(Mods::Command) => PaneKey::Move(Toward::Up),
            Key::ArrowDown if with(Mods::Command) => PaneKey::Move(Toward::Down),
            Key::Enter if with(Mods::Command) => PaneKey::Run,
            Key::Escape if with(Mods::None) => PaneKey::Close,
            Key::Enter if with(Mods::None) => PaneKey::Message,
            Key::ArrowUp if with(Mods::None) => PaneKey::Step(Step::Up),
            Key::ArrowDown if with(Mods::None) => PaneKey::Step(Step::Down),
            Key::PageUp if with(Mods::None) => PaneKey::Step(Step::PageUp),
            Key::PageDown if with(Mods::None) => PaneKey::Step(Step::PageDown),
            Key::Home if with(Mods::None) => PaneKey::Step(Step::Home),
            Key::End if with(Mods::None) => PaneKey::Step(Step::End),
            _ => return None,
        })
    }
}

impl PlannerView {
    /// The planner of `model` (`rebase::open`'s, or `Model::new` in a test) on `repo`, the bar
    /// on the newest row.
    pub fn new(model: Model, repo: &Repo) -> PlannerView {
        PlannerView {
            model,
            repo: repo.clone(),
            scroll: Scroll::default(),
            note: None,
            inner: None,
            shown: None,
        }
    }

    /// The model as the keys have made it.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// A refresh came while the planner is open: the checks and the run's plan are made again
    /// against `repo`; the model, the owner's plan, stays.
    pub fn set_repo(&mut self, repo: Repo) {
        self.repo = repo;
        if let Some(Inner::Run { planned, .. }) = &mut self.inner {
            *planned = None;
        }
    }

    /// The bar's row, by its commit.
    pub fn bar(&self) -> Option<&Oid> {
        self.model
            .rows
            .get(self.scroll.bar)
            .map(|r| &r.commit.row.oid)
    }

    /// A dialog of the planner is up (the message, the author, the question, the run): count it
    /// in the panel's `dialog_up()`.
    pub fn dialog_up(&self) -> bool {
        self.inner.is_some()
    }

    /// The cells of `grid` (the panels') the planner's own dialog covers, its box and its shadow,
    /// for the panel and the other side to blank (10 §17, X4-graph's seam): none while egui draws
    /// them, until X4-planner2.
    pub fn covers(&self, _grid: &Grid) -> Vec<Cells> {
        Vec::new()
    }

    /// While the run's confirmation is up (Cmd/Ctrl+Enter): the edit it plans, for the reads
    /// its plan needs (`rebase::needs`), which the integration makes and hands back through
    /// [`PlannerView::set_repo`].
    pub fn run_draft(&self) -> Option<Edit> {
        match &self.inner {
            Some(Inner::Run { .. }) => Some(Edit::Plan(self.model.clone())),
            _ => None,
        }
    }

    /// The edit and the plan the run dialog showed when it returned [`PlannerEnd::Run`], taken
    /// once.
    pub fn take_shown(&mut self) -> Option<(Edit, Plan)> {
        self.shown.take()
    }

    /// The pane's note: the last op's refusal, or what a key does.
    pub fn note(&self) -> Option<&str> {
        match &self.note {
            Some(Note::Refused(why)) => Some(why),
            Some(Note::Hint(hint)) => Some(hint),
            None => None,
        }
    }

    /// `op` applied through the seam, the bar staying on its row; a refusal becomes the note.
    fn apply(&mut self, op: Op, seams: &Seams) {
        let bar = self.bar().cloned();
        self.note = match (seams.apply)(&mut self.model, op) {
            Ok(()) => None,
            Err(Refused(why)) => Some(Note::Refused(why)),
        };
        let at = bar.and_then(|b| self.model.rows.iter().position(|r| r.commit.row.oid == b));
        if let Some(at) = at {
            self.scroll.to(at);
        }
    }

    /// The bar's row and its index.
    fn bar_row(&self) -> Option<(usize, &rebase::Row)> {
        let k = self.scroll.bar;
        self.model.rows.get(k).map(|r| (k, r))
    }

    /// Enter: the message editor of a `reword` or `squash` row.
    fn edit_message(&mut self) {
        let Some((k, row)) = self.bar_row() else {
            return;
        };
        let commit = row.commit.row.oid.clone();
        let text = match row.verb {
            Verb::Reword => match &row.message {
                Some(m) => Ok(m.clone()),
                None if row.commit.message_lossy => Err(lossy_text(&commit)),
                None => Ok(row.commit.message.clone()),
            },
            Verb::Squash => group_message(&self.model, k),
            _ => {
                self.note = Some(Note::Hint(ENTER_HINT));
                return;
            }
        };
        let what = match row.verb {
            Verb::Squash => "the squash group's message",
            _ => "its message",
        };
        let title = format!(
            "{} {}: {what}",
            commit.short(),
            first_line(&row.commit.row.subject, 40)
        );
        match text {
            Ok(text) => {
                self.note = None;
                self.inner = Some(Inner::Message {
                    commit,
                    title,
                    text,
                    cancel: None,
                })
            }
            Err(why) => self.note = Some(Note::Refused(why)),
        }
    }

    /// `A`: the author dialog of the bar's row.
    fn edit_author(&mut self) {
        let Some((_, row)) = self.bar_row() else {
            return;
        };
        let own = row.commit.author.clone();
        let form = AuthorForm::of(row.author.as_ref().unwrap_or(&own));
        let commit = row.commit.row.oid.clone();
        let title = format!(
            "{} {}: author and dates",
            commit.short(),
            first_line(&row.commit.row.subject, 40)
        );
        self.inner = Some(Inner::Author {
            commit,
            title,
            own,
            form,
            cancel: None,
            own_button: None,
        });
    }

    /// Esc, or Close: closes at once when nothing changed, else asks.
    fn close(&mut self, seams: &Seams) -> Option<PlannerEnd> {
        if (seams.changed)(&self.model) {
            self.inner = Some(Inner::Ask { keep: None });
            return None;
        }
        Some(PlannerEnd::Close)
    }

    /// Cmd/Ctrl+Enter, or Run…: the run's confirmation.
    fn ask_run(&mut self) {
        self.inner = Some(Inner::Run {
            planned: None,
            cancel: None,
        });
    }

    /// The pane's keys (03 §7.8.2), with exact modifiers (03 §9), one press at a time in the
    /// pass's order — two ↓ two rows, a letter typed before ↓ on the row it was typed on — until
    /// one opens a dialog of the pane's or ends it. What the pass holds after a key that opens a
    /// dialog — keys and text, the rest of a text that key was typed in too — stays where it
    /// was, in its order, for the dialog [`draw`] shows in the same pass, as if typed once it
    /// was open (10 §17, X4-planner).
    fn keys(&mut self, ctx: &egui::Context, seams: &Seams) -> Option<PlannerEnd> {
        while let Some((at, pressed)) = ctx.input_mut(|i| PaneKey::take(&mut i.events)) {
            match pressed {
                PaneKey::Move(toward) => {
                    if let Some(commit) = self.bar().cloned() {
                        self.apply(Op::Move { commit, toward }, seams);
                    }
                }
                PaneKey::Run => self.ask_run(),
                PaneKey::Close => return self.close(seams),
                PaneKey::Message => self.edit_message(),
                PaneKey::Step(step) => {
                    let delta = self.scroll.delta(step);
                    self.scroll.step(delta, self.model.rows.len());
                }
                // Typed letters set verbs here, and never start Find (03 §7.8.2): every typed
                // character is the pane's, as typed or as an input method commits it — up to
                // one that opens a dialog, after which the rest of the text is the dialog's.
                PaneKey::Typed(text) => {
                    for (k, c) in text.char_indices() {
                        if self.inner.is_some() {
                            let rest = Event::Text(text[k..].to_string());
                            ctx.input_mut(|i| i.events.insert(at, rest));
                            break;
                        }
                        let Some(commit) = self.bar().cloned() else {
                            break;
                        };
                        if c.eq_ignore_ascii_case(&'a') {
                            self.edit_author();
                        } else if let Some(verb) = Verb::of_letter(c) {
                            self.apply(Op::Verb { commit, verb }, seams);
                        }
                    }
                }
            }
            if self.inner.is_some() {
                self.carets_at_the_end(ctx);
                return None;
            }
        }
        None
    }

    /// A dialog of the pane's has just opened: each of its fields' carets at the end of its
    /// text, as egui puts a field's caret the first time — it keeps a field's caret between
    /// dialogs, and every row's dialog has the same field ids, so the caret left in one row's
    /// message would stand in the middle of the next row's.
    fn carets_at_the_end(&self, ctx: &egui::Context) {
        let fields = match &self.inner {
            Some(Inner::Message { text, .. }) => vec![(message_field_id(), text)],
            Some(Inner::Author { form, .. }) => {
                let texts = [&form.name, &form.email, &form.date];
                (0..)
                    .zip(texts)
                    .map(|(n, t)| (author_field_id(n), t))
                    .collect()
            }
            _ => Vec::new(),
        };
        for (id, text) in fields {
            let mut state = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
            let end = egui::text::CCursor::new(text.chars().count());
            let at = egui::text::CCursorRange::one(end);
            state.cursor.set_char_range(Some(at));
            state.store(ctx, id);
        }
    }

    /// The dialog over the pane, one pass, on `grid` (the panels', where it stands once it is in
    /// cells).
    fn inner_ui(&mut self, ctx: &egui::Context, _grid: Grid, seams: &Seams) -> Option<PlannerEnd> {
        let inner = self.inner.take()?;
        let take = |key: Key| ctx.input_mut(|i| keys::take(i, key, Mods::None));
        // Enter, and the button that had the focus when it came (`None`: no button had it).
        let enter = |ctx: &egui::Context| {
            let focused = ctx.memory(|m| m.focused());
            take(Key::Enter).then_some(focused)
        };
        let is = |at: Option<Option<egui::Id>>, button: Option<egui::Id>| matches!(at, Some(Some(f)) if Some(f) == button);
        match inner {
            Inner::Message {
                commit,
                title,
                mut text,
                cancel: cancel_button,
            } => {
                if take(Key::Escape) {
                    return None;
                }
                let id = message_field_id();
                type_into(ctx, id);
                let at = enter(ctx);
                if is(at, cancel_button) {
                    return None;
                }
                let mut ok = at.is_some();
                let (mut cancel, mut cancel_id) = (false, None);
                let response = chrome::dialog(ctx, inner_id("message"), &title, |ui| {
                    ui.set_width(width(ctx, 72.0));
                    message_field(ui, "Message", &mut text, id);
                    ui.add_space(chrome::CELL.y / 2.0);
                    ui.horizontal(|ui| {
                        ok |= ui.button("OK").clicked();
                        let no = ui.button("Cancel");
                        cancel |= no.clicked();
                        cancel_id = Some(no.id);
                    });
                    ui.label(
                        RichText::new("Enter keeps it · Shift+Enter a new line · Esc cancels")
                            .weak(),
                    );
                });
                if cancel || response.should_close() {
                    return None;
                }
                if ok {
                    self.apply(
                        Op::Message {
                            commit,
                            message: text,
                        },
                        seams,
                    );
                    return None;
                }
                self.inner = Some(Inner::Message {
                    commit,
                    title,
                    text,
                    cancel: cancel_id,
                });
                None
            }
            Inner::Author {
                commit,
                title,
                own,
                mut form,
                cancel: cancel_button,
                own_button,
            } => {
                if take(Key::Escape) {
                    return None;
                }
                let id = author_field_id;
                type_into(ctx, id(0));
                let at = enter(ctx);
                if is(at, cancel_button) {
                    return None;
                }
                let mut own_back = is(at, own_button);
                let mut ok = at.is_some() && !own_back;
                let mut cancel = false;
                let (mut cancel_id, mut own_id) = (None, None);
                let response = chrome::dialog(ctx, inner_id("author"), &title, |ui| {
                    ui.set_width(width(ctx, 72.0));
                    form.ui(ui, id);
                    ui.add_space(chrome::CELL.y / 2.0);
                    let parsed = form.author();
                    if let Err(why) = parsed {
                        refusal(ui, why);
                    }
                    ui.horizontal(|ui| {
                        ok |= ui
                            .add_enabled(parsed.is_ok(), egui::Button::new("OK"))
                            .clicked();
                        let back = ui.button(OWN_AUTHOR);
                        own_back |= back.clicked();
                        own_id = Some(back.id);
                        let no = ui.button("Cancel");
                        cancel |= no.clicked();
                        cancel_id = Some(no.id);
                    });
                    ui.label(RichText::new("Enter keeps it · Esc cancels").weak());
                });
                if own_back {
                    self.apply(
                        Op::Author {
                            commit,
                            author: None,
                        },
                        seams,
                    );
                    return None;
                }
                if cancel || response.should_close() {
                    return None;
                }
                if ok {
                    if let Ok(a) = form.author() {
                        let author = if a == own { None } else { Some(a) };
                        self.apply(Op::Author { commit, author }, seams);
                        return None;
                    }
                }
                self.inner = Some(Inner::Author {
                    commit,
                    title,
                    own,
                    form,
                    cancel: cancel_id,
                    own_button: own_id,
                });
                None
            }
            Inner::Ask { keep: keep_button } => {
                if take(Key::Escape) {
                    return None;
                }
                // 03 §9.2: inside a dialog Enter runs — here, the close the question asks about;
                // Enter while "Keep planning" has the focus is its click.
                let at = enter(ctx);
                if is(at, keep_button) {
                    return None;
                }
                let mut close = at.is_some();
                let (mut keep, mut keep_id) = (false, None);
                let response = chrome::dialog(ctx, inner_id("ask"), "Close the planner", |ui| {
                    ui.set_width(width(ctx, 56.0));
                    ui.add(egui::Label::new(ASK_CLOSE).wrap());
                    ui.add_space(chrome::CELL.y / 2.0);
                    ui.horizontal(|ui| {
                        close |= ui.button("Close").clicked();
                        let stay = ui.button("Keep planning");
                        keep |= stay.clicked();
                        keep_id = Some(stay.id);
                    });
                    ui.label(RichText::new("Enter closes · Esc keeps planning").weak());
                });
                if close {
                    return Some(PlannerEnd::Close);
                }
                if !keep && !response.should_close() {
                    self.inner = Some(Inner::Ask { keep: keep_id });
                }
                None
            }
            Inner::Run {
                mut planned,
                cancel,
            } => {
                let replan = |planned: &mut Option<(Model, Result<Plan, Refused>)>,
                              model: &Model,
                              repo: &Repo| {
                    if planned.as_ref().is_none_or(|(m, _)| m != model) {
                        let plan = (seams.plan)(&Edit::Plan(model.clone()), repo);
                        *planned = Some((model.clone(), plan));
                    }
                };
                replan(&mut planned, &self.model, &self.repo);
                if take(Key::Escape) {
                    return None;
                }
                let focused = ctx.memory(|m| m.focused());
                let enter = take(Key::Enter);
                if enter && focused.is_some() && focused == cancel {
                    return None;
                }
                let offer_update_refs = self.repo.version.at_least(2, 38);
                let title =
                    plan_title(shown(&planned)).unwrap_or_else(|| "Run the plan".to_string());
                let room = (ctx.content_rect().height() * 0.5).max(8.0 * chrome::CELL.y);
                let (mut run, mut no, mut cancel_id) = (enter, false, cancel);
                let mut problems = Vec::new();
                let response = chrome::dialog(ctx, inner_id("run"), &title, |ui| {
                    ui.set_width(width(ctx, 80.0));
                    if let Some(line) = loses(shown(&planned)) {
                        ui.add(egui::Label::new(line).wrap());
                        ui.add_space(chrome::CELL.y / 2.0);
                    }
                    ui.horizontal_wrapped(|ui| {
                        toggle(ui, &mut self.model.autostash, AUTOSTASH);
                        if offer_update_refs {
                            toggle(ui, &mut self.model.update_refs, UPDATE_REFS);
                        }
                    });
                    replan(&mut planned, &self.model, &self.repo);
                    problems = (seams.problems)(&self.model, &self.repo);
                    for p in &problems {
                        refusal(ui, p);
                    }
                    ui.add_space(chrome::CELL.y / 2.0);
                    egui::ScrollArea::vertical()
                        .id_salt("sopen-git-planner-plan")
                        .max_height(room)
                        .min_scrolled_height(room)
                        .show(ui, |ui| plan_ui(ui, None, shown(&planned)));
                    ui.add_space(chrome::CELL.y / 2.0);
                    let ready = problems.is_empty()
                        && matches!(&planned, Some((m, Ok(_))) if *m == self.model);
                    let (go, stop, id) = run_buttons(ui, ready, "Run");
                    run |= go;
                    no |= stop;
                    cancel_id = Some(id);
                });
                if no || response.should_close() {
                    return None;
                }
                let ready =
                    problems.is_empty() && matches!(&planned, Some((m, Ok(_))) if *m == self.model);
                if run && ready {
                    let edit = Edit::Plan(self.model.clone());
                    if let Some((_, Ok(plan))) = planned {
                        self.shown = Some((edit.clone(), plan));
                    }
                    return Some(PlannerEnd::Run(edit));
                }
                self.inner = Some(Inner::Run {
                    planned,
                    cancel: cancel_id,
                });
                None
            }
        }
    }
}

/// The run dialog's plan, as last planned.
fn shown(planned: &Option<(Model, Result<Plan, Refused>)>) -> Option<&Result<Plan, Refused>> {
    planned.as_ref().map(|(_, plan)| plan)
}

/// What the pane's foot buttons ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Foot {
    Run,
    Close,
}

/// The keys' hint at the pane's foot. In an ambiguous-wide terminal (`wide`) its own `↑↓` and
/// `·` are 10 §10's stand-ins (`term::narrow`: `^v`, `.`), put at the source: the hint is cut to
/// its row, so the printer has no blanks to move a glyph laid in two cells into (T38).
fn hint(wide: bool) -> String {
    let cmd = Mods::Command.prefix();
    let hint = format!(
        "P R E S F D verb · A author and dates · {cmd}↑↓ move · Enter message · {cmd}Enter run · Esc closes"
    );
    if !wide {
        return hint;
    }
    hint.chars()
        .map(|c| {
            let mut utf8 = [0u8; 4];
            let s: &str = c.encode_utf8(&mut utf8);
            term::narrow(s).map_or_else(|| s.to_string(), str::to_string)
        })
        .collect()
}

/// The columns of a row, in cells from the pane's first: the verb (`reword`, `squash`: 6 cells),
/// the `A` mark, the hash (8), the subject, and at the right end the author and the date (16
/// each) — each [`GAP`] blanks from the next, so each reads as a text of its own (10 §17,
/// X4-planner's decision).
const GAP: usize = 2;
const VERB: usize = 0;
const VERB_CELLS: usize = 6;
const MARK: usize = VERB + VERB_CELLS + GAP;
const HASH: usize = MARK + 1 + GAP;
const HASH_CELLS: usize = 8;
const SUBJECT: usize = HASH + HASH_CELLS + GAP;
const AUTHOR_CELLS: usize = 16;
const DATE_CELLS: usize = 16;
/// The subject keeps at least this many cells, or the author and the date are left out.
const SUBJECT_MIN: usize = 10;

/// Where a pane `chars` cells wide puts the author and the date, if it has room for them with
/// [`SUBJECT_MIN`] cells of subject before them.
fn right_columns(chars: usize) -> Option<(usize, usize)> {
    let need = SUBJECT + SUBJECT_MIN + GAP + AUTHOR_CELLS + GAP + DATE_CELLS;
    (chars >= need).then(|| {
        let date = chars - DATE_CELLS;
        (date - GAP - AUTHOR_CELLS, date)
    })
}

/// A button's text: `[ word ]`.
fn button(word: &str) -> String {
    format!("[ {word} ]")
}

/// The pane at cells `area` of `buf` (on `grid`), cut by cells (unicode-width: a Hangul glyph two
/// cells, never half of one, `…` where a text stops) and never written past `area` — but for the
/// scroll bar, on the border right of it, beside the rows, as the graph's: over the panel's body,
/// the column heads; the rows from the newest, each a cell tall, with the bar; then at the foot
/// the checks that fail and the note (an op's refusal in the error colour, a key's hint dim), a
/// row each, and `[ Run… ]`, `[ Close ]` and the keys' hint. A click on a row's cells puts the bar
/// there, the wheel over the rows scrolls them by cells; what a button's click asked is returned.
fn paint(
    ui: &mut egui::Ui,
    grid: &Grid,
    area: Cells,
    buf: &mut Buffer,
    view: &mut PlannerView,
    active: bool,
    problems: &[String],
) -> Option<Foot> {
    let ctx = ui.ctx().clone();
    let p = PALETTE;
    let wide = cells::ambiguous_wide(&ctx);
    // Over the git panel's body: the graph under it does not show through.
    cells::fill(buf, area, Pen::new(p.text, p.back));
    let mut foot_lines: Vec<(String, Color32)> =
        problems.iter().map(|p| (p.clone(), LOOK.error)).collect();
    match &view.note {
        Some(Note::Refused(why)) => foot_lines.push((why.clone(), LOOK.error)),
        Some(Note::Hint(hint)) => foot_lines.push((hint.to_string(), LOOK.dim)),
        None => {}
    }
    if view.model.update_refs {
        for b in &view.model.left_out {
            let name = b.refname.strip_prefix("refs/heads/").unwrap_or(&b.refname);
            foot_lines.push((
                format!("{name} is checked out in another worktree; it stays where it is"),
                LOOK.dim,
            ));
        }
    }
    let chars = usize::from(area.width);
    // Cell `c` of the pane, on the grid (every `c` here is below `chars`).
    let x = |c: usize| area.x.saturating_add(u16::try_from(c).unwrap_or(u16::MAX));
    // `text` (its control characters shown) from cell `c` of row `y` in at most `room` cells, cut
    // by cells; nothing past the pane's cells.
    let put = |buf: &mut Buffer, y: u16, c: usize, room: usize, text: &str, pen: Pen| {
        if c < chars && (area.top()..area.bottom()).contains(&y) {
            let room = room.min(chars - c);
            let shown = cells::fit_start(&printable(text), room);
            cells::put(buf, x(c), y, &shown, room, pen);
        }
    };
    let right = right_columns(chars);
    let subject_room = match right {
        Some((author, _)) => author - GAP - SUBJECT,
        None => chars.saturating_sub(SUBJECT),
    };
    // The heads.
    let dim = Pen::new(LOOK.dim, p.back);
    put(buf, area.y, VERB, VERB_CELLS, "Verb", dim);
    put(buf, area.y, HASH, HASH_CELLS, "Commit", dim);
    put(buf, area.y, SUBJECT, subject_room, "Subject", dim);
    if let Some((author, date)) = right {
        put(buf, area.y, author, AUTHOR_CELLS, "Author", dim);
        put(buf, area.y, date, DATE_CELLS, "Date", dim);
    }
    // The rows between the heads and the foot: its lines, then the buttons' row.
    let foot_rows = u16::try_from(foot_lines.len() + 1).unwrap_or(u16::MAX);
    let list_top = area.y.saturating_add(1).min(area.bottom());
    let foot_top = area.bottom().saturating_sub(foot_rows).max(list_top);
    let list = Cells::new(area.x, list_top, area.width, foot_top - list_top);
    let len = view.model.rows.len();
    if len == 0 {
        put(buf, list.y, 0, chars, NO_ROWS, dim);
    }
    // A page is the rows shown, one at least for the keys' steps; only those shown are drawn.
    let visible = usize::from(list.height).max(1);
    if super::super::pointer_on_pane(ui, grid.rect(list)) {
        let lines: f32 = ctx.input(|i| {
            i.events
                .iter()
                .map(|e| match e {
                    Event::MouseWheel { unit, delta, .. } => match unit {
                        egui::MouseWheelUnit::Point => -delta.y / chrome::CELL.y,
                        _ => -delta.y * 3.0,
                    },
                    _ => 0.0,
                })
                .sum()
        });
        view.scroll.visible = visible;
        view.scroll.wheel(lines, len);
    }
    view.scroll.settle(visible, len);
    let range = view.scroll.range(len);
    let shown = range.start..range.end.min(range.start + usize::from(list.height));
    let mut clicked_row = None;
    for (n, k) in shown.clone().enumerate() {
        let r = &view.model.rows[k];
        let y = list.y + n as u16;
        let row_cells = Cells::new(area.x, y, area.width, 1);
        let on_bar = k == view.scroll.bar;
        let back = match (on_bar, active) {
            (false, _) => p.back,
            (true, true) => p.bar,
            (true, false) => p.bar_idle,
        };
        let fg = if on_bar && active {
            LOOK.on_bar
        } else if r.verb == Verb::Drop {
            LOOK.dim
        } else {
            LOOK.text
        };
        let pen = Pen::new(fg, back);
        cells::fill(buf, row_cells, pen);
        let row_commit = &r.commit.row;
        put(buf, y, VERB, VERB_CELLS, r.verb.word(), pen);
        if r.author.is_some() {
            put(buf, y, MARK, 1, "A", pen);
        }
        put(buf, y, HASH, HASH_CELLS, row_commit.oid.short(), pen);
        put(buf, y, SUBJECT, subject_room, &row_commit.subject, pen);
        if let Some((author, date)) = right {
            let (name, time) = match &r.author {
                Some(a) => (a.name.as_str(), a.date.seconds),
                None => (row_commit.author_name.as_str(), row_commit.author_time),
            };
            put(buf, y, author, AUTHOR_CELLS, name, pen);
            put(buf, y, date, DATE_CELLS, &graph_view::date_text(time), pen);
        }
        let resp = ui.interact(
            grid.rect(row_cells),
            ui.scope_id().with(("sopen-git-planner-row", k)),
            Sense::click(),
        );
        if resp.clicked() {
            clicked_row = Some(k);
        }
    }
    if let Some(k) = clicked_row {
        view.scroll.to(k);
    }
    // The scroll bar on the border beside the rows.
    if list.height > 0 {
        let max_top = len.saturating_sub(visible);
        let at = (max_top > 0).then(|| view.scroll.top as f32 / max_top as f32);
        cells::scroll_bar(buf, area.right(), list.y, list.height, at);
    }
    // The foot.
    for (n, (text, colour)) in foot_lines.iter().enumerate() {
        let y = foot_top.saturating_add(u16::try_from(n).unwrap_or(u16::MAX));
        put(buf, y, 0, chars, text, Pen::new(*colour, p.back));
    }
    let y = foot_top.saturating_add(u16::try_from(foot_lines.len()).unwrap_or(u16::MAX));
    let mut clicked = None;
    let mut c = 0;
    if (area.top()..area.bottom()).contains(&y) {
        for (word, foot, salt) in [
            ("Run…", Foot::Run, "sopen-git-planner-run"),
            ("Close", Foot::Close, "sopen-git-planner-close"),
        ] {
            let label = button(word);
            let w = cells::width(&label);
            // A button is drawn, and acts, only where it fits whole.
            if c + w <= chars {
                let at = Cells::new(x(c), y, w as u16, 1);
                let id = ui.scope_id().with(salt);
                if ui.interact(grid.rect(at), id, Sense::click()).clicked() {
                    clicked = Some(foot);
                }
                // egui's button colours: black on cyan, the bar's when it has the focus (10 §6).
                let pen = if ctx.memory(|m| m.has_focus(id)) {
                    Pen::new(p.bar_text, p.bar)
                } else {
                    Pen::new(p.dialog_text, p.dialog_back)
                };
                cells::put(buf, x(c), y, &label, w, pen);
            }
            c += w + GAP;
        }
        put(buf, y, c, chars.saturating_sub(c), &hint(wide), dim);
    }
    clicked
}

/// Whether [`draw`] paints the planner's cells (10 §17, X4-graph's seam): it does (X4-planner) —
/// the pane is cells in the panel's buffer; its own dialogs are still egui's (X4-planner2).
pub(super) const PAINTS_CELLS: bool = true;

/// The planner at cells `cells` of `grid` (the git panel's body), painted into `buf` (the panel's
/// buffer, painted once by the panel), one pass, its own dialogs on `grid`: its keys first while
/// it has the keyboard (`active`) and no dialog of its own was up when the pass began — so a key
/// that closes a dialog never acts on the pane too — then the pane ([`paint`]), then its dialog.
/// While it is active or a dialog of its own is up it swallows every typed character, so no typed
/// letter reaches the git context's Find. `seams` are `Model::apply`, `changed`, `problems` and
/// `rebase::plan` ([`SEAMS`]).
pub fn draw(
    ui: &mut egui::Ui,
    grid: &Grid,
    cells: Cells,
    buf: &mut Buffer,
    view: &mut PlannerView,
    active: bool,
    seams: &Seams,
) -> Option<PlannerEnd> {
    let ctx = ui.ctx().clone();
    let mut end = None;
    if active && view.inner.is_none() {
        end = view.keys(&ctx, seams);
    }
    if end.is_none() {
        let problems = (seams.problems)(&view.model, &view.repo);
        let clicked = paint(ui, grid, cells, buf, view, active, &problems);
        if view.inner.is_none() {
            match clicked {
                Some(Foot::Run) => view.ask_run(),
                Some(Foot::Close) => end = view.close(seams),
                None => {}
            }
        }
        if end.is_none() && view.inner.is_some() {
            end = view.inner_ui(&ctx, *grid, seams);
        }
    }
    if active || view.inner.is_some() {
        ctx.input_mut(|i| {
            i.events
                .retain(|e| !matches!(e, Event::Text(_) | Event::Ime(egui::ImeEvent::Commit(_))))
        });
    }
    end
}
