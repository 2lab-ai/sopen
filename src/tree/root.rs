//! The root (05 §2): the folder resolved by `realpath` and the home once per host; the root is the
//! home when the folder is the home or inside it — locally by `(st_dev, st_ino)`, remotely by a
//! component-wise prefix of the resolved paths — otherwise `/` (`T-root`).

use crate::remote::treefs::Resolved;

/// Where the tree starts, and where its bar starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rooted {
    /// The root's path: the home, or `/`.
    pub root: String,
    /// The root is the home (its row reads `~`).
    pub home: bool,
    /// The current folder as the tree names it: under the home when it is inside it
    /// (`/System/Volumes/Data/Users/u/a` → `/Users/u/a`), else as resolved.
    pub folder: String,
}

/// Locally: "inside" is identity — the folder, or one of its ancestors, has the home's
/// `(st_dev, st_ino)`; a string test fails where `realpath` keeps an alias (05 §2).
pub fn local(folder: &Resolved, home: Option<&Resolved>) -> Rooted {
    let home_id = home.and_then(|h| h.ids.last().map(|(_, id)| *id));
    let (Some(home), Some(home_id)) = (home, home_id) else {
        return slash(&folder.path);
    };
    if home.path == "/" {
        return slash(&folder.path);
    }
    // The deepest ancestor (the folder itself first) that is the home.
    for (at, id) in folder.ids.iter().rev() {
        if *id == home_id {
            let rest = folder.path[at.len().min(folder.path.len())..].trim_start_matches('/');
            return Rooted {
                root: home.path.clone(),
                home: true,
                folder: crate::remote::join(&home.path, rest)
                    .trim_end_matches('/')
                    .to_string(),
            };
        }
    }
    slash(&folder.path)
}

/// Remotely (SFTP has no inode): a component-wise prefix of the resolved paths — home `/home/ab`
/// does not contain `/home/abc/x`.
pub fn remote(folder: &str, home: Option<&str>) -> Rooted {
    match home {
        Some(home) if home != "/" && inside(folder, home) => Rooted {
            root: home.to_string(),
            home: true,
            folder: folder.to_string(),
        },
        _ => slash(folder),
    }
}

/// `path` is `dir` or below it, by whole components.
pub fn inside(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    path == dir || dir.is_empty() || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

fn slash(folder: &str) -> Rooted {
    Rooted {
        root: "/".to_string(),
        home: false,
        folder: folder.to_string(),
    }
}
