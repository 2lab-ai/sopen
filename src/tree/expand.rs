//! Opening by itself (05 §5): the root and the path always open, then the candidates nearest an
//! anchor with 1 to N − 1 shown subfolders, within D levels and E added rows, decided once when F10
//! builds the tree (`T-auto`).
//!
//! The decision is made level by level on a settled snapshot ([`super::scan`] runs each level
//! only once every drawn row of it near the current folder is read), so the order listings
//! arrive in never changes what opens: all candidates of a level come before any of the next
//! (the order is by level first), and the rows of the next level are those the openings drew.

use super::{Drawn, Tree};

/// 05 §5's numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// A folder opens by itself with 1 to `n − 1` shown subfolders (the owner's 5).
    pub n: usize,
    /// … when it is fewer than `d` drawn levels below its nearest anchor.
    pub d: usize,
    /// The rows the tree shows: 05 §5 adds at most this many, and phase 2 lists this many rows
    /// around the screen's.
    pub e: usize,
}

impl Default for Params {
    fn default() -> Self {
        Self { n: 5, d: 2, e: 36 }
    }
}

impl Tree {
    /// 05 §5 at `level`, on the rows as they are: the candidates of that level nearest the
    /// current folder's row first (then the upper row), each opened when the rows it adds — its
    /// shown subfolders, and one `· N hidden` row if it has hidden ones — still fit E; one that
    /// does not fit stays closed and the next is tried.
    pub(super) fn auto_open(&mut self, level: usize) {
        loop {
            let (drawn, current) = self.positions();
            let current = current.unwrap_or(0);
            let best = drawn
                .iter()
                .enumerate()
                .filter_map(|(ri, d)| match *d {
                    Drawn::Folder { end, .. } if self.candidate(end, level) => {
                        Some((ri.abs_diff(current), ri, end))
                    }
                    _ => None,
                })
                .min();
            let Some((_, _, x)) = best else {
                return;
            };
            let (shown, hidden) = self.shown_hidden(x);
            let cost = shown + usize::from(hidden > 0);
            if self.auto_added + cost > self.params.e {
                self.nodes[x].refused = true;
                continue;
            }
            self.auto_added += cost;
            self.nodes[x].open = true;
            self.nodes[x].auto = true;
        }
    }

    /// A drawn folder 05 §5 may open at `level`: listed completely, 1 to N − 1 shown subfolders,
    /// no rule or heuristic of 05 §4 (the current folder too: only H1 gives way to the path),
    /// not opened or closed by the owner, not refused by the budget already.
    fn candidate(&self, x: usize, level: usize) -> bool {
        let n = &self.nodes[x];
        if n.open || n.touched || n.refused || n.link || x == self.root {
            return false;
        }
        // Strict ancestors are open by rule 1; the current folder keeps its rules (H1's path
        // override is in the rule itself).
        if (n.on_path && x != self.current) || self.rule(x).is_some() {
            return false;
        }
        if !n.digest.as_ref().is_some_and(|d| d.complete) {
            return false;
        }
        // 05 §4.1: on a remote without a table a folder is opened only once its fence, and its
        // parent's, are known.
        if self.mounts.as_ref().is_some_and(|m| m.fences) {
            let parent = n.parent.map(|p| self.nodes[p].fs);
            if n.fs.is_none() || parent.is_some_and(|f| f.is_none()) {
                return false;
            }
        }
        let (shown, _) = self.shown_hidden(x);
        (1..self.params.n).contains(&shown) && self.level(x) == level
    }

    /// `x`'s subfolder rows: shown, and grouped in its hidden row.
    pub(super) fn shown_hidden(&self, x: usize) -> (usize, usize) {
        let hidden = self.nodes[x]
            .children
            .iter()
            .filter(|&&c| self.grouped(c))
            .count();
        (self.nodes[x].children.len() - hidden, hidden)
    }

    /// Drawn levels between `x`'s row and its nearest anchor (the root, a strict ancestor of the
    /// current folder, or the current folder): a compact chain's folders share one level.
    pub(super) fn level(&self, x: usize) -> usize {
        let mut level = 0;
        let mut at = x;
        loop {
            if self.nodes[at].on_path {
                return level;
            }
            // Up to the first folder of `at`'s row, then to the row above.
            let mut head = at;
            while let Some(p) = self.nodes[head].parent {
                if self.chain_next(p) == Some(head) {
                    head = p;
                } else {
                    break;
                }
            }
            match self.nodes[head].parent {
                Some(p) => {
                    level += 1;
                    at = p;
                }
                None => return level,
            }
        }
    }
}
