//! The git panel's part of the switch's state (10 §7.4, v2; X4-state): what [`GitView::part`]
//! takes of a view — the bar and the marks by their rows, the label cursor, the details pane's
//! subject and keyboard, the filters, Find's open field, an open dialog with what is typed in it
//! (from v3, X4-state2, a quick edit's and the planner's own too) and the planner's plan — and
//! how a new view on the receiving host is given it, a part at a time as the integration
//! (`switch_state`) gets there: the filters before the graph is read
//! ([`GitView::give_filters`]); the rest once the bar's row is loaded ([`GitView::give_view`]),
//! every commit the part names looked up on this host first ([`GitView::commits`]) and what it
//! lacks left out; the planner opened as Cmd/Ctrl+E opens it, on this host's own range
//! ([`GitView::open_plan`], [`GitView::give_plan`]), then the dialog as its key opens it, on this
//! host's own rows, refs and files ([`GitView::give_dialog`]) — a quick edit's from its range
//! read, the planner's own on its row of the plan given — and what was typed given once it is
//! open ([`GitView::give_typed`]). Nothing here runs anything: a view of the menu's, or an open
//! that would run at once, is refused, and what is given starts unseen (its box's `OnScreen`), so
//! no Enter runs it before the owner has seen it.

use super::filters::{BranchChoice, Filters, Order};
use super::planner::{copy_text, AuthorForm, QuickDialog, Typed};
use super::{details, end_text, find, GitAction, GitView, Row, RowKey, Subject, Target};
use crate::git::actions::ActionId;
use crate::git::model::{CommitRow, Oid};
use crate::git::rebase::{Author, Base, CommitterDate, Date, Edit, EditId, Model, Op, Quick, Verb};
use crate::gui::browser::switch_state::{
    bound, GitAuthor, GitAuthorForm, GitChoice, GitCommitter, GitDetails, GitDialog, GitFilters,
    GitFind, GitOrder, GitPlan, GitPlanRow, GitRow, GitState, GitSubject, GitTarget, GitText,
};
use eframe::egui;
use std::collections::{HashMap, HashSet};

fn row(key: &RowKey) -> GitRow {
    match key {
        RowKey::Uncommitted => GitRow::Uncommitted,
        RowKey::Commit(c) => GitRow::Commit(c.as_str().into()),
        RowKey::Stash(c) => GitRow::Stash(c.as_str().into()),
    }
}

/// `row`'s key (its id was checked where the state came in, `GitState::check`).
fn key(row: &GitRow) -> Option<RowKey> {
    Some(match row {
        GitRow::Uncommitted => RowKey::Uncommitted,
        GitRow::Commit(c) => RowKey::Commit(Oid::parse(c)?),
        GitRow::Stash(c) => RowKey::Stash(Oid::parse(c)?),
    })
}

/// Whether this host has `key`'s commit (`known`: the ids the lookup found).
fn has(known: &HashSet<Oid>, key: &RowKey) -> bool {
    match key {
        RowKey::Uncommitted => true,
        RowKey::Commit(c) | RowKey::Stash(c) => known.contains(c),
    }
}

/// `text`'s commit, when this host has it.
fn known_oid(known: &HashSet<Oid>, text: &str) -> Option<Oid> {
    Oid::parse(text).filter(|o| known.contains(o))
}

fn subject(s: &Subject) -> GitSubject {
    match s {
        Subject::Row(k) => GitSubject::Row(row(k)),
        Subject::Compare { from, to } => GitSubject::Compare {
            from: row(from),
            to: row(to),
        },
    }
}

fn order_part(o: Order) -> GitOrder {
    match o {
        Order::Date => GitOrder::Date,
        Order::AuthorDate => GitOrder::AuthorDate,
        Order::Topo => GitOrder::Topo,
    }
}

fn order_of(o: GitOrder) -> Order {
    match o {
        GitOrder::Date => Order::Date,
        GitOrder::AuthorDate => Order::AuthorDate,
        GitOrder::Topo => Order::Topo,
    }
}

fn committer_part(c: CommitterDate) -> GitCommitter {
    match c {
        CommitterDate::Keep => GitCommitter::Keep,
        CommitterDate::Now => GitCommitter::Now,
        CommitterDate::Author => GitCommitter::Author,
    }
}

fn committer_of(c: GitCommitter) -> CommitterDate {
    match c {
        GitCommitter::Keep => CommitterDate::Keep,
        GitCommitter::Now => CommitterDate::Now,
        GitCommitter::Author => CommitterDate::Author,
    }
}

/// An author's fields as the state carries them (v3), and back.
fn form_part(f: &AuthorForm) -> GitAuthorForm {
    GitAuthorForm {
        name: f.name.clone(),
        email: f.email.clone(),
        date: f.date.clone(),
        committer: committer_part(f.committer),
    }
}

fn form_of(a: &GitAuthorForm) -> AuthorForm {
    AuthorForm {
        name: a.name.clone(),
        email: a.email.clone(),
        date: a.date.clone(),
        committer: committer_of(a.committer),
    }
}

/// A quick edit's dialog with what is typed in it (v3): reword's or squash's message, author's
/// fields, on its commit; `None` for any other — a confirmation alone, or one that refuses its
/// commit's message, holds nothing typed.
fn quick_part(q: &QuickDialog) -> Option<GitDialog> {
    let commit = |c: &Oid| c.as_str().to_string();
    Some(match (q.draft(), q.typed()?) {
        (Edit::Quick(Quick::Reword { commit: c, .. }), Typed::Message(m)) => GitDialog::Reword {
            commit: commit(c),
            message: copy_text(m),
        },
        (Edit::Quick(Quick::Squash { commit: c, .. }), Typed::Message(m)) => GitDialog::Squash {
            commit: commit(c),
            message: copy_text(m),
        },
        (Edit::Quick(Quick::Author { commit: c, .. }), Typed::Author(f)) => GitDialog::Author {
            commit: commit(c),
            author: form_part(f),
        },
        _ => return None,
    })
}

/// Whether `q` holds what is typed in it, as [`quick_part`] takes it.
fn quick_holds(q: &QuickDialog) -> bool {
    matches!(
        (q.draft(), q.typed()),
        (
            Edit::Quick(Quick::Reword { .. } | Quick::Squash { .. }),
            Some(Typed::Message(_))
        ) | (Edit::Quick(Quick::Author { .. }), Some(Typed::Author(_)))
    )
}

/// An open dialog that holds something a switch carries ([`GitView::holding`]).
#[derive(Debug, Clone, Copy)]
enum Holds {
    Quick,
    Action,
    Search,
    Filters,
    Planner,
}

impl Holds {
    /// As the question names it when another crosses in its place.
    fn named(self) -> &'static str {
        match self {
            // Never shown: a quick edit's is always in front. Kept so the match covers each case.
            Holds::Quick => "what is typed in the history edit's dialog",
            Holds::Action => "what is typed or chosen in the action's dialog",
            Holds::Search => "what is typed in the history search",
            Holds::Filters => "what is chosen in the filters' dialog",
            Holds::Planner => "what is typed in the planner's dialog",
        }
    }

    /// As the question names it when it is in front but past what a state holds of it.
    fn too_long(self) -> &'static str {
        match self {
            Holds::Quick => "what is typed in the history edit's dialog (too long to carry)",
            Holds::Action => "what is typed or chosen in the action's dialog (too long to carry)",
            Holds::Search => "what is typed in the history search (too long to carry)",
            Holds::Filters => "what is chosen in the filters' dialog (too long to carry)",
            Holds::Planner => "what is typed in the planner's dialog (too long to carry)",
        }
    }
}

/// Whether `q` is quick edit `id`'s dialog on `commit`.
fn is_quick(q: &QuickDialog, id: EditId, commit: &str) -> bool {
    match q.draft() {
        Edit::Quick(
            quick @ (Quick::Reword { commit: c, .. }
            | Quick::Squash { commit: c, .. }
            | Quick::Author { commit: c, .. }),
        ) => quick.id() == id && c.as_str() == commit,
        _ => false,
    }
}

/// The filters as the state carries them.
fn filters_part(f: &Filters) -> GitFilters {
    GitFilters {
        branches: match &f.branches {
            BranchChoice::All => None,
            BranchChoice::Only(chosen) => Some(chosen.iter().cloned().collect()),
        },
        hidden_remotes: f.hidden_remotes.iter().cloned().collect(),
        tags: f.tags,
        stashes: f.stashes,
        first_parent: f.first_parent,
        order: order_part(f.order),
        reflog: f.reflog,
    }
}

/// The filters the state carries. A branch or remote this host has not is compared with its
/// refs only, never sent to git (`Filters::revisions`).
fn filters_of(f: &GitFilters) -> Filters {
    Filters {
        branches: match &f.branches {
            None => BranchChoice::All,
            Some(chosen) => BranchChoice::Only(chosen.iter().cloned().collect()),
        },
        hidden_remotes: f.hidden_remotes.iter().cloned().collect(),
        tags: f.tags,
        stashes: f.stashes,
        first_parent: f.first_parent,
        order: order_of(f.order),
        reflog: f.reflog,
    }
}

fn target_part(t: &Target) -> GitTarget {
    let oid = |o: &Oid| o.as_str().to_string();
    match t {
        Target::Commit { commit, marked } => GitTarget::Commit {
            commit: oid(&commit.oid),
            marked: marked.iter().map(|c| oid(&c.oid)).collect(),
        },
        Target::Label(r) => GitTarget::Label(r.refname.clone()),
        Target::Stash(s) => GitTarget::Stash(oid(&s.oid)),
        Target::Uncommitted => GitTarget::Uncommitted,
        Target::Files(files) => GitTarget::Files(files.iter().map(|f| f.path.clone()).collect()),
        Target::Repository => GitTarget::Repository,
    }
}

fn plan_part(model: &Model, bar: Option<&Oid>) -> GitPlan {
    let author = |a: &Author| GitAuthor {
        name: a.name.clone(),
        email: a.email.clone(),
        seconds: a.date.seconds,
        offset_minutes: a.date.offset_minutes,
        committer: committer_part(a.committer),
    };
    GitPlan {
        base: match &model.base {
            Base::Commit(c) => Some(c.as_str().into()),
            Base::Root => None,
        },
        rows: (model.rows.iter())
            .map(|r| GitPlanRow {
                commit: r.commit.row.oid.as_str().into(),
                verb: r.verb.word().into(),
                message: r.message.clone(),
                author: r.author.as_ref().map(author),
            })
            .collect(),
        autostash: model.autostash,
        update_refs: model.update_refs,
        bar: bar.map(|b| b.as_str().into()),
    }
}

/// `plan` made of `model`, the planner's range as this host read it: the same base and the same
/// commits — only their order, verbs, messages and authors are the plan's — each change through
/// `Model::apply`, which refuses what it refuses from the keys. `Err`: it does not fit.
fn plan_onto(model: &mut Model, plan: &GitPlan) -> Result<(), String> {
    let base = match &model.base {
        Base::Commit(c) => Some(c.as_str()),
        Base::Root => None,
    };
    let order: Vec<Oid> = (plan.rows.iter())
        .filter_map(|r| Oid::parse(&r.commit))
        .collect();
    let place: HashMap<&Oid, usize> = order.iter().enumerate().map(|(k, o)| (o, k)).collect();
    let fits = base == plan.base.as_deref()
        && place.len() == plan.rows.len()
        && model.rows.len() == place.len()
        && (model.rows.iter()).all(|r| place.contains_key(&r.commit.row.oid));
    if !fits {
        return Err("its commits are not the range read here".into());
    }
    model.rows.sort_by_key(|r| place[&r.commit.row.oid]);
    let apply = |m: &mut Model, op| m.apply(op).map_err(|e| e.0);
    for (k, (r, oid)) in plan.rows.iter().zip(&order).enumerate() {
        let verb = Verb::ALL.into_iter().find(|v| v.word() == r.verb);
        let verb = verb.ok_or("a verb sopen has not")?;
        if verb == Verb::Pick && r.message.is_none() && r.author.is_none() {
            continue;
        }
        // Its ops on a model of its row alone, through `Model::apply`'s own checks (the row's
        // place in `model` is `k`: no search a row), then the row as they left it.
        let mut one = Model::new(model.base.clone(), vec![model.rows[k].commit.clone()]);
        // A typed message stays on its row whatever its verb now (`Model::changed`); Enter types
        // one only on a reword row.
        if let Some(message) = &r.message {
            let (commit, verb) = (oid.clone(), Verb::Reword);
            apply(&mut one, Op::Verb { commit, verb })?;
            let (commit, message) = (oid.clone(), message.clone());
            apply(&mut one, Op::Message { commit, message })?;
        }
        apply(
            &mut one,
            Op::Verb {
                commit: oid.clone(),
                verb,
            },
        )?;
        if let Some(a) = &r.author {
            let author = Author {
                name: a.name.clone(),
                email: a.email.clone(),
                date: Date {
                    seconds: a.seconds,
                    offset_minutes: a.offset_minutes,
                },
                committer: committer_of(a.committer),
            };
            let author = Some(author);
            apply(
                &mut one,
                Op::Author {
                    commit: oid.clone(),
                    author,
                },
            )?;
        }
        let done = one.rows.pop().expect("its row");
        let row = &mut model.rows[k];
        (row.verb, row.message, row.author) = (done.verb, done.message, done.author);
    }
    (model.autostash, model.update_refs) = (plan.autostash, plan.update_refs);
    Ok(())
}

impl GitView {
    /// This view's part of the switch's state (10 §7.4, v2), of the repository at `top`.
    pub fn part(&self, top: &str) -> GitState {
        let mut marks: Vec<GitRow> = self.marks.iter().map(row).collect();
        marks.sort();
        GitState {
            top: top.to_string(),
            bar: self.bar_row().map(|r| row(&RowKey::of(r))),
            label: self.label_cursor,
            marks,
            details: (self.pane.as_ref()).map(|p| GitDetails {
                subject: subject(&p.subject),
                keyboard: p.keyboard,
            }),
            filters: (!self.filters.is_default()).then(|| filters_part(&self.filters)),
            find: (self.find.as_ref()).map(|f| GitFind {
                text: f.text.clone(),
                case: f.case,
            }),
            dialog: self.dialog_part(),
            plan: (self.planner.as_ref()).map(|p| plan_part(p.model(), p.bar())),
        }
    }

    /// The open dialogs that hold something a switch carries — typed or chosen — front first among
    /// those that hold something, as `modals` draws them over the filters and the history search,
    /// the history search over the filters, all of them over the planner's own, and the keyboard
    /// is the front one's: a quick edit's, an action's, the history search's, the filters', the
    /// planner's own. One crosses — the first, one git dialog at a time ([`GitView::part`]); the
    /// question names the rest ([`GitView::untaken`]). The menu, the result, the log's entry, the stop question, a
    /// confirmation alone, a restore and the planner's question and run hold nothing typed.
    fn holding(&self) -> Vec<Holds> {
        [
            (self.quick.as_deref().is_some_and(quick_holds), Holds::Quick),
            (
                self.dialog.as_deref().is_some_and(|d| d.carried()),
                Holds::Action,
            ),
            (self.search_dialog.is_some(), Holds::Search),
            (self.filters_dialog.is_some(), Holds::Filters),
            (
                self.planner.as_ref().is_some_and(|p| p.typed().is_some()),
                Holds::Planner,
            ),
        ]
        .into_iter()
        .filter_map(|(open, what)| open.then_some(what))
        .collect()
    }

    /// The front one of [`GitView::holding`] with what is typed or chosen in it.
    fn dialog_part(&self) -> Option<GitDialog> {
        Some(match self.holding().first()? {
            Holds::Quick => quick_part(self.quick.as_deref()?)?,
            Holds::Action => {
                let (id, on, texts, choices) = self.dialog.as_ref()?.taken()?;
                let texts = (texts.into_iter())
                    .map(|(field, text)| GitText { field, text })
                    .collect();
                let choices = (choices.into_iter())
                    .map(|(row, chosen)| GitChoice { row, chosen })
                    .collect();
                let (id, on) = (id.as_str().to_string(), target_part(on));
                GitDialog::Action {
                    id,
                    on,
                    texts,
                    choices,
                }
            }
            Holds::Search => {
                let q = &self.search_dialog.as_ref()?.query;
                let fields = [&q.message, &q.author, &q.text, &q.path, &q.since, &q.until];
                let fields = fields.map(|f| f.clone());
                GitDialog::Search {
                    fields,
                    case: q.case,
                }
            }
            Holds::Filters => {
                let (filters, typed) = self.filters_dialog.as_ref()?.taken();
                let filters = filters_part(filters);
                let typed = typed.to_string();
                GitDialog::Filters { filters, typed }
            }
            Holds::Planner => {
                let (commit, typed) = self.planner.as_ref()?.typed()?;
                let commit = commit.as_str().to_string();
                match typed {
                    Typed::Message(m) => GitDialog::PlanMessage {
                        commit,
                        message: copy_text(m),
                    },
                    Typed::Author(f) => GitDialog::PlanAuthor {
                        commit,
                        author: form_part(f),
                    },
                }
            }
        })
    }

    /// What of this panel's the switch's question says is dropped (10 §7.4), each as it names it:
    /// of a panel the state does not carry (`carried` false: not on screen), its planner's plan and
    /// its open dialog; of one it carries, what is chosen in a restore's, the front dialog's typing
    /// when it is past what a state holds (`fit` leaves it out), and what is typed or chosen in
    /// each dialog behind the one that crosses ([`GitView::holding`]).
    pub fn untaken(&self, carried: bool) -> Vec<&'static str> {
        use crate::git::actions::Action;
        if !carried {
            let plan = self
                .planner
                .is_some()
                .then_some("the unsaved plan of a planner not on screen");
            let dialog =
                (self.dialog_up()).then_some("the open dialog of a git panel not on screen");
            return plan.into_iter().chain(dialog).collect();
        }
        let restore =
            |d: &super::dialogs::ActionDialog| matches!(d.draft(), Action::Restore { .. });
        let restore = (self.dialog.as_deref().is_some_and(restore))
            .then_some("what is chosen in the restore's dialog");
        let holding = self.holding();
        // The front one crosses — unless it is past what a state holds of it.
        let front = (holding.first())
            .filter(|_| !self.carried_whole())
            .map(|h| h.too_long());
        let behind = holding.iter().skip(1).map(|h| h.named());
        restore.into_iter().chain(front).chain(behind).collect()
    }

    /// Whether the dialog that crosses ([`GitView::dialog_part`]'s) is within what a state holds
    /// of what is typed in it — the bounds `GitState::check_typed` holds those texts to, by which
    /// `fit` would leave it out: a message or a field's text [`bound::TEXT`], an author's name,
    /// email and date [`bound::NAME`], the filters' names as `check_filters` holds them. Read in
    /// place, nothing copied: the question asks it every pass it is up (F-git-drafted's rule).
    fn carried_whole(&self) -> bool {
        let text = |t: &str| t.len() <= bound::TEXT;
        let typed = |t: Typed| match t {
            Typed::Message(m) => text(m),
            Typed::Author(f) => [&f.name, &f.email, &f.date]
                .iter()
                .all(|t| t.len() <= bound::NAME),
        };
        let Some(front) = self.holding().first().copied() else {
            return true;
        };
        match front {
            Holds::Quick => (self.quick.as_deref().and_then(|q| q.typed())).is_none_or(typed),
            Holds::Action => (self.dialog.as_deref()).is_none_or(|d| d.texts_within(bound::TEXT)),
            Holds::Search => self.search_dialog.as_ref().is_none_or(|d| {
                let q = &d.query;
                [&q.message, &q.author, &q.text, &q.path, &q.since, &q.until]
                    .iter()
                    .all(|f| text(f))
            }),
            Holds::Filters => self.filters_dialog.as_ref().is_none_or(|d| {
                let (f, typed) = d.taken();
                let names = |n: usize, mut all: std::collections::btree_set::Iter<'_, String>| {
                    n <= bound::NAMES && all.all(|s| s.len() <= bound::NAME)
                };
                let branches = match &f.branches {
                    BranchChoice::All => true,
                    BranchChoice::Only(chosen) => names(chosen.len(), chosen.iter()),
                };
                text(typed) && branches && names(f.hidden_remotes.len(), f.hidden_remotes.iter())
            }),
            Holds::Planner => {
                (self.planner.as_ref().and_then(|p| p.typed())).is_none_or(|(_, t)| typed(t))
            }
        }
    }

    /// Every commit `part` names, once, to look up on this host before any is used: the bar's,
    /// the marks', the details pane's, its plan's base, rows and bar, its dialog's target or row.
    pub fn commits(part: &GitState) -> Vec<Oid> {
        let mut rows: Vec<&GitRow> = part.bar.iter().chain(&part.marks).collect();
        match part.details.as_ref().map(|d| &d.subject) {
            Some(GitSubject::Row(r)) => rows.push(r),
            Some(GitSubject::Compare { from, to }) => rows.extend([from, to]),
            None => {}
        }
        let mut ids: Vec<&str> = (rows.into_iter())
            .filter_map(|r| match r {
                GitRow::Commit(c) | GitRow::Stash(c) => Some(c.as_str()),
                GitRow::Uncommitted => None,
            })
            .collect();
        if let Some(plan) = &part.plan {
            ids.extend(plan.base.iter().chain(&plan.bar).map(String::as_str));
            ids.extend(plan.rows.iter().map(|r| r.commit.as_str()));
        }
        match &part.dialog {
            Some(GitDialog::Action {
                on: GitTarget::Commit { commit, marked },
                ..
            }) => ids.extend(std::iter::once(commit).chain(marked).map(String::as_str)),
            Some(GitDialog::Action {
                on: GitTarget::Stash(c),
                ..
            }) => ids.push(c),
            Some(
                GitDialog::Reword { commit, .. }
                | GitDialog::Squash { commit, .. }
                | GitDialog::Author { commit, .. }
                | GitDialog::PlanMessage { commit, .. }
                | GitDialog::PlanAuthor { commit, .. },
            ) => ids.push(commit),
            _ => {}
        }
        let mut seen = HashSet::new();
        (ids.into_iter().filter_map(Oid::parse))
            .filter(|o| seen.insert(o.clone()))
            .collect()
    }

    /// `part`'s filters, before the graph is read with them: whether they changed (a graph read
    /// already is read again, `GitAction::FiltersChanged`).
    pub fn give_filters(&mut self, part: &GitState) -> bool {
        let filters = part
            .filters
            .as_ref()
            .map_or_else(Filters::default, filters_of);
        if filters == self.filters {
            return false;
        }
        self.filters = filters;
        self.relabel();
        true
    }

    /// The bar to `part`'s row once a page brings it, `known` the commits this host has
    /// ([`GitView::selecting`] while it waits): `false` when there is none to go to.
    pub fn give_bar(&mut self, part: &GitState, known: &HashSet<Oid>) -> bool {
        // The uncommitted row is the first one where there is one: never paged for.
        let here = |k: &RowKey| *k != RowKey::Uncommitted || self.rows.iter().any(|r| k.is(r));
        match part
            .bar
            .as_ref()
            .and_then(key)
            .filter(|k| has(known, k) && here(k))
        {
            Some(k) => {
                self.select(k);
                true
            }
            None => false,
        }
    }

    /// The bar's row is not coming (the history ended without it): no page later takes the bar.
    pub fn give_up_bar(&mut self) {
        (self.pending, self.pending_asked) = (None, false);
    }

    /// The rest of `part`'s view once the bar is on its row: its label cursor, the marks this host
    /// has, the details pane (its read asked for) and Find's field — open with its text, the bar
    /// left where it is (no jump to a first match). What to read, and how many marks this host
    /// lacks.
    pub fn give_view(&mut self, part: &GitState, known: &HashSet<Oid>) -> (Vec<GitAction>, usize) {
        self.set_label_cursor(part.label);
        let keys: Vec<RowKey> = part.marks.iter().filter_map(key).collect();
        let lacking = keys.iter().filter(|k| !has(known, k)).count();
        self.marks = keys.into_iter().filter(|k| has(known, k)).collect();
        let mut asked = Vec::new();
        let known_row = |r: &GitRow| key(r).filter(|k| has(known, k));
        let pane = part.details.as_ref().and_then(|d| match &d.subject {
            GitSubject::Row(r) => known_row(r).map(|k| (Subject::Row(k), d.keyboard)),
            GitSubject::Compare { from, to } => match (known_row(from), known_row(to)) {
                (Some(from), Some(to)) => Some((Subject::Compare { from, to }, d.keyboard)),
                _ => None,
            },
        });
        if let Some((subject, keyboard)) = pane {
            let header = match &subject {
                Subject::Compare { from, to } => {
                    let text = |k: &RowKey| {
                        (self.rows.iter().find(|r| k.is(r)))
                            .map_or_else(String::new, |r| end_text(r, self.uncommitted))
                    };
                    vec![("From", text(from)), ("To", text(to))]
                }
                Subject::Row(_) => Vec::new(),
            };
            self.pane = Some(details::Pane::new(subject.clone(), keyboard, header));
            asked.push(GitAction::Details(subject));
        }
        if let Some(f) = &part.find {
            let origin = self.graph_scroll.bar;
            self.find = Some(find::Find::given(f.text.clone(), f.case, origin));
        }
        (asked, lacking)
    }

    /// The planner `plan` opens in, as Cmd/Ctrl+E opens it: on this host's range above its base
    /// (`GitAction::OpenPlanner`, which reads it). `Err`: a commit of it this host lacks.
    pub fn open_plan(plan: &GitPlan, known: &HashSet<Oid>) -> Result<GitAction, String> {
        let ids = plan.base.iter().chain(plan.rows.iter().map(|r| &r.commit));
        let lacking = ids.filter(|c| known_oid(known, c).is_none()).count();
        if lacking > 0 {
            return Err(format!("{lacking} of its commits are not here"));
        }
        let base = plan.base.as_deref().and_then(|b| known_oid(known, b));
        Ok(GitAction::OpenPlanner(
            base.map_or(Base::Root, Base::Commit),
        ))
    }

    /// `part`'s plan given to the planner once it is open here (`None`: not yet): the bar on its
    /// row where this host has it. `Some(Err)` says why not (a plan that is not this host's range:
    /// the planner as read).
    pub fn give_plan(
        &mut self,
        part: &GitState,
        known: &HashSet<Oid>,
    ) -> Option<Result<(), String>> {
        let plan = part.plan.as_ref()?;
        let planner = self.planner.as_mut()?;
        let bar = plan.bar.as_deref().and_then(|b| known_oid(known, b));
        let given = planner.give(|m| plan_onto(m, plan), bar.as_ref());
        let said = |why| format!("its plan was not given: {why}; the planner shows it as read");
        Some(given.map_err(said))
    }

    /// `part`'s dialog opened as its key opens it, on this host's own (10 §7.4, v2): the filters'
    /// and the history search's at once, with what was typed; an action's on the rows, refs or
    /// files of this host its target names — the read it starts from is the integration's
    /// (`GitAction::Open`), and what was typed comes once it is open ([`GitView::give_typed`]); a
    /// quick edit's (v3) on its commit's row, from its range read (`GitAction::OpenEdit`), its
    /// typing likewise once it is open; the planner's own (v3) at once, on its row of the planner
    /// given, with what was typed. `Err`: why it is not opened again — a view of the menu's (it
    /// would act at once: a copy, the editor, a filter), a target this host lacks, one this host's
    /// menu does not offer there, one that would run at once (no dialog of it reached the screen
    /// here), a row the planner here has not or opens no such dialog on.
    pub fn give_dialog(
        &mut self,
        ctx: &egui::Context,
        part: &GitState,
        known: &HashSet<Oid>,
    ) -> Result<Vec<GitAction>, String> {
        Ok(match &part.dialog {
            None => Vec::new(),
            Some(GitDialog::Filters { filters, typed }) => {
                self.open_filters();
                if let Some(d) = &mut self.filters_dialog {
                    d.give(ctx, filters_of(filters), typed.clone());
                }
                Vec::new()
            }
            Some(GitDialog::Search { fields, case }) => {
                let [message, author, text, path, since, until] = fields.clone();
                let query = find::SearchQuery {
                    message,
                    author,
                    text,
                    path,
                    since,
                    until,
                    case: *case,
                };
                self.bump(super::Front::Search);
                self.search_dialog = Some(find::SearchDialog::given(ctx, query));
                Vec::new()
            }
            Some(GitDialog::Action { id, on, .. }) => {
                let id = ActionId::parse(id).ok_or("an action sopen has not")?;
                if super::menu::is_view(id) {
                    return Err("it is a view, not a dialog".into());
                }
                let target = (self.target_of(on, known)).ok_or("what it acts on is not here")?;
                let asked = self.act(id, target);
                // Its dialog, or the read its dialog starts from: anything else would act now.
                if asked.iter().any(|a| !matches!(a, GitAction::Open { .. })) {
                    return Err("here it would run at once, with no dialog".into());
                }
                if asked.is_empty() && self.dialog.is_none() {
                    return Err("it is not offered here".into());
                }
                asked
            }
            Some(GitDialog::Reword { commit, .. }) => {
                self.give_quick(EditId::Reword, commit, known)?
            }
            Some(GitDialog::Squash { commit, .. }) => {
                self.give_quick(EditId::Squash, commit, known)?
            }
            Some(GitDialog::Author { commit, .. }) => {
                self.give_quick(EditId::Author, commit, known)?
            }
            Some(GitDialog::PlanMessage { commit, message }) => {
                self.give_planners(commit, known, Typed::Message(message))?;
                Vec::new()
            }
            Some(GitDialog::PlanAuthor { commit, author }) => {
                let form = form_of(author);
                self.give_planners(commit, known, Typed::Author(&form))?;
                Vec::new()
            }
        })
    }

    /// Quick edit `id` on `commit`'s row, as its key opens it (v3): its range read first
    /// (`GitAction::OpenEdit`), where this host's history row offers it there.
    fn give_quick(
        &mut self,
        id: EditId,
        commit: &str,
        known: &HashSet<Oid>,
    ) -> Result<Vec<GitAction>, String> {
        let row = self
            .loaded(commit, known)
            .ok_or("what it acts on is not here")?;
        if !self.history_of(&row).contains(&id) {
            return Err("it is not offered here".into());
        }
        let asked = self.act_edit(id, row);
        // Its range's read, which opens it: nothing else, and not nothing (a squash whose
        // parent is not loaded here).
        match !asked.is_empty() && (asked.iter()).all(|a| matches!(a, GitAction::OpenEdit { .. })) {
            true => Ok(asked),
            false => Err("it is not offered here".into()),
        }
    }

    /// The planner's own dialog on its row `commit` of the planner given here (v3), with what was
    /// typed in it.
    fn give_planners(
        &mut self,
        commit: &str,
        known: &HashSet<Oid>,
        typed: Typed,
    ) -> Result<(), String> {
        let row = known_oid(known, commit).ok_or("its row is not here")?;
        let planner = self
            .planner
            .as_mut()
            .ok_or("its planner is not open here")?;
        planner.give_typed(&row, typed).map_err(str::to_string)
    }

    /// `oid`'s row, loaded, when this host has its commit.
    fn loaded(&self, oid: &str, known: &HashSet<Oid>) -> Option<CommitRow> {
        let oid = known_oid(known, oid)?;
        self.rows.iter().find_map(|r| match r {
            Row::Commit { commit, .. } if commit.oid == oid => Some(commit.clone()),
            _ => None,
        })
    }

    /// What was typed in `part`'s action or quick edit's dialog, given once it is open here:
    /// `None` while it is not yet; `Some(Err)` says what went otherwise (another dialog in its
    /// place, a choice or field this host does not offer, the owner's own opened meanwhile).
    pub fn give_typed(&mut self, part: &GitState) -> Option<Result<(), String>> {
        let (id, texts, choices) = match &part.dialog {
            Some(GitDialog::Action {
                id, texts, choices, ..
            }) => (id, texts, choices),
            Some(GitDialog::Reword { commit, message }) => {
                return self.give_quick_typed(EditId::Reword, commit, Typed::Message(message))
            }
            Some(GitDialog::Squash { commit, message }) => {
                return self.give_quick_typed(EditId::Squash, commit, Typed::Message(message))
            }
            Some(GitDialog::Author { commit, author }) => {
                let form = form_of(author);
                return self.give_quick_typed(EditId::Author, commit, Typed::Author(&form));
            }
            _ => return Some(Ok(())),
        };
        let d = self.dialog.as_mut()?;
        if d.draft().id().as_str() != id {
            let why = "its dialog was not opened again: another opened in its place";
            return Some(Err(why.into()));
        }
        let texts: Vec<_> = (texts.iter()).map(|t| (t.field, t.text.clone())).collect();
        let choices: Vec<_> = (choices.iter())
            .map(|c| (c.row, c.chosen.clone()))
            .collect();
        Some(d.give(&texts, &choices).map_err(|w| {
            format!("its dialog opened, but {w} is not offered here: that choice is as it opens")
        }))
    }

    /// What was typed in quick edit `id`'s dialog on `commit`, given once its range is read and it
    /// is open here (`None` until then), as afresh. The owner's own dialog opened meanwhile —
    /// before it came, or beside it — stays, and this one goes (v2's rule).
    fn give_quick_typed(
        &mut self,
        id: EditId,
        commit: &str,
        typed: Typed,
    ) -> Option<Result<(), String>> {
        let quick = self.quick.take();
        let given = quick.as_deref().is_some_and(|q| is_quick(q, id, commit));
        if self.dialog_up() {
            self.quick = quick.filter(|_| !given);
            return Some(Err("its dialog was not given: one of yours is open".into()));
        }
        let mut q = quick?;
        if !given {
            self.quick = Some(q);
            let why = "its dialog was not opened again: another opened in its place";
            return Some(Err(why.into()));
        }
        let done = q.give(typed);
        self.quick = Some(q);
        Some(done.map_err(|what| format!("its dialog opened, but {what} is not edited here")))
    }

    /// The target `on` names, as this host has it: the rows loaded and the commits looked up
    /// (`known`), the refs and the status read — the bar's and the marked commits, the label's
    /// ref, the stash, the changed files.
    fn target_of(&self, on: &GitTarget, known: &HashSet<Oid>) -> Option<Target> {
        let repo = self.repo.as_ref()?;
        let rows: HashMap<&Oid, &crate::git::model::CommitRow> = (self.rows.iter())
            .filter_map(|r| match r {
                Row::Commit { commit, .. } => Some((&commit.oid, commit)),
                _ => None,
            })
            .collect();
        let commit = |oid: &str| Some(rows.get(&known_oid(known, oid)?)?.to_owned().clone());
        Some(match on {
            GitTarget::Commit { commit: c, marked } => Target::Commit {
                commit: commit(c)?,
                marked: marked.iter().map(|m| commit(m)).collect::<Option<_>>()?,
            },
            GitTarget::Label(refname) => {
                Target::Label(repo.refs.iter().find(|r| r.refname == *refname)?.clone())
            }
            GitTarget::Stash(oid) => {
                let oid = known_oid(known, oid)?;
                Target::Stash(repo.stashes.iter().find(|s| s.oid == oid)?.clone())
            }
            GitTarget::Uncommitted => Target::Uncommitted,
            GitTarget::Files(paths) => {
                let entries: HashMap<&str, _> = (repo.status.entries.iter())
                    .map(|e| (e.path.as_str(), e))
                    .collect();
                let files = paths
                    .iter()
                    .map(|p| entries.get(p.as_str()).map(|&e| e.clone()));
                Target::Files(files.collect::<Option<_>>()?)
            }
            GitTarget::Repository => Target::Repository,
        })
    }
}
