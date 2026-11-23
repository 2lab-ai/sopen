//! The directory tree's reads (05 §7): bulk listings with device, inode, flags and mount status,
//! the mount table, `statvfs`, the home and `realpath`, and the deep size of a folder. They are
//! made by a **worker with a request channel and a result channel**, [`TreeFs`], not by a
//! `&mut self` call per request (05 §7): [`LocalFs::tree`](super::LocalFs::tree) builds the local
//! one in `local/treefs.rs` — the watchdog pool of 05 §6.2 running [`Disk`]'s calls — and
//! [`SftpRemote::into_tree`](super::SftpRemote::into_tree) the remote one in `sftp/treefs.rs`,
//! which takes the tree's own connection and its runtime. Both are submodules, which see their
//! parent's private fields, so `sftp.rs`, `local.rs` and [`RemoteFs`](super::RemoteFs) stay as
//! they are (02 §5). Nothing runs on the remote: SFTP requests only (05 §6.3).
//!
//! Every request carries an id of the asker's choosing and a priority (lower first); its answer
//! comes back with that id ([`Reply`]). A request that has not answered in [`NOT_ANSWERING`] is
//! reported once as [`Got::NotAnswering`]: locally its call is abandoned then and its answer
//! never comes (05 §6.2); remotely it keeps waiting up to the request timeout. A cancelled
//! request's answer never comes either.

use super::Wake;
use std::time::Duration;

/// A request with no answer this long is reported `not answering` (05 §6.2).
pub const NOT_ANSWERING: Duration = Duration::from_secs(2);

/// Every automatic listing stops at the first of these (05 §6.3).
pub const LIST_BUDGET: Budget = Budget {
    entries: 10_000,
    time: Duration::from_secs(2),
};

/// Remote requests in flight at most (05 §6.4).
pub const IN_FLIGHT: usize = 16;

/// A remote row's automatic deep scan stops at the first of these (05 §6.3).
pub const REMOTE_SCAN_BUDGET: ScanBudget = ScanBudget {
    listings: 300,
    entries: 200_000,
    time: Duration::from_secs(10),
};

/// The first 43 bytes of a `CACHEDIR.TAG` that marks a cache folder (R8).
pub const CACHEDIR_SIGNATURE: &[u8; 43] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// What a link points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Dir,
    File,
    /// Anything else, a broken link, or not asked (a scan's listing does not follow links, and a
    /// row's listing whose budget ran out stops asking).
    Other,
    /// Not read: the target lies at or below a mount the rules forbid touching (R4–R6, 05 §4),
    /// where even a `stat` can mount a share or hang. Shown as a link row; Enter resolves it.
    Unread,
}

/// An entry's type, as `lstat` gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Link(Target),
    /// A special file, or an entry whose attributes could not be read.
    Other,
}

/// A subfolder's mount status as the listing reports it (macOS `ATTR_DIR_MOUNTSTATUS`, Linux
/// `STATX_ATTR_MOUNT_ROOT` / `STATX_ATTR_AUTOMOUNT`); never known over SFTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mnt {
    #[default]
    No,
    /// A mount point.
    Point,
    /// An automounter trigger: a lookup below it can mount a share.
    Trigger,
}

/// One entry of a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    /// The name as shown: a name that is not UTF-8 has U+FFFD for its bad bytes.
    pub name: String,
    /// The name is not UTF-8: it is counted, never a row (paths here are UTF-8).
    pub lossy: bool,
    pub kind: Kind,
    /// A regular file's bytes (0 for anything else).
    pub size: u64,
    /// `(st_dev, st_ino)`, locally; `None` over SFTP.
    pub id: Option<(u64, u64)>,
    /// The link count, locally (a file with more than one is counted once per inode); 1 over
    /// SFTP.
    pub nlink: u32,
    /// The macOS hidden flag (`UF_HIDDEN`).
    pub hidden: bool,
    pub mnt: Mnt,
}

impl Child {
    /// A plain entry: no inode, one link, no flags (SFTP's, and the tests').
    pub fn plain(name: &str, kind: Kind, size: u64) -> Self {
        Self {
            name: name.to_string(),
            lossy: false,
            kind,
            size,
            id: None,
            nlink: 1,
            hidden: false,
            mnt: Mnt::No,
        }
    }
}

/// One folder's entries (`.` and `..` left out), in the order read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    pub children: Vec<Child>,
    /// Every entry was read; `false`: the listing stopped at its [`Budget`].
    pub complete: bool,
    /// The folder holds a `CACHEDIR.TAG` whose first 43 bytes are [`CACHEDIR_SIGNATURE`] (one
    /// 43-byte read, made only when the name is there).
    pub cachedir: bool,
}

/// Where an automatic listing stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub entries: usize,
    pub time: Duration,
}

/// One mounted filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// The mount point, absolute.
    pub at: String,
    pub fstype: String,
    /// macOS `MNT_DONTBROWSE`.
    pub nobrowse: bool,
}

/// What the host says of its mounts (05 §4, §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mounts {
    /// The mount table: locally always; remotely a Linux host's `/proc/self/mountinfo`.
    pub table: Option<Vec<Mount>>,
    /// A macOS host (locally: this build; remotely: `realpath("/tmp")` is `/private/tmp` and
    /// `/System/Volumes/Data` is there).
    pub macos: bool,
    /// `statvfs` can be asked (locally always; remotely `statvfs@openssh.com`).
    pub statvfs: bool,
}

/// One `statvfs` of the filesystem holding a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsStat {
    /// `(f_blocks − f_bfree) × f_frsize`.
    pub used_bytes: u64,
    /// `f_files − f_ffree`.
    pub used_files: u64,
    pub fsid: u64,
}

/// A path resolved by `realpath`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub path: String,
    /// Locally: `(st_dev, st_ino)` of `/`, of each folder below it on the way and of the path
    /// itself, in that order (05 §2's identity test). Empty over SFTP, which has no inode.
    pub ids: Vec<(String, (u64, u64))>,
}

/// Where a deep scan stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanBudget {
    pub listings: u64,
    pub entries: u64,
    pub time: Duration,
}

/// What a deep scan has counted so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    /// Bytes of the regular files found, a hard-linked file once (locally).
    pub bytes: u64,
    pub entries: u64,
    pub listings: u64,
    /// Folders that could not be listed: more may be below them.
    pub unread: u64,
    /// Folders listed only in part (a listing stopped at its budget): more may be in them.
    pub partial: u64,
    /// Of `bytes`, those of files with more than one link (each inode once): the part another
    /// scan may have counted too (05 §6.1).
    pub multi: u64,
    pub time: Duration,
}

impl Totals {
    /// Whether `budget` has run out.
    pub fn over(&self, budget: Option<ScanBudget>) -> bool {
        budget.is_some_and(|b| {
            self.listings >= b.listings || self.entries >= b.entries || self.time >= b.time
        })
    }
}

/// A deep scan (05 §6.3 phase 3): breadth first from `start`, on one filesystem, links not
/// followed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanAsk {
    /// The folders to list first: a row's subfolders (its own listing is the row's), or where a
    /// stopped scan left off.
    pub start: Vec<String>,
    /// Mount points: never entered (another filesystem: not part of the size).
    pub skip: Vec<String>,
    /// Folders never entered whose content is part of the size all the same (H3): one met is
    /// counted as read in part, so the size keeps its `+`.
    pub hold: Vec<String>,
    /// A remote without a mount table (05 §4.1): one `statvfs` before each folder, which is
    /// entered only when its `f_fsid` is one of these.
    pub fence: Option<Vec<u64>>,
    /// `None`: no budget (local scans, and the owner's Space).
    pub budget: Option<ScanBudget>,
    /// Each folder's listing stops at this (05 §6.3: every automatic listing is bounded).
    pub listing: Option<Budget>,
    /// What earlier scans of the same row counted: the budget counts from it.
    pub base: Totals,
    /// Inodes already counted (the row's own hard-linked files).
    pub seen: Vec<(u64, u64)>,
}

/// How a deep scan ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanEnd {
    /// Everything below was listed (but the `unread` folders).
    Complete,
    /// The budget ran out: the folders not listed yet.
    Budget(Vec<String>),
}

/// A deep scan's progress, or its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scanned {
    pub totals: Totals,
    /// `None`: still running.
    pub end: Option<ScanEnd>,
}

/// A request of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// `realpath`, with the identities on the way locally.
    Resolve(String),
    /// The home: locally `$HOME` resolved; remotely `expand-path@openssh.com` of `~<user>` (the
    /// account's home even when the start folder was moved), else `realpath(".")`.
    Home(Option<String>),
    Mounts,
    /// A row's listing; `budget: None`: whole (the owner's action). A link whose target lies at
    /// or below one of `guard` (mounts of R4–R6) is not followed: [`Target::Unread`].
    List {
        path: String,
        budget: Option<Budget>,
        guard: Vec<String>,
    },
    Statvfs(String),
    Scan(ScanAsk),
}

/// An answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Got {
    Resolved(Result<Resolved, String>),
    Home(Result<Resolved, String>),
    Mounts(Mounts),
    Listed(Result<Listing, String>),
    Statvfs(Result<FsStat, String>),
    Scan(Scanned),
    /// No answer for [`NOT_ANSWERING`].
    NotAnswering,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub id: u64,
    pub got: Got,
}

/// The tree's reads of one host, as a worker (05 §7). Nothing here blocks the caller: `send`
/// queues, `next` takes what has come, `close` stops everything and never waits — a call
/// blocked in the kernel is left behind (05 §6.2).
pub trait TreeFs: Send {
    /// Queues `ask` under `id`; `prio` orders what is waiting (lower first).
    fn send(&mut self, id: u64, prio: u64, ask: Ask);
    /// Drops the request: remotely its future, locally its answer (a call already running in the
    /// kernel cannot be stopped); a scan lists nothing more.
    fn cancel(&mut self, id: u64);
    /// The next answer, waiting up to `wait` (zero: only what has come).
    fn next(&mut self, wait: Duration) -> Option<Reply>;
    /// Called after every answer sent (the window's repaint).
    fn set_wake(&mut self, wake: Wake);
    /// Cancels everything and ends the worker, without waiting for it.
    fn close(&mut self);
    /// Why no call starts, when that is so: the process's abandoned calls are at their cap
    /// (05 §6.2). Always `None` remotely.
    fn held(&self) -> Option<String>;
    /// The local disk (the identity root rule, unbudgeted scans), not a remote.
    fn is_local(&self) -> bool;
}

/// A call's link to the watchdog: `beat` says it is still making progress (the 2 s watchdog runs
/// from the last beat, so a long listing that is moving is never abandoned), `stop` that its
/// answer is no longer wanted.
pub struct Live<'a> {
    pub beat: &'a dyn Fn(),
    pub stop: &'a dyn Fn() -> bool,
}

/// The local tree's filesystem calls, each blocking; [`LocalFs::tree`](super::LocalFs::tree)
/// runs them on the watchdog pool (05 §6.2). The real one is the disk (`local/treefs.rs`); tests
/// put a fake behind the same trait (a mount table, calls that never return, counts).
pub trait Disk: Send + Sync + 'static {
    /// One folder; `full`: a row's listing — each link's target kind (never at or below one of
    /// `guard`) and the `CACHEDIR.TAG` read — rather than a scan's.
    fn list(
        &self,
        path: &str,
        budget: Option<Budget>,
        full: bool,
        guard: &[String],
        live: &Live,
    ) -> std::io::Result<Listing>;
    fn resolve(&self, path: &str) -> std::io::Result<Resolved>;
    fn home(&self) -> std::io::Result<Resolved>;
    fn mounts(&self) -> std::io::Result<Mounts>;
    fn statvfs(&self, path: &str) -> std::io::Result<FsStat>;
}

/// `/proc/self/mountinfo` (Linux, locally and over SFTP): field 5 is the mount point (octal
/// escapes such as `\040` decoded), the field after `-` the filesystem type.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    fn unescape(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            let octal = b
                .get(i + 1..i + 4)
                .filter(|d| d.iter().all(|c| (b'0'..=b'7').contains(c)));
            match (b[i], octal) {
                (b'\\', Some(d)) => {
                    out.push((d[0] - b'0') * 64 + (d[1] - b'0') * 8 + (d[2] - b'0'));
                    i += 4;
                }
                (c, _) => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    let mut table = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let Some(dash) = fields.iter().position(|f| *f == "-") else {
            continue;
        };
        if dash < 5 || dash + 1 >= fields.len() {
            continue;
        }
        table.push(Mount {
            at: unescape(fields[4]),
            fstype: fields[dash + 1].to_string(),
            nobrowse: false,
        });
    }
    table
}

/// `path` with `.` and `..` taken out by its words alone (no link is read): `/a/b/../c` →
/// `/a/c`; above `/` stays at `/`.
pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Whether `path` is one of `guard` or below one (by whole components).
pub fn guarded(path: &str, guard: &[String]) -> bool {
    guard.iter().any(|g| {
        let g = g.trim_end_matches('/');
        path == g || g.is_empty() || path.strip_prefix(g).is_some_and(|r| r.starts_with('/'))
    })
}

/// Whether `head` (a file's first bytes) marks a cache folder (R8).
pub fn is_cachedir_tag(head: &[u8]) -> bool {
    head.starts_with(CACHEDIR_SIGNATURE)
}

/// What a listing adds to a deep scan: its regular files' bytes (an inode with more than one
/// link once, by `seen`) and entries, and the subfolders to enter — not links, not mount points
/// (by `skip` and by the listing's own mount status), and not the folders of `hold`, each of
/// which leaves the size a lower bound (`partial`).
pub(crate) fn scan_step(
    dir: &str,
    listing: &Listing,
    skip: &std::collections::HashSet<String>,
    hold: &std::collections::HashSet<String>,
    seen: &mut std::collections::HashSet<(u64, u64)>,
    totals: &mut Totals,
) -> Vec<String> {
    let mut below = Vec::new();
    totals.listings += 1;
    if !listing.complete {
        totals.partial += 1;
    }
    for child in &listing.children {
        totals.entries += 1;
        match child.kind {
            Kind::File => {
                let (first, multi) = match child.id {
                    Some(id) if child.nlink > 1 => (seen.insert(id), true),
                    _ => (true, false),
                };
                if first {
                    totals.bytes += child.size;
                    if multi {
                        totals.multi += child.size;
                    }
                }
            }
            Kind::Dir if !child.lossy && child.mnt == Mnt::No => {
                let path = super::join(dir, &child.name);
                if hold.contains(&path) {
                    totals.partial += 1;
                } else if !skip.contains(&path) {
                    below.push(path);
                }
            }
            _ => {}
        }
    }
    below
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_is_read_by_its_fields_with_escapes() {
        let text = "22 1 0:21 / / rw,relatime shared:1 - btrfs /dev/nvme0n1p2 rw\n\
                    25 22 0:5 / /proc rw,nosuid - proc proc rw\n\
                    30 22 0:30 / /mnt/my\\040disk rw - ext4 /dev/sdb1 rw\n\
                    31 22 0:31 / /run/user/1000/doc rw shared:9 master:3 - fuse.portal portal rw\n\
                    garbage line\n";
        let table = parse_mountinfo(text);
        let got: Vec<(&str, &str)> = table
            .iter()
            .map(|m| (m.at.as_str(), m.fstype.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("/", "btrfs"),
                ("/proc", "proc"),
                ("/mnt/my disk", "ext4"),
                ("/run/user/1000/doc", "fuse.portal"),
            ]
        );
    }

    #[test]
    fn a_cachedir_tag_is_its_signature() {
        assert!(is_cachedir_tag(
            b"Signature: 8a477f597d28d172789f06886806bc55\n# made by cargo"
        ));
        assert!(!is_cachedir_tag(
            b"Signature: 8a477f597d28d172789f06886806bc5"
        ));
        assert!(!is_cachedir_tag(
            b"signature: 8a477f597d28d172789f06886806bc55"
        ));
    }
}
