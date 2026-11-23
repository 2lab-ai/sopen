//! The per-process cache (05 §6.1, §6.3): finished and partial sizes keyed by host and resolved
//! path, in memory only — a size from it shows as `~9.8M`; nothing is written anywhere. The home
//! is kept here too, once per host (05 §2).

use crate::remote::treefs::{Resolved, Totals};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// A deep size an earlier scan reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sized {
    pub totals: Totals,
    pub complete: bool,
}

#[derive(Default)]
struct Cache {
    /// Host → its home, once it was read.
    homes: HashMap<String, Option<Resolved>>,
    sizes: HashMap<(String, String), Sized>,
}

fn cache() -> std::sync::MutexGuard<'static, Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// The home of `host`, when it was read before in this process.
pub fn home(host: &str) -> Option<Option<Resolved>> {
    cache().homes.get(host).cloned()
}

pub fn set_home(host: &str, home: Option<Resolved>) {
    cache().homes.insert(host.to_string(), home);
}

/// The size an earlier scan of `path` on `host` reached.
pub fn size(host: &str, path: &str) -> Option<Sized> {
    cache()
        .sizes
        .get(&(host.to_string(), path.to_string()))
        .copied()
}

/// Keeps a scan's figure; a partial one never replaces a complete one of the same folder.
pub fn store(host: &str, path: &str, totals: Totals, complete: bool) {
    let mut c = cache();
    let key = (host.to_string(), path.to_string());
    if !complete && c.sizes.get(&key).is_some_and(|s| s.complete) {
        return;
    }
    c.sizes.insert(key, Sized { totals, complete });
}
