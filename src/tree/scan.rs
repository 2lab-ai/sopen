//! What is read, when (05 §6.3): phase 1 before the first frame, phase 2 the listings of the rows
//! on screen and one screen around, phase 3 the deep size of closed rows nearest the bar — every
//! automatic listing bounded, every scan stopped when its row, the tree or the window goes
//! (`T-lazy`).
//!
//! - **Phase 1**: the folder, the home (once per host) and the mount table; then the root and
//!   every folder on the path, one listing each, and their rows (a path folder is drawn even
//!   when its parent's listing stopped before reaching it).
//! - **Phase 2**: each drawn row from one screen above the screen to one below it is listed
//!   once, nearest the bar first — while the tree is being built, around the current folder's
//!   row, so 05 §5 opens nothing it has not read. A row the rules keep unread (R1, R4–R7, H3) is
//!   not listed; a compact chain's next folder is (it is part of the row).
//! - **Phase 3**: each closed, sizable row on screen is scanned once, nearest the bar first —
//!   locally without a budget, remotely within [`REMOTE_SCAN_BUDGET`]; Space scans a row
//!   without one. A row off screen for more than [`LEAVE`] stops its scan, as does a row no
//!   longer drawn; what it reached is kept, in the row and in the process cache.

use super::{cache, Drawn, For, Node, RowId, Stage, Tree, View};
use crate::remote::treefs::{Ask, ScanAsk, LIST_BUDGET, REMOTE_SCAN_BUDGET};
use std::time::{Duration, Instant};

/// A row off screen this long stops its scan.
pub const LEAVE: Duration = Duration::from_secs(1);

/// Request priorities: the owner's and phase 1's first, then phase 2's, then phase 3's, each by
/// its row's distance to the bar.
const PHASE2: u64 = 1_000;
const PHASE3: u64 = 1_000_000;

impl Tree {
    /// Moves the tree on after answers or a change of view: the next phase's requests.
    pub(super) fn pump(&mut self) -> bool {
        if self.closed {
            return false;
        }
        let mut changed = false;
        loop {
            let moved = match self.stage {
                Stage::Resolving => self.root_known(),
                Stage::Path => self.path_listed(),
                Stage::Building(level) => {
                    self.list_near();
                    self.build(level)
                }
                Stage::Done | Stage::Failed(_) => false,
            };
            if !moved {
                break;
            }
            changed = true;
        }
        if self.ready() {
            self.list_near();
        }
        if self.stage == Stage::Done {
            self.size_on_screen();
        }
        changed
    }

    /// Phase 1's first step: once the folder, the home and the mount table are known, the root,
    /// the path's rows and their listings.
    fn root_known(&mut self) -> bool {
        let (Some(folder), Some(home), Some(_)) = (&self.folder, &self.home_got, &self.mounts)
        else {
            return false;
        };
        let rooted = if self.local {
            super::root::local(folder, home.as_ref())
        } else {
            super::root::remote(&folder.path, home.as_ref().map(|h| h.path.as_str()))
        };
        self.home = home.as_ref().map(|h| h.path.clone());
        self.root_is_home = rooted.home;
        let mut root = Node::new(rooted.root.clone(), rooted.root.clone(), None);
        root.hidden = false;
        root.on_path = true;
        root.open = true;
        self.nodes.push(root);
        self.root = 0;
        let mut at = 0;
        let rest = rooted.folder[rooted.root.len().min(rooted.folder.len())..].to_string();
        for part in rest.split('/').filter(|p| !p.is_empty()) {
            let path = crate::remote::join(&self.nodes[at].path, part);
            let mut node = Node::new(path, part.to_string(), Some(at));
            node.on_path = true;
            node.open = true;
            self.nodes.push(node);
            let k = self.nodes.len() - 1;
            self.nodes[at].children.push(k);
            at = k;
        }
        self.current = at;
        if at != self.root {
            // The current folder opens by 05 §5 like any other candidate.
            self.nodes[at].open = false;
        }
        for i in 0..self.nodes.len() {
            self.list_auto(i, 0);
        }
        if self.mounts.as_ref().is_some_and(|m| m.fences) {
            for i in 0..self.nodes.len() {
                self.statvfs(i, 0);
            }
            if self.mounts.as_ref().is_some_and(|m| m.macos) {
                self.ask(For::MacFence(0), 0, Ask::Statvfs("/".into()));
                self.ask(
                    For::MacFence(1),
                    0,
                    Ask::Statvfs("/System/Volumes/Data".into()),
                );
            }
        }
        self.stage = Stage::Path;
        true
    }

    /// Phase 1's end: every folder on the path has answered (or failed, or does not answer).
    fn path_listed(&mut self) -> bool {
        let waiting = self
            .nodes
            .iter()
            .any(|n| n.on_path && n.digest.is_none() && !self.settled(n));
        if waiting {
            return false;
        }
        self.stage = if self.params.d > 0 {
            Stage::Building(0)
        } else {
            Stage::Done
        };
        true
    }

    /// A listing that will not change any more by itself: answered, failed, or not answering.
    fn settled(&self, n: &Node) -> bool {
        n.asked.is_none() || n.failed || n.not_answering
    }

    /// 05 §5 at `level`, once the drawn rows of that level near the current folder are read
    /// (and, on a remote without a mount table, fenced).
    fn build(&mut self, level: usize) -> bool {
        let (drawn, _) = self.positions();
        let window = self.window(&drawn, true);
        let fences = self.mounts.as_ref().is_some_and(|m| m.fences);
        let unread = drawn[window.clone()].iter().any(|d| match *d {
            Drawn::Folder { end, .. } => {
                let n = &self.nodes[end];
                self.level(end) == level
                    && self.listable(end)
                    && (n.digest.is_none() && !(n.failed || n.not_answering)
                        || !self.settled(n)
                        || fences && n.fs_asked && !n.fs_done)
            }
            Drawn::Hidden { .. } => false,
        });
        if unread {
            return false;
        }
        self.auto_open(level);
        self.stage = if level + 1 < self.params.d {
            Stage::Building(level + 1)
        } else {
            Stage::Done
        };
        true
    }

    /// Whether a row is read without the owner opening it (05 §4 "listed unopened").
    pub(super) fn listable(&self, i: usize) -> bool {
        self.rule(i).is_none_or(|r| r.listed_unopened())
    }

    /// The rows phase 2 lists: one screen above the screen to one below it. While the tree is
    /// built the screen is centred on the current folder's row and E rows high.
    fn window(&self, drawn: &[Drawn], building: bool) -> std::ops::Range<usize> {
        let (top, height) = self.screen(drawn, building);
        top.saturating_sub(height)..(top + 2 * height).min(drawn.len())
    }

    /// The screen's first row and height.
    fn screen(&self, drawn: &[Drawn], building: bool) -> (usize, usize) {
        match self.view {
            Some(View { top, height }) if !building => (top, height),
            _ => {
                let height = self.params.e.max(1);
                let at = self.row_index(drawn, self.current).unwrap_or(0);
                (at.saturating_sub(height / 2), height)
            }
        }
    }

    /// The bar's row: the one the view names, else the current folder's.
    fn bar(&self, drawn: &[Drawn]) -> usize {
        self.bar
            .and_then(|id| drawn.iter().position(|d| self.id_of(d) == id))
            .or_else(|| self.row_index(drawn, self.current))
            .unwrap_or(0)
    }

    fn row_index(&self, drawn: &[Drawn], node: usize) -> Option<usize> {
        drawn.iter().position(|d| match *d {
            Drawn::Folder { head, end, .. } => self.chain_holds(head, end, node),
            Drawn::Hidden { .. } => false,
        })
    }

    /// Phase 2: lists the rows near the screen that were never listed, nearest the bar first;
    /// asks `statvfs` where 05 §4.1's fences or an R3 row's `=… fs` need it.
    fn list_near(&mut self) {
        let drawn = self.drawn();
        let building = matches!(self.stage, Stage::Building(_));
        let window = self.window(&drawn, building);
        let bar = self.bar(&drawn);
        let fences = self.mounts.as_ref().is_some_and(|m| m.fences);
        for (ri, d) in drawn
            .iter()
            .enumerate()
            .skip(window.start)
            .take(window.len())
        {
            let Drawn::Folder { end, .. } = *d else {
                continue;
            };
            let prio = PHASE2 + ri.abs_diff(bar) as u64;
            if !self.listable(end) {
                continue;
            }
            if fences {
                self.statvfs(end, prio);
            }
            self.list_auto(end, prio);
            if self.class(end) == Some(super::rules::Class::Disk) {
                self.statvfs(end, prio);
            }
        }
    }

    /// An automatic listing of `i`, unless it was ever listed or is being listed.
    fn list_auto(&mut self, i: usize, prio: u64) {
        let n = &self.nodes[i];
        if n.listed_once || n.asked.is_some() || n.digest.is_some() {
            return;
        }
        self.list_with(i, prio, false);
    }

    /// Requests a listing of `i`: bounded unless it is the owner's.
    pub(super) fn list_with(&mut self, i: usize, prio: u64, owner: bool) {
        if let Some(old) = self.nodes[i].asked {
            if !owner || old.owner {
                return;
            }
            self.nodes[i].asked = None;
            self.cancel(old.id);
        }
        let budget = (!owner).then_some(LIST_BUDGET);
        let path = self.nodes[i].path.clone();
        let id = self.ask(For::List(i), prio, Ask::List { path, budget });
        let n = &mut self.nodes[i];
        n.asked = Some(super::Asked { id, owner });
        n.listed_once = true;
        n.not_answering = false;
    }

    /// One `statvfs` of `i`, once.
    fn statvfs(&mut self, i: usize, prio: u64) {
        if self.nodes[i].fs_asked {
            return;
        }
        self.nodes[i].fs_asked = true;
        let path = self.nodes[i].path.clone();
        self.ask(For::Statvfs(i), prio, Ask::Statvfs(path));
    }

    /// Phase 3: scans the closed, sizable rows on screen nearest the bar first; stops the scans
    /// of rows off screen for more than [`LEAVE`], and of rows no longer drawn.
    fn size_on_screen(&mut self) {
        let drawn = self.drawn();
        let (top, height) = self.screen(&drawn, false);
        let bar = self.bar(&drawn);
        let now = Instant::now();
        let mut ends = std::collections::HashSet::new();
        for (ri, d) in drawn.iter().enumerate() {
            let Drawn::Folder { end, .. } = *d else {
                continue;
            };
            ends.insert(end);
            let on_screen = (top..top + height).contains(&ri);
            if on_screen {
                self.nodes[end].left = None;
                if self.scannable(end) {
                    self.scan(end, PHASE3 + ri.abs_diff(bar) as u64);
                }
            } else if let Some(id) = self.nodes[end].deep.running {
                match self.nodes[end].left {
                    None => self.nodes[end].left = Some(now),
                    Some(at) if now.duration_since(at) > LEAVE => {
                        self.stop_scan(end, id);
                    }
                    Some(_) => {}
                }
            }
        }
        // A folder closed above it: the row is gone.
        for i in 0..self.nodes.len() {
            if let Some(id) = self.nodes[i].deep.running {
                if !ends.contains(&i) {
                    self.stop_scan(i, id);
                }
            }
        }
    }

    fn stop_scan(&mut self, i: usize, id: u64) {
        self.store_partial(i);
        let n = &mut self.nodes[i];
        n.deep.running = None;
        n.left = None;
        self.cancel(id);
    }

    /// Whether phase 3 (or the owner's Space) sizes `i` now.
    fn scannable(&self, i: usize) -> bool {
        let n = &self.nodes[i];
        let Some(d) = &n.digest else {
            return false;
        };
        if n.deep.running.is_some() || n.deep.done || n.deep.not_answering || !d.complete {
            return false;
        }
        if d.real_dirs == 0 && !n.deep.wanted {
            return false;
        }
        let rule = self.rule(i);
        if n.deep.wanted {
            return rule.is_none_or(|r| r.sizable_on_space());
        }
        if n.link || n.open || rule.is_some_and(|r| !r.sized()) {
            return false;
        }
        // 05 §4.1: on a remote without a table, the fence first.
        !self.mounts.as_ref().is_some_and(|m| m.fences) || !n.fs_asked || n.fs_done
    }

    /// Starts the deep scan of `i` (where a stopped remote scan left off, if it did).
    fn scan(&mut self, i: usize, prio: u64) {
        let wanted = self.nodes[i].deep.wanted;
        let remote = !self.local;
        let resume = self.nodes[i].deep.frontier.take();
        let base = match &resume {
            Some(_) => self.nodes[i].deep.below.unwrap_or_default(),
            None => Default::default(),
        };
        let start = match resume {
            Some(frontier) => frontier,
            None => self.nodes[i]
                .children
                .iter()
                .filter(|&&c| !self.nodes[c].link && self.class(c).is_none())
                .map(|&c| self.nodes[c].path.clone())
                .collect(),
        };
        let path = self.nodes[i].path.clone();
        let fence = self.mounts.as_ref().filter(|m| m.fences).and_then(|m| {
            let own = self.nodes[i].fs?.fsid;
            let mut allowed = vec![own];
            if m.macos && self.mac_fsids.contains(&Some(own)) {
                allowed.extend(self.mac_fsids.iter().flatten());
            }
            Some(allowed)
        });
        let ask = ScanAsk {
            start,
            skip: self
                .mounts
                .as_ref()
                .map(|m| m.below(&path))
                .unwrap_or_default(),
            fence,
            budget: (remote && !wanted).then_some(REMOTE_SCAN_BUDGET),
            base,
            seen: self.nodes[i]
                .digest
                .as_ref()
                .map(|d| d.multi.clone())
                .unwrap_or_default(),
        };
        let id = self.ask(For::Scan(i), prio, Ask::Scan(ask));
        let n = &mut self.nodes[i];
        n.deep.running = Some(id);
        n.deep.cached = false;
        n.left = None;
    }

    /// Space: scans `i` now if it can be, else once its whole listing is in.
    pub(super) fn size_wanted(&mut self) {
        for i in 0..self.nodes.len() {
            if self.nodes[i].deep.wanted && self.scannable(i) {
                self.scan(i, 0);
            }
        }
    }

    /// The bar the view named, for distances.
    pub(super) fn set_bar(&mut self, bar: Option<RowId>) {
        self.bar = bar;
    }

    /// The process cache's figure for a new row (`~`).
    pub(super) fn cached_size(&mut self, k: usize) {
        if let Some(s) = cache::size(&self.host, &self.nodes[k].path) {
            let deep = &mut self.nodes[k].deep;
            deep.below = Some(s.totals);
            deep.complete = s.complete;
            deep.cached = true;
        }
    }
}
