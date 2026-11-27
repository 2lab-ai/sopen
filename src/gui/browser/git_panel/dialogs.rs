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
//! - **In cells** (X4-dialogs, 10 §17): every dialog without options, 03 §7.6–§7.7's forms and
//!   a restore's are `ops::dialog`'s box on the panels' grid — the lost line, the options as the
//!   kit's fields, message fields and choices, the plan in rows that scroll (at most three fifths
//!   of the grid, eight at least, the kit giving them way first on a short grid), `[ Run ]` (off
//!   until the plan is in and nothing refuses it, and while the grid holds less than all of the
//!   box), `[ Cancel ]` and the hint — and [`ActionDialog::covers`] says its cells. Every other
//!   form of 03 §7.1–§7.5 is egui's until X4-dialogs2. The keyboard opens on the form's first
//!   text field, or on nothing; Enter runs (03 §8.1), but each Enter of a pass goes to what has
//!   the keyboard at that point — before the pass's first Tab what had it then, after it what the
//!   Tab gave it to — so Enter on a focused button is that button's press, and one after a
//!   Shift+Tab or an arrow that moves the keyboard (egui moves it later) is dropped, a Space too;
//!   only what the screen showed runs: a Run or an Enter runs when what would run is what the
//!   pass before painted and the host put on screen (`cells::presented`), else it is dropped and
//!   [`CHANGED`] says so; after two moves in a pass every later Enter is dropped, a Space too
//!   unless the pass began on a text field. Space is the kit's, once a
//!   press in the pass's order; Esc, Cancel or a click outside cancels, and a pass that both
//!   cancels and runs cancels.

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
use crate::gui::browser::ops::dialog::{self as kit, Button, Ink, Input, Inputs, Part};
use crate::gui::cells::{self, Cells, Grid};
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
    /// The form laid out a text field last pass: the typing goes there while nothing has it — in
    /// cells also when it shows anew (Amend's message once "edit it" is chosen).
    typing: bool,
    /// The Cancel button's id, last pass: Enter while it has the focus cancels.
    cancel: Option<egui::Id>,
    /// Rebase's "Plan the commits first…" button's id, last pass: Enter while it has the focus
    /// is its click, never the rebase's Run.
    plan_first: Option<egui::Id>,
    /// In cells: the first of the plan's rows shown.
    plan_top: usize,
    /// In cells: a Run or an Enter came when what would run was not what the screen showed, and
    /// was not acted on — the hint says the plan changed and Enter runs it.
    changed: bool,
    /// In cells: what the last pass painted, which the owner saw when they pressed this pass's
    /// keys — `None` until a pass has painted where it can be seen (egui lays a new modal out
    /// unseen the first time).
    on_screen: Option<OnScreen>,
    /// In cells: a pass has laid the box out (the next one paints where it can be seen).
    laid_once: bool,
}

/// What a pass of an action's dialog in cells painted: the draft, its plan's rows and whether its
/// Run was on. A Run or an Enter runs only what the screen showed: the last pass's, equal to what
/// would run at this pass's end.
#[derive(Debug, Clone, PartialEq)]
struct OnScreen {
    draft: Action,
    plan: Vec<PlanRow>,
    runs: bool,
}

fn dialog_id() -> egui::Id {
    egui::Id::unique("sopen-git-action")
}

/// The id of a form's `n`th text field; the first has the typing.
pub(super) fn field_id(n: usize) -> egui::Id {
    dialog_id().with(("field", n))
}

/// In cells: the box's id (its modal), and its buttons'.
fn cells_id() -> egui::Id {
    dialog_id().with("cells")
}

pub(super) fn run_id() -> egui::Id {
    cells_id().with("run")
}

pub(super) fn cancel_id() -> egui::Id {
    cells_id().with("cancel")
}

fn plan_first_id() -> egui::Id {
    cells_id().with("plan-first")
}

/// In cells: the id of a form's `k`th row of choices.
pub(super) fn row_id(k: usize) -> egui::Id {
    cells_id().with(("row", k))
}

/// An action's dialog's nominal width in cells, as egui's was (10 §6's rule widens it to its
/// widest text, at most 76 cells inside).
const NOMINAL: usize = 72;

/// The rows an action's message field shows (Commit's, Amend's), as egui's did (X4-kit).
const MESSAGE_ROWS: u16 = 4;

/// The plan's rows before they scroll, on a grid of `rows` (X4-dialogs' decision): three fifths
/// of it and eight at least, as egui's 60 % of the window and eight rows — the kit gives them way
/// first where the grid leaves less (10 §6: a dialog stands between the title and status rows).
fn plan_most(rows: u16) -> u16 {
    let most = u32::from(rows) * 3 / 5;
    u16::try_from(most).unwrap_or(u16::MAX).max(8)
}

/// The heading of the backups a plan writes first.
const BACKED_UP: &str = "Backed up first, under refs/sopen-backup/<stamp>/";

/// Delete backup's line: its group is the one thing removed without a backup (03 §8.2).
const NO_BACKUP: &str = "Deleting a backup is the one removal sopen does without a backup.";

/// What an action's dialog says Enter and Esc do (03 §8.1).
const HINT: &str = "Enter runs · Esc cancels";

/// The hint once a Run or an Enter came in a pass that changed the plan: only a plan that was on
/// screen runs (X4-dialogs' round-1 decision).
pub(super) const CHANGED: &str = "the plan changed — Enter runs it · Esc cancels";

/// The box's first line when the grid holds less than it: its parts that never give way stand
/// past the grid's foot, so it does not run until all of it shows (X4-dialogs, until a restore's
/// entries scroll).
pub(super) const TOO_SHORT: &str =
    "the window is too short for all of it: make it taller to run it";

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
            plan_top: 0,
            changed: false,
            on_screen: None,
            laid_once: false,
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

    /// The dialog is cells (X4-dialogs): it has no options, or it is 03 §7.6–§7.7's form or a
    /// restore's; the other forms are egui's until X4-dialogs2.
    fn in_cells(&self) -> bool {
        !self.form.options
            || matches!(
                self.form.draft,
                Action::Commit { .. }
                    | Action::Amend { .. }
                    | Action::Stash { .. }
                    | Action::ResetChanges { .. }
                    | Action::Clean { .. }
                    | Action::Fetch { .. }
                    | Action::FetchRemote { .. }
                    | Action::PruneRemote { .. }
                    | Action::AddRemote { .. }
                    | Action::EditRemote { .. }
                    | Action::DeleteRemote { .. }
                    | Action::User { .. }
                    | Action::UserRemove { .. }
                    | Action::Restore { .. }
            )
    }

    /// The cells of `grid` (the panels') the dialog covers, its box and its shadow, for the panel
    /// and the other side to blank (10 §17, X4-graph's seam), as the dialog stands now: none
    /// while egui draws it (03 §7.1–§7.5's forms, until X4-dialogs2).
    pub fn covers(&self, grid: &Grid) -> Vec<Cells> {
        if !self.in_cells() {
            return Vec::new();
        }
        // A restore's arrow is one cell either way: the box stands where the pass lays it.
        let controls = controls(&self.form, false);
        self.layout(*grid, &controls).0.covers()
    }

    /// One pass on `grid` (the panels'): Esc and Enter are taken before its widgets see them
    /// (Shift+Enter is a message field's new line); `planner` plans the draft again whenever an
    /// option changed it. In cells where [`ActionDialog::in_cells`] says so (see the module's
    /// notes), else egui's.
    pub fn ui(&mut self, ctx: &egui::Context, grid: Grid, planner: Planner) -> Option<DialogEnd> {
        if self.in_cells() {
            return self.cells_ui(ctx, grid, planner);
        }
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
            ui.label(RichText::new(HINT).weak());
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

    /// The plan's rows ([`plan_rows`]), and Delete backup's line.
    fn plan_lines(&self) -> Vec<PlanRow> {
        let own = self.form.refusal();
        let planned = self.planned.as_ref().map(|(_, p)| p);
        let shown = own.is_none() && matches!(planned, Some(Ok(_)));
        let mut out = plan_rows(own, planned);
        if shown && matches!(self.form.draft, Action::DeleteBackup { .. }) {
            out.push(PlanRow::new(NO_BACKUP, Ink::Text, false));
        }
        out
    }

    /// The plan's rows in egui ([`ActionDialog::plan_lines`]), a heading's lines indented.
    fn plan_ui(&self, ui: &mut egui::Ui) {
        let error = ui.visuals().error_fg_color;
        let rows = self.plan_lines();
        let mut k = 0;
        while k < rows.len() {
            if rows[k].under {
                let end = (k..rows.len()).find(|&i| !rows[i].under);
                let end = end.unwrap_or(rows.len());
                ui.indent(("sopen-git-plan", k), |ui| {
                    for row in &rows[k..end] {
                        ui.add(egui::Label::new(row.text.as_str()).wrap());
                    }
                });
                k = end;
                continue;
            }
            let row = &rows[k];
            let text = match row.ink {
                Ink::Error => RichText::new(row.text.as_str()).color(error),
                Ink::Weak => RichText::new(row.text.as_str()).weak(),
                Ink::Text => RichText::new(row.text.as_str()),
            };
            ui.add(egui::Label::new(text).wrap());
            k += 1;
        }
    }

    // -----------------------------------------------------------------------------------------
    // In cells (X4-dialogs).

    /// The box on `grid` with `controls` (the form's, [`controls`]), and whether it stands whole
    /// there: if not, it says so first ([`TOO_SHORT`]) and its Run is off.
    fn layout(&self, grid: Grid, controls: &[Control]) -> (kit::Layout, bool) {
        let whole = self.laid(grid, controls, true);
        if whole.fits() {
            return (whole, true);
        }
        (self.laid(grid, controls, false), false)
    }

    /// The box on `grid` with `controls`: the lost line first, then the options, the plan in rows
    /// that scroll ([`plan_part`]), the buttons and the hint; as wide as its widest text by 10
    /// §6's rule. Not `fits`: [`TOO_SHORT`] before all of it, Run off.
    fn laid(&self, grid: Grid, controls: &[Control], fits: bool) -> kit::Layout {
        let title = self.title();
        let lost = self.confirmation().and_then(|c| c.loses.clone());
        let plan = self.plan_lines();
        let wanted = (plan_wants(&plan))
            .max(lost.as_deref().map_or(0, cells::width))
            .max(cells::width(&title) + 2);
        let width = kit::inside_width(grid.cols, NOMINAL, wanted);
        let mut parts = Vec::new();
        if !fits {
            parts.push(Part::Text(TOO_SHORT.to_string(), Ink::Error));
        }
        if let Some(lost) = lost {
            parts.extend([Part::Text(lost, Ink::Text), Part::Blank]);
        }
        if !controls.is_empty() {
            parts.extend(controls.iter().map(Control::part));
            parts.push(Part::Blank);
        }
        if let Some(part) = plan_part(plan, grid, width, self.plan_top) {
            parts.extend([part, Part::Blank]);
        }
        let mut run = Button::new("Run", run_id());
        run.on = fits && self.ready();
        let mut buttons = vec![run, Button::new("Cancel", cancel_id())];
        // 03 §7.1: Rebase offers the planner (its form is egui's until X4-dialogs2).
        if matches!(self.form.draft, Action::Rebase { .. }) {
            buttons.push(Button::new(PLAN_FIRST, plan_first_id()));
        }
        parts.push(Part::Buttons(buttons));
        // Said only while what shows can run.
        parts.push(match self.changed && fits && self.ready() {
            true => Part::Text(CHANGED.to_string(), Ink::Text),
            false => Part::Text(HINT.to_string(), Ink::Weak),
        });
        kit::layout(grid, &title, NOMINAL, &parts)
    }

    /// What a pass paints as the dialog stands now, the box `fits` its grid or not.
    fn painted(&self, fits: bool) -> OnScreen {
        OnScreen {
            draft: self.form.draft.clone(),
            plan: self.plan_lines(),
            runs: fits && self.ready(),
        }
    }

    /// One pass in cells (see the module's notes).
    fn cells_ui(&mut self, ctx: &egui::Context, grid: Grid, planner: Planner) -> Option<DialogEnd> {
        if self.replan(planner) {
            ctx.request_repaint();
        }
        let escape = ctx.input_mut(|i| keys::take(i, Key::Escape, Mods::None));
        let began = ctx.memory(|m| m.focused());
        let mut controls = controls(&self.form, cells::ambiguous_wide(ctx));
        // egui moves the keyboard by an arrow unless a field has it (its caret's).
        let in_field = (controls.iter())
            .filter_map(Control::text_field)
            .any(|n| began == Some(field_id(n)));
        let enters = take_enters(ctx, began.is_some() && !in_field, in_field);
        let (layout, fits) = self.layout(grid, &controls);
        // What the owner saw (the last pass's painting), and what this pass paints.
        // A pass the host did not put on screen was never seen (the terminal prints none while
        // input waits): no record, so its Run or Enter waits for one that was.
        let seen = self.on_screen.take().filter(|_| cells::presented(ctx));
        if std::mem::replace(&mut self.laid_once, true) {
            self.on_screen = Some(self.painted(fits));
        }
        // The form's first text field takes the keyboard when it shows anew, or while nothing
        // has it.
        let typing = (controls.iter()).any(|c| c.text_field() == Some(0));
        if typing && (!self.typing || began.is_none()) {
            ctx.memory_mut(|m| m.request_focus(field_id(0)));
        }
        self.typing = typing;
        let (run, cancel, plan_first) = (run_id(), cancel_id(), plan_first_id());
        let mut by_id: Vec<(egui::Id, Input)> =
            (controls.iter_mut()).filter_map(Control::input).collect();
        // The buttons take the kit's presses, in the pass's order (F-kit-tab).
        by_id.extend([run, cancel, plan_first].map(|b| (b, Input::Button)));
        let inputs = Inputs {
            top: Some(&mut self.plan_top),
            by_id,
            ..Inputs::default()
        };
        let pass = kit::show(ctx, cells_id(), &layout, inputs);
        let now = ctx.memory(|m| m.focused());
        apply(&mut self.form, &controls);
        // The options as this pass left them, planned: the next pass shows it.
        if self.replan(planner) {
            ctx.request_repaint();
        }
        let mut cancelled = escape || pass.clicked(cancel) || pass.modal.should_close();
        let (mut ran, mut first) = (pass.clicked(run), pass.clicked(plan_first));
        for after_tab in enters {
            match if after_tab { now } else { began } {
                Some(id) if id == cancel => cancelled = true,
                Some(id) if id == plan_first => first = true,
                _ => ran = true,
            }
        }
        if cancelled {
            return Some(DialogEnd::Cancel);
        }
        if let (true, Action::Rebase { onto, .. }) = (first, &self.form.draft) {
            return Some(DialogEnd::PlanFirst(onto.clone()));
        }
        if !ran {
            return None;
        }
        // Only what the screen showed runs: what would run now — this pass's options, a refresh's
        // plan, reads that came — is what the last pass painted, its Run on; else the screen shows
        // it now and the next Run or Enter runs it.
        let wide = cells::ambiguous_wide(ctx);
        let (_, fits_now) = self.layout(grid, &self::controls(&self.form, wide));
        let end = self.painted(fits_now);
        if seen.as_ref() != Some(&end) {
            self.changed |= seen.is_some();
            ctx.request_repaint();
            return None;
        }
        if end.runs {
            let dont_ask_again =
                self.form.dont_ask && matches!(self.form.draft, Action::Checkout { .. });
            return Some(DialogEnd::Run {
                action: self.form.draft.clone(),
                dont_ask_again,
            });
        }
        None
    }
}

/// A row of a plan's part: its text, its ink, and whether it is a line under a heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlanRow {
    pub(super) text: String,
    pub(super) ink: Ink,
    pub(super) under: bool,
}

impl PlanRow {
    fn new(text: &str, ink: Ink, under: bool) -> Self {
        Self {
            text: text.to_string(),
            ink,
            under,
        }
    }
}

/// A plan's rows (03 §8.1, §8.4), or the refusal — `own`, the dialog's, else the plan's: what
/// changes, the backups written first and the exact command lines, each under its heading in the
/// weak colour, then the notes and §8.4's warning of each overwrite or delete on a remote in the
/// error colour. Nothing while no plan is in. An action's dialog's, and a quick edit's to come
/// (X4-quick).
pub(super) fn plan_rows(
    own: Option<String>,
    planned: Option<&Result<Plan, Refused>>,
) -> Vec<PlanRow> {
    if let Some(why) = own {
        return vec![PlanRow::new(&why, Ink::Error, false)];
    }
    let plan = match planned {
        None => return Vec::new(),
        Some(Err(Refused(why))) => return vec![PlanRow::new(why, Ink::Error, false)],
        Some(Ok(plan)) => plan,
    };
    let mut out = Vec::new();
    let heading = |out: &mut Vec<PlanRow>, heading: &str, lines: &[String]| {
        if !lines.is_empty() {
            out.push(PlanRow::new(heading, Ink::Weak, false));
            out.extend(lines.iter().map(|l| PlanRow::new(l, Ink::Text, true)));
        }
    };
    match &plan.confirm {
        Some(c) => {
            heading(&mut out, "Changes", &c.changes);
            heading(&mut out, BACKED_UP, &c.backups);
            heading(&mut out, "Runs", &c.commands);
            out.extend(c.notes.iter().map(|n| PlanRow::new(n, Ink::Text, false)));
        }
        None => {
            let steps: Vec<String> = plan.steps.iter().map(step_line).collect();
            heading(&mut out, "Runs", &steps);
        }
    }
    for change in plan.remote_changes() {
        out.push(PlanRow::new(&remote_warning(change), Ink::Error, false));
    }
    out
}

/// How many cells a box wants inside for `rows` to stand whole, a heading's lines two cells in:
/// a box at least this wide ([`kit::inside_width`]) holds [`plan_part`]'s rows.
pub(super) fn plan_wants(rows: &[PlanRow]) -> usize {
    (rows.iter())
        .map(|r| cells::width(&r.text) + if r.under { 2 } else { 0 })
        .max()
        .unwrap_or(0)
}

/// `rows` as the kit's rows that scroll in a box `width` cells inside on `grid`, from row `top`:
/// a heading's lines two cells in, wrapped there (a long line goes on two cells in), at most
/// [`plan_most`] of them on screen; none for no rows.
pub(super) fn plan_part(rows: Vec<PlanRow>, grid: Grid, width: usize, top: usize) -> Option<Part> {
    let rows: Vec<(String, Ink)> = (rows.into_iter())
        .flat_map(|r| match r.under {
            true => (kit::wrap(&r.text, width.saturating_sub(2)).into_iter())
                .map(|row| (format!("  {row}"), r.ink))
                .collect(),
            false => vec![(r.text, r.ink)],
        })
        .collect();
    (!rows.is_empty()).then(|| Part::Scroll {
        rows,
        most: plan_most(grid.rows),
        top,
    })
}

/// The pass's presses of Enter with no modifiers, taken out of its input in order, each saying
/// whether it came after the pass's first move of the keyboard (a Tab, or an arrow where
/// `arrows_move`: on a control whose arrows egui takes as a move, not a field's caret) — so no
/// choice, button or field the keyboard is on reads it as its own: the dialog gives each to what
/// has the keyboard then. Where egui cannot follow the moves the owner made before an Enter, that
/// Enter is taken and dropped, and the owner presses it again on what now shows the keyboard:
/// - after a move, in a pass with a Shift+Tab or an arrow that moves the keyboard — egui moves it
///   only on the next pass, or at the pass's end (round 1);
/// - after a second move — egui moves the keyboard once a pass, its last direction (review of
///   X4-find).
///
/// A Space there is dropped too, with its text, which a button or a choice the keyboard is
/// leaving would take (fix-check) — unless the pass began on a text field (`!on_text`), which
/// types it and whose Tab the kit already keeps any other control from pressing (review of X4-find).
fn take_enters(ctx: &egui::Context, arrows_move: bool, on_text: bool) -> Vec<bool> {
    let arrow = |key: &Key| {
        matches!(
            key,
            Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight
        )
    };
    // Whether a key moves the keyboard as egui reads it (`None`: not), and whether egui makes
    // that move only later (`true`: a Shift+Tab, an arrow) rather than in the pass (a Tab).
    let moves = |e: &egui::Event| match e {
        egui::Event::Key {
            key: Key::Tab,
            pressed: true,
            modifiers,
            ..
        } if !modifiers.any() || modifiers.shift_only() => Some(modifiers.shift),
        egui::Event::Key {
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
            (egui::Event::Text(text), true) if text == " " => false,
            (
                egui::Event::Key {
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
                egui::Event::Key {
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

// ---------------------------------------------------------------------------------------------
// The forms in cells (X4-dialogs: 03 §7.6–§7.7's and a restore's).

/// A control of a form in cells and its value this pass: copied from the form ([`controls`]),
/// given to the kit by its id ([`Control::input`]) and written back ([`apply`]).
#[derive(Debug)]
enum Control {
    /// The form's one-line field `n`.
    Field {
        label: &'static str,
        n: usize,
        text: String,
    },
    /// The form's message field `n` (Shift+Enter a new line).
    Message {
        label: &'static str,
        n: usize,
        text: String,
    },
    /// The form's row of choices `row`, one of several: the chosen one.
    One {
        label: &'static str,
        row: usize,
        words: Vec<String>,
        at: Option<usize>,
    },
    /// The form's row of choices `row`, any of several: each on or off.
    Any {
        row: usize,
        words: Vec<String>,
        on: Vec<bool>,
    },
    /// A line of the form (a restore entry's state).
    Note(String),
}

impl Control {
    /// Its part of the dialog.
    fn part(&self) -> Part {
        match self {
            Control::Field { label, n, .. } => Part::Field {
                label: label.to_string(),
                id: field_id(*n),
            },
            Control::Message { label, n, .. } => Part::Message {
                label: label.to_string(),
                id: field_id(*n),
                rows: MESSAGE_ROWS,
            },
            Control::One {
                label, row, words, ..
            } => Part::Choices {
                label: label.to_string(),
                id: row_id(*row),
                choices: words.clone(),
                many: false,
            },
            Control::Any { row, words, .. } => Part::Choices {
                label: String::new(),
                id: row_id(*row),
                choices: words.clone(),
                many: true,
            },
            Control::Note(text) => Part::Text(text.clone(), Ink::Text),
        }
    }

    /// Its state for the kit, by its part's id.
    fn input(&mut self) -> Option<(egui::Id, Input<'_>)> {
        match self {
            Control::Field { n, text, .. } | Control::Message { n, text, .. } => {
                Some((field_id(*n), Input::Text(text)))
            }
            Control::One { row, at, .. } => Some((row_id(*row), Input::One(at))),
            Control::Any { row, on, .. } => Some((row_id(*row), Input::Any(on))),
            Control::Note(_) => None,
        }
    }

    /// The number of the text field it is, if it is one.
    fn text_field(&self) -> Option<usize> {
        match self {
            Control::Field { n, .. } | Control::Message { n, .. } => Some(*n),
            _ => None,
        }
    }
}

fn words(all: &[&str]) -> Vec<String> {
    all.iter().map(|w| w.to_string()).collect()
}

/// The controls of `form`'s options in cells, with their values now (none for a form without
/// options, or one egui draws); `wide`: for an ambiguous-wide terminal ([`restore_controls`]).
fn controls(form: &Form, wide: bool) -> Vec<Control> {
    let one = |label, row, all: &[String], value: &str| Control::One {
        label,
        row,
        words: all.to_vec(),
        at: all.iter().position(|w| w == value),
    };
    let field = |label, n, text: &str| Control::Field {
        label,
        n,
        text: text.to_string(),
    };
    let any = |row, all: &[&str], on: &[bool]| Control::Any {
        row,
        words: words(all),
        on: on.to_vec(),
    };
    let scopes = |local: &str, scope: ConfigScope| Control::One {
        label: "Where",
        row: 0,
        words: words(&[local, "globally"]),
        at: Some(usize::from(scope == ConfigScope::Global)),
    };
    if !form.options {
        return Vec::new();
    }
    match &form.draft {
        Action::Commit {
            message,
            amend,
            no_verify,
        } => vec![
            Control::Message {
                label: "Message",
                n: 0,
                text: message.clone(),
            },
            any(
                0,
                &["Amend the last commit instead", "Skip hooks"],
                &[*amend, *no_verify],
            ),
        ],
        Action::Amend { message } => {
            let mut all = vec![Control::One {
                label: "Message",
                row: 0,
                words: words(&["keep its message", "edit it"]),
                at: Some(usize::from(message.is_some())),
            }];
            if let Some(m) = message {
                all.push(Control::Message {
                    label: "New message",
                    n: 0,
                    text: m.clone(),
                });
            }
            all
        }
        Action::Stash {
            message,
            include_untracked,
        } => vec![
            field("Message", 0, message),
            any(0, &["Include untracked files"], &[*include_untracked]),
        ],
        Action::ResetChanges { hard } => vec![Control::One {
            label: "Mode",
            row: 0,
            words: words(&["mixed (unstage all)", "hard (discard all tracked changes)"]),
            at: Some(usize::from(*hard)),
        }],
        Action::Clean { folders } => vec![any(0, &["Folders too (-d)"], &[*folders])],
        Action::Fetch { prune, prune_tags } => {
            vec![any(0, &["Prune", "Prune tags"], &[*prune, *prune_tags])]
        }
        Action::FetchRemote {
            remote,
            prune,
            prune_tags,
        } => vec![
            one("Remote", 0, &form.remotes, remote),
            any(1, &["Prune", "Prune tags"], &[*prune, *prune_tags]),
        ],
        Action::PruneRemote { remote } | Action::DeleteRemote { remote } => {
            vec![one("Remote", 0, &form.remotes, remote)]
        }
        Action::AddRemote {
            name,
            url,
            fetch_now,
            ..
        } => vec![
            field("Name", 0, name),
            field("Fetch URL", 1, url),
            field("Push URL (blank: the fetch URL)", 2, &form.text),
            any(0, &["Fetch now"], &[*fetch_now]),
        ],
        Action::EditRemote { was, now } => vec![
            one("Remote", 0, &form.remotes, &was.name),
            field("Name", 0, &now.name),
            field("Fetch URL", 1, &form.url),
            field("Push URL (blank: the fetch URL)", 2, &form.text),
        ],
        Action::User { name, email, scope } => vec![
            field("Name", 0, name),
            field("Email", 1, email),
            scopes("for this repository", *scope),
        ],
        Action::UserRemove { scope } => vec![scopes("this repository", *scope)],
        Action::Restore { entries, .. } => restore_controls(entries, wide),
        // The forms egui draws until X4-dialogs2 (`form_ui`).
        _ => Vec::new(),
    }
}

/// A restore's entries (03 §8.3): each a row of its own — `[X] <subject> → <value then>`, and for
/// a path in the way its `[ ] replace` — with `moved since (now …)` and the occupied path's rule
/// under it. The `→` is sopen's own, between a name and a value: `wide`, an ambiguous-wide
/// terminal gets §10's `>` there, at the source (T38), as the panel's `↔` in a label.
fn restore_controls(entries: &[RestoreEntry], wide: bool) -> Vec<Control> {
    if entries.is_empty() {
        return vec![Control::Note("nothing in this group to restore".into())];
    }
    let arrow = crate::gui::term::narrow("→")
        .filter(|_| wide)
        .unwrap_or("→");
    let mut out = Vec::new();
    for (k, e) in entries.iter().enumerate() {
        let (subject, value) = (subject_text(&e.subject), value_text(&e.before));
        let mut all = vec![format!("{subject} {arrow} {value}")];
        let mut on = vec![e.chosen];
        // In the way as S3-A's `entries` looked: a leaf of other bytes (`now` is its blob) or
        // anything that is not a file (`occupied`) — S3-C: not `Occupied` alone.
        let blocked = crate::git::backup::blocked(e);
        if blocked {
            all.push("replace".into());
            on.push(e.replace);
        }
        out.push(Control::Any {
            row: k,
            words: all,
            on,
        });
        // Under the entry's words, past its `[X] `.
        if e.moved_since {
            let now = value_text(&e.now);
            out.push(Control::Note(format!("    moved since (now {now})")));
        }
        if blocked {
            let rule = "    occupied: left as it is unless replace is chosen";
            out.push(Control::Note(rule.into()));
        }
    }
    out
}

/// `controls`, as the pass left them, written back into `form`'s draft (and what it keeps).
fn apply(form: &mut Form, controls: &[Control]) {
    let text = |n: usize| {
        controls.iter().find_map(|c| match c {
            Control::Field { n: k, text, .. } | Control::Message { n: k, text, .. } if *k == n => {
                Some(text.clone())
            }
            _ => None,
        })
    };
    let one = |row: usize| {
        controls.iter().find_map(|c| match c {
            Control::One { row: k, at, .. } if *k == row => *at,
            _ => None,
        })
    };
    let any = |row: usize, i: usize| {
        controls.iter().find_map(|c| match c {
            Control::Any { row: k, on, .. } if *k == row => on.get(i).copied(),
            _ => None,
        })
    };
    fn set<T>(to: &mut T, value: Option<T>) {
        if let Some(value) = value {
            *to = value;
        }
    }
    let scope = |row| one(row).map(|k| [ConfigScope::Local, ConfigScope::Global][k.min(1)]);
    let Form {
        draft,
        remotes,
        kept,
        text: push_text,
        known: urls,
        url: fetch_text,
        url_touched,
        push_touched,
        ..
    } = form;
    let remote = |row| one(row).and_then(|k| remotes.get(k)).cloned();
    match draft {
        Action::Commit {
            message,
            amend,
            no_verify,
        } => {
            set(message, text(0));
            set(amend, any(0, 0));
            set(no_verify, any(0, 1));
        }
        Action::Amend { message: m } => {
            if let (Some(m), Some(typed)) = (m.as_mut(), text(0)) {
                *m = typed;
            }
            // A message kept while "keep its message" is chosen, so "edit it" brings it back.
            let edit = one(0) == Some(1);
            if edit && m.is_none() {
                *m = Some(std::mem::take(kept));
            } else if !edit && m.is_some() {
                *kept = m.take().unwrap_or_default();
            }
        }
        Action::Stash {
            message,
            include_untracked,
        } => {
            set(message, text(0));
            set(include_untracked, any(0, 0));
        }
        Action::ResetChanges { hard } => set(hard, one(0).map(|k| k == 1)),
        Action::Clean { folders } => set(folders, any(0, 0)),
        Action::Fetch { prune, prune_tags } => {
            set(prune, any(0, 0));
            set(prune_tags, any(0, 1));
        }
        Action::FetchRemote {
            remote: r,
            prune,
            prune_tags,
        } => {
            set(r, remote(0));
            set(prune, any(1, 0));
            set(prune_tags, any(1, 1));
        }
        Action::PruneRemote { remote: r } | Action::DeleteRemote { remote: r } => set(r, remote(0)),
        Action::AddRemote {
            name,
            url,
            push_url,
            fetch_now,
        } => {
            set(name, text(0));
            set(url, text(1));
            set(push_text, text(2));
            *push_url = Some(push_text.trim().to_string()).filter(|u| !u.is_empty());
            set(fetch_now, any(0, 0));
        }
        Action::EditRemote { was, now } => {
            if let Some(chosen) = remote(0).filter(|r| *r != was.name) {
                // Another remote: its URLs as read, nothing touched (this pass's typing was the
                // other remote's).
                *was = known(urls, &chosen);
                *now = was.clone();
                *fetch_text = was.url.clone();
                *push_text = was.push_url.clone().unwrap_or_default();
                (*url_touched, *push_touched) = (false, false);
                return;
            }
            set(&mut now.name, text(0));
            if let Some(typed) = text(1).filter(|t| t != fetch_text) {
                (*fetch_text, *url_touched) = (typed, true);
            }
            if let Some(typed) = text(2).filter(|t| t != push_text) {
                (*push_text, *push_touched) = (typed, true);
            }
            // A field left alone is the URL as it is set: `now` keeps `was`'s (masked) value, so
            // the plan writes nothing for it (03 §3.7); a changed one is the URL as typed.
            now.url = match url_touched {
                true => fetch_text.trim().to_string(),
                false => was.url.clone(),
            };
            now.push_url = match push_touched {
                true => Some(push_text.trim().to_string()).filter(|u| !u.is_empty()),
                false => was.push_url.clone(),
            };
        }
        Action::User {
            name,
            email,
            scope: s,
        } => {
            set(name, text(0));
            set(email, text(1));
            set(s, scope(0));
        }
        Action::UserRemove { scope: s } => set(s, scope(0)),
        Action::Restore { entries, .. } => {
            for (k, e) in entries.iter_mut().enumerate() {
                set(&mut e.chosen, any(k, 0));
                if crate::git::backup::blocked(e) {
                    set(&mut e.replace, any(k, 1));
                }
            }
        }
        _ => {}
    }
}

/// The options of the form's draft in egui (03 §7.1–§7.5's, until X4-dialogs2), edited in place;
/// whether a text field was laid out.
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
        how: kept_how,
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
        // 03 §7.6–§7.7's forms and a restore's are cells (`controls`); the actions without
        // options have their confirmation alone.
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
