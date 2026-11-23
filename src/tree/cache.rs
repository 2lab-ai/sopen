//! The per-process cache (05 §6.1, §6.3): finished and partial sizes keyed by host and resolved
//! path, in memory only — a size from it shows as `~9.8M`; nothing is written anywhere. The home
//! is kept here too, once per host (05 §2).

use crate::remote::treefs::{Resolved, Totals};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};

/// Sizes kept at most.
pub const SIZES: usize = 20_000;
/// Homes kept at most.
pub const HOMES: usize = 256;

/// A deep size an earlier scan reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sized {
    pub totals: Totals,
    pub complete: bool,
}

/// A map that keeps at most `cap` entries, forgetting the one used least recently.
struct Lru<K, V> {
    cap: usize,
    clock: u64,
    map: HashMap<K, (V, u64)>,
    /// Last use → key, oldest first.
    order: BTreeMap<u64, K>,
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> Lru<K, V> {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            clock: 0,
            map: HashMap::new(),
            order: BTreeMap::new(),
        }
    }

    fn touch(&mut self, key: &K) {
        self.clock += 1;
        if let Some((_, used)) = self.map.get_mut(key) {
            self.order.remove(used);
            *used = self.clock;
            self.order.insert(self.clock, key.clone());
        }
    }

    fn get(&mut self, key: &K) -> Option<V> {
        self.touch(key);
        self.map.get(key).map(|(v, _)| v.clone())
    }

    fn peek(&self, key: &K) -> Option<&V> {
        self.map.get(key).map(|(v, _)| v)
    }

    fn insert(&mut self, key: K, value: V) {
        self.clock += 1;
        if let Some((_, used)) = self.map.insert(key.clone(), (value, self.clock)) {
            self.order.remove(&used);
        }
        self.order.insert(self.clock, key);
        while self.map.len() > self.cap {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            self.map.remove(&oldest);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

/// The cache's maps, each bounded: over a long session it forgets what was used least recently.
pub struct Cache {
    homes: Lru<String, Resolved>,
    sizes: Lru<(String, String), Sized>,
}

impl Cache {
    pub fn new(sizes_cap: usize, homes_cap: usize) -> Self {
        Self {
            homes: Lru::new(homes_cap),
            sizes: Lru::new(sizes_cap),
        }
    }

    pub fn home(&mut self, host: &str) -> Option<Resolved> {
        self.homes.get(&host.to_string())
    }

    pub fn set_home(&mut self, host: &str, home: Resolved) {
        self.homes.insert(host.to_string(), home);
    }

    pub fn size(&mut self, host: &str, path: &str) -> Option<Sized> {
        self.sizes.get(&(host.to_string(), path.to_string()))
    }

    /// Keeps a scan's figure; a partial one never replaces a complete one of the same folder.
    pub fn store(&mut self, host: &str, path: &str, totals: Totals, complete: bool) {
        let key = (host.to_string(), path.to_string());
        if !complete && self.sizes.peek(&key).is_some_and(|s| s.complete) {
            self.sizes.touch(&key);
            return;
        }
        self.sizes.insert(key, Sized { totals, complete });
    }

    /// Entries kept: sizes, homes (tests).
    #[cfg(test)]
    pub fn len(&self) -> (usize, usize) {
        (self.sizes.len(), self.homes.len())
    }
}

fn cache() -> std::sync::MutexGuard<'static, Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(Cache::new(SIZES, HOMES)))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// The home of `host`, when it was read before in this process.
pub fn home(host: &str) -> Option<Option<Resolved>> {
    cache().home(host).map(Some)
}

pub fn set_home(host: &str, home: Option<Resolved>) {
    if let Some(home) = home {
        cache().set_home(host, home);
    }
}

/// The size an earlier scan of `path` on `host` reached.
pub fn size(host: &str, path: &str) -> Option<Sized> {
    cache().size(host, path)
}

/// Keeps a scan's figure in the process's cache.
pub fn store(host: &str, path: &str, totals: Totals, complete: bool) {
    cache().store(host, path, totals, complete);
}

/// Runs `f` on the process's cache (tests).
#[cfg(test)]
pub fn with<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
    f(&mut cache())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Item 13: past its bound the cache forgets what was used least recently.
    #[test]
    fn the_cache_keeps_the_most_recently_used() {
        let mut c = Cache::new(3, 2);
        let t = Totals::default();
        for p in ["/a", "/b", "/c"] {
            c.store("h", p, t, true);
        }
        assert!(c.size("h", "/a").is_some(), "a is used again");
        c.store("h", "/d", t, true);
        assert!(c.size("h", "/b").is_none(), "b, the least recent, went");
        for p in ["/a", "/c", "/d"] {
            assert!(c.size("h", p).is_some(), "{p}");
        }
        assert_eq!(c.len().0, 3);
        let home = |p: &str| Resolved {
            path: p.into(),
            ids: Vec::new(),
        };
        c.set_home("x", home("/x"));
        c.set_home("y", home("/y"));
        assert!(c.home("x").is_some());
        c.set_home("z", home("/z"));
        assert!(c.home("y").is_none());
        assert_eq!(c.len().1, 2);
    }
}
