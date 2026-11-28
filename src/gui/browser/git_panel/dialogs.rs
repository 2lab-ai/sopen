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
//! - **In cells** (X4-dialogs, X4-dialogs2, 10 §17): every action's dialog is `ops::dialog`'s
//!   box on the panels' grid — the lost line, the options as the kit's fields, message fields and
//!   choices, the plan in rows that scroll (at most three fifths of the grid, eight at least, the
//!   kit giving them way first on a short grid), `[ Run ]` (off until the plan is in and nothing
//!   refuses it, and while the grid holds less than all of the box), `[ Cancel ]`, Rebase's
//!   `[ Plan the commits first… ]` and the hint — and [`ActionDialog::covers`] says its cells; so
//!   are the result ([`ResultDialog::covers`]) and "Stop git?" ([`stop_question`]). The keyboard
//!   opens on the form's first text field, or on nothing; Enter runs (03 §8.1), but each Enter of
//!   a pass goes to what has the keyboard at that point — before the pass's first Tab what had it
//!   then, after it what the Tab gave it to — so Enter on a focused button is that button's press,
//!   and one after a Shift+Tab or an arrow that moves the keyboard (egui moves it later) is
//!   dropped, a Space too; only what the screen showed runs: a Run or an Enter runs when what
//!   would run is what the last pass the host put on screen painted (`cells::OnScreen`) — the
//!   draft, its plan's rows, whether Run was on and Check out's "Don't ask again" — else it is
//!   dropped and [`CHANGED`] says so; after two moves in a pass every later Enter is dropped, a
//!   Space too unless the pass began on a text field. Space is the kit's, once a press in the
//!   pass's order; Esc, Cancel or a click outside cancels, and a pass that both cancels and runs
//!   cancels. "Stop git?" stops only what the screen showed it asking about, by the same rule.

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
use eframe::egui::{self, Key};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

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
    /// The draft's revision last planned, and its plan.
    planned: Option<(u64, Result<Plan, Refused>)>,
    /// A refresh came: the draft is planned again against the new [`Repo`].
    refreshed: bool,
    /// The draft's revision: it moves whenever a pass changes the draft or Check out's "Don't ask
    /// again in this window" (an edit, an option).
    drafted: u64,
    /// What the screen's painting is compared by ([`Painting`]): it moves with the draft's
    /// revision and whenever a plan's rows change (an option's, a refresh's, reads landing), so
    /// a pass compares and copies no draft.
    revision: u64,
    /// The form laid out a text field last pass: the typing goes there while nothing has it, and
    /// when it shows anew (Amend's message once "edit it" is chosen).
    typing: bool,
    /// The first of the plan's rows shown.
    plan_top: usize,
    /// A Run or an Enter came when what would run was not what the screen showed, and was not
    /// acted on — the hint says the plan changed and Enter runs it.
    changed: bool,
    /// What the screen shows of the dialog — the painting of the last pass that reached it — and
    /// the last pass's painting. Nothing until a pass has painted where it can be seen (egui lays
    /// a new modal out unseen the first time).
    screen: cells::OnScreen<Painting>,
    /// A pass has laid the box out (the next one paints where it can be seen).
    laid_once: bool,
}

/// What a pass of an action's dialog painted: the revision of what it showed — the draft, its
/// plan's rows and Check out's "Don't ask again in this window" (which a run carries out of the
/// dialog, so every later checkout in the window runs unasked) — and whether its Run was on. A Run
/// or an Enter runs only what the screen shows ([`cells::OnScreen`]), equal to what would run at
/// its pass's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Painting {
    revision: u64,
    runs: bool,
}

fn dialog_id() -> egui::Id {
    egui::Id::unique("sopen-git-action")
}

/// The id of a form's `n`th text field; the first has the typing.
pub(super) fn field_id(n: usize) -> egui::Id {
    dialog_id().with(("field", n))
}

/// The box's id (its modal), and its buttons'.
fn cells_id() -> egui::Id {
    dialog_id().with("cells")
}

pub(super) fn run_id() -> egui::Id {
    cells_id().with("run")
}

pub(super) fn cancel_id() -> egui::Id {
    cells_id().with("cancel")
}

pub(super) fn plan_first_id() -> egui::Id {
    cells_id().with("plan-first")
}

/// The id of a form's `k`th row of choices.
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

/// An egui dialog's width (the log's entry's and the planner's, until their units): `cells`
/// cells, within the window.
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
            planned: Some((0, planned)),
            refreshed: false,
            drafted: 0,
            revision: 0,
            form,
            repo: repo.clone(),
            typing: false,
            plan_top: 0,
            changed: false,
            screen: cells::OnScreen::default(),
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
        self.refreshed = true;
    }

    /// The draft planned again when it changed since the last plan, or a refresh came; whether it
    /// was. A plan that shows otherwise than the last — its title, its lost line or its rows —
    /// moves the revision the screen is compared by.
    fn replan(&mut self, planner: Planner) -> bool {
        if self.form.refusal().is_some() {
            return false;
        }
        let stale =
            self.refreshed || (self.planned.as_ref()).is_none_or(|(at, _)| *at != self.drafted);
        if stale {
            let shown = self.plan_shown();
            let plan = planner(&self.form.draft, &self.repo);
            (self.planned, self.refreshed) = (Some((self.drafted, plan)), false);
            if self.plan_shown() != shown {
                self.revision += 1;
            }
        }
        stale
    }

    /// What the box shows of the plan: its title, its lost line and its rows.
    fn plan_shown(&self) -> (String, Option<String>, Vec<PlanRow>) {
        let lost = self.confirmation().and_then(|c| c.loses.clone());
        (self.title(), lost, self.plan_lines())
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
    /// and the other side to blank (10 §17, X4-graph's seam), as the dialog stands now.
    pub fn covers(&self, grid: &Grid) -> Vec<Cells> {
        // A restore's arrow is one cell either way: the box stands where the pass lays it.
        let controls = controls(&self.form, false);
        self.layout(*grid, &controls).0.covers()
    }

    /// Rebase's "Plan the commits first…" button, which its box draws (tests).
    #[cfg(test)]
    pub fn plan_first_button(&self) -> Option<egui::Id> {
        matches!(self.form.draft, Action::Rebase { .. }).then(plan_first_id)
    }

    /// The draft's plan is in and neither the dialog nor the plan refused it.
    fn ready(&self) -> bool {
        self.form.refusal().is_none()
            && !self.refreshed
            && matches!(&self.planned, Some((at, Ok(_))) if *at == self.drafted)
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

    // -----------------------------------------------------------------------------------------
    // In cells.

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
        // 03 §7.1: Rebase offers the planner, which names the merges it would flatten.
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
    fn painted(&self, fits: bool) -> Painting {
        Painting {
            revision: self.revision,
            runs: fits && self.ready(),
        }
    }

    /// One pass on `grid` (the panels'; see the module's notes): Esc and Enter are taken before
    /// its controls see them (Shift+Enter is a message field's new line); `planner` plans the
    /// draft again whenever an option changed it.
    pub fn ui(&mut self, ctx: &egui::Context, grid: Grid, planner: Planner) -> Option<DialogEnd> {
        if self.replan(planner) {
            ctx.request_repaint();
        }
        // A pass whose input may change a control: the draft as it began, to tell whether it did
        // (a pass without such input copies nothing and changes nothing).
        let before = may_change(ctx).then(|| {
            copying(&self.form.draft);
            (self.form.draft.clone(), self.form.dont_ask)
        });
        let escape = ctx.input_mut(|i| keys::take(i, Key::Escape, Mods::None));
        let began = ctx.memory(|m| m.focused());
        let mut controls = controls(&self.form, cells::ambiguous_wide(ctx));
        // egui moves the keyboard by an arrow unless a field has it (its caret's).
        let in_field = (controls.iter())
            .filter_map(Control::text_field)
            .any(|n| began == Some(field_id(n)));
        let enters = take_enters(ctx, began.is_some() && !in_field, in_field);
        let (layout, fits) = self.layout(grid, &controls);
        // What the owner saw — the painting of the last pass that reached the screen (the
        // terminal prints none while input waits) — and what this pass paints.
        let seen = self.screen.start(ctx).cloned();
        if std::mem::replace(&mut self.laid_once, true) {
            let painting = self.painted(fits);
            self.screen.painted(painting);
        } else {
            fresh_fields(ctx, &controls, &mut self.form);
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
        // The fields edit the form's own texts in place.
        let texts = texts(&mut self.form).into_iter();
        by_id.extend(texts.map(|(n, text)| (field_id(n), Input::Text(text))));
        // The buttons take the kit's presses, in the pass's order (F-kit-tab).
        by_id.extend([run, cancel, plan_first].map(|b| (b, Input::Button)));
        let inputs = Inputs {
            top: Some(&mut self.plan_top),
            by_id,
            ..Inputs::default()
        };
        let pass = kit::show(ctx, cells_id(), &layout, inputs);
        let now = ctx.memory(|m| m.focused());
        if let Some((draft, dont_ask)) = before {
            apply(&mut self.form, &controls);
            copying(&draft);
            // A choice swapped a field's text for another's (Move tag's tag, Edit a remote's
            // remote): its fields start afresh, nothing of the text before to undo (round 1).
            if swapped(&draft, &self.form.draft) {
                let now = self::controls(&self.form, cells::ambiguous_wide(ctx));
                fresh_fields(ctx, &now, &mut self.form);
            }
            if draft != self.form.draft || dont_ask != self.form.dont_ask {
                self.drafted += 1;
                self.revision += 1;
            }
        }
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

#[cfg(test)]
thread_local! {
    /// The bytes of drafts' texts the dialogs' passes copied or compared on this thread.
    static COPIED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The bytes of drafts' texts copied or compared since the last call (a test's bound on a pass).
#[cfg(test)]
pub(super) fn copied() -> usize {
    COPIED.with(|c| c.replace(0))
}

/// Counts a copy or a comparison of `draft`'s texts — its message — ([`copied`], in the tests).
#[cfg(test)]
fn copying(draft: &Action) {
    let bytes = match draft {
        Action::Commit { message, .. } | Action::Stash { message, .. } => message.len(),
        Action::Amend { message }
        | Action::Tag { message, .. }
        | Action::MoveTag { message, .. } => message.as_ref().map_or(0, String::len),
        _ => 0,
    };
    COPIED.with(|c| c.set(c.get() + bytes));
}

#[cfg(not(test))]
#[inline(always)]
fn copying(_: &Action) {}

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
pub(super) fn take_enters(ctx: &egui::Context, arrows_move: bool, on_text: bool) -> Vec<bool> {
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
// The forms in cells (X4-dialogs: 03 §7.6–§7.7's and a restore's; X4-dialogs2: §7.1–§7.5's).

/// Whether the pass's input may change a control of a dialog: anything but the pointer's
/// moves, the wheel (it scrolls), the window's focus, zoom and rotation, a change of modifiers, a
/// copy and a key's release.
fn may_change(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        (i.events.iter()).any(|e| {
            !matches!(
                e,
                egui::Event::PointerMoved(_)
                    | egui::Event::MouseMoved(_)
                    | egui::Event::PointerGone
                    | egui::Event::MouseWheel { .. }
                    | egui::Event::WindowFocused(_)
                    | egui::Event::Zoom(_)
                    | egui::Event::Rotate(_)
                    | egui::Event::ModifiersChanged(_)
                    | egui::Event::Copy
                    | egui::Event::Key { pressed: false, .. }
            )
        })
    })
}

/// The form's texts its fields edit in place, by field number: the draft's own (a message, a name,
/// a URL) and the form's (`archive`'s path, a remote's push URL, `edit-remote`'s fetch URL) —
/// exactly the fields [`controls`] shows.
fn texts(form: &mut Form) -> Vec<(usize, &mut String)> {
    let Form {
        draft, text, url, ..
    } = form;
    match draft {
        Action::Commit { message, .. } | Action::Stash { message, .. } => vec![(0, message)],
        Action::Amend { message: Some(m) }
        | Action::MoveTag {
            message: Some(m), ..
        } => {
            vec![(0, m)]
        }
        Action::AddRemote { name, url: u, .. } => vec![(0, name), (1, u), (2, text)],
        Action::EditRemote { now, .. } => vec![(0, &mut now.name), (1, url), (2, text)],
        Action::User { name, email, .. } => vec![(0, name), (1, email)],
        Action::Tag { name, message, .. } => {
            let mut all = vec![(0, name)];
            all.extend(message.as_mut().map(|m| (1, m)));
            all
        }
        Action::Branch { name, .. }
        | Action::CheckoutRemote { name, .. }
        | Action::StashBranch { name, .. } => vec![(0, name)],
        Action::Rename { to, .. } => vec![(0, to)],
        Action::Archive { .. } => vec![(0, text)],
        _ => Vec::new(),
    }
}

/// Whether a pass's choice swapped the texts of `before`'s fields for others': Move tag's tag (its
/// message), Edit a remote's remote (its name and URLs).
fn swapped(before: &Action, now: &Action) -> bool {
    match (before, now) {
        (Action::MoveTag { tag: a, .. }, Action::MoveTag { tag: b, .. }) => a != b,
        (Action::EditRemote { was: a, .. }, Action::EditRemote { was: b, .. }) => a.name != b.name,
        _ => false,
    }
}

/// The dialog has just opened, or a choice swapped its fields' texts ([`swapped`]): each of its
/// fields starts afresh — caret, selection and undo — whatever an earlier dialog, or the text
/// before, left under the same ids (the git dialogs reuse their fields' ids across actions): a
/// message with its caret at its start (the kit's rule for a message field), a one-line field
/// with its caret at its end, as egui puts a new one's (found by X4-quick).
fn fresh_fields(ctx: &egui::Context, controls: &[Control], form: &mut Form) {
    use egui::text::{CCursor, CCursorRange};
    use egui::text_edit::TextEditState;
    let texts = texts(form);
    for control in controls {
        let mut state = TextEditState::default();
        match control {
            Control::Message { n, .. } => state.store(ctx, field_id(*n)),
            Control::Field { n, .. } => {
                let text = (texts.iter())
                    .find(|(k, _)| k == n)
                    .map_or("", |(_, t)| t.as_str());
                let end = CCursor::new(text.chars().count());
                state.cursor.set_char_range(Some(CCursorRange::one(end)));
                state.store(ctx, field_id(*n));
            }
            _ => {}
        }
    }
}

/// A control of a form in cells and its value this pass: copied from the form ([`controls`]),
/// given to the kit by its id ([`Control::input`]) and written back ([`apply`]) — a field's text
/// is the form's own, which the field edits in place ([`texts`]).
#[derive(Debug)]
enum Control {
    /// The form's one-line field `n`.
    Field { label: &'static str, n: usize },
    /// The form's message field `n` (Shift+Enter a new line).
    Message { label: &'static str, n: usize },
    /// The form's row of choices `row`, one of several: the chosen one.
    One {
        label: &'static str,
        row: usize,
        words: Vec<String>,
        at: Option<usize>,
    },
    /// The form's row of choices `row`, any of several, after `label` (empty: none): each on or
    /// off.
    Any {
        label: &'static str,
        row: usize,
        words: Vec<String>,
        on: Vec<bool>,
    },
    /// A line of the form (a restore entry's state, how many commits a pick takes).
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
            Control::Any {
                label, row, words, ..
            } => Part::Choices {
                label: label.to_string(),
                id: row_id(*row),
                choices: words.clone(),
                many: true,
            },
            Control::Note(text) => Part::Text(text.clone(), Ink::Text),
        }
    }

    /// Its state for the kit, by its part's id (a field's is the form's text: [`texts`]).
    fn input(&mut self) -> Option<(egui::Id, Input<'_>)> {
        match self {
            Control::One { row, at, .. } => Some((row_id(*row), Input::One(at))),
            Control::Any { row, on, .. } => Some((row_id(*row), Input::Any(on))),
            Control::Field { .. } | Control::Message { .. } | Control::Note(_) => None,
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

/// Merge's fast-forward, its "on conflicts prefer", Reset's mode and Archive's format, in the order
/// their choices show them.
const FAST_FORWARDS: [FastForward; 3] = [
    FastForward::WhenPossible,
    FastForward::Never,
    FastForward::Only,
];
const PREFERS: [Prefer; 3] = [Prefer::Nothing, Prefer::Ours, Prefer::Theirs];
const MODES: [ResetMode; 4] = [
    ResetMode::Soft,
    ResetMode::Mixed,
    ResetMode::Keep,
    ResetMode::Hard,
];
const FORMATS: [ArchiveFormat; 2] = [ArchiveFormat::Zip, ArchiveFormat::Tar];

/// Where `value` is among `all`.
fn index<T: PartialEq>(all: &[T], value: &T) -> Option<usize> {
    all.iter().position(|v| v == value)
}

/// The controls of `form`'s options in cells, with their values now (none for a form without
/// options); `wide`: for an ambiguous-wide terminal ([`restore_controls`]).
fn controls(form: &Form, wide: bool) -> Vec<Control> {
    let one = |label, row, all: &[String], value: &str| Control::One {
        label,
        row,
        words: all.to_vec(),
        at: all.iter().position(|w| w == value),
    };
    let field = |label, n| Control::Field { label, n };
    let any = |row, all: &[&str], on: &[bool]| Control::Any {
        label: "",
        row,
        words: words(all),
        on: on.to_vec(),
    };
    let pick = |label, row, all: &[&str], at: Option<usize>| Control::One {
        label,
        row,
        words: words(all),
        at,
    };
    // None (`none`, first) or one of `names`.
    let optional = |label, row, none: &str, names: &[String], value: Option<&str>| Control::One {
        label,
        row,
        words: (std::iter::once(none.to_string()))
            .chain(names.iter().cloned())
            .collect(),
        at: match value {
            None => Some(0),
            Some(v) => names.iter().position(|n| n == v).map(|k| k + 1),
        },
    };
    // Any of `names`, each in `chosen` on.
    let several = |label, row, names: &[String], chosen: &[String]| Control::Any {
        label,
        row,
        words: names.to_vec(),
        on: names.iter().map(|n| chosen.contains(n)).collect(),
    };
    // A merge's parent (`-m <n>`, 1…n) when a commit is a merge.
    let parent = |mainline: Option<u32>| {
        (form.parents > 1).then(|| Control::One {
            label: "Parent",
            row: 0,
            words: (1..=form.parents).map(|n| n.to_string()).collect(),
            at: mainline.and_then(|n| (n as usize).checked_sub(1)),
        })
    };
    // `pull`'s how (03 §7.3) and, with merge, its two options.
    let how = |how: PullHow| {
        let kind = match how {
            PullHow::Merge { .. } => 0,
            PullHow::Rebase => 1,
            PullHow::FastForwardOnly => 2,
        };
        let all = ["merge", "rebase", "fast-forward only"];
        let mut out = vec![pick("How", 0, &all, Some(kind))];
        if let PullHow::Merge { no_ff, squash } = how {
            out.push(any(1, &["Never fast-forward", "Squash"], &[no_ff, squash]));
        }
        out
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
            amend, no_verify, ..
        } => vec![
            Control::Message {
                label: "Message",
                n: 0,
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
            if message.is_some() {
                all.push(Control::Message {
                    label: "New message",
                    n: 0,
                });
            }
            all
        }
        Action::Stash {
            include_untracked, ..
        } => vec![
            field("Message", 0),
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
        Action::AddRemote { fetch_now, .. } => vec![
            field("Name", 0),
            field("Fetch URL", 1),
            field("Push URL (blank: the fetch URL)", 2),
            any(0, &["Fetch now"], &[*fetch_now]),
        ],
        Action::EditRemote { was, .. } => vec![
            one("Remote", 0, &form.remotes, &was.name),
            field("Name", 0),
            field("Fetch URL", 1),
            field("Push URL (blank: the fetch URL)", 2),
        ],
        Action::User { scope, .. } => vec![
            field("Name", 0),
            field("Email", 1),
            scopes("for this repository", *scope),
        ],
        Action::UserRemove { scope } => vec![scopes("this repository", *scope)],
        Action::Restore { entries, .. } => restore_controls(entries, wide),
        // 03 §7.1–§7.5 (X4-dialogs2).
        Action::Tag { message, push, .. } => {
            let kind = Some(usize::from(message.is_none()));
            let mut all = vec![
                field("Name", 0),
                pick("Kind", 0, &["annotated", "lightweight"], kind),
            ];
            if message.is_some() {
                all.push(Control::Message {
                    label: "Message",
                    n: 1,
                });
            }
            let push = push.as_deref();
            all.push(optional("Push to", 1, "no remote", &form.remotes, push));
            all
        }
        Action::Branch { checkout, .. } => {
            vec![field("Name", 0), any(0, &["Check it out"], &[*checkout])]
        }
        Action::MoveBranch { branch, .. } => vec![one("Branch", 0, &form.names, branch)],
        Action::Checkout { .. } => {
            vec![any(
                0,
                &["Don't ask again in this window"],
                &[form.dont_ask],
            )]
        }
        Action::Pick {
            commits,
            mainline,
            record_origin,
            no_commit,
        } => {
            let mut all = Vec::new();
            if commits.len() > 1 {
                let n = commits.len();
                all.push(Control::Note(format!("{n} commits, oldest first")));
            }
            all.extend(parent(*mainline));
            let on = [*record_origin, *no_commit];
            all.push(any(1, &["Record origin (-x)", "Don't commit"], &on));
            all
        }
        Action::Revert { commits, mainline } => {
            let mut all = Vec::new();
            if commits.len() > 1 {
                let n = commits.len();
                all.push(Control::Note(format!("{n} commits, newest first")));
            }
            all.extend(parent(*mainline));
            all
        }
        Action::Merge { options: o, .. } => {
            let ff = index(&FAST_FORWARDS, &o.fast_forward);
            let prefer = index(&PREFERS, &o.prefer);
            let all = [
                "Squash",
                "Don't commit",
                "Allow unrelated histories",
                "Skip hooks",
            ];
            let on = [o.squash, o.no_commit, o.allow_unrelated, o.no_verify];
            vec![
                pick("Fast-forward", 0, &["when possible", "never", "only"], ff),
                pick(
                    "On conflicts prefer",
                    1,
                    &["nothing", "ours", "theirs"],
                    prefer,
                ),
                any(2, &all, &on),
            ]
        }
        Action::Rebase { options: o, .. } => {
            let mut all = vec!["Ignore dates", "Stash my changes for it"];
            let mut on = vec![o.ignore_date, o.autostash];
            if form.update_refs {
                all.push("Move other branches along");
                on.push(o.update_refs);
            }
            vec![any(0, &all, &on)]
        }
        Action::Reset { mode, .. } => {
            let all = ["soft", "mixed", "keep", "hard"];
            vec![pick("Mode", 0, &all, index(&MODES, mode))]
        }
        Action::Archive { format, .. } => vec![
            pick("Format", 0, &["zip", "tar"], index(&FORMATS, format)),
            field("Save to", 0),
        ],
        Action::Rename { .. } => vec![field("New name", 0)],
        Action::DeleteBranch { force, remotes, .. } => {
            let mut all = vec![any(0, &["Force"], &[*force])];
            if !form.remotes.is_empty() {
                all.push(several("Also delete it on", 1, &form.remotes, remotes));
            }
            all
        }
        Action::Push {
            remotes,
            options,
            overwrite,
            ..
        } => {
            let all = [
                "Set upstream",
                "Overwrite the remote branch, with lease",
                "Skip hooks",
            ];
            let on = [options.set_upstream, *overwrite, options.no_verify];
            vec![
                several("Remotes", 0, &form.remotes, remotes),
                any(1, &all, &on),
            ]
        }
        Action::CheckoutRemote {
            remote,
            branch,
            name,
            pull,
        } => {
            let mut all = vec![field("Local name", 0)];
            if form.locals.contains(name) {
                let taken =
                    format!("{name} exists: check it out and pull {remote}/{branch} into it");
                all.push(Control::Note(taken));
                all.extend(how(pull.unwrap_or(form.how)));
            }
            all
        }
        Action::FetchInto { force, .. } => {
            vec![any(0, &["Force, when it is not a fast-forward"], &[*force])]
        }
        Action::Pull { how: h, .. } => how(*h),
        Action::DeleteTag { remote, .. } => {
            let remote = remote.as_deref();
            vec![optional("Also on", 0, "no remote", &form.remotes, remote)]
        }
        Action::PushTag {
            remotes, replace, ..
        } => {
            let all = ["Replace the remote's tag when it differs, with lease"];
            vec![
                several("Remotes", 0, &form.remotes, remotes),
                any(1, &all, &[*replace]),
            ]
        }
        Action::MoveTag { tag, message, .. } => {
            let on = [message.is_some()];
            let mut all = vec![
                one("Tag", 0, &form.names, tag),
                any(1, &["Keep it annotated with its message"], &on),
            ];
            if message.is_some() {
                all.push(Control::Message {
                    label: "Message",
                    n: 0,
                });
            }
            all
        }
        Action::StashApply { index: i, .. } | Action::StashPop { index: i, .. } => {
            vec![any(0, &["Restore the index too (--index)"], &[*i])]
        }
        Action::StashBranch { .. } => vec![field("Name", 0)],
        // The actions without options have their confirmation alone.
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
            label: "",
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

/// `controls`, as the pass left them, written back into `form`'s draft (and what it keeps); its
/// texts the fields edited in place ([`texts`]).
fn apply(form: &mut Form, controls: &[Control]) {
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
    // `pull`'s how from its row of three (`kind`) and, while it was merge, its two options.
    fn pull_how(how: &mut PullHow, kind: Option<usize>, no_ff: Option<bool>, squash: Option<bool>) {
        let was_merge = matches!(how, PullHow::Merge { .. });
        match kind {
            Some(0) if !was_merge => *how = PullHow::default(),
            Some(1) => *how = PullHow::Rebase,
            Some(2) => *how = PullHow::FastForwardOnly,
            _ => {}
        }
        if let (
            true,
            PullHow::Merge {
                no_ff: n,
                squash: s,
            },
        ) = (was_merge, how)
        {
            set(n, no_ff);
            set(s, squash);
        }
    }
    let scope = |row| one(row).map(|k| [ConfigScope::Local, ConfigScope::Global][k.min(1)]);
    let Form {
        draft,
        remotes,
        names,
        locals,
        tags,
        update_refs,
        kept,
        text: field_text,
        dont_ask,
        known: urls,
        how: kept_how,
        url: fetch_text,
        url_touched,
        push_touched,
        ..
    } = form;
    let remote = |row| one(row).and_then(|k| remotes.get(k)).cloned();
    let name = |row| one(row).and_then(|k| names.get(k)).cloned();
    // None (the first choice) or one of the remotes.
    let optional = |row| one(row).map(|k| k.checked_sub(1).and_then(|k| remotes.get(k)).cloned());
    // Any of the remotes, in their order; those of a row not shown as `chosen` has them.
    let several = |row, chosen: &[String]| -> Vec<String> {
        (remotes.iter().enumerate())
            .filter(|&(k, r)| any(row, k).unwrap_or(chosen.contains(r)))
            .map(|(_, r)| r.clone())
            .collect()
    };
    let parent = |mainline: &mut Option<u32>| {
        if let Some(k) = one(0) {
            *mainline = u32::try_from(k + 1).ok();
        }
    };
    match draft {
        Action::Commit {
            amend, no_verify, ..
        } => {
            set(amend, any(0, 0));
            set(no_verify, any(0, 1));
        }
        Action::Amend { message: m } => {
            // A message kept while "keep its message" is chosen, so "edit it" brings it back.
            let edit = one(0) == Some(1);
            if edit && m.is_none() {
                *m = Some(std::mem::take(kept));
            } else if !edit && m.is_some() {
                *kept = m.take().unwrap_or_default();
            }
        }
        Action::Stash {
            include_untracked, ..
        } => set(include_untracked, any(0, 0)),
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
            push_url,
            fetch_now,
            ..
        } => {
            *push_url = Some(field_text.trim().to_string()).filter(|u| !u.is_empty());
            set(fetch_now, any(0, 0));
        }
        Action::EditRemote { was, now } => {
            if let Some(chosen) = remote(0).filter(|r| *r != was.name) {
                // Another remote: its URLs as read, nothing touched (this pass's typing was the
                // other remote's).
                *was = known(urls, &chosen);
                *now = was.clone();
                *fetch_text = was.url.clone();
                *field_text = was.push_url.clone().unwrap_or_default();
                (*url_touched, *push_touched) = (false, false);
                return;
            }
            // The fields edited the name and the URLs in place: a URL field the owner changed from
            // the URL as it is set is touched from then on.
            *url_touched |= *fetch_text != was.url;
            *push_touched |= field_text.as_str() != was.push_url.as_deref().unwrap_or("");
            // A field left alone is the URL as it is set: `now` keeps `was`'s (masked) value, so
            // the plan writes nothing for it (03 §3.7); a changed one is the URL as typed.
            now.url = match url_touched {
                true => fetch_text.trim().to_string(),
                false => was.url.clone(),
            };
            now.push_url = match push_touched {
                true => Some(field_text.trim().to_string()).filter(|u| !u.is_empty()),
                false => was.push_url.clone(),
            };
        }
        Action::User { scope: s, .. } => set(s, scope(0)),
        Action::UserRemove { scope: s } => set(s, scope(0)),
        Action::Restore { entries, .. } => {
            for (k, e) in entries.iter_mut().enumerate() {
                set(&mut e.chosen, any(k, 0));
                if crate::git::backup::blocked(e) {
                    set(&mut e.replace, any(k, 1));
                }
            }
        }
        // 03 §7.1–§7.5 (X4-dialogs2): the fields edited their texts in place; this pass's choices
        // first, then what they make.
        Action::Tag {
            message: m, push, ..
        } => {
            // A message kept while "lightweight" is chosen, so "annotated" brings it back.
            match one(0) {
                Some(0) if m.is_none() => *m = Some(std::mem::take(kept)),
                Some(1) if m.is_some() => *kept = m.take().unwrap_or_default(),
                _ => {}
            }
            set(push, optional(1));
        }
        Action::Branch { checkout, .. } => set(checkout, any(0, 0)),
        Action::MoveBranch { branch, .. } => set(branch, name(0)),
        Action::Checkout { .. } => set(dont_ask, any(0, 0)),
        Action::Pick {
            mainline,
            record_origin,
            no_commit,
            ..
        } => {
            parent(mainline);
            set(record_origin, any(1, 0));
            set(no_commit, any(1, 1));
        }
        Action::Revert { mainline, .. } => parent(mainline),
        Action::Merge { options: o, .. } => {
            set(
                &mut o.fast_forward,
                one(0).and_then(|k| FAST_FORWARDS.get(k)).copied(),
            );
            set(&mut o.prefer, one(1).and_then(|k| PREFERS.get(k)).copied());
            set(&mut o.squash, any(2, 0));
            set(&mut o.no_commit, any(2, 1));
            set(&mut o.allow_unrelated, any(2, 2));
            set(&mut o.no_verify, any(2, 3));
        }
        Action::Rebase { options: o, .. } => {
            set(&mut o.ignore_date, any(0, 0));
            set(&mut o.autostash, any(0, 1));
            if *update_refs {
                set(&mut o.update_refs, any(0, 2));
            }
        }
        Action::Reset { mode, .. } => set(mode, one(0).and_then(|k| MODES.get(k)).copied()),
        Action::Archive { format, to, .. } => {
            // Another format: the path's extension follows it.
            let chosen = one(0).and_then(|k| FORMATS.get(k)).copied();
            if let Some(chosen) = chosen.filter(|f| *f != *format) {
                let old = format!(".{}", extension(*format));
                if let Some(stem) = field_text.strip_suffix(&old) {
                    *field_text = format!("{stem}.{}", extension(chosen));
                }
                *format = chosen;
            }
            *to = PathBuf::from(field_text.trim());
        }
        Action::DeleteBranch {
            force,
            remotes: chosen,
            ..
        } => {
            set(force, any(0, 0));
            *chosen = several(1, chosen);
        }
        Action::Push {
            remotes: chosen,
            options,
            overwrite,
            ..
        } => {
            *chosen = several(0, chosen);
            set(&mut options.set_upstream, any(1, 0));
            set(overwrite, any(1, 1));
            set(&mut options.no_verify, any(1, 2));
        }
        Action::CheckoutRemote { name: n, pull, .. } => {
            if locals.contains(n) {
                // The pull into the branch of that name, its how kept while the name is not.
                let how = pull.get_or_insert(*kept_how);
                pull_how(how, one(0), any(1, 0), any(1, 1));
                *kept_how = *how;
            } else {
                *pull = None;
            }
        }
        Action::FetchInto { force, .. } => set(force, any(0, 0)),
        Action::Pull { how, .. } => pull_how(how, one(0), any(1, 0), any(1, 1)),
        Action::DeleteTag { remote: r, .. } => set(r, optional(0)),
        Action::PushTag {
            remotes: chosen,
            replace,
            ..
        } => {
            *chosen = several(0, chosen);
            set(replace, any(1, 0));
        }
        Action::MoveTag {
            tag, message: m, ..
        } => {
            // Each tag keeps its own message (03 §7.4): this pass's typing is the message of the
            // tag shown as it began, kept as that tag's before another is chosen.
            if let Some(m) = m.as_ref() {
                let known = (tags.edited.get(tag)).or(tags.read.get(tag));
                if known.map_or("", String::as_str) != m {
                    tags.edited.insert(tag.clone(), m.clone());
                }
            }
            let shown_on = m.is_some();
            let turned = any(1, 0).is_some_and(|on| on != shown_on);
            if let Some(chosen) = name(0).filter(|t| *t != *tag) {
                *m = tags.annotated_message(&chosen);
                *tag = chosen;
            }
            // "Keep it annotated" turned this pass: the chosen tag's state turned.
            if turned {
                *m = match m {
                    Some(_) => None,
                    None => Some(tags.message_of(tag)),
                };
            }
        }
        Action::StashApply { index: i, .. } | Action::StashPop { index: i, .. } => {
            set(i, any(0, 0))
        }
        _ => {}
    }
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
///
/// In cells (X4-dialogs2): the box titled with the action holds, in rows that scroll, each step —
/// its word in a column of its own, two blanks before its line (the line wrapped under itself),
/// a failed step's row in the error colour — then, a blank row apart, the failed command's text
/// whole, a row a line, its tabs at every eighth cell and its control characters escaped; under
/// them the command line in the weak colour, the warning in the error colour or the backup line,
/// `[ OK ]` and the hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultDialog {
    /// The action.
    title: String,
    /// Each step's outcome and line, in order.
    steps: Vec<(StepOutcome, String)>,
    /// The failed command's text — git's stderr, else stdout, else its ending — whole, a line
    /// each as shown ([`shown_line`]), and its command line as the log keeps it.
    error: Option<(Vec<String>, Option<String>)>,
    /// The group written before the first step, whatever came after.
    backup: Option<String>,
    /// The run's warning (`Outcome::warning`: `backup <stamp> incomplete: …`), said in the
    /// error colour in place of the backup's undo line.
    warning: Option<String>,
    /// The first of its rows that scroll shown.
    top: usize,
    /// Its rows that scroll as last wrapped, and the width they were wrapped to: `covers` and
    /// each pass wrap nothing again until the width changes (X4-dialogs2, round 1).
    wrapped: std::cell::RefCell<Option<(usize, Wrapped)>>,
}

/// A result's rows that scroll, wrapped to a width: each its text and its ink.
type Wrapped = Arc<Vec<(String, Ink)>>;

fn result_id() -> egui::Id {
    egui::Id::unique("sopen-git-result")
}

fn ok_id() -> egui::Id {
    result_id().with("ok")
}

/// The result's nominal width in cells, egui's.
const RESULT_NOMINAL: usize = 80;

/// The cells a step's word takes, two blanks after the longest (`not run`).
const STEP_WORD: usize = 9;

/// What the result says Enter and Esc do.
const RESULT_HINT: &str = "Enter or Esc closes";

/// A line of a command's text as a row of cells shows it: tabs to the next multiple of eight
/// cells, control characters escaped (`\u{7}`), as the details pane shows a message.
fn shown_line(line: &str) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let mut out = String::with_capacity(line.len());
    let mut column = 0;
    for g in line.graphemes(true) {
        if g == "\t" {
            let n = 8 - column % 8;
            out.extend(std::iter::repeat_n(' ', n));
            column += n;
        } else if g.chars().any(char::is_control) {
            let at = out.len();
            out.extend(g.chars().flat_map(char::escape_default));
            column += out.len() - at;
        } else {
            out.push_str(g);
            column += cells::width(g);
        }
    }
    out
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
                (Self::lines(&mask_credentials(&d.error_text())), Some(line))
            }),
            backup: outcome.backup.clone(),
            warning,
            top: 0,
            wrapped: Default::default(),
        })
    }

    /// A failure with no plan behind it (a read the integration made: the Backups list's, a
    /// restore's): its text and, when a command ran, its line.
    pub fn error(title: &str, text: &str, command_line: Option<&str>) -> ResultDialog {
        ResultDialog {
            title: title.to_string(),
            steps: Vec::new(),
            error: Some((
                Self::lines(&mask_credentials(text)),
                command_line.map(mask_credentials),
            )),
            backup: None,
            warning: None,
            top: 0,
            wrapped: Default::default(),
        }
    }

    /// `text`'s lines as rows show them ([`shown_line`]), its blank lines at the end left out.
    fn lines(text: &str) -> Vec<String> {
        text.trim_end().split('\n').map(shown_line).collect()
    }

    /// The rows that scroll in a box `width` cells inside: the ones kept for that width, else
    /// wrapped and kept.
    fn rows(&self, width: usize) -> Wrapped {
        if let Some((at, rows)) = &*self.wrapped.borrow() {
            if *at == width {
                return rows.clone();
            }
        }
        let rows = Arc::new(self.wrap(width));
        *self.wrapped.borrow_mut() = Some((width, rows.clone()));
        rows
    }

    /// The rows that scroll, wrapped to `width` (see the type's notes).
    fn wrap(&self, width: usize) -> Vec<(String, Ink)> {
        let mut rows = Vec::new();
        for (outcome, line) in &self.steps {
            let ink = match outcome {
                StepOutcome::Failed => Ink::Error,
                _ => Ink::Text,
            };
            let word = outcome_word(*outcome);
            let room = width.saturating_sub(STEP_WORD);
            let wrapped = super::details::wrap_cells(&shown_line(line), room);
            for (k, part) in wrapped.into_iter().enumerate() {
                let lead = match k {
                    0 => format!("{word:<width$}", width = STEP_WORD),
                    _ => " ".repeat(STEP_WORD),
                };
                rows.push((format!("{lead}{}", part.trim_end()), ink));
            }
        }
        if let Some((lines, _)) = &self.error {
            if !rows.is_empty() {
                rows.push((String::new(), Ink::Text));
            }
            for line in lines {
                let wrapped = super::details::wrap_cells(line, width);
                rows.extend(
                    wrapped
                        .into_iter()
                        .map(|r| (r.trim_end().to_string(), Ink::Text)),
                );
            }
        }
        rows
    }

    /// The box on `grid` (see the type's notes).
    fn layout(&self, grid: Grid) -> kit::Layout {
        let width = kit::inside_width(grid.cols, RESULT_NOMINAL, RESULT_NOMINAL);
        let rows = self.rows(width);
        let mut parts = Vec::new();
        if !rows.is_empty() {
            let most = plan_most(grid.rows);
            parts.push(Part::Scroll {
                rows: rows.to_vec(),
                most,
                top: self.top,
            });
        }
        if let Some((_, Some(line))) = &self.error {
            parts.push(Part::Text(line.clone(), Ink::Weak));
        }
        if let Some(warning) = &self.warning {
            parts.extend([Part::Blank, Part::Text(warning.clone(), Ink::Error)]);
        } else if let Some(stamp) = &self.backup {
            parts.extend([Part::Blank, Part::Text(backup_line(stamp), Ink::Text)]);
        }
        parts.extend([
            Part::Blank,
            Part::Buttons(vec![Button::new("OK", ok_id())]),
            Part::Text(RESULT_HINT.to_string(), Ink::Weak),
        ]);
        kit::layout(grid, &self.title, RESULT_NOMINAL, &parts)
    }

    /// The cells of `grid` (the panels') the result covers, its box and its shadow, for the panel
    /// and the other side to blank (10 §17, X4-graph's seam).
    pub fn covers(&self, grid: &Grid) -> Vec<Cells> {
        self.layout(*grid).covers()
    }

    /// One pass on `grid` (the panels'); `true` once Enter, Esc, OK or a click outside closed it.
    /// Its rows scroll by the wheel over the box and the scroll bar on its border.
    pub fn ui(&mut self, ctx: &egui::Context, grid: Grid) -> bool {
        let close = ctx.input_mut(|i| {
            keys::take(i, Key::Enter, Mods::None) | keys::take(i, Key::Escape, Mods::None)
        });
        let layout = self.layout(grid);
        let inputs = Inputs {
            top: Some(&mut self.top),
            by_id: vec![(ok_id(), Input::Button)],
            ..Inputs::default()
        };
        let pass = kit::show(ctx, result_id(), &layout, inputs);
        close || pass.clicked(ok_id()) || pass.modal.should_close()
    }
}

// ---------------------------------------------------------------------------------------------
// "Stop git?" (03 §3.4).

fn stop_id() -> egui::Id {
    egui::Id::unique("sopen-git-stop")
}

fn stop_yes_id() -> egui::Id {
    stop_id().with("yes")
}

/// "Stop git?"'s `[ No ]`.
pub(super) fn stop_no_id() -> egui::Id {
    stop_id().with("no")
}

/// What stopping a write may leave (03 §3.4).
const STOP_WHY: &str = "It may stop between steps; the panel then shows the state it left.";

/// What "Stop git?" says Enter and Esc do.
pub(super) const STOP_HINT: &str = "Enter stops it · Esc lets it run";

/// The question's nominal width in cells: it is as wide as its widest line.
const STOP_NOMINAL: usize = 40;

/// What "Stop git?" keeps between passes, in the context's memory (the panel holds only the
/// write's words): what the screen showed it asking about ([`cells::OnScreen`]), the pass it was
/// last laid out in and over which write — a question not laid out in the pass before, or over
/// another write, starts afresh, laid out unseen first.
#[derive(Debug, Clone, Default)]
struct Stop {
    screen: cells::OnScreen<String>,
    laid: Option<u64>,
    what: String,
}

/// The question over the write `what` on `grid` (the panels').
fn stop_layout(grid: Grid, what: &str) -> kit::Layout {
    let parts = [
        Part::Text(format!("{what} is still running."), Ink::Text),
        Part::Text(STOP_WHY.to_string(), Ink::Text),
        Part::Blank,
        Part::Buttons(vec![
            Button::new("Yes, stop it", stop_yes_id()),
            Button::new("No", stop_no_id()),
        ]),
        Part::Text(STOP_HINT.to_string(), Ink::Weak),
    ];
    kit::layout(grid, "Stop git?", STOP_NOMINAL, &parts)
}

/// The cells of `grid` (the panels') "Stop git?" over `what` covers, its box and its shadow, for
/// the panel and the other side to blank (10 §17, X4-graph's seam).
pub(super) fn stop_covers(grid: &Grid, what: &str) -> Vec<Cells> {
    stop_layout(*grid, what).covers()
}

/// "Stop git?" (03 §3.4) over the write `what`, one pass on `grid` (the panels'): `Some(true)` on
/// Enter or Yes, `Some(false)` on Esc, No or a click outside. It comes up on its own over a write
/// the owner pressed Esc for, so it stops only what the screen showed it asking about — not in its
/// first pass, laid out unseen, nor in a pass the terminal did not print — and its Enters and
/// Spaces go to its buttons as an action dialog's do ([`take_enters`]).
pub(super) fn stop_question(ctx: &egui::Context, grid: Grid, what: &str) -> Option<bool> {
    let kept = stop_id().with("kept");
    let pass_nr = ctx.cumulative_pass_nr();
    let mut stop: Stop = ctx.data(|d| d.get_temp(kept)).unwrap_or_default();
    if stop.laid.is_none_or(|nr| nr + 1 < pass_nr) || stop.what != what {
        stop = Stop {
            what: what.to_string(),
            ..Stop::default()
        };
    }
    let seen = stop.screen.start(ctx).cloned();
    if stop.laid.replace(pass_nr).is_some() {
        stop.screen.painted(what.to_string());
    }
    ctx.data_mut(|d| d.insert_temp(kept, stop));
    let escape = ctx.input_mut(|i| keys::take(i, Key::Escape, Mods::None));
    let began = ctx.memory(|m| m.focused());
    // Only buttons: an arrow moves the keyboard among them.
    let enters = take_enters(ctx, began.is_some(), false);
    let (yes, no) = (stop_yes_id(), stop_no_id());
    let inputs = Inputs {
        by_id: vec![(yes, Input::Button), (no, Input::Button)],
        ..Inputs::default()
    };
    let pass = kit::show(ctx, stop_id(), &stop_layout(grid, what), inputs);
    let now = ctx.memory(|m| m.focused());
    let mut answer_no = escape || pass.clicked(no) || pass.modal.should_close();
    let mut answer_yes = pass.clicked(yes);
    for after_tab in enters {
        match if after_tab { now } else { began } {
            Some(id) if id == no => answer_no = true,
            _ => answer_yes = true,
        }
    }
    let answer = match answer_no {
        true => Some(false),
        false => (answer_yes && seen.as_deref() == Some(what)).then_some(true),
    };
    // Answered: asked again, even in the very next pass, it starts afresh.
    if answer.is_some() {
        ctx.data_mut(|d| d.remove::<Stop>(kept));
    }
    answer
}
