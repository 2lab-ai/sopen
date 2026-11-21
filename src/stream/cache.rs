//! The block cache of one stream (06 §3): 64 KiB blocks, 64 MiB of RAM, least recently used
//! first. Blocks inside a live reader's window are spared while anything else can go; the bound
//! itself is hard, and a window yields to it. Nothing a reader waits for depends on what the
//! cache keeps: a waiter holds its own references to the blocks that came for it
//! (`Prefetcher::deliver`), so the cache evicts freely.

use bytes::Bytes;
use std::collections::{BTreeMap, HashMap};

/// One block, and one read request (06 §3).
pub const BLOCK: u64 = 64 * 1024;
/// RAM per stream: 1024 blocks.
pub const CACHE_BYTES: u64 = 64 << 20;
/// What the readers' protected windows may take together before each is scaled down.
pub const PROTECTED_BYTES: u64 = 48 << 20;
/// A scaled window never goes below these.
pub const FLOOR_AHEAD: u64 = 256 << 10;
pub const FLOOR_BEHIND: u64 = 64 << 10;

/// What eviction spares, as inclusive block ranges.
#[derive(Debug, Default, Clone)]
pub struct Protect {
    /// The readers' `[cursor − behind, cursor + ahead]`.
    pub windows: Vec<(u64, u64)>,
}

impl Protect {
    fn spares(&self, block: u64) -> bool {
        self.windows.iter().any(|(a, b)| (*a..=*b).contains(&block))
    }
}

/// Each reader's `(ahead, behind)` once the windows share [`PROTECTED_BYTES`]: when together
/// they would pass it, a window larger than an equal share is scaled down to that share, never
/// below the floors (or the reader's own smaller maximum).
pub fn shares(wants: &[(u64, u64)]) -> Vec<(u64, u64)> {
    // In 128 bits: a window, or their sum, may pass 64.
    let total: u128 = wants.iter().map(|(a, b)| *a as u128 + *b as u128).sum();
    if total <= PROTECTED_BYTES as u128 {
        return wants.to_vec();
    }
    let share = PROTECTED_BYTES / wants.len() as u64;
    wants
        .iter()
        .map(|&(ahead, behind)| {
            let window = ahead as u128 + behind as u128;
            if window <= share as u128 {
                return (ahead, behind);
            }
            let a = (ahead as u128 * share as u128 / window) as u64;
            let b = share - a;
            (
                a.max(FLOOR_AHEAD.min(ahead)),
                b.max(FLOOR_BEHIND.min(behind)),
            )
        })
        .collect()
}

/// The validated blocks of one stream. Only blocks whose fence returned the fingerprint are put
/// here (06 §5).
pub struct BlockCache {
    limit: usize,
    blocks: HashMap<u64, (Bytes, i64)>,
    /// Age → block: the first is the next to go.
    order: BTreeMap<i64, u64>,
    hot: i64,
    cold: i64,
}

impl Default for BlockCache {
    fn default() -> Self {
        Self::with_blocks((CACHE_BYTES / BLOCK) as usize)
    }
}

impl BlockCache {
    pub fn with_blocks(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            blocks: HashMap::new(),
            order: BTreeMap::new(),
            hot: 0,
            cold: 0,
        }
    }

    pub fn contains(&self, block: u64) -> bool {
        self.blocks.contains_key(&block)
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Bytes held.
    pub fn bytes(&self) -> u64 {
        self.blocks.values().map(|(b, _)| b.len() as u64).sum()
    }

    /// The block; `touch` makes it the most recently used (a Bulk reader's read does not).
    pub fn get(&mut self, block: u64, touch: bool) -> Option<Bytes> {
        let (bytes, age) = self.blocks.get_mut(&block)?;
        if touch {
            self.order.remove(age);
            self.hot += 1;
            *age = self.hot;
            self.order.insert(self.hot, block);
        }
        Some(bytes.clone())
    }

    /// The block, its age untouched.
    pub fn peek(&self, block: u64) -> Option<Bytes> {
        self.blocks.get(&block).map(|(bytes, _)| bytes.clone())
    }

    /// Adds a block of at most [`BLOCK`] bytes, evicting to stay within the bound. `cold` (a
    /// Bulk reader's block) puts it at the cold end: the next to go.
    pub fn insert(&mut self, block: u64, bytes: Bytes, cold: bool, protect: &Protect) {
        debug_assert!(
            bytes.len() as u64 <= BLOCK,
            "a block of {} bytes",
            bytes.len()
        );
        if let Some((_, age)) = self.blocks.remove(&block) {
            self.order.remove(&age);
        }
        while self.blocks.len() >= self.limit {
            self.evict(protect);
        }
        let age = if cold {
            self.cold -= 1;
            self.cold
        } else {
            self.hot += 1;
            self.hot
        };
        self.blocks.insert(block, (bytes, age));
        self.order.insert(age, block);
    }

    /// Drops the least recently used block no window spares, or else the least recently
    /// used: the bound is hard.
    fn evict(&mut self, protect: &Protect) {
        let pick = self
            .order
            .iter()
            .find(|(_, block)| !protect.spares(**block))
            .or_else(|| self.order.iter().next())
            .map(|(age, block)| (*age, *block));
        if let Some((age, block)) = pick {
            self.order.remove(&age);
            self.blocks.remove(&block);
        }
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.order.clear();
    }
}
