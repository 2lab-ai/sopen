//! The action dialogs (03 §7, §8.1, §3.8): each action's options and its confirmation — what
//! changes, the backups written first, the exact command lines, updated as the options change
//! (`git::actions::plan`) — and the error dialog of a failed run, each step `done`, `failed` or
//! `not run`.
//!
//! - **An action's dialog** ([`ActionDialog`]) holds the action as its options make it — a draft
//!   [`Action`], edited in place — and plans it again whenever the draft changes, through the
//!   [`Planner`] it is given each pass (`git::actions::plan`, [`PLAN`]; the tests give their own).
//!   It shows the plan's confirmation (03 §8.1): the line of what is lost first, the options, what
//!   changes, the backups written first, the exact command lines, the notes, the §8.4 warning of
//!   each overwrite or delete on a remote, or the plan's refusal. Enter or Run returns the action
//!   ([`DialogEnd::Run`]); the dialog never runs anything. An action without options whose plan
//!   has no confirmation (§8.1's "No confirmation") is returned at once by [`ActionDialog::open`]
//!   and [`ActionDialog::confirm`] ([`Opened::Run`]); the views are the panel's ([`Opened::View`]).
//! - **The restore dialog** ([`ActionDialog::restore`], §8.3) lists a group's entries, never over
//!   anything: an entry that moved since is listed as `moved since (now …)` and starts unchosen,
//!   an occupied path is left as it is unless its "replace" is chosen.
//! - **The result dialog** ([`ResultDialog`], §3.8): each step's outcome, the failed command's
//!   text whole and its command line, and the backup line; [`status_text`] is the status row's
//!   `<what was done> · backup <stamp> · Cmd/Ctrl+Z undoes` (§8.3).
//! - **Masking** (03 §3.7) is for what sopen shows from git: a URL the owner types (Add remote, a
//!   retyped Edit remote field) shows as typed, while the result dialog and the log mask it.

use super::menu::{self, Target};
use crate::git::actions::{
    self, Action, ActionId, ArchiveFormat, ConfigScope, FastForward, MergeOptions, Outcome,
    Overwrite, Plan, Prefer, PullHow, PushOptions, RebaseOptions, Refused, Remote, RemoteChange,
    Repo, ResetMode, Rev, Step, StepOutcome,
};
use crate::git::backup::{RestoreEntry, Subject, Value};
use crate::git::model::{ObjectType, RefKind, RefLabel, StatusEntry};
use crate::git::runner::mask_credentials;
use crate::gui::browser::keys::{self, Mods};
use crate::gui::cells::{Cells, Grid};
use crate::gui::chrome;
use eframe::egui::{self, Key, RichText};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// What plans an action for a dialog: `git::actions::plan`, pure (it reads and runs nothing).
pub type Planner = fn(&Action, &Repo) -> Result<Plan, Refused>;

/// The real planner, for the integration.
pub const PLAN: Planner = actions::plan;

/// What a dialog starts from that [`Repo`] does not hold: reads the integration makes, or window
/// state. Every field may stay empty; the form then starts blank there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prefill {
    /// 01 §6a's download folder: `archive`'s path field starts there.
    pub save_dir: PathBuf,
    /// A commit message to start from: `amend`'s last commit message, `commit`'s (git's
    /// prepared squash message after `merge --squash`).
    pub message: Option<String>,
    /// The annotated tags' messages, by tag name (`move-tag` keeps the chosen tag's, 03 §7.4): a
    /// tag missing here has its message not loaded, and keeping it annotated is refused.
    pub tag_messages: BTreeMap<String, String>,
    /// The remotes' URLs (`edit-remote`'s prefill, 03 §7.7).
    pub remotes: Vec<Remote>,
    /// `user`'s name and email now, and where they are set (03 §7.7).
    pub user: Option<(String, String, ConfigScope)>,
    /// `push`'s remote when the config names one (`branch.<b>.pushRemote`, `remote.pushDefault`,
    /// 03 §7.2); without it the branch's upstream's remote, else `origin`, else the first.
    pub push_remote: Option<String>,
    /// "Don't ask again in this window" was chosen in a `checkout` dialog (03 §7.1): `checkout`
    /// runs without its dialog.
    pub checkout_unasked: bool,
}

/// What opening an action gave.
#[derive(Debug)]
pub enum Opened {
    /// A view or the clipboard (`menu::is_view`): the panel carries it out, with no plan.
    View(ActionId),
    /// An id this target's menu does not offer, or one only the panel can build (the Backups
    /// list's `restore` and `delete-backup`, the banner's steps): build the action and use
    /// [`ActionDialog::confirm`] or [`ActionDialog::restore`].
    NotHere(ActionId),
    /// No options and a plan with no confirmation (03 §8.1): run it now.
    Run(Action),
    /// The dialog: draw it with [`ActionDialog::ui`] until it ends.
    Dialog(Box<ActionDialog>),
}

/// How an action's dialog ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogEnd {
    /// Enter or Run on a plan that was not refused: the action as chosen, for the caller to
    /// plan and run. `dont_ask_again`: `checkout`'s "Don't ask again in this window" was chosen
    /// ([`Prefill::checkout_unasked`] from then on).
    Run {
        action: Action,
        dont_ask_again: bool,
    },
    /// Esc, Cancel or a click outside it.
    Cancel,
    /// Rebase's "Plan the commits first…" (03 §7.1: "plan the commits first → the planner"):
    /// nothing runs; the caller opens the planner over the commits a rebase onto this revision
    /// would move (S4-C).
    PlanFirst(Rev),
}

/// What Rebase's dialog offers besides Run (03 §7.1's last option).
pub const PLAN_FIRST: &str = "Plan the commits first…";

/// The options of one action, as its dialog edits them.
#[derive(Debug, Clone)]
struct Form {
    /// The action as the options make it now.
    draft: Action,
    /// What the dialog is called until a plan names it (the menu's words).
    label: String,
    /// It has options to show; without, its confirmation only.
    options: bool,
    /// The remotes it may name.
    remotes: Vec<String>,
    /// `move-branch`'s branches, `move-tag`'s tags.
    names: Vec<String>,
    /// The local branches by short name (`checkout-remote`: is the name taken).
    locals: Vec<String>,
    /// `pick`, `revert`: the most parents among the commits (a merge offers its parent).
    parents: usize,
    /// `move-tag`'s annotated tags and their messages.
    tags: TagMessages,
    /// `rebase`'s `--update-refs` is offered (git ≥ 2.38).
    update_refs: bool,
    /// A message kept while its option is off (a lightweight tag, an amend keeping its own), so
    /// turning it on again brings it back.
    kept: String,
    /// `archive`'s path field; `add-remote`'s and `edit-remote`'s push URL field.
    text: String,
    /// `checkout`'s "Don't ask again in this window".
    dont_ask: bool,
    /// The remotes' URLs (`edit-remote`).
    known: Vec<Remote>,
    /// `checkout-remote`'s pull, kept while the name is not taken.
    how: PullHow,
    /// `edit-remote`'s fetch URL field (its push URL field is `text`).
    url: String,
    /// `edit-remote`: the owner changed the fetch URL field, the push URL field. A URL field left
    /// alone keeps the URL as it is set — `now`'s URL is `was`'s, so nothing is written — since
    /// its prefill is masked (03 §3.7) and must never be written back.
    url_touched: bool,
    push_touched: bool,
}

impl Form {
    /// The options of `draft`, called `label`, with nothing to choose from.
    fn fixed(draft: Action, label: String, options: bool) -> Form {
        Form {
            draft,
            label,
            options,
            remotes: Vec::new(),
            names: Vec::new(),
            locals: Vec::new(),
            parents: 1,
            tags: TagMessages::default(),
            update_refs: false,
            kept: String::new(),
            text: String::new(),
            dont_ask: false,
            known: Vec::new(),
            how: PullHow::default(),
            url: String::new(),
            url_touched: false,
            push_touched: false,
        }
    }

    /// The form of `id` on `target`, at the options' defaults (03 §7's Options column); `None`
    /// for an id with no form here.
    fn of(id: ActionId, target: &Target, repo: &Repo, prefill: &Prefill) -> Option<Form> {
        use ActionId as A;
        let remotes = repo.remotes.clone();
        let locals: Vec<String> = repo
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::LocalBranch && !r.lossy)
            .map(|r| r.name().to_string())
            .collect();
        let message = prefill.message.clone().unwrap_or_default();
        // Its choices first; the draft below replaces this one.
        let mut f = Form::fixed(
            Action::Fetch {
                prune: false,
                prune_tags: false,
            },
            menu::label(id).to_string(),
            true,
        );
        f.remotes = remotes.clone();
        f.locals = locals;
        f.known = prefill.remotes.clone();
        let label = match target {
            Target::Label(r) if !r.lossy => Some(r),
            _ => None,
        };
        let draft = match (id, target) {
            (A::Tag, Target::Commit { commit, .. }) => Action::Tag {
                at: commit.oid.clone(),
                name: String::new(),
                message: Some(String::new()),
                push: None,
            },
            (A::Branch, Target::Commit { commit, .. }) => Action::Branch {
                at: commit.oid.clone(),
                name: String::new(),
                checkout: false,
            },
            (A::MoveBranch, Target::Commit { commit, .. }) => {
                f.names = menu::movable(repo);
                Action::MoveBranch {
                    branch: f.names.first()?.clone(),
                    to: commit.oid.clone(),
                }
            }
            (A::Checkout, Target::Commit { commit, .. }) => Action::Checkout {
                at: commit.oid.clone(),
            },
            (A::Pick | A::Revert, Target::Commit { commit, marked }) => {
                // The marked commits, else the bar's: newest first as the rows show them.
                let rows = if marked.is_empty() {
                    std::slice::from_ref(commit)
                } else {
                    &marked[..]
                };
                f.parents = rows.iter().map(|c| c.parents.len()).max().unwrap_or(1);
                let mainline = (f.parents > 1).then_some(1);
                let newest_first = rows.iter().map(|c| c.oid.clone());
                match id {
                    A::Pick => Action::Pick {
                        commits: newest_first.rev().collect(),
                        mainline,
                        record_origin: false,
                        no_commit: false,
                    },
                    _ => Action::Revert {
                        commits: newest_first.collect(),
                        mainline,
                    },
                }
            }
            (A::Merge, _) => Action::Merge {
                from: rev(target, repo)?,
                options: MergeOptions::default(),
            },
            (A::Rebase, _) => {
                f.update_refs = repo.version.at_least(2, 38);
                Action::Rebase {
                    onto: rev(target, repo)?,
                    options: RebaseOptions {
                        autostash: repo.status.entries.iter().any(menu::tracked_change),
                        ..RebaseOptions::default()
                    },
                }
            }
            (A::Reset, Target::Commit { commit, .. }) => Action::Reset {
                to: commit.oid.clone(),
                mode: ResetMode::default(),
            },
            (A::Archive, _) => {
                let rev = rev(target, repo)?;
                f.text = archive_path(prefill, repo, &rev, ArchiveFormat::default());
                Action::Archive {
                    rev,
                    format: ArchiveFormat::default(),
                    to: PathBuf::from(&f.text),
                }
            }
            (A::CheckoutBranch, _) => Action::CheckoutBranch {
                branch: label?.name().to_string(),
            },
            (A::Rename, _) => {
                let name = label?.name().to_string();
                Action::Rename {
                    branch: name.clone(),
                    to: name,
                }
            }
            (A::DeleteBranch, _) => {
                let branch = label?.name().to_string();
                // The remotes that have `<r>/<b>`.
                f.remotes = remotes
                    .iter()
                    .filter(|r| {
                        let refname = format!("refs/remotes/{r}/{branch}");
                        repo.refs.iter().any(|l| !l.lossy && l.refname == refname)
                    })
                    .cloned()
                    .collect();
                Action::DeleteBranch {
                    branch,
                    force: false,
                    remotes: Vec::new(),
                }
            }
            (A::Push, _) => {
                let r = label?;
                Action::Push {
                    branch: r.name().to_string(),
                    remotes: push_remote(prefill, Some(r), repo).into_iter().collect(),
                    options: PushOptions {
                        set_upstream: true,
                        no_verify: false,
                    },
                    overwrite: false,
                }
            }
            (A::CheckoutRemote | A::DeleteRemoteBranch | A::FetchInto | A::Pull, _) => {
                let (remote, branch) = super::filters::split_remote(&label?.refname, &remotes)?;
                let (remote, branch) = (remote.to_string(), branch.to_string());
                match id {
                    A::CheckoutRemote => Action::CheckoutRemote {
                        pull: f.locals.contains(&branch).then(PullHow::default),
                        name: branch.clone(),
                        remote,
                        branch,
                    },
                    A::DeleteRemoteBranch => Action::DeleteRemoteBranch { remote, branch },
                    A::FetchInto => Action::FetchInto {
                        remote,
                        branch,
                        force: false,
                    },
                    _ => Action::Pull {
                        remote,
                        branch,
                        how: PullHow::default(),
                    },
                }
            }
            (A::DeleteTag, _) => Action::DeleteTag {
                tag: label?.name().to_string(),
                remote: None,
            },
            (A::PushTag, _) => Action::PushTag {
                tag: label?.name().to_string(),
                remotes: push_remote(prefill, None, repo).into_iter().collect(),
                replace: false,
            },
            (A::MoveTag, Target::Commit { commit, .. }) => {
                f.names = menu::tags(repo);
                f.tags = TagMessages {
                    annotated: repo
                        .refs
                        .iter()
                        .filter(|r| r.kind == RefKind::Tag && r.object_type == ObjectType::Tag)
                        .map(|r| r.name().to_string())
                        .collect(),
                    read: prefill.tag_messages.clone(),
                    edited: BTreeMap::new(),
                };
                let tag = f.names.first()?.clone();
                Action::MoveTag {
                    message: f.tags.annotated_message(&tag),
                    tag,
                    to: commit.oid.clone(),
                }
            }
            (A::StashApply, Target::Stash(s)) => Action::StashApply {
                stash: s.clone(),
                index: false,
            },
            (A::StashPop, Target::Stash(s)) => Action::StashPop {
                stash: s.clone(),
                index: false,
            },
            (A::StashBranch, Target::Stash(s)) => Action::StashBranch {
                stash: s.clone(),
                name: String::new(),
            },
            (A::StashDrop, Target::Stash(s)) => Action::StashDrop { stash: s.clone() },
            (A::Stage, _) => Action::Stage {
                paths: paths(target, repo, menu::unstaged),
            },
            (A::Unstage, _) => Action::Unstage {
                paths: paths(target, repo, menu::staged),
            },
            (A::Commit, Target::Uncommitted) => Action::Commit {
                message,
                amend: false,
                no_verify: false,
            },
            (A::Amend, Target::Uncommitted) => {
                f.kept = message;
                Action::Amend { message: None }
            }
            (A::Stash, Target::Uncommitted) => Action::Stash {
                message: String::new(),
                include_untracked: false,
            },
            (A::Discard, Target::Files(_)) => Action::Discard {
                paths: paths(target, repo, |e| !menu::conflicted(e)),
            },
            (A::ResetChanges, Target::Uncommitted) => Action::ResetChanges { hard: false },
            (A::Clean, Target::Uncommitted) => Action::Clean { folders: true },
            (A::Ours | A::Theirs | A::Resolved | A::RedoMerge, Target::Files(_)) => {
                let paths = paths(target, repo, menu::conflicted);
                match id {
                    A::Ours => Action::Ours { paths },
                    A::Theirs => Action::Theirs { paths },
                    A::Resolved => Action::Resolved { paths },
                    _ => Action::RedoMerge { paths },
                }
            }
            (A::Fetch, _) => Action::Fetch {
                prune: false,
                prune_tags: false,
            },
            (A::FetchRemote, _) => Action::FetchRemote {
                remote: remotes.first()?.clone(),
                prune: false,
                prune_tags: false,
            },
            (A::PruneRemote, _) => Action::PruneRemote {
                remote: remotes.first()?.clone(),
            },
            (A::AddRemote, _) => Action::AddRemote {
                name: String::new(),
                url: String::new(),
                push_url: None,
                fetch_now: true,
            },
            (A::EditRemote, _) => {
                let was = known(&f.known, remotes.first()?);
                f.url = was.url.clone();
                f.text = was.push_url.clone().unwrap_or_default();
                Action::EditRemote {
                    now: was.clone(),
                    was,
                }
            }
            (A::DeleteRemote, _) => Action::DeleteRemote {
                remote: remotes.first()?.clone(),
            },
            (A::User, _) => {
                let (name, email, scope) = prefill.user.clone().unwrap_or((
                    String::new(),
                    String::new(),
                    ConfigScope::Local,
                ));
                Action::User { name, email, scope }
            }
            (A::UserRemove, _) => Action::UserRemove {
                scope: prefill.user.as_ref().map_or(ConfigScope::Local, |u| u.2),
            },
            _ => return None,
        };
        f.options = has_options(&draft);
        f.draft = draft;
        Some(f)
    }
}

/// What the dialog refuses before any plan: an `edit-remote` URL field the owner changed that
/// still holds the mask of a hidden password (03 §3.7: the prefill is masked, `user:***@`), which
/// would set the URL to the mask itself.
pub const RETYPE: &str = "retype the whole URL — its password is hidden";

/// Whether `url`'s user part holds the mask of a hidden password or token (`user:***@host`,
/// `***@host`, as `runner::mask_credentials` writes it).
pub fn hides_password(url: &str) -> bool {
    let Some(at) = url.find("://") else {
        return false;
    };
    let rest = &url[at + 3..];
    let authority = rest
        .find(|c: char| "/?#".contains(c) || c.is_whitespace())
        .map_or(rest, |end| &rest[..end]);
    authority
        .rfind('@')
        .is_some_and(|user_end| authority[..user_end].contains("***"))
}

/// What the dialog refuses before any plan: `move-tag` keeping an annotated tag annotated when
/// its message was not read and the owner typed none, rather than another tag's (03 §7.4).
pub const NOT_LOADED: &str = "its message is not loaded";

/// `move-tag`'s tags (03 §7.4): which are annotated, and each one's message as read and as the
/// owner edited it in this dialog — the chosen tag's own, never another's.
#[derive(Debug, Clone, Default)]
struct TagMessages {
    annotated: Vec<String>,
    /// [`Prefill::tag_messages`].
    read: BTreeMap<String, String>,
    edited: BTreeMap<String, String>,
}

impl TagMessages {
    /// `tag`'s message: the owner's edit, else as read, else empty (not loaded, refused by
    /// [`Form::refusal`]).
    fn message_of(&self, tag: &str) -> String {
        self.edited
            .get(tag)
            .or(self.read.get(tag))
            .cloned()
            .unwrap_or_default()
    }

    /// The message `tag` is moved with when chosen: an annotated tag stays annotated with its
    /// own message by default, another is moved lightweight.
    fn annotated_message(&self, tag: &str) -> Option<String> {
        self.annotated
            .iter()
            .any(|t| t == tag)
            .then(|| self.message_of(tag))
    }

    /// An annotated tag whose message was neither read nor typed.
    fn not_loaded(&self, tag: &str) -> bool {
        self.annotated.iter().any(|t| t == tag)
            && !self.read.contains_key(tag)
            && !self.edited.contains_key(tag)
    }
}

impl Form {
    /// The dialog's own refusal of the draft ([`RETYPE`], [`NOT_LOADED`]), before any plan.
    fn refusal(&self) -> Option<String> {
        match &self.draft {
            Action::EditRemote { .. } => {
                let masked = (self.url_touched && hides_password(&self.url))
                    || (self.push_touched && hides_password(&self.text));
                masked.then(|| RETYPE.to_string())
            }
            Action::MoveTag {
                tag,
                message: Some(_),
                ..
            } => self
                .tags
                .not_loaded(tag)
                .then(|| format!("{tag}: {NOT_LOADED}")),
            _ => None,
        }
    }
}

/// Whether `action` has options its dialog shows; an action without is its confirmation alone.
fn has_options(action: &Action) -> bool {
    !matches!(
        action,
        Action::CheckoutBranch { .. }
            | Action::Stage { .. }
            | Action::Unstage { .. }
            | Action::DeleteRemoteBranch { .. }
            | Action::StashDrop { .. }
            | Action::Discard { .. }
            | Action::Ours { .. }
            | Action::Theirs { .. }
            | Action::Resolved { .. }
            | Action::RedoMerge { .. }
            | Action::DeleteBackup { .. }
            | Action::Proceed(_)
    )
}

/// The revision a merge, rebase or archive names: the target's commit, branch, remote branch or
/// tag (03 §7's `<c>`, `<b>`, `<r>/<b>`, `<t>`).
fn rev(target: &Target, repo: &Repo) -> Option<Rev> {
    match target {
        Target::Commit { commit, .. } => Some(Rev::Commit(commit.oid.clone())),
        Target::Label(r) if !r.lossy => match r.kind {
            RefKind::LocalBranch => Some(Rev::Branch(r.name().to_string())),
            RefKind::RemoteBranch => {
                let (remote, branch) = super::filters::split_remote(&r.refname, &repo.remotes)?;
                Some(Rev::RemoteBranch {
                    remote: remote.to_string(),
                    branch: branch.to_string(),
                })
            }
            RefKind::Tag => Some(Rev::Tag(r.name().to_string())),
        },
        _ => None,
    }
}

/// The paths of the target's files (the uncommitted row: all of the status read's) that `keep`
/// keeps; a path that is not UTF-8 names no file.
fn paths(target: &Target, repo: &Repo, keep: fn(&StatusEntry) -> bool) -> Vec<String> {
    let entries = match target {
        Target::Files(es) => &es[..],
        _ => &repo.status.entries[..],
    };
    entries
        .iter()
        .filter(|e| !e.lossy && keep(e))
        .map(|e| e.path.clone())
        .collect()
}

/// `push`'s preselected remote (03 §7.2): the config's push remote, else the branch's upstream's
/// remote, else `origin`, else the first.
fn push_remote(prefill: &Prefill, branch: Option<&RefLabel>, repo: &Repo) -> Option<String> {
    let has = |r: &str| repo.remotes.iter().any(|x| x == r);
    let upstream = branch
        .and_then(|b| b.upstream.as_deref())
        .and_then(|u| super::filters::split_remote(u, &repo.remotes))
        .map(|(r, _)| r);
    prefill
        .push_remote
        .as_deref()
        .filter(|r| has(r))
        .or(upstream.filter(|r| has(r)))
        .or(Some("origin").filter(|r| has(r)))
        .or(repo.remotes.first().map(String::as_str))
        .map(str::to_string)
}

/// A remote's URLs as the integration read them, else its name alone.
fn known(known: &[Remote], name: &str) -> Remote {
    known
        .iter()
        .find(|r| r.name == name)
        .cloned()
        .unwrap_or_else(|| Remote {
            name: name.to_string(),
            url: String::new(),
            push_url: None,
        })
}

/// `archive`'s path field to start with: the download folder and `<repository>-<rev>.<ext>`.
fn archive_path(prefill: &Prefill, repo: &Repo, rev: &Rev, format: ArchiveFormat) -> String {
    let top = repo.top.trim_end_matches('/');
    let base = top.rsplit('/').next().unwrap_or("archive");
    let what = match rev {
        Rev::Commit(oid) => oid.short().to_string(),
        Rev::Branch(b) | Rev::Tag(b) => b.clone(),
        Rev::RemoteBranch { remote, branch } => format!("{remote}-{branch}"),
    };
    let name = format!("{base}-{}.{}", what.replace('/', "-"), extension(format));
    prefill.save_dir.join(name).to_string_lossy().into_owned()
}

fn extension(format: ArchiveFormat) -> &'static str {
    match format {
        ArchiveFormat::Zip => "zip",
        ArchiveFormat::Tar => "tar",
    }
}

/// An action's dialog, open (see the module's notes).
#[derive(Debug)]
pub struct ActionDialog {
    form: Form,
    repo: Repo,
    /// The draft last planned and its plan.
    planned: Option<(Action, Result<Plan, Refused>)>,
    /// The form laid out a text field last pass: the typing goes there while nothing has it.
    typing: bool,
    /// The Cancel button's id, last pass: Enter while it has the focus cancels.
    cancel: Option<egui::Id>,
    /// Rebase's "Plan the commits first…" button's id, last pass: Enter while it has the focus
    /// is its click, never the rebase's Run.
    plan_first: Option<egui::Id>,
}

fn dialog_id() -> egui::Id {
    egui::Id::unique("sopen-git-action")
}

/// The id of a form's `n`th text field; the first has the typing.
fn field_id(n: usize) -> egui::Id {
    dialog_id().with(("field", n))
}

/// A dialog's width: `cells` cells, within the window.
pub(super) fn width(ctx: &egui::Context, cells: f32) -> f32 {
    let room = ctx.content_rect().width() - 8.0 * chrome::CELL.x;
    (cells * chrome::CELL.x)
        .min(room)
        .max(20.0 * chrome::CELL.x)
}

impl ActionDialog {
    /// The dialog of `id` on `target` — the menu's choice, or an F-key's action — at its
    /// defaults, planned once. See [`Opened`] for what else it may be.
    pub fn open(
        id: ActionId,
        target: &Target,
        repo: &Repo,
        prefill: &Prefill,
        planner: Planner,
    ) -> Opened {
        if menu::is_view(id) {
            return Opened::View(id);
        }
        if !menu::items(target, repo).iter().any(|i| i.id == id) {
            return Opened::NotHere(id);
        }
        let Some(form) = Form::of(id, target, repo, prefill) else {
            return Opened::NotHere(id);
        };
        if id == ActionId::Checkout && prefill.checkout_unasked {
            let planned = planner(&form.draft, repo);
            if planned.is_ok() {
                return Opened::Run(form.draft);
            }
            return Self::with(form, repo, planned);
        }
        Self::start(form, repo, planner)
    }

    /// The dialog of an action built elsewhere, with no options: the banner's steps
    /// (`Action::Proceed`), the Backups list's Delete (`Action::DeleteBackup`), Fetch all's key
    /// (`Action::Fetch`, which runs at once when its plan asks nothing). A restore goes to
    /// [`ActionDialog::restore`].
    pub fn confirm(action: Action, repo: &Repo, planner: Planner) -> Opened {
        if let Action::Restore { stamp, entries } = action {
            return Self::restore(stamp, entries, None, repo, planner);
        }
        let label = match &action {
            Action::DeleteBackup { stamp } => format!("Delete backup {stamp}"),
            other => menu::label(other.id()).trim_end_matches('…').to_string(),
        };
        Self::start(Form::fixed(action, label, false), repo, planner)
    }

    /// The restore of group `stamp` (03 §8.3; Undo's, and the Backups list's Restore), its
    /// entries as the integration read them now: never over anything — an entry that moved
    /// since starts unchosen (restoring it is a second, explicit choice), and nothing is
    /// replaced until its "replace" is chosen. `only`: one item of the group, by its backup
    /// ref's name — that entry alone is chosen, unless it moved since.
    pub fn restore(
        stamp: String,
        mut entries: Vec<RestoreEntry>,
        only: Option<&str>,
        repo: &Repo,
        planner: Planner,
    ) -> Opened {
        for e in &mut entries {
            let asked = only.is_none_or(|o| e.backup.as_ref().is_some_and(|b| b.refname == o));
            e.chosen = asked && !e.moved_since;
            e.replace = false;
        }
        let label = format!("Restore {stamp}");
        let form = Form::fixed(Action::Restore { stamp, entries }, label, true);
        Self::start(form, repo, planner)
    }

    fn start(form: Form, repo: &Repo, planner: Planner) -> Opened {
        let planned = planner(&form.draft, repo);
        if !form.options && matches!(planned, Ok(Plan { confirm: None, .. })) {
            return Opened::Run(form.draft);
        }
        Self::with(form, repo, planned)
    }

    fn with(form: Form, repo: &Repo, planned: Result<Plan, Refused>) -> Opened {
        Opened::Dialog(Box::new(ActionDialog {
            planned: Some((form.draft.clone(), planned)),
            form,
            repo: repo.clone(),
            typing: false,
            cancel: None,
            plan_first: None,
        }))
    }

    /// The action as the options make it now.
    pub fn draft(&self) -> &Action {
        &self.form.draft
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

    /// The draft planned again when it changed since the last plan; whether it was.
    fn replan(&mut self, planner: Planner) -> bool {
        if self.form.refusal().is_some() {
            return false;
        }
        let stale = self
            .planned
            .as_ref()
            .is_none_or(|(a, _)| *a != self.form.draft);
        if stale {
            let plan = planner(&self.form.draft, &self.repo);
            self.planned = Some((self.form.draft.clone(), plan));
        }
        stale
    }

    /// The confirmation of the current plan, if it has one.
    fn confirmation(&self) -> Option<&actions::Confirmation> {
        match &self.planned {
            Some((_, Ok(plan))) => plan.confirm.as_ref(),
            _ => None,
        }
    }

    /// The title: the confirmation's, else the action's name.
    fn title(&self) -> String {
        match self.confirmation() {
            Some(c) if !c.title.is_empty() => c.title.clone(),
            _ => self.form.label.trim_end_matches('…').to_string(),
        }
    }

    /// The cells of `grid` (the panels') the dialog covers, its box and its shadow, for the panel
    /// and the other side to blank (10 §17, X4-graph's seam): none while egui draws it, until
    /// X4-dialogs (X4-dialogs2 for 03 §7.1–§7.5's forms).
    pub fn covers(&self, _grid: &Grid) -> Vec<Cells> {
        Vec::new()
    }

    /// One pass on `grid` (the panels', where it stands once it is in cells): Esc and Enter are
    /// taken before its widgets see them (Shift+Enter is a message field's new line); `planner`
    /// plans the draft again whenever an option changed it.
    pub fn ui(&mut self, ctx: &egui::Context, _grid: Grid, planner: Planner) -> Option<DialogEnd> {
        if self.replan(planner) {
            ctx.request_repaint();
        }
        if ctx.input_mut(|i| keys::take(i, Key::Escape, Mods::None)) {
            return Some(DialogEnd::Cancel);
        }
        let focused = ctx.memory(|m| m.focused());
        let enter = ctx.input_mut(|i| keys::take(i, Key::Enter, Mods::None));
        // Enter on the focused Cancel cancels, as its click would; on Rebase's focused "Plan the
        // commits first…" it is that button's click.
        if enter && focused.is_some() && focused == self.cancel {
            return Some(DialogEnd::Cancel);
        }
        if enter && focused.is_some() && focused == self.plan_first {
            if let Action::Rebase { onto, .. } = &self.form.draft {
                return Some(DialogEnd::PlanFirst(onto.clone()));
            }
        }
        let mut run = enter;
        let mut cancel = false;
        if self.typing && focused.is_none() {
            ctx.memory_mut(|m| m.request_focus(field_id(0)));
        }
        let title = self.title();
        let width = width(ctx, 72.0);
        let room = (ctx.content_rect().height() * 0.6).max(8.0 * chrome::CELL.y);
        let mut typing = false;
        let mut cancel_button = None;
        let (mut plan_first, mut plan_first_button) = (false, None);
        let response = chrome::dialog(ctx, dialog_id(), &title, |ui| {
            ui.set_width(width);
            // 03 §8.1: a destructive or rewriting action's dialog starts with what is lost.
            if let Some(loses) = self.confirmation().and_then(|c| c.loses.clone()) {
                ui.add(egui::Label::new(loses).wrap());
                ui.add_space(chrome::CELL.y / 2.0);
            }
            if self.form.options {
                typing = form_ui(ui, &mut self.form);
                ui.add_space(chrome::CELL.y / 2.0);
            }
            if self.replan(planner) {
                ui.ctx().request_repaint();
            }
            egui::ScrollArea::vertical()
                .id_salt("sopen-git-action-plan")
                .max_height(room)
                .show(ui, |ui| self.plan_ui(ui));
            ui.add_space(chrome::CELL.y / 2.0);
            let ready = self.ready();
            ui.horizontal(|ui| {
                run |= ui.add_enabled(ready, egui::Button::new("Run")).clicked();
                let no = ui.add(egui::Button::new("Cancel"));
                cancel_button = Some(no.id);
                cancel |= no.clicked();
                // 03 §7.1: Rebase offers the planner, which names the merges it would flatten.
                if matches!(self.form.draft, Action::Rebase { .. }) {
                    let plan = ui.add(egui::Button::new(PLAN_FIRST));
                    plan_first_button = Some(plan.id);
                    plan_first |= plan.clicked();
                }
            });
            ui.label(RichText::new("Enter runs · Esc cancels").weak());
        });
        self.typing = typing;
        self.cancel = cancel_button;
        self.plan_first = plan_first_button;
        if cancel || response.should_close() {
            return Some(DialogEnd::Cancel);
        }
        if let (true, Action::Rebase { onto, .. }) = (plan_first, &self.form.draft) {
            return Some(DialogEnd::PlanFirst(onto.clone()));
        }
        if run && self.ready() {
            let dont_ask_again =
                self.form.dont_ask && matches!(self.form.draft, Action::Checkout { .. });
            return Some(DialogEnd::Run {
                action: self.form.draft.clone(),
                dont_ask_again,
            });
        }
        None
    }

    /// Rebase's "Plan the commits first…" button, as last drawn (tests).
    #[cfg(test)]
    pub fn plan_first_button(&self) -> Option<egui::Id> {
        self.plan_first
    }

    /// The draft's plan is in and neither the dialog nor the plan refused it.
    fn ready(&self) -> bool {
        self.form.refusal().is_none()
            && matches!(&self.planned, Some((a, Ok(_))) if *a == self.form.draft)
    }

    /// The plan's parts (03 §8.1, §8.4), or the refusal — the dialog's own, else the plan's.
    fn plan_ui(&self, ui: &mut egui::Ui) {
        let error = ui.visuals().error_fg_color;
        if let Some(why) = self.form.refusal() {
            ui.add(egui::Label::new(RichText::new(why).color(error)).wrap());
            return;
        }
        let Some((_, planned)) = &self.planned else {
            return;
        };
        let plan = match planned {
            Ok(plan) => plan,
            Err(Refused(why)) => {
                ui.add(egui::Label::new(RichText::new(why).color(error)).wrap());
                return;
            }
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
        if let Action::DeleteBackup { .. } = self.form.draft {
            ui.label("Deleting a backup is the one removal sopen does without a backup.");
        }
        for change in plan.remote_changes() {
            ui.add(egui::Label::new(RichText::new(remote_warning(change)).color(error)).wrap());
        }
    }
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

/// §8.4's warning for one overwrite or delete on a remote: never a plain `--force`.
pub fn remote_warning(change: &RemoteChange) -> String {
    let verb = match change.change {
        Overwrite::Replace { .. } => "overwrites",
        Overwrite::Delete => "deletes",
    };
    format!(
        "{verb} {} on {}, with a lease on the value read at Run",
        change.refname, change.remote
    )
}

/// A step's line where a plan has no confirmation to list its commands: the command line as
/// the log will show it.
pub fn step_line(step: &Step) -> String {
    match step {
        Step::Backup => "backup (refs/sopen-backup/<stamp>/)".to_string(),
        Step::Git(command) => command.command_line(),
        Step::Remote(change) => {
            let mut line = format!(
                "git push --progress --force-with-lease={}:<value read at Run>",
                change.refname
            );
            if change.options.set_upstream {
                line.push_str(" --set-upstream");
            }
            if change.options.no_verify {
                line.push_str(" --no-verify");
            }
            match &change.change {
                Overwrite::Replace { src } => {
                    line.push_str(&format!(" {} {src}:{}", change.remote, change.refname))
                }
                Overwrite::Delete => {
                    line.push_str(&format!(" {} --delete {}", change.remote, change.refname))
                }
            }
            line
        }
        Step::Archive { command, to } => format!("{} > {}", command.command_line(), to.display()),
    }
}

// ---------------------------------------------------------------------------------------------
// The forms.

/// A choice among `options` (one of them chosen): whether it changed.
fn one_of<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    heading: &str,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(heading);
        for (option, word) in options {
            if chrome::choice(ui, *value == *option, word).clicked() && *value != *option {
                *value = *option;
                changed = true;
            }
        }
    });
    changed
}

/// An option on or off.
fn toggle(ui: &mut egui::Ui, on: &mut bool, word: &str) {
    if chrome::choice(ui, *on, word).clicked() {
        *on = !*on;
    }
}

/// A one-line text field: whether it was laid out (it can have the typing).
fn field(ui: &mut egui::Ui, heading: &str, text: &mut String, n: usize) -> bool {
    ui.horizontal(|ui| {
        ui.label(heading);
        ui.add(
            chrome::field(text)
                .id(field_id(n))
                .desired_width(ui.available_width()),
        );
    });
    true
}

/// A URL field: whether the owner changed it this pass.
fn url_field(ui: &mut egui::Ui, heading: &str, text: &mut String, n: usize) -> bool {
    ui.horizontal(|ui| {
        ui.label(heading);
        ui.add(
            chrome::field(text)
                .id(field_id(n))
                .desired_width(ui.available_width()),
        )
        .changed()
    })
    .inner
}

/// A message field: several lines, Shift+Enter a new line (Enter runs the dialog). Whether the
/// owner changed it this pass.
fn message(ui: &mut egui::Ui, heading: &str, text: &mut String, n: usize) -> bool {
    ui.label(heading);
    ui.add(
        egui::TextEdit::multiline(text)
            .id(field_id(n))
            .text_color(chrome::PALETTE.field_text)
            .desired_rows(4)
            .desired_width(ui.available_width())
            .return_key(egui::KeyboardShortcut::new(
                egui::Modifiers::SHIFT,
                Key::Enter,
            )),
    )
    .changed()
}

/// One name among `names`.
fn pick_name(ui: &mut egui::Ui, heading: &str, value: &mut String, names: &[String]) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(heading);
        for name in names {
            if chrome::choice(ui, value == name, name).clicked() && value != name {
                *value = name.clone();
                changed = true;
            }
        }
    });
    changed
}

/// Any of `names`, each on or off, in `names`' order.
fn pick_names(ui: &mut egui::Ui, heading: &str, chosen: &mut Vec<String>, names: &[String]) {
    ui.horizontal_wrapped(|ui| {
        ui.label(heading);
        for name in names {
            let on = chosen.contains(name);
            if chrome::choice(ui, on, name).clicked() {
                if on {
                    chosen.retain(|c| c != name);
                } else {
                    chosen.push(name.clone());
                    chosen.sort_by_key(|c| names.iter().position(|n| n == c));
                }
            }
        }
    });
}

/// None (`none`) or one of `names`.
fn pick_optional(
    ui: &mut egui::Ui,
    heading: &str,
    value: &mut Option<String>,
    none: &str,
    names: &[String],
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(heading);
        if chrome::choice(ui, value.is_none(), none).clicked() {
            *value = None;
        }
        for name in names {
            if chrome::choice(ui, value.as_deref() == Some(name), name).clicked() {
                *value = Some(name.clone());
            }
        }
    });
}

/// A merge's parent (`-m <n>`, 1…n) when a commit is a merge.
fn parent(ui: &mut egui::Ui, mainline: &mut Option<u32>, parents: usize) {
    if parents < 2 {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("Parent");
        for n in 1..=parents as u32 {
            if chrome::choice(ui, *mainline == Some(n), &n.to_string()).clicked() {
                *mainline = Some(n);
            }
        }
    });
}

/// `pull`'s how (03 §7.3).
fn pull_how(ui: &mut egui::Ui, how: &mut PullHow) {
    let mut kind = match how {
        PullHow::Merge { .. } => 0,
        PullHow::Rebase => 1,
        PullHow::FastForwardOnly => 2,
    };
    if one_of(
        ui,
        "How",
        &mut kind,
        &[(0, "merge"), (1, "rebase"), (2, "fast-forward only")],
    ) {
        *how = match kind {
            0 => PullHow::default(),
            1 => PullHow::Rebase,
            _ => PullHow::FastForwardOnly,
        };
    }
    if let PullHow::Merge { no_ff, squash } = how {
        ui.horizontal_wrapped(|ui| {
            toggle(ui, no_ff, "Never fast-forward");
            toggle(ui, squash, "Squash");
        });
    }
}

/// `merge`'s options (03 §7.1).
fn merge_options(ui: &mut egui::Ui, o: &mut MergeOptions) {
    one_of(
        ui,
        "Fast-forward",
        &mut o.fast_forward,
        &[
            (FastForward::WhenPossible, "when possible"),
            (FastForward::Never, "never"),
            (FastForward::Only, "only"),
        ],
    );
    one_of(
        ui,
        "On conflicts prefer",
        &mut o.prefer,
        &[
            (Prefer::Nothing, "nothing"),
            (Prefer::Ours, "ours"),
            (Prefer::Theirs, "theirs"),
        ],
    );
    ui.horizontal_wrapped(|ui| {
        toggle(ui, &mut o.squash, "Squash");
        toggle(ui, &mut o.no_commit, "Don't commit");
        toggle(ui, &mut o.allow_unrelated, "Allow unrelated histories");
        toggle(ui, &mut o.no_verify, "Skip hooks");
    });
}

/// What a backup's subject is called in the restore dialog.
fn subject_text(subject: &Subject) -> String {
    match subject {
        Subject::Ref(r) => r.clone(),
        Subject::Pushed { remote, refname } => format!("{refname} on {remote}"),
        Subject::Head => "HEAD".to_string(),
        Subject::Stash { commit } => format!("stash {}", commit.short()),
        Subject::WorkTree => "the work tree".to_string(),
        Subject::Path(p) => p.clone(),
    }
}

/// A manifest value as the restore dialog shows it.
fn value_text(value: &Value) -> String {
    match value {
        Value::Object(oid) => oid.short().to_string(),
        Value::Absent => "absent".to_string(),
        Value::Clean => "clean".to_string(),
        Value::Occupied => "occupied".to_string(),
    }
}

/// The restore's entries (03 §8.3): each chosen or not, its value then, `moved since (now …)`,
/// and for an occupied path its "replace".
fn restore_ui(ui: &mut egui::Ui, entries: &mut [RestoreEntry]) {
    if entries.is_empty() {
        ui.label("nothing in this group to restore");
    }
    for e in entries {
        ui.horizontal_wrapped(|ui| {
            let text = format!("{} → {}", subject_text(&e.subject), value_text(&e.before));
            toggle(ui, &mut e.chosen, &text);
            if e.moved_since {
                ui.label(format!("moved since (now {})", value_text(&e.now)));
            }
            // In the way as S3-A's `entries` looked: a leaf of other bytes (`now` is its blob) or
            // anything that is not a file (`occupied`) — S3-C: not `Occupied` alone.
            if crate::git::backup::blocked(e) {
                ui.label("occupied: left as it is unless replace is chosen");
                toggle(ui, &mut e.replace, "replace");
            }
        });
    }
}

/// The options of the form's draft, edited in place; whether a text field was laid out.
fn form_ui(ui: &mut egui::Ui, form: &mut Form) -> bool {
    let Form {
        draft,
        remotes,
        names,
        locals,
        parents,
        tags,
        update_refs,
        kept,
        text,
        dont_ask,
        known: urls,
        how: kept_how,
        url,
        url_touched,
        push_touched,
        ..
    } = form;
    let mut typing = false;
    match draft {
        Action::Tag {
            name,
            message: m,
            push,
            ..
        } => {
            typing = field(ui, "Name", name, 0);
            let mut on = m.is_some();
            if one_of(
                ui,
                "Kind",
                &mut on,
                &[(true, "annotated"), (false, "lightweight")],
            ) {
                if on {
                    *m = Some(std::mem::take(kept));
                } else {
                    *kept = m.take().unwrap_or_default();
                }
            }
            if let Some(m) = m {
                message(ui, "Message", m, 1);
            }
            pick_optional(ui, "Push to", push, "no remote", remotes);
        }
        Action::Branch { name, checkout, .. } => {
            typing = field(ui, "Name", name, 0);
            toggle(ui, checkout, "Check it out");
        }
        Action::MoveBranch { branch, .. } => {
            pick_name(ui, "Branch", branch, names);
        }
        Action::Checkout { .. } => toggle(ui, dont_ask, "Don't ask again in this window"),
        Action::Pick {
            commits,
            mainline,
            record_origin,
            no_commit,
        } => {
            if commits.len() > 1 {
                ui.label(format!("{} commits, oldest first", commits.len()));
            }
            parent(ui, mainline, *parents);
            ui.horizontal_wrapped(|ui| {
                toggle(ui, record_origin, "Record origin (-x)");
                toggle(ui, no_commit, "Don't commit");
            });
        }
        Action::Revert { commits, mainline } => {
            if commits.len() > 1 {
                ui.label(format!("{} commits, newest first", commits.len()));
            }
            parent(ui, mainline, *parents);
        }
        Action::Merge { options, .. } => merge_options(ui, options),
        Action::Rebase { options, .. } => {
            ui.horizontal_wrapped(|ui| {
                toggle(ui, &mut options.ignore_date, "Ignore dates");
                toggle(ui, &mut options.autostash, "Stash my changes for it");
                if *update_refs {
                    toggle(ui, &mut options.update_refs, "Move other branches along");
                }
            });
        }
        Action::Reset { mode, .. } => {
            one_of(
                ui,
                "Mode",
                mode,
                &[
                    (ResetMode::Soft, "soft"),
                    (ResetMode::Mixed, "mixed"),
                    (ResetMode::Keep, "keep"),
                    (ResetMode::Hard, "hard"),
                ],
            );
        }
        Action::Archive { format, to, .. } => {
            let was = *format;
            if one_of(
                ui,
                "Format",
                format,
                &[(ArchiveFormat::Zip, "zip"), (ArchiveFormat::Tar, "tar")],
            ) {
                let old = format!(".{}", extension(was));
                if let Some(stem) = text.strip_suffix(&old) {
                    *text = format!("{stem}.{}", extension(*format));
                }
            }
            typing = field(ui, "Save to", text, 0);
            *to = PathBuf::from(text.trim());
        }
        Action::Rename { to, .. } => typing = field(ui, "New name", to, 0),
        Action::DeleteBranch {
            force,
            remotes: chosen,
            ..
        } => {
            toggle(ui, force, "Force");
            if !remotes.is_empty() {
                pick_names(ui, "Also delete it on", chosen, remotes);
            }
        }
        Action::Push {
            remotes: chosen,
            options,
            overwrite,
            ..
        } => {
            pick_names(ui, "Remotes", chosen, remotes);
            ui.horizontal_wrapped(|ui| {
                toggle(ui, &mut options.set_upstream, "Set upstream");
                toggle(ui, overwrite, "Overwrite the remote branch, with lease");
                toggle(ui, &mut options.no_verify, "Skip hooks");
            });
        }
        Action::CheckoutRemote {
            remote,
            branch,
            name,
            pull,
        } => {
            typing = field(ui, "Local name", name, 0);
            if locals.contains(name) {
                let how = pull.get_or_insert(*kept_how);
                ui.label(format!(
                    "{name} exists: check it out and pull {remote}/{branch} into it"
                ));
                pull_how(ui, how);
                *kept_how = *how;
            } else {
                *pull = None;
            }
        }
        Action::FetchInto { force, .. } => {
            toggle(ui, force, "Force, when it is not a fast-forward")
        }
        Action::Pull { how, .. } => pull_how(ui, how),
        Action::DeleteTag { remote, .. } => {
            pick_optional(ui, "Also on", remote, "no remote", remotes)
        }
        Action::PushTag {
            remotes: chosen,
            replace,
            ..
        } => {
            pick_names(ui, "Remotes", chosen, remotes);
            toggle(
                ui,
                replace,
                "Replace the remote's tag when it differs, with lease",
            );
        }
        Action::MoveTag {
            tag, message: m, ..
        } => {
            // Each tag keeps its own message (03 §7.4): a choice loads the chosen tag's.
            if pick_name(ui, "Tag", tag, names) {
                *m = tags.annotated_message(tag);
            }
            let mut on = m.is_some();
            toggle(ui, &mut on, "Keep it annotated with its message");
            if on != m.is_some() {
                *m = on.then(|| tags.message_of(tag));
            }
            if let Some(text) = m {
                if message(ui, "Message", text, 0) {
                    tags.edited.insert(tag.clone(), text.clone());
                }
                typing = true;
            }
        }
        Action::StashApply { index, .. } | Action::StashPop { index, .. } => {
            toggle(ui, index, "Restore the index too (--index)")
        }
        Action::StashBranch { name, .. } => typing = field(ui, "Name", name, 0),
        Action::Commit {
            message: m,
            amend,
            no_verify,
        } => {
            message(ui, "Message", m, 0);
            typing = true;
            ui.horizontal_wrapped(|ui| {
                toggle(ui, amend, "Amend the last commit instead");
                toggle(ui, no_verify, "Skip hooks");
            });
        }
        Action::Amend { message: m } => {
            let mut edit = m.is_some();
            if one_of(
                ui,
                "Message",
                &mut edit,
                &[(false, "keep its message"), (true, "edit it")],
            ) {
                if edit {
                    *m = Some(std::mem::take(kept));
                } else {
                    *kept = m.take().unwrap_or_default();
                }
            }
            if let Some(m) = m {
                message(ui, "New message", m, 0);
                typing = true;
            }
        }
        Action::Stash {
            message: m,
            include_untracked,
        } => {
            typing = field(ui, "Message", m, 0);
            toggle(ui, include_untracked, "Include untracked files");
        }
        Action::ResetChanges { hard } => {
            one_of(
                ui,
                "Mode",
                hard,
                &[
                    (false, "mixed (unstage all)"),
                    (true, "hard (discard all tracked changes)"),
                ],
            );
        }
        Action::Clean { folders } => toggle(ui, folders, "Folders too (-d)"),
        Action::Fetch { prune, prune_tags } => {
            ui.horizontal_wrapped(|ui| {
                toggle(ui, prune, "Prune");
                toggle(ui, prune_tags, "Prune tags");
            });
        }
        Action::FetchRemote {
            remote,
            prune,
            prune_tags,
        } => {
            pick_name(ui, "Remote", remote, remotes);
            ui.horizontal_wrapped(|ui| {
                toggle(ui, prune, "Prune");
                toggle(ui, prune_tags, "Prune tags");
            });
        }
        Action::PruneRemote { remote } | Action::DeleteRemote { remote } => {
            pick_name(ui, "Remote", remote, remotes);
        }
        Action::AddRemote {
            name,
            url,
            push_url,
            fetch_now,
        } => {
            typing = field(ui, "Name", name, 0);
            field(ui, "Fetch URL", url, 1);
            field(ui, "Push URL (blank: the fetch URL)", text, 2);
            *push_url = Some(text.trim().to_string()).filter(|u| !u.is_empty());
            toggle(ui, fetch_now, "Fetch now");
        }
        Action::EditRemote { was, now } => {
            let mut chosen = was.name.clone();
            if pick_name(ui, "Remote", &mut chosen, remotes) {
                *was = known(urls, &chosen);
                *now = was.clone();
                *url = was.url.clone();
                *text = was.push_url.clone().unwrap_or_default();
                (*url_touched, *push_touched) = (false, false);
            }
            typing = field(ui, "Name", &mut now.name, 0);
            *url_touched |= url_field(ui, "Fetch URL", url, 1);
            *push_touched |= url_field(ui, "Push URL (blank: the fetch URL)", text, 2);
            // A field left alone is the URL as it is set: `now` keeps `was`'s (masked) value, so
            // the plan writes nothing for it (03 §3.7); a changed one is the URL as typed.
            now.url = match url_touched {
                true => url.trim().to_string(),
                false => was.url.clone(),
            };
            now.push_url = match push_touched {
                true => Some(text.trim().to_string()).filter(|u| !u.is_empty()),
                false => was.push_url.clone(),
            };
        }
        Action::User { name, email, scope } => {
            typing = field(ui, "Name", name, 0);
            field(ui, "Email", email, 1);
            one_of(
                ui,
                "Where",
                scope,
                &[
                    (ConfigScope::Local, "for this repository"),
                    (ConfigScope::Global, "globally"),
                ],
            );
        }
        Action::UserRemove { scope } => {
            one_of(
                ui,
                "Where",
                scope,
                &[
                    (ConfigScope::Local, "this repository"),
                    (ConfigScope::Global, "globally"),
                ],
            );
        }
        Action::Restore { entries, .. } => restore_ui(ui, entries),
        // The actions without options (`has_options`), and any added later: their confirmation.
        _ => {}
    }
    typing
}

// ---------------------------------------------------------------------------------------------
// The result dialog (03 §3.8, §8.3).

/// What 03 §3.8 calls a step's outcome.
pub fn outcome_word(outcome: StepOutcome) -> &'static str {
    match outcome {
        StepOutcome::Done => "done",
        StepOutcome::Failed => "failed",
        StepOutcome::NotRun => "not run",
    }
}

/// `backup <stamp> · Cmd/Ctrl+Z undoes` (03 §8.3).
pub fn backup_line(stamp: &str) -> String {
    format!("backup {stamp} · {}Z undoes", Mods::Command.prefix())
}

/// The status row after a run (03 §8.3): `<what was done> · backup <stamp> · Cmd/Ctrl+Z undoes`
/// when it wrote a backup, else `what` alone.
pub fn status_text(what: &str, outcome: &Outcome) -> String {
    match &outcome.backup {
        Some(stamp) => format!("{what} · {}", backup_line(stamp)),
        None => what.to_string(),
    }
}

/// The error dialog of a run that failed (03 §3.8). Everything it shows is masked (03 §3.7),
/// whoever made the text: a URL the owner typed is masked here as in the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultDialog {
    /// The action.
    title: String,
    /// Each step's outcome and line, in order.
    steps: Vec<(StepOutcome, String)>,
    /// The failed command's text — git's stderr, else stdout, else its ending — whole, and its
    /// command line as the log keeps it.
    error: Option<(String, Option<String>)>,
    /// The group written before the first step, whatever came after.
    backup: Option<String>,
    /// The run's warning (`Outcome::warning`: `backup <stamp> incomplete: …`), said in the
    /// error colour in place of the backup's undo line.
    warning: Option<String>,
}

fn result_id() -> egui::Id {
    egui::Id::unique("sopen-git-result")
}

impl ResultDialog {
    /// The dialog of `outcome`, titled with the action; `None` when every step is done and the
    /// backup is whole (the status row then says what was done, [`status_text`]). A run whose
    /// backup is incomplete (`Outcome::warning`) has its dialog too, the warning in place of the
    /// undo line (S3-C).
    pub fn of(title: &str, outcome: &Outcome) -> Option<ResultDialog> {
        let warning = outcome.warning().map(|w| mask_credentials(&w));
        let failed = outcome
            .steps
            .iter()
            .find(|s| s.outcome == StepOutcome::Failed);
        if failed.is_none() && warning.is_none() {
            return None;
        }
        let done = failed.and_then(|failed| {
            failed
                .ran
                .iter()
                .rev()
                .find(|d| !d.ok())
                .or(failed.ran.last())
        });
        Some(ResultDialog {
            title: title.to_string(),
            steps: outcome
                .steps
                .iter()
                .map(|s| (s.outcome, mask_credentials(&s.line)))
                .collect(),
            error: done.map(|d| {
                let line = mask_credentials(&d.entry.line);
                (mask_credentials(&d.error_text()), Some(line))
            }),
            backup: outcome.backup.clone(),
            warning,
        })
    }

    /// A failure with no plan behind it (a read the integration made: the Backups list's, a
    /// restore's): its text and, when a command ran, its line.
    pub fn error(title: &str, text: &str, command_line: Option<&str>) -> ResultDialog {
        ResultDialog {
            title: title.to_string(),
            steps: Vec::new(),
            error: Some((mask_credentials(text), command_line.map(mask_credentials))),
            backup: None,
            warning: None,
        }
    }

    /// The cells of `grid` (the panels') the result covers, its box and its shadow, for the panel
    /// and the other side to blank (10 §17, X4-graph's seam): none while egui draws it, until
    /// X4-dialogs2.
    pub fn covers(&self, _grid: &Grid) -> Vec<Cells> {
        Vec::new()
    }

    /// One pass on `grid` (the panels', where it stands once it is in cells); `true` once Enter,
    /// Esc, OK or a click outside closed it.
    pub fn ui(&mut self, ctx: &egui::Context, _grid: Grid) -> bool {
        let close = ctx.input_mut(|i| {
            keys::take(i, Key::Enter, Mods::None) | keys::take(i, Key::Escape, Mods::None)
        });
        let width = width(ctx, 80.0);
        let room = (ctx.content_rect().height() * 0.5).max(6.0 * chrome::CELL.y);
        let mut ok = false;
        let response = chrome::dialog(ctx, result_id(), &self.title, |ui| {
            ui.set_width(width);
            let error = ui.visuals().error_fg_color;
            for (outcome, line) in &self.steps {
                ui.horizontal(|ui| {
                    let word = outcome_word(*outcome);
                    let word = match outcome {
                        StepOutcome::Failed => RichText::new(word).color(error),
                        _ => RichText::new(word),
                    };
                    ui.add_sized(
                        egui::vec2(8.0 * chrome::CELL.x, chrome::CELL.y),
                        egui::Label::new(word),
                    );
                    ui.add(egui::Label::new(line.as_str()).wrap());
                });
            }
            if let Some((text, line)) = &self.error {
                ui.add_space(chrome::CELL.y / 2.0);
                egui::ScrollArea::vertical()
                    .id_salt("sopen-git-result-text")
                    .max_height(room)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(text.as_str()).wrap());
                    });
                if let Some(line) = line {
                    ui.add(egui::Label::new(RichText::new(line.as_str()).weak()).wrap());
                }
            }
            if let Some(warning) = &self.warning {
                ui.add_space(chrome::CELL.y / 2.0);
                ui.add(egui::Label::new(RichText::new(warning.as_str()).color(error)).wrap());
            } else if let Some(stamp) = &self.backup {
                ui.add_space(chrome::CELL.y / 2.0);
                ui.label(backup_line(stamp));
            }
            ui.add_space(chrome::CELL.y / 2.0);
            ok = ui.button("OK").clicked();
            ui.label(RichText::new("Enter or Esc closes").weak());
        });
        close || ok || response.should_close()
    }
}
