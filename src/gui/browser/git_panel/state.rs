//! The git panel's part of the switch's state (10 §7.4, v2; X4-state): what [`GitView::part`]
//! takes of a view — the bar and the marks by their rows, the label cursor, the details pane's
//! subject and keyboard, the filters, Find's open field, an open dialog with what is typed in it
//! and the planner's plan — and how a new view on the receiving host is given it, a part at a
//! time as the integration (`switch_state`) gets there: the filters before the graph is read
//! ([`GitView::give_filters`]); the rest once the bar's row is loaded ([`GitView::give_view`]),
//! every commit the part names looked up on this host first ([`GitView::commits`]) and what it
//! lacks left out; the planner opened as Cmd/Ctrl+E opens it, on this host's own range
//! ([`GitView::open_plan`], [`GitView::give_plan`]), then the dialog as its key opens it, on this
//! host's own rows, refs and files ([`GitView::give_dialog`]), and what was typed given once it
//! is open ([`GitView::give_typed`]). Nothing here runs anything: a view of the menu's, or an open
//! that would run at once, is refused, and what is given starts unseen (its box's `OnScreen`), so
//! no Enter runs it before the owner has seen it.

use super::filters::{BranchChoice, Filters, Order};
use super::{details, end_text, find, GitAction, GitView, Row, RowKey, Subject, Target};
use crate::git::actions::ActionId;
use crate::git::model::Oid;
use crate::git::rebase::{Author, Base, CommitterDate, Date, Model, Op, Verb};
use crate::gui::browser::switch_state::{
    GitAuthor, GitChoice, GitCommitter, GitDetails, GitDialog, GitFilters, GitFind, GitOrder,
    GitPlan, GitPlanRow, GitRow, GitState, GitSubject, GitTarget, GitText,
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

    /// The open dialog with what is typed in it (the menu, the result, the log's entry, the stop
    /// question, a confirmation alone, a restore and the planner's question and run hold nothing
    /// typed).
    fn dialog_part(&self) -> Option<GitDialog> {
        if let Some(d) = &self.filters_dialog {
            let (filters, typed) = d.taken();
            let filters = filters_part(filters);
            let typed = typed.to_string();
            return Some(GitDialog::Filters { filters, typed });
        }
        if let Some(d) = &self.search_dialog {
            let q = &d.query;
            let fields = [&q.message, &q.author, &q.text, &q.path, &q.since, &q.until];
            let fields = fields.map(|f| f.clone());
            return Some(GitDialog::Search {
                fields,
                case: q.case,
            });
        }
        let (id, on, texts, choices) = self.dialog.as_ref()?.taken()?;
        let texts = (texts.into_iter())
            .map(|(field, text)| GitText { field, text })
            .collect();
        let choices = (choices.into_iter())
            .map(|(row, chosen)| GitChoice { row, chosen })
            .collect();
        let (id, on) = (id.as_str().to_string(), target_part(on));
        Some(GitDialog::Action {
            id,
            on,
            texts,
            choices,
        })
    }

    /// What of this panel's the switch's question says is dropped (10 §7.4, v2), each as it names
    /// it: of a panel the state does not carry (`carried` false: not on screen), its planner's plan
    /// and its open dialog; of one it carries, what is typed in a quick edit's dialog or the
    /// planner's own message or author dialog (X4-state2's), and what is chosen in a restore's.
    pub fn untaken(&self, carried: bool) -> Vec<&'static str> {
        use crate::git::actions::Action;
        use crate::git::rebase::{Edit, Quick};
        if !carried {
            let plan = self
                .planner
                .is_some()
                .then_some("the unsaved plan of a planner not on screen");
            let dialog =
                (self.dialog_up()).then_some("the open dialog of a git panel not on screen");
            return plan.into_iter().chain(dialog).collect();
        }
        let typed = |q: &super::planner::QuickDialog| {
            matches!(
                q.draft(),
                Edit::Quick(Quick::Reword { .. } | Quick::Squash { .. } | Quick::Author { .. })
            )
        };
        let restore =
            |d: &super::dialogs::ActionDialog| matches!(d.draft(), Action::Restore { .. });
        [
            (self.quick.as_deref().is_some_and(typed))
                .then_some("what is typed in the history edit's dialog"),
            (self.dialog.as_deref().is_some_and(restore))
                .then_some("what is chosen in the restore's dialog"),
            (self.planner.as_ref().is_some_and(|p| p.typing()))
                .then_some("what is typed in the planner's dialog"),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Every commit `part` names, once, to look up on this host before any is used: the bar's,
    /// the marks', the details pane's, its plan's base, rows and bar, its dialog's target's.
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
    /// (`GitAction::Open`), and what was typed comes once it is open ([`GitView::give_typed`]).
    /// `Err`: why it is not opened again — a view of the menu's (it would act at once: a copy, the
    /// editor, a filter), a target this host lacks, one this host's menu does not offer there, one
    /// that would run at once (no dialog of it reached the screen here).
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
        })
    }

    /// What was typed in `part`'s action dialog, given once it is open here: `None` while it is
    /// not yet; `Some(Err)` says what went otherwise (another dialog in its place, a choice this
    /// host does not offer).
    pub fn give_typed(&mut self, part: &GitState) -> Option<Result<(), String>> {
        let Some(GitDialog::Action {
            id, texts, choices, ..
        }) = &part.dialog
        else {
            return Some(Ok(()));
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
