//! The edit journal (`.prd/07-streaming-edit.md` §3, §3.1): every unsaved edit of the hex, image
//! and media windows is kept on the local disk as well as in memory, so that a crash, a kill or a
//! power loss loses at most the edits the window had not yet shown as kept.
//!
//! - One directory per editing session, `<cache>/.edits/<host>/<abs path>.journal-<UTC stamp>-<8
//!   hex>/`, made at the first edit ([`Journal::create`]): owner-only (0700, files 0600), created
//!   exclusively, never through a link, its creation fsynced in its parent.
//! - Each generation is one immutable file `gen-<n>` holding the baseline, the evidence (`seen`)
//!   and the edits together, with its own SHA-256 on its last line. `CURRENT` names the current
//!   one and is replaced atomically ([`Journal::publish`]). Pasted pictures are hash-named blobs,
//!   made durable before the generation that names them.
//! - A Save records what it is doing in `save` before each request that changes the remote
//!   ([`Journal::begin_save`] and the phases after it), so that a reopen can tell whether a
//!   cut-off Save landed ([`reconcile`]).
//! - The next open lists the journals of the file whose lock it can take ([`orphans`]), checks
//!   their content against the file as it is now ([`restore_check`]) and offers Restore, Later or
//!   Discard.
//!
//! The editors' edits are opaque text here (`.hexpatch` lines, image operations, the media
//! timeline); the evidence is typed ([`Seen`], [`PostEvidence`]). All file system work goes
//! through [`Fs`], so the tests stop the process — or cut the power — before any step. Nothing
//! here talks to a remote: what reconciliation and Restore need from it comes through
//! [`Inspect`], which the save code implements.

use crate::remote::{Fingerprint, Stat, HASH_CHECK_MAX};
use anyhow::{anyhow, bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub type Sha = [u8; 32];

/// The hex window's page: `seen` holds the SHA-256 of each edited page as shown.
pub const PAGE: u64 = 4096;
/// Media above 256 MiB: `seen` holds the SHA-256 of the first and the last MiB.
pub const ENDS: u64 = 1 << 20;
/// A generation is written at most this long after the first change since the last one.
pub const DELAY: Duration = Duration::from_millis(250);

/// Restore's question when the journal has no evidence (`seen: pending`): its row then offers
/// Restore anyway, Later and Discard.
pub const ASK_PENDING: &str = "these edits were made before the file could be checked";
/// Restore's note when the metadata changed but the edited pages did not.
pub const NOTE_PAGES: &str = "the file changed elsewhere since; the bytes under your edits did not";
/// Restore's note for media above 256 MiB.
pub const NOTE_ENDS: &str =
    "checked size, modification time and the first and last MiB, not the whole content";

pub fn sha256(bytes: &[u8]) -> Sha {
    Sha256::digest(bytes).into()
}

// ---------------------------------------------------------------------------------------------
// The file system seam.

/// What is at a path, as [`Fs::node`] tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    Dir,
    File,
    /// A symbolic link (only when not following).
    Link,
    /// A fifo, socket or device.
    Other,
}

/// A lock taken by [`Fs::lock`]: held until dropped.
pub struct Held {
    _guard: Box<dyn Send>,
}

impl Held {
    pub fn new(guard: impl Send + 'static) -> Held {
        Held {
            _guard: Box::new(guard),
        }
    }
}

/// The file system steps a journal takes. Each mutating call is one step a crash can come
/// before; [`Disk`] is the real one.
pub trait Fs: Send + Sync {
    /// A new directory, 0700; fails if anything is at `path` (a link included).
    fn create_dir(&self, path: &Path) -> io::Result<()>;
    /// A new file holding `bytes`, 0600, created exclusively, never through a link. Not yet
    /// durable.
    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// Makes a file's data durable (fsync).
    fn sync_file(&self, path: &Path) -> io::Result<()>;
    /// Makes a directory's entries durable (fsync on the directory).
    fn sync_dir(&self, path: &Path) -> io::Result<()>;
    /// Renames `from` to `to` in the same directory, replacing a file at `to`.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    /// Removes an empty directory.
    fn remove_dir(&self, path: &Path) -> io::Result<()>;
    /// A whole file, never through a link.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// A directory's entry names, sorted.
    fn list(&self, dir: &Path) -> io::Result<Vec<String>>;
    /// What is at `path` (`None`: nothing), following a final link only with `follow`.
    fn node(&self, path: &Path, follow: bool) -> io::Result<Option<Node>>;
    /// Takes the advisory lock on the file at `path` without waiting: `None` when another
    /// holder has it. With `create` the file is made first, exclusively (`AlreadyExists` when
    /// it is there); without, a missing file is `NotFound`.
    fn lock(&self, path: &Path, create: bool) -> io::Result<Option<Held>>;
}

/// The local disk.
pub struct Disk;

/// The local disk as a shared [`Fs`].
pub fn disk() -> Arc<dyn Fs> {
    Arc::new(Disk)
}

fn open_nofollow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

impl Fs for Disk {
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        DirBuilder::new().mode(0o700).create(path)
    }

    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        file.write_all(bytes)
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        // fsync flushes the file, whichever descriptor asks (F_FULLFSYNC on macOS).
        open_nofollow(path)?.sync_all()
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        File::open(path)?.sync_all()
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_dir(path)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        open_nofollow(path)?.read_to_end(&mut out)?;
        Ok(out)
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }

    fn node(&self, path: &Path, follow: bool) -> io::Result<Option<Node>> {
        let meta = if follow {
            std::fs::metadata(path)
        } else {
            std::fs::symlink_metadata(path)
        };
        let kind = match meta {
            Ok(meta) => meta.file_type(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(Some(if kind.is_symlink() {
            Node::Link
        } else if kind.is_dir() {
            Node::Dir
        } else if kind.is_file() {
            Node::File
        } else {
            Node::Other
        }))
    }

    fn lock(&self, path: &Path, create: bool) -> io::Result<Option<Held>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(create)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
        // The name may have been removed (a journal being retired) or replaced since it was
        // opened: a lock on a file no longer at `path` holds nothing.
        let held = file.metadata()?;
        match std::fs::symlink_metadata(path) {
            Ok(now) if now.dev() == held.dev() && now.ino() == held.ino() => {
                Ok(Some(Held::new(file)))
            }
            _ => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// What a generation holds.

/// The editor a journal belongs to. A journal of one kind is never applied in another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Hex,
    Image,
    Media,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Hex => "hex",
            Kind::Image => "image",
            Kind::Media => "media",
        }
    }

    fn parse(word: &[u8]) -> Result<Kind> {
        Ok(match word {
            b"hex" => Kind::Hex,
            b"image" => Kind::Image,
            b"media" => Kind::Media,
            _ => bail!("unknown kind"),
        })
    }
}

/// The SHA-256 of a file's first and last MiB (the whole file when it is smaller).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ends {
    pub first: Sha,
    pub last: Sha,
}

impl Ends {
    /// The ends of `bytes`, a whole file.
    pub fn of(bytes: &[u8]) -> Ends {
        let n = (ENDS as usize).min(bytes.len());
        Ends {
            first: sha256(&bytes[..n]),
            last: sha256(&bytes[bytes.len() - n..]),
        }
    }
}

/// Content evidence about one file. Which parts are set depends on the kind and the size
/// (§3 `seen`, §3.1's post-save table).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Evidence {
    /// Page offset (a multiple of [`PAGE`]) → the SHA-256 of that page's bytes.
    pub pages: BTreeMap<u64, Sha>,
    /// The whole file's SHA-256.
    pub whole: Option<Sha>,
    /// The first and last MiB.
    pub ends: Option<Ends>,
}

/// `seen`: the evidence Restore and Save check — hex: each edited page as shown; image: the
/// whole file; media: the whole file at most 256 MiB, else its ends — or `pending` until known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    Pending,
    Known(Evidence),
}

impl Seen {
    pub fn pages(pages: impl IntoIterator<Item = (u64, Sha)>) -> Seen {
        Seen::Known(Evidence {
            pages: pages.into_iter().collect(),
            ..Evidence::default()
        })
    }

    pub fn whole(sha: Sha) -> Seen {
        Seen::Known(Evidence {
            whole: Some(sha),
            ..Evidence::default()
        })
    }

    pub fn ends(ends: Ends) -> Seen {
        Seen::Known(Evidence {
            ends: Some(ends),
            ..Evidence::default()
        })
    }
}

/// A pasted picture (image), kept in the journal as `paste-<sha256>.png`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paste {
    pub sha256: Sha,
    pub png: Arc<[u8]>,
}

impl Paste {
    pub fn new(png: impl Into<Arc<[u8]>>) -> Paste {
        let png = png.into();
        Paste {
            sha256: sha256(&png),
            png,
        }
    }
}

/// What a window hands the journal for a generation: the baseline its edits are against, the
/// evidence, the edits as the editor serializes them, and every pasted picture they name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub baseline: Fingerprint,
    pub seen: Seen,
    pub edits: String,
    pub pastes: Vec<Paste>,
}

/// A generation by number and its own SHA-256 (the hash on its last line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenRef {
    pub n: u64,
    pub sha256: Sha,
}

/// A generation's `head`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub kind: Kind,
    pub host: String,
    pub path: String,
    /// The sopen that wrote it ([`crate::version_text`]).
    pub version: String,
    /// When this generation was written.
    pub created: SystemTime,
    pub baseline: Fingerprint,
}

/// One whole generation, as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    pub at: GenRef,
    pub head: Head,
    pub seen: Seen,
    pub edits: String,
    /// The pastes the edits name, sorted.
    pub pastes: Vec<Sha>,
}

/// Content a generation holds, for telling whether a new one is needed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Content {
    baseline: Fingerprint,
    seen: Seen,
    edits: String,
    pastes: Vec<Sha>,
}

impl Content {
    fn of(snap: &Snapshot) -> Content {
        let pastes: BTreeSet<Sha> = snap.pastes.iter().map(|p| p.sha256).collect();
        Content {
            baseline: snap.baseline,
            seen: snap.seen.clone(),
            edits: snap.edits.clone(),
            pastes: pastes.into_iter().collect(),
        }
    }

    fn of_generation(gen: &Generation) -> Content {
        Content {
            baseline: gen.head.baseline,
            seen: gen.seen.clone(),
            edits: gen.edits.clone(),
            pastes: gen.pastes.clone(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The save intent.

/// A remote file's attributes as a Save records them: SFTP v3 carries no inode, so a temp's
/// identity is what can be compared (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    pub size: u64,
    pub mtime: SystemTime,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub mode: u32,
}

impl FileStat {
    pub fn of(stat: &Stat) -> FileStat {
        FileStat {
            size: stat.size,
            mtime: stat.mtime,
            uid: stat.uid,
            gid: stat.gid,
            mode: stat.mode,
        }
    }

    /// `now` (an `lstat`) is a regular file with exactly these attributes.
    pub fn matches(&self, now: &Stat) -> bool {
        now.is_file && !now.is_symlink && FileStat::of(now) == *self
    }

    fn fingerprint(&self, sha256: Option<Sha>) -> Fingerprint {
        Fingerprint {
            size: self.size,
            mtime: self.mtime,
            sha256,
        }
    }
}

/// How a Save writes the remote file (§3.1's phases by route).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Hex: `copy-data` into a temp, patch, rename (§4.2).
    ServerCopy,
    /// Hex: the whole file through this machine into a temp, rename (§4.3).
    StreamedRewrite,
    /// Image and media: the encoded or rendered output uploaded by `write_atomic` (§8, §9).
    Upload,
    /// Hex: the runs written into the target itself (§4.4).
    InPlace,
}

impl Route {
    fn word(self) -> &'static str {
        match self {
            Route::ServerCopy => "server-copy",
            Route::StreamedRewrite => "streamed-rewrite",
            Route::Upload => "upload",
            Route::InPlace => "in-place",
        }
    }

    fn parse(word: &[u8]) -> Result<Route> {
        Ok(match word {
            b"server-copy" => Route::ServerCopy,
            b"streamed-rewrite" => Route::StreamedRewrite,
            b"upload" => Route::Upload,
            b"in-place" => Route::InPlace,
            _ => bail!("unknown route"),
        })
    }
}

/// The post-save evidence of a temp route (§3.1), its two parts kept apart: `check` decides on a
/// reopen whether the Save landed; `next_baseline` and `next_seen` are what the next Save and a
/// later Restore compare against. Made by [`PostEvidence::hex`], [`PostEvidence::image`] or
/// [`PostEvidence::media`], which hold §3.1's table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostEvidence {
    pub kind: Kind,
    pub check: Evidence,
    pub next_baseline: Fingerprint,
    pub next_seen: Seen,
}

impl PostEvidence {
    /// Hex: each edited page's new SHA-256, and at most 256 MiB the new whole SHA-256 (computed
    /// while reading the temp back, §4.2 step 6) — for both parts.
    pub fn hex(
        staged: &FileStat,
        pages: BTreeMap<u64, Sha>,
        whole: Option<Sha>,
    ) -> Result<PostEvidence> {
        ensure!(!pages.is_empty(), "a hex save names the pages it changed");
        ensure!(
            pages.keys().all(|off| off % PAGE == 0),
            "page offsets are multiples of {PAGE}"
        );
        if staged.size <= HASH_CHECK_MAX {
            ensure!(
                whole.is_some(),
                "at most 256 MiB the new whole SHA-256 is known"
            );
        } else {
            ensure!(whole.is_none(), "above 256 MiB no whole SHA-256 is taken");
        }
        Ok(PostEvidence {
            kind: Kind::Hex,
            check: Evidence {
                pages: pages.clone(),
                whole,
                ends: None,
            },
            next_baseline: staged.fingerprint(whole),
            next_seen: Seen::pages(pages),
        })
    }

    /// Image: the output's whole SHA-256 is always known (sopen encoded it); reconciliation reads
    /// it back at most 256 MiB, above that the first and last MiB. The next baseline is the whole
    /// hash at every size, since image Restore checks a whole hash.
    pub fn image(staged: &FileStat, whole: Sha, ends: Option<Ends>) -> Result<PostEvidence> {
        let check = if staged.size <= HASH_CHECK_MAX {
            ensure!(ends.is_none(), "at most 256 MiB the whole hash decides");
            Evidence {
                whole: Some(whole),
                ..Evidence::default()
            }
        } else {
            ensure!(
                ends.is_some(),
                "above 256 MiB the first and last MiB decide"
            );
            Evidence {
                ends,
                ..Evidence::default()
            }
        };
        Ok(PostEvidence {
            kind: Kind::Image,
            check,
            next_baseline: staged.fingerprint(Some(whole)),
            next_seen: Seen::whole(whole),
        })
    }

    /// Media: at most 256 MiB the output's whole SHA-256, above that its first and last MiB —
    /// for both parts, as media Restore checks.
    pub fn media(
        staged: &FileStat,
        whole: Option<Sha>,
        ends: Option<Ends>,
    ) -> Result<PostEvidence> {
        let (check, seen) = if staged.size <= HASH_CHECK_MAX {
            let (Some(sha), None) = (whole, ends) else {
                bail!("at most 256 MiB a media save gives the whole SHA-256 alone");
            };
            (
                Evidence {
                    whole: Some(sha),
                    ..Evidence::default()
                },
                Seen::whole(sha),
            )
        } else {
            let (None, Some(e)) = (whole, ends) else {
                bail!("above 256 MiB a media save gives the first and last MiB alone");
            };
            (
                Evidence {
                    ends: Some(e),
                    ..Evidence::default()
                },
                Seen::ends(e),
            )
        };
        Ok(PostEvidence {
            kind: Kind::Media,
            check,
            next_baseline: staged.fingerprint(whole),
            next_seen: seen,
        })
    }
}

/// One run of a write in place: the bytes at `offset` before (`old`) and after (`new`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub offset: u64,
    pub old: Vec<u8>,
    pub new: Vec<u8>,
}

/// Where a Save is (§3.1). Each phase is made durable before the request that moves the Save
/// to it; a write-in-place Save has the one phase `Writing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// The temp's name, before the exclusive create. It is never assumed to be sopen's.
    Naming { temp: String },
    /// After the create's reply (`SSH_FXP_HANDLE`): the temp's stat at creation.
    Created { temp: String, created: FileStat },
    /// After the last write and fsync: the temp's stat and the post-save evidence.
    Staged {
        temp: String,
        created: FileStat,
        staged: FileStat,
        post: PostEvidence,
    },
    /// Just before the rename.
    Renaming {
        temp: String,
        created: FileStat,
        staged: FileStat,
        post: PostEvidence,
    },
    /// Before the first write in place (the undo file already durable): each run.
    Writing { undo: PathBuf, runs: Vec<Run> },
}

impl Phase {
    fn word(&self) -> &'static str {
        match self {
            Phase::Naming { .. } => "naming",
            Phase::Created { .. } => "created",
            Phase::Staged { .. } => "staged",
            Phase::Renaming { .. } => "renaming",
            Phase::Writing { .. } => "writing",
        }
    }

    fn temp(&self) -> Option<&str> {
        match self {
            Phase::Naming { temp }
            | Phase::Created { temp, .. }
            | Phase::Staged { temp, .. }
            | Phase::Renaming { temp, .. } => Some(temp),
            Phase::Writing { .. } => None,
        }
    }

    fn staged(&self) -> Option<(&FileStat, &PostEvidence)> {
        match self {
            Phase::Staged { staged, post, .. } | Phase::Renaming { staged, post, .. } => {
                Some((staged, post))
            }
            _ => None,
        }
    }
}

/// The `save` file: what a Save is doing, with the generation whose edits it writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveIntent {
    /// `gen-<m>`, kept until this Save is reconciled.
    pub saved: GenRef,
    pub kind: Kind,
    pub route: Route,
    /// The remote file the Save writes (its absolute path).
    pub target: String,
    pub started: SystemTime,
    /// The pre-save evidence: `gen-<m>`'s baseline and `seen`.
    pub baseline: Fingerprint,
    pub seen: Seen,
    pub phase: Phase,
}

/// How a Save will write, given to [`Journal::begin_save`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// A temp, then a rename: the temp's name is recorded (`naming`) before its create.
    Temp {
        route: Route,
        target: String,
        temp: String,
    },
    /// Write in place: the runs are recorded (`writing`) before the first write.
    InPlace {
        target: String,
        undo: PathBuf,
        runs: Vec<Run>,
    },
}

// ---------------------------------------------------------------------------------------------
// A live session's journal.

/// The journal of one editing session, holding its lock while it lives. Made at the first edit;
/// gone when nothing is unsaved ([`Journal::remove`], [`Journal::save_landed`]).
pub struct Journal {
    fs: Arc<dyn Fs>,
    dir: PathBuf,
    _lock: Held,
    kind: Kind,
    host: String,
    path: String,
    /// The number the next generation gets; a number once tried is never used again (its file
    /// may exist).
    next: u64,
    current: Option<GenRef>,
    current_pastes: Vec<Sha>,
    /// What `current` holds, for skipping a generation that would hold the same; `None` after a
    /// failed publication, whose `CURRENT` may already name it on disk.
    last: Option<Content>,
    /// The first number whose publication failed since the last that succeeded: `CURRENT` may
    /// name it (or a later one), so nothing from it on, and no paste, is removed until a
    /// generation is published again.
    unsettled: Option<u64>,
    /// Pastes known durable in the directory.
    kept_pastes: BTreeSet<Sha>,
    save: Option<SaveIntent>,
    saved_pastes: Vec<Sha>,
    /// A Save is running: no new generation until it ends (§3.1, §10).
    running: bool,
}

const LOCK: &str = "lock";
const CURRENT: &str = "CURRENT";
const SAVE: &str = "save";
const TOMBSTONE: &str = ".gone";

fn gen_name(n: u64) -> String {
    format!("gen-{n}")
}

fn paste_name(sha: &Sha) -> String {
    format!("paste-{}.png", crate::cache::hex(sha))
}

/// `gen-<n>` (also as a `.tmp`) → n.
fn gen_number(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("gen-")?;
    let digits = digits.strip_suffix(".tmp").unwrap_or(digits);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn paste_sha(name: &str) -> Option<Sha> {
    parse_sha(
        name.strip_prefix("paste-")?
            .strip_suffix(".png")?
            .as_bytes(),
    )
    .ok()
}

/// `<journal root>` of a file: the directory its journals are in, and the name they start with.
fn journal_home(cache: &Path, host: &str, path: &str) -> Result<(PathBuf, String)> {
    let base = crate::cache::edits_path(cache, host, path);
    let named = Path::new(path)
        .components()
        .any(|c| matches!(c, Component::Normal(_)));
    let (Some(parent), Some(name), true) = (base.parent(), base.file_name(), named) else {
        bail!("{path} names no file");
    };
    Ok((parent.to_path_buf(), name.to_string_lossy().into_owned()))
}

/// `<stamp>-<8 hex>` as [`Journal::create`] makes it.
fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 25
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'T'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[15] == b'Z'
        && b[16] == b'-'
        && b[17..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

/// An entry of the journal home: `Some(false)` a journal of `file`, `Some(true)` one being
/// removed (a tombstone).
fn journal_entry(entry: &str, file: &str) -> Option<bool> {
    let rest = entry.strip_prefix(file)?.strip_prefix(".journal-")?;
    match rest.strip_suffix(TOMBSTONE) {
        Some(id) if valid_id(id) => Some(true),
        _ if valid_id(rest) => Some(false),
        _ => None,
    }
}

fn io_err(e: io::Error, what: &str, path: &Path) -> anyhow::Error {
    anyhow::Error::new(e).context(format!("cannot {what} {}", path.display()))
}

/// The directory `path` is an entry of (`.` for a relative path's first component).
fn containing(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        Some(_) => Path::new("."),
        None => Path::new("/"),
    }
}

/// Makes the cache root, if missing, and the directories below it down to `dir`, owner-only,
/// and makes each of them durable in the directory containing it — the cache root in its own
/// parent — found or made, at every call, failing on any error: an earlier creation may have made
/// one and failed at that fsync, and a fetch makes the cache's directories without one. Nothing
/// above the cache root is made or fsynced: those are the owner's directories, and one that is
/// missing refuses the journal. Below `cache` every directory must be a real one (never through
/// a link); `cache` itself may be a link (the owner's choice).
fn make_dirs(fs: &dyn Fs, cache: &Path, dir: &Path) -> Result<()> {
    let below = dir
        .strip_prefix(cache)
        .with_context(|| format!("{} is not under the cache", dir.display()))?;
    let outside = containing(cache);
    match fs
        .node(outside, true)
        .map_err(|e| io_err(e, "look at", outside))?
    {
        Some(Node::Dir) => {}
        Some(_) => bail!("{} is not a directory", outside.display()),
        None => bail!(
            "{} does not exist; the journal makes nothing above the cache root",
            outside.display()
        ),
    }
    let mut at = cache.to_path_buf();
    let steps = std::iter::once(None).chain(below.components().map(Some));
    for step in steps {
        if let Some(comp) = step {
            at.push(comp);
        }
        // The cache root may be a link; nothing below it.
        let root = step.is_none();
        match fs.node(&at, root).map_err(|e| io_err(e, "look at", &at))? {
            Some(Node::Dir) => {}
            Some(_) if root => bail!("{} is not a directory", at.display()),
            Some(_) => bail!(
                "{} is not a directory (a link or a file is there)",
                at.display()
            ),
            None => fs.create_dir(&at).map_err(|e| io_err(e, "create", &at))?,
        }
        let parent = containing(&at);
        fs.sync_dir(parent)
            .map_err(|e| io_err(e, "make durable the folder", parent))?;
    }
    Ok(())
}

/// The byte runs hex edits change, as `[start, end)`: the `.hexpatch` lines `0xOFF: BYTES`, each
/// optionally followed by ` was OLD` (§3 `edits`); `#` lines and blank lines are not edits.
fn hex_runs(edits: &str) -> Result<Vec<(u64, u64)>> {
    let hex_len = |h: &str| -> Result<u64> {
        ensure!(
            !h.is_empty() && h.len().is_multiple_of(2) && h.bytes().all(|c| c.is_ascii_hexdigit()),
            "bytes are pairs of hex digits"
        );
        Ok(h.len() as u64 / 2)
    };
    let mut out = Vec::new();
    for line in edits.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let parsed = (|| -> Result<(u64, u64)> {
            let (off, rest) = line.split_once(": ").context("no `: `")?;
            let off = off.strip_prefix("0x").context("an offset is 0x…")?;
            ensure!(
                !off.is_empty() && off.bytes().all(|c| c.is_ascii_hexdigit()),
                "not a hex offset"
            );
            let off = u64::from_str_radix(off, 16)?;
            let (new, old) = match rest.split_once(" was ") {
                Some((new, old)) => (new, Some(old)),
                None => (rest, None),
            };
            let len = hex_len(new)?;
            if let Some(old) = old {
                ensure!(
                    hex_len(old)? == len,
                    "the bytes replaced have another length"
                );
            }
            let end = off.checked_add(len).context("past the end of any file")?;
            Ok((off, end))
        })();
        out.push(parsed.with_context(|| format!("`{line}` is not a hex edit"))?);
    }
    Ok(out)
}

/// The 4 KiB pages hex edits touch.
fn edited_pages(edits: &str) -> Result<BTreeSet<u64>> {
    let mut pages = BTreeSet::new();
    for (start, end) in hex_runs(edits)? {
        let mut page = Some(start / PAGE * PAGE);
        while let Some(at) = page.filter(|&at| at < end) {
            pages.insert(at);
            page = at.checked_add(PAGE);
        }
    }
    Ok(pages)
}

/// Checks that `seen` is the evidence its kind keeps (§3 `seen`): hex the pages — every page
/// the edits touch, when known; image the whole SHA-256; media the whole SHA-256 up to 256 MiB,
/// else the first and last MiB. Hex edits must be `.hexpatch` lines.
fn check_seen(kind: Kind, baseline: &Fingerprint, seen: &Seen, edits: &str) -> Result<()> {
    if kind == Kind::Hex {
        let pages = edited_pages(edits)?;
        if let Seen::Known(ev) = seen {
            if let Some(page) = pages.iter().find(|p| !ev.pages.contains_key(p)) {
                bail!("the edited page at 0x{page:x} has no evidence");
            }
        }
    }
    let Seen::Known(ev) = seen else {
        return Ok(());
    };
    ensure!(
        ev.pages.keys().all(|off| off % PAGE == 0),
        "page offsets are multiples of {PAGE}"
    );
    let ok = match kind {
        Kind::Hex => ev.whole.is_none() && ev.ends.is_none(),
        Kind::Image => ev.pages.is_empty() && ev.ends.is_none() && ev.whole.is_some(),
        Kind::Media if baseline.size <= HASH_CHECK_MAX => {
            ev.pages.is_empty() && ev.ends.is_none() && ev.whole.is_some()
        }
        Kind::Media => ev.pages.is_empty() && ev.whole.is_none() && ev.ends.is_some(),
    };
    ensure!(
        ok,
        "{} evidence must be {}",
        kind.word(),
        match kind {
            Kind::Hex => "the edited pages",
            Kind::Image => "the whole file's SHA-256",
            Kind::Media => "the whole SHA-256 up to 256 MiB, else the first and last MiB",
        }
    );
    Ok(())
}

/// Checks that `snap` holds the evidence its kind keeps, and pastes only where they belong.
fn check_shape(kind: Kind, snap: &Snapshot) -> Result<()> {
    ensure!(
        kind == Kind::Image || snap.pastes.is_empty(),
        "only image edits paste pictures"
    );
    for p in &snap.pastes {
        ensure!(
            sha256(&p.png) == p.sha256,
            "a paste does not match its hash"
        );
    }
    check_seen(kind, &snap.baseline, &snap.seen, &snap.edits)
}

/// The pre-save evidence a Save starts from (§3.1): known, of the kind's shape, and for hex at
/// least one page.
fn check_pre(kind: Kind, baseline: &Fingerprint, seen: &Seen) -> Result<()> {
    let Seen::Known(ev) = seen else {
        bail!("a save starts once the file's evidence is known");
    };
    check_seen(kind, baseline, seen, "")?;
    ensure!(
        kind != Kind::Hex || !ev.pages.is_empty(),
        "a hex save names the pages it changes"
    );
    Ok(())
}

/// The post-save evidence is exactly what §3.1's table gives for the kind and the staged size.
fn check_post(kind: Kind, staged: &FileStat, post: &PostEvidence) -> Result<()> {
    ensure!(
        post.kind == kind,
        "{} evidence for a {} save",
        post.kind.word(),
        kind.word()
    );
    let rebuilt = match kind {
        Kind::Hex => PostEvidence::hex(staged, post.check.pages.clone(), post.check.whole)?,
        Kind::Image => {
            let whole = post
                .next_baseline
                .sha256
                .context("an image's next baseline is its whole SHA-256")?;
            PostEvidence::image(staged, whole, post.check.ends)?
        }
        Kind::Media => PostEvidence::media(staged, post.check.whole, post.check.ends)?,
    };
    ensure!(
        rebuilt == *post,
        "the post-save evidence is not what the {} save of this size keeps",
        kind.word()
    );
    Ok(())
}

/// What every recorded Save holds (§3.1), checked before a record is written and when one is
/// read back: a route its kind has; phase `writing` exactly for a write in place, its runs
/// apart; known pre-save evidence of the kind's shape; from `staged` on, the post-save evidence
/// the table gives for the kind and the staged size.
fn check_intent(intent: &SaveIntent) -> Result<()> {
    let kind = intent.kind;
    ensure!(
        route_fits(kind, intent.route),
        "a {} save has no {} route",
        kind.word(),
        intent.route.word()
    );
    ensure!(
        matches!(intent.phase, Phase::Writing { .. }) == (intent.route == Route::InPlace),
        "a write in place, and only it, has phase writing"
    );
    check_pre(kind, &intent.baseline, &intent.seen)?;
    if let Phase::Writing { runs, .. } = &intent.phase {
        check_runs(runs)?;
    }
    if let Some((staged, post)) = intent.phase.staged() {
        check_post(kind, staged, post)?;
    }
    Ok(())
}

/// The routes a kind has (§3.1).
fn route_fits(kind: Kind, route: Route) -> bool {
    match kind {
        Kind::Hex => matches!(
            route,
            Route::ServerCopy | Route::StreamedRewrite | Route::InPlace
        ),
        Kind::Image | Kind::Media => route == Route::Upload,
    }
}

/// Write-in-place runs: at least one, each with old and new bytes of one length, none
/// overlapping another (each is judged by its own bytes).
fn check_runs(runs: &[Run]) -> Result<()> {
    ensure!(!runs.is_empty(), "a write in place has runs");
    let mut spans = Vec::with_capacity(runs.len());
    for run in runs {
        ensure!(
            !run.new.is_empty() && run.old.len() == run.new.len(),
            "a run's old and new bytes have one length"
        );
        let end = run
            .offset
            .checked_add(run.new.len() as u64)
            .context("a run past the end of any file")?;
        spans.push((run.offset, end));
    }
    spans.sort_unstable();
    ensure!(
        spans.windows(2).all(|w| w[0].1 <= w[1].0),
        "write-in-place runs overlap"
    );
    Ok(())
}

impl Journal {
    /// Makes the journal directory of a new editing session of `path` on `host` and takes its
    /// lock: each missing directory of `<cache>/.edits/<host>/<dirs of path>` created owner-only
    /// with its parent fsynced, then `<name>.journal-<UTC stamp>-<8 hex>` exclusively, its
    /// parent fsynced, then the lock file inside it, made exclusively.
    pub fn create(
        fs: Arc<dyn Fs>,
        cache: &Path,
        host: &str,
        path: &str,
        kind: Kind,
    ) -> Result<Journal> {
        const TRIES: u32 = 32;
        let (home, file) = journal_home(cache, host, path)?;
        make_dirs(fs.as_ref(), cache, &home)?;
        for _ in 0..TRIES {
            let id = format!(
                "{}-{}",
                crate::cache::utc_stamp(SystemTime::now()),
                &crate::remote::random_hex()[..8]
            );
            let dir = home.join(format!("{file}.journal-{id}"));
            match fs.create_dir(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_err(e, "create", &dir)),
            }
            fs.sync_dir(&home)
                .map_err(|e| io_err(e, "make durable the folder", &home))?;
            #[cfg(test)]
            gap::hold(cache);
            // A listing that found this directory before its lock existed makes the lock itself
            // (exclusively) and owns the directory — and, finding nothing in it, removes it
            // (`orphans`): this session then takes another.
            let lock = match fs.lock(&dir.join(LOCK), true) {
                Ok(Some(held)) => held,
                Ok(None) => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound
                    ) =>
                {
                    continue
                }
                Err(e) => return Err(io_err(e, "lock", &dir.join(LOCK))),
            };
            return Ok(Journal {
                fs,
                dir,
                _lock: lock,
                kind,
                host: host.to_string(),
                path: path.to_string(),
                next: 1,
                current: None,
                current_pastes: Vec::new(),
                last: None,
                unsettled: None,
                kept_pastes: BTreeSet::new(),
                save: None,
                saved_pastes: Vec::new(),
                running: false,
            });
        }
        bail!(
            "no free journal name in {} after {TRIES} tries",
            home.display()
        )
    }

    /// The journal directory (for `kept at <journal>`).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The last generation made durable.
    pub fn current(&self) -> Option<GenRef> {
        self.current
    }

    /// The Save recorded in `save`: running, or cut off with an unknown outcome.
    pub fn pending_save(&self) -> Option<&SaveIntent> {
        self.save.as_ref()
    }

    /// Makes `snap` the current generation (§3 steps 1–4): each new paste written, fsynced,
    /// renamed and the directory fsynced; `gen-<n>` the same; then `CURRENT` naming it, replaced
    /// the same way; then generations and pastes no kept generation names are removed (the
    /// generation a `save` names is kept). Returns once `CURRENT`'s directory fsync is done —
    /// only then may the window say `kept`. A snapshot equal to the current generation writes
    /// nothing. Refused while a Save runs.
    pub fn publish(&mut self, snap: &Snapshot) -> Result<GenRef> {
        ensure!(
            !self.running,
            "a save is running: no new generation until it ends"
        );
        self.write_generation(snap, false)
    }

    fn write_generation(&mut self, snap: &Snapshot, force: bool) -> Result<GenRef> {
        check_shape(self.kind, snap)?;
        let content = Content::of(snap);
        if let (Some(current), Some(last), false) = (self.current, &self.last, force) {
            if *last == content {
                return Ok(current);
            }
        }
        let n = self.next;
        self.next += 1;
        self.last = None;
        match self.write_steps(n, snap, &content) {
            Ok(at) => {
                self.current = Some(at);
                self.current_pastes = content.pastes.clone();
                self.last = Some(content);
                self.unsettled = None;
                self.collect();
                Ok(at)
            }
            Err(e) => {
                self.unsettled.get_or_insert(n);
                Err(e)
            }
        }
    }

    /// §3 steps 1–3 for generation `n`.
    fn write_steps(&mut self, n: u64, snap: &Snapshot, content: &Content) -> Result<GenRef> {
        for paste in &snap.pastes {
            if self.kept_pastes.contains(&paste.sha256) {
                continue;
            }
            self.replace(&paste_name(&paste.sha256), &paste.png)?;
            self.kept_pastes.insert(paste.sha256);
        }
        let head = Head {
            kind: self.kind,
            host: self.host.clone(),
            path: self.path.clone(),
            version: crate::version_text(),
            created: SystemTime::now(),
            baseline: snap.baseline,
        };
        let body = generation_body(n, &head, &content.seen, &content.edits, &content.pastes);
        let at = GenRef {
            n,
            sha256: sha256(&body),
        };
        self.replace(&gen_name(n), &signed(body))?;
        self.replace(CURRENT, current_text(&at).as_bytes())?;
        Ok(at)
    }

    /// `name` written as `name.tmp` (a stale one removed first), fsynced, renamed over `name`,
    /// and the directory fsynced.
    fn replace(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let fs = self.fs.as_ref();
        let to = self.dir.join(name);
        let tmp = self.dir.join(format!("{name}.tmp"));
        if fs
            .node(&tmp, false)
            .map_err(|e| io_err(e, "look at", &tmp))?
            .is_some()
        {
            fs.remove_file(&tmp)
                .map_err(|e| io_err(e, "remove", &tmp))?;
        }
        fs.write_new(&tmp, bytes)
            .map_err(|e| io_err(e, "write", &tmp))?;
        fs.sync_file(&tmp)
            .map_err(|e| io_err(e, "make durable", &tmp))?;
        fs.rename(&tmp, &to)
            .map_err(|e| io_err(e, "publish", &to))?;
        fs.sync_dir(&self.dir)
            .map_err(|e| io_err(e, "make durable the folder", &self.dir))
    }

    /// Removes generations other than the current and the saved one, pastes neither names, and
    /// stale temps — after a failed publication, nothing it may have made current. Best effort:
    /// what stays is removed by a later pass.
    fn collect(&mut self) {
        let mut gens: BTreeSet<u64> = self.current.iter().map(|g| g.n).collect();
        let mut pastes: BTreeSet<Sha> = self.current_pastes.iter().copied().collect();
        if let Some(save) = &self.save {
            gens.insert(save.saved.n);
            pastes.extend(self.saved_pastes.iter().copied());
        }
        let Ok(names) = self.fs.list(&self.dir) else {
            return;
        };
        for name in names {
            let stale = if name.ends_with(".tmp") {
                true
            } else if let Some(n) = gen_number(&name) {
                !gens.contains(&n) && self.unsettled.is_none_or(|from| n < from)
            } else if let Some(sha) = paste_sha(&name) {
                !pastes.contains(&sha) && self.unsettled.is_none()
            } else {
                false
            };
            if stale {
                if let Err(e) = self.fs.remove_file(&self.dir.join(&name)) {
                    crate::debug!("journal: {name} not removed: {e}");
                }
            }
        }
        if self.unsettled.is_none() {
            self.kept_pastes.retain(|sha| pastes.contains(sha));
        }
    }

    /// Starts a Save of `snap`'s edits: writes it as a generation at once if it differs from the
    /// current one (§3 "before any Save starts"), then records the intent durably — phase
    /// `naming` with the temp's name, or `writing` with the runs — before anything is sent. No
    /// generation is written again until the Save ends ([`Journal::save_landed`],
    /// [`Journal::save_not_landed`], [`Journal::save_unknown`]). Refused while another save is
    /// recorded, for a route the kind does not have, and without the evidence a Save needs.
    ///
    /// An error while the record is written may still leave it on disk. The journal then keeps
    /// it as an unknown outcome — its generation and pastes kept, no other Save started — and the
    /// caller, having sent nothing, clears it with [`Journal::clear_save`].
    pub fn begin_save(&mut self, snap: &Snapshot, plan: Plan) -> Result<()> {
        if let Some(save) = &self.save {
            bail!(
                "the save at {} is still recorded: reconcile it first",
                when(save.started)
            );
        }
        let (route, target, phase) = match plan {
            Plan::Temp {
                route,
                target,
                temp,
            } => (route, target, Phase::Naming { temp }),
            Plan::InPlace { target, undo, runs } => {
                (Route::InPlace, target, Phase::Writing { undo, runs })
            }
        };
        check_shape(self.kind, snap)?;
        let mut intent = SaveIntent {
            // Known once the generation is written, below.
            saved: GenRef {
                n: 0,
                sha256: [0; 32],
            },
            kind: self.kind,
            route,
            target,
            started: SystemTime::now(),
            baseline: snap.baseline,
            seen: snap.seen.clone(),
            phase,
        };
        // Everything the record will hold is checked before anything is written.
        check_intent(&intent)?;
        intent.saved = self.write_generation(snap, false)?;
        let recorded = self.replace(SAVE, &intent_bytes(&intent));
        // Whatever the outcome, a record naming `saved` may be on disk now.
        self.saved_pastes = Content::of(snap).pastes;
        self.save = Some(intent);
        self.running = recorded.is_ok();
        recorded
    }

    /// Moves the running save to `phase`, durably, before the request that needs it.
    fn advance(&mut self, phase: Phase) -> Result<()> {
        let Some(save) = &self.save else {
            bail!("no save is running");
        };
        ensure!(self.running, "the save is no longer running");
        let mut next = save.clone();
        next.phase = phase;
        check_intent(&next)?;
        self.replace(SAVE, &intent_bytes(&next))?;
        self.save = Some(next);
        Ok(())
    }

    /// Phase `created`: after the exclusive create's reply, with the temp's stat then.
    pub fn save_created(&mut self, created: FileStat) -> Result<()> {
        let temp = match self.save.as_ref().map(|s| &s.phase) {
            Some(Phase::Naming { temp }) => temp.clone(),
            _ => bail!("phase created comes after naming"),
        };
        self.advance(Phase::Created { temp, created })
    }

    /// Still phase `created`: the record takes the temp's stat again, once the Save has given
    /// the temp its owner and mode and before it fills it. A Save cut off between this and its
    /// first copy request then leaves a temp that still equals the record, and a reopen removes
    /// it ([`reconcile`]); once the temp is being filled it no longer does, and it is kept.
    pub fn save_ready(&mut self, ready: FileStat) -> Result<()> {
        let temp = match self.save.as_ref().map(|s| &s.phase) {
            Some(Phase::Created { temp, .. }) => temp.clone(),
            _ => bail!("the temp is made ready in phase created"),
        };
        self.advance(Phase::Created {
            temp,
            created: ready,
        })
    }

    /// Phase `staged`: after the last write and fsync, with the temp's stat and the post-save
    /// evidence (of this journal's kind, for that stat).
    pub fn save_staged(&mut self, staged: FileStat, post: PostEvidence) -> Result<()> {
        let (temp, created) = match self.save.as_ref().map(|s| &s.phase) {
            Some(Phase::Created { temp, created }) => (temp.clone(), *created),
            _ => bail!("phase staged comes after created"),
        };
        self.advance(Phase::Staged {
            temp,
            created,
            staged,
            post,
        })
    }

    /// Phase `renaming`: just before the rename.
    pub fn save_renaming(&mut self) -> Result<()> {
        let phase = match self.save.as_ref().map(|s| &s.phase) {
            Some(Phase::Staged {
                temp,
                created,
                staged,
                post,
            }) => Phase::Renaming {
                temp: temp.clone(),
                created: *created,
                staged: *staged,
                post: post.clone(),
            },
            _ => bail!("phase renaming comes after staged"),
        };
        self.advance(phase)
    }

    /// The Save landed (the rename's or the in-place fsync's reply, and the `lstat`, are in).
    /// With nothing left unsaved the journal is retired (`Ok(None)`). With `remaining` edits —
    /// against the next baseline — they become one new generation, made current before the
    /// `save` record goes, and the saved generation is then removed.
    pub fn save_landed(mut self, remaining: Option<&Snapshot>) -> Result<Option<Journal>> {
        match self.save.as_ref().map(|s| &s.phase) {
            Some(Phase::Renaming { .. } | Phase::Writing { .. }) => {}
            _ => bail!("a save lands from renaming or writing"),
        }
        let Some(rest) = remaining else {
            self.remove()?;
            return Ok(None);
        };
        self.write_generation(rest, true)?;
        self.end_save()?;
        Ok(Some(self))
    }

    /// The Save did not land (refused, failed, and any temp it created removed by it): the
    /// `save` record goes and the edits stay.
    pub fn save_not_landed(&mut self) -> Result<()> {
        ensure!(self.save.is_some(), "no save is recorded");
        self.end_save()
    }

    /// The Save's outcome is unknown (a request with no reply): editing resumes, and the `save`
    /// record and the generation it names stay until it is reconciled.
    pub fn save_unknown(&mut self) {
        self.running = false;
    }

    /// Clears a cut-off save's record once the caller has reconciled it (and removed what the
    /// outcome said to remove).
    pub fn clear_save(&mut self) -> Result<()> {
        ensure!(!self.running, "the save is still running");
        self.end_save()
    }

    fn end_save(&mut self) -> Result<()> {
        // The save has ended either way; if the record cannot be removed it stays pinned, and a
        // later `clear_save` tries again.
        self.running = false;
        let path = self.dir.join(SAVE);
        match self.fs.remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(e, "remove", &path)),
        }
        self.fs
            .sync_dir(&self.dir)
            .map_err(|e| io_err(e, "make durable the folder", &self.dir))?;
        self.save = None;
        self.saved_pastes.clear();
        self.collect();
        Ok(())
    }

    /// Removes the journal (nothing unsaved, or Discard): renamed out of the listing at once,
    /// durably, then emptied.
    pub fn remove(self) -> Result<()> {
        retire(self.fs.as_ref(), &self.dir)
    }
}

/// Renames `dir` to its tombstone (`<name>.gone`), fsyncs the parent — from then on no listing
/// offers it — and empties and removes it.
fn retire(fs: &dyn Fs, dir: &Path) -> Result<()> {
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push(TOMBSTONE);
    let gone = dir.with_file_name(name);
    fs.rename(dir, &gone)
        .map_err(|e| io_err(e, "remove", dir))?;
    let parent = dir.parent().unwrap_or(Path::new("/"));
    fs.sync_dir(parent)
        .map_err(|e| io_err(e, "make durable the folder", parent))?;
    empty_out(fs, &gone);
    Ok(())
}

/// Removes a tombstone's files (the lock last) and the tombstone. Best effort.
fn empty_out(fs: &dyn Fs, gone: &Path) {
    for name in fs.list(gone).unwrap_or_default() {
        if name != LOCK {
            let _ = fs.remove_file(&gone.join(&name));
        }
    }
    let _ = fs.remove_file(&gone.join(LOCK));
    if let Err(e) = fs.remove_dir(gone) {
        crate::debug!("journal: {} not removed: {e}", gone.display());
    }
}

// ---------------------------------------------------------------------------------------------
// Recovery.

/// What a journal found on open holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// A whole generation with every paste it names: offered by Restore (after
    /// [`restore_check`]).
    Edits(Generation),
    /// Nothing that can be applied: no generation verifies, or the one found names a paste that
    /// is missing or torn. Kept and named, with the reason.
    Unusable {
        why: String,
        generation: Option<Generation>,
    },
}

/// A journal's `save` file as found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveFile {
    None,
    Intent(Box<SaveIntent>),
    /// There, but torn or not sopen's: an unknown outcome.
    Unreadable(String),
}

/// A journal left by an earlier session, its lock held by this listing until it is answered:
/// Restore ([`Orphan::adopt`]), Later (drop it) or Discard ([`Orphan::remove`]).
pub struct Orphan {
    fs: Arc<dyn Fs>,
    dir: PathBuf,
    _lock: Held,
    host: String,
    path: String,
    pub found: Found,
    pub save: SaveFile,
    /// What recovery had to work around (a missing or bad `CURRENT`), and what was settled.
    pub notes: Vec<String>,
}

/// The journals of this file (`abs` on `host`) on disk, by directory: each named as a journal of
/// it (not one being removed), and — where its generation can be read — written for this host
/// and path (a journal of another file whose cache path maps to the same place is not this
/// file's: [`orphans`] skips it). One whose generation cannot be read counts: the listing names
/// it too, once its lock is free. What a listing should find once no other session holds one —
/// the windows' recovery lists again, for a while, until it has found them all. Read straight
/// off the disk, without taking any lock.
pub fn this_files_journals(cache: &Path, host: &str, abs: &str) -> Vec<PathBuf> {
    let base = crate::cache::edits_path(cache, host, abs);
    let (Some(home), Some(name)) = (base.parent(), base.file_name()) else {
        return Vec::new();
    };
    let file = name.to_string_lossy();
    let head_is_ours = |dir: &Path| -> bool {
        let gen = std::fs::read(dir.join(CURRENT))
            .ok()
            .and_then(|bytes| parse_current(&bytes).ok())
            .and_then(|at| std::fs::read(dir.join(gen_name(at.n))).ok())
            .and_then(|bytes| parse_generation(&bytes).ok());
        gen.is_none_or(|g| g.head.host == host && g.head.path == abs)
    };
    std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| journal_entry(&e.file_name().to_string_lossy(), &file) == Some(false))
        .map(|e| e.path())
        .filter(|dir| head_is_ours(dir))
        .collect()
}

/// The journals of `path` on `host` left by sessions that are gone (their lock free), newest
/// first. A journal whose lock file was never made gets one, made exclusively. One that holds
/// nothing but its lock is removed, not offered ([`holds_nothing`]). Journals being removed are
/// emptied on the way; one written for another host or path that maps to the same place is
/// skipped.
pub fn orphans(fs: &Arc<dyn Fs>, cache: &Path, host: &str, path: &str) -> Result<Vec<Orphan>> {
    let (home, file) = journal_home(cache, host, path)?;
    // Never through a link below the cache, as when they are made.
    let below = home.strip_prefix(cache).unwrap_or(Path::new(""));
    let mut at = cache.to_path_buf();
    for comp in below.components() {
        at.push(comp);
        match fs.node(&at, false) {
            Ok(Some(Node::Dir)) => {}
            _ => return Ok(Vec::new()),
        }
    }
    let mut out = Vec::new();
    // Whether the renames into tombstones found here are durable (an fsync of `home`, once).
    let mut tombstones_durable = None;
    let names = fs.list(&home).map_err(|e| io_err(e, "list", &home))?;
    for name in names.iter().rev() {
        let Some(tombstone) = journal_entry(name, &file) else {
            continue;
        };
        let dir = home.join(name);
        if fs.node(&dir, false).ok().flatten() != Some(Node::Dir) {
            continue;
        }
        let lock = match fs.lock(&dir.join(LOCK), false) {
            Ok(Some(held)) => Some(held),
            // Taken: a live session's — or, for a moment, a copy of the descriptor in a child
            // another thread is starting (from its fork until its exec). A caller that lists
            // right after it dropped a journal lists again (§3 "Recovery").
            Ok(None) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => match fs.lock(&dir.join(LOCK), true) {
                Ok(Some(held)) => Some(held),
                _ if tombstone => None,
                _ => continue,
            },
            Err(e) => {
                crate::debug!("journal: {} not locked: {e}", dir.display());
                continue;
            }
        };
        if tombstone {
            // Only once its rename is durable: a power cut could otherwise bring the journal
            // back under its own name, emptied.
            if *tombstones_durable.get_or_insert_with(|| fs.sync_dir(&home).is_ok()) {
                empty_out(fs.as_ref(), &dir);
            }
            continue;
        }
        let Some(lock) = lock else { continue };
        // §3: a journal that holds no edit is removed when it is listed — there is nothing to
        // offer. Its lock held here, nothing else is written into it.
        if holds_nothing(fs.as_ref(), &dir) {
            if let Err(e) = retire(fs.as_ref(), &dir) {
                crate::debug!("journal: an empty one stays: {e:#}");
            }
            drop(lock);
            continue;
        }
        let (found, notes) = read_found(fs.as_ref(), &dir);
        let head = match &found {
            Found::Edits(g) => Some(&g.head),
            Found::Unusable { generation, .. } => generation.as_ref().map(|g| &g.head),
        };
        if head.is_some_and(|h| h.host != host || h.path != path) {
            continue;
        }
        let save = match fs.read(&dir.join(SAVE)) {
            Ok(bytes) => match parse_save(&bytes) {
                Ok(intent) => SaveFile::Intent(Box::new(intent)),
                Err(e) => SaveFile::Unreadable(format!("{e:#}")),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => SaveFile::None,
            Err(e) => SaveFile::Unreadable(e.to_string()),
        };
        out.push(Orphan {
            fs: fs.clone(),
            dir,
            _lock: lock,
            host: host.to_string(),
            path: path.to_string(),
            found,
            save,
            notes,
        });
    }
    Ok(out)
}

/// Whether the journal folder `dir` holds nothing but its lock: no `CURRENT`, no generation
/// (whole, torn or still being written), no pasted picture, no `save` record — what a session
/// leaves when it ends between its `mkdir` and its first generation, or what a listing took from
/// a session still between the folder and its lock (that session then makes another,
/// [`Journal::create`]). Anything else in it may hold an edit or a Save's record: it is offered.
fn holds_nothing(fs: &dyn Fs, dir: &Path) -> bool {
    fs.list(dir)
        .is_ok_and(|names| names.iter().all(|name| name == LOCK))
}

/// Reads `gen-<n>` and checks it is whole and is generation `n`.
fn read_generation(fs: &dyn Fs, dir: &Path, n: u64) -> std::result::Result<Generation, String> {
    let name = gen_name(n);
    let bytes = fs.read(&dir.join(&name)).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => format!("{name} is missing"),
        _ => format!("{name} cannot be read ({e})"),
    })?;
    let gen = parse_generation(&bytes).map_err(|e| format!("{name} is not whole ({e:#})"))?;
    if gen.at.n != n {
        return Err(format!("{name} holds generation {}", gen.at.n));
    }
    Ok(gen)
}

/// The generation `CURRENT` names, or else the newest that verifies (with a note); then its
/// pastes, each read back against its hash.
fn read_found(fs: &dyn Fs, dir: &Path) -> (Found, Vec<String>) {
    let mut notes = Vec::new();
    let mut tried = None;
    let chosen = match fs.read(&dir.join(CURRENT)) {
        Ok(bytes) => match parse_current(&bytes) {
            Ok(at) => match read_generation(fs, dir, at.n) {
                Ok(gen) if gen.at == at => Ok(gen),
                Ok(_) => Err(format!(
                    "CURRENT names {}, whose hash differs",
                    gen_name(at.n)
                )),
                Err(why) => {
                    tried = Some(at.n);
                    Err(format!("CURRENT names {why}"))
                }
            },
            Err(e) => Err(format!("CURRENT cannot be read ({e:#})")),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err("there is no CURRENT".to_string()),
        Err(e) => Err(format!("CURRENT cannot be read ({e})")),
    };
    let gen = match chosen {
        Ok(gen) => gen,
        Err(why) => {
            let mut numbers: Vec<u64> = fs
                .list(dir)
                .unwrap_or_default()
                .iter()
                .filter(|name| !name.ends_with(".tmp"))
                .filter_map(|name| gen_number(name))
                .collect();
            numbers.sort_unstable_by(|a, b| b.cmp(a));
            let mut reasons = vec![why.clone()];
            let mut fallback = None;
            for n in numbers {
                if Some(n) == tried {
                    continue;
                }
                match read_generation(fs, dir, n) {
                    Ok(gen) => {
                        fallback = Some(gen);
                        break;
                    }
                    Err(why) => reasons.push(why),
                }
            }
            match fallback {
                Some(gen) => {
                    notes.push(format!(
                        "{why}; restored {}, the newest whole generation",
                        gen_name(gen.at.n)
                    ));
                    gen
                }
                None => {
                    let found = Found::Unusable {
                        why: format!("no generation could be read: {}", reasons.join("; ")),
                        generation: None,
                    };
                    return (found, notes);
                }
            }
        }
    };
    for sha in &gen.pastes {
        if let Err(why) = read_paste_at(fs, dir, sha) {
            let found = Found::Unusable {
                why: format!(
                    "{} names a picture that cannot be used: {why}",
                    gen_name(gen.at.n)
                ),
                generation: Some(gen),
            };
            return (found, notes);
        }
    }
    (Found::Edits(gen), notes)
}

fn read_paste_at(fs: &dyn Fs, dir: &Path, sha: &Sha) -> std::result::Result<Vec<u8>, String> {
    let name = paste_name(sha);
    let png = fs.read(&dir.join(&name)).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => format!("{name} is missing"),
        _ => format!("{name} cannot be read ({e})"),
    })?;
    if sha256(&png) != *sha {
        return Err(format!("{name} is torn (its bytes do not match its hash)"));
    }
    Ok(png)
}

impl Orphan {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A pasted picture of this journal, checked against its hash.
    pub fn read_paste(&self, sha: &Sha) -> Result<Vec<u8>> {
        read_paste_at(self.fs.as_ref(), &self.dir, sha).map_err(|why| anyhow!(why))
    }

    /// Reconciles the cut-off Save this journal records, if any ([`reconcile`]); an unreadable
    /// record is an unknown outcome.
    pub fn reconcile(&self, inspect: &mut dyn Inspect) -> Option<Reconciled> {
        match &self.save {
            SaveFile::None => None,
            SaveFile::Intent(intent) => Some(reconcile(intent, &self.host, inspect)),
            SaveFile::Unreadable(why) => Some(Reconciled {
                outcome: Outcome::Unknown,
                temp: Temp::None,
                next: None,
                line: format!(
                    "a save was cut off, and its record cannot be read ({why}); check {}:{}",
                    self.host, self.path
                ),
            }),
        }
    }

    /// The recorded Save landed (found on reopen). Every edit of the generation it saved is in
    /// the file, so the journal is retired (`Ok(None)`) — unless a later generation exists
    /// (edits made after an unknown outcome, or a rebase): then only the record goes, and those
    /// edits stay, offered under Restore's checks.
    pub fn landed(mut self) -> Result<Option<Orphan>> {
        let SaveFile::Intent(intent) = &self.save else {
            bail!("no save is recorded");
        };
        let (saved, started) = (intent.saved.n, intent.started);
        let newest = match &self.found {
            Found::Edits(gen) => Some(gen.at.n),
            Found::Unusable { generation, .. } => generation.as_ref().map(|g| g.at.n),
        };
        if newest.is_some_and(|n| n > saved) {
            self.clear_save()?;
            self.notes.push(format!(
                "the save at {} landed (found on reopen); the edits made later are kept",
                when(started)
            ));
            return Ok(Some(self));
        }
        self.remove()?;
        Ok(None)
    }

    /// Removes the `save` record (once reconciled and acted on), durably.
    pub fn clear_save(&mut self) -> Result<()> {
        let path = self.dir.join(SAVE);
        match self.fs.remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(e, "remove", &path)),
        }
        self.fs
            .sync_dir(&self.dir)
            .map_err(|e| io_err(e, "make durable the folder", &self.dir))?;
        self.save = SaveFile::None;
        Ok(())
    }

    /// Restore: this session takes the journal over — its lock, its directory and its numbering
    /// — and its next generation holds the restored edits against the file as now opened. Only a
    /// whole generation is taken over; a recorded save stays until reconciled.
    pub fn adopt(self) -> Result<Journal> {
        let Found::Edits(gen) = &self.found else {
            bail!("these edits cannot be restored: Later or Discard");
        };
        let (save, saved_pastes) = match &self.save {
            SaveFile::None => (None, Vec::new()),
            SaveFile::Intent(intent) => {
                let pastes = read_generation(self.fs.as_ref(), &self.dir, intent.saved.n)
                    .map(|g| g.pastes)
                    .unwrap_or_default();
                (Some(SaveIntent::clone(intent)), pastes)
            }
            SaveFile::Unreadable(_) => bail!("the save record cannot be read: clear it first"),
        };
        let next = self
            .fs
            .list(&self.dir)
            .map_err(|e| io_err(e, "list", &self.dir))?
            .iter()
            .filter_map(|name| gen_number(name))
            .max()
            .unwrap_or(0)
            + 1;
        Ok(Journal {
            kind: gen.head.kind,
            current: Some(gen.at),
            current_pastes: gen.pastes.clone(),
            last: Some(Content::of_generation(gen)),
            unsettled: None,
            kept_pastes: gen.pastes.iter().copied().collect(),
            host: self.host,
            path: self.path,
            next,
            save,
            saved_pastes,
            running: false,
            fs: self.fs,
            dir: self.dir,
            _lock: self._lock,
        })
    }

    /// Discard: the journal is removed.
    pub fn remove(self) -> Result<()> {
        retire(self.fs.as_ref(), &self.dir)
    }
}

// ---------------------------------------------------------------------------------------------
// What Restore and reconciliation read from the remote.

/// Fresh reads of the remote file, never the view's cache (§4.2): implemented by the save code
/// over its `Origin` (and by the window for Restore). The hashes have defaults built on
/// [`Inspect::read_at`]; an implementer that already has one (the image window's streamed read)
/// overrides it.
pub trait Inspect {
    /// `lstat`; `None` when nothing is at `path`.
    fn lstat(&mut self, path: &str) -> Result<Option<Stat>>;
    /// Up to `len` bytes at `offset`, fewer only at the end of the file.
    fn read_at(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>>;

    /// The SHA-256 of the 4 KiB page at `offset` (shorter at the end); `None` past the end.
    fn page_sha256(&mut self, path: &str, offset: u64) -> Result<Option<Sha>> {
        let bytes = self.read_at(path, offset, PAGE as usize)?;
        Ok((!bytes.is_empty()).then(|| sha256(&bytes)))
    }

    /// The SHA-256 of a file of `size` bytes (the size its `lstat` gave), reading those bytes and
    /// one past them, nothing more: `None` when the file is shorter or longer than `size` — it
    /// changed since that `lstat`. Reconciliation asks for it only at most 256 MiB; image Restore
    /// at any size (the window has it from its streamed read).
    fn whole_sha256(&mut self, path: &str, size: u64) -> Result<Option<Sha>> {
        const CHUNK: u64 = 1 << 20;
        let mut hasher = Sha256::new();
        let mut offset = 0u64;
        while offset < size {
            let want = CHUNK.min(size - offset) as usize;
            let bytes = self.read_at(path, offset, want)?;
            ensure!(bytes.len() <= want, "a read gave more than was asked");
            if bytes.is_empty() {
                return Ok(None);
            }
            hasher.update(&bytes);
            offset += bytes.len() as u64;
        }
        if !self.read_at(path, size, 1)?.is_empty() {
            return Ok(None);
        }
        Ok(Some(hasher.finalize().into()))
    }

    /// The SHA-256 of the first and last MiB of a file of `size` bytes.
    fn ends_sha256(&mut self, path: &str, size: u64) -> Result<Ends> {
        let n = ENDS.min(size);
        let first = self.read_at(path, 0, n as usize)?;
        let last = self.read_at(path, size - n, n as usize)?;
        Ok(Ends {
            first: sha256(&first),
            last: sha256(&last),
        })
    }
}

/// What the evidence says of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Holds,
    Differs,
    /// Nothing in the evidence can be checked within the bounds (a whole hash above 256 MiB
    /// alone, or no evidence at all): a stat never decides by itself.
    Cannot,
}

/// Whether `path`, a file of `size` bytes, holds what `ev` says: every page, the ends, and the
/// whole hash at most 256 MiB — cheapest first, `Differs` at the first difference.
fn holds(inspect: &mut dyn Inspect, path: &str, size: u64, ev: &Evidence) -> Result<Verdict> {
    let mut checked = false;
    for (&off, sha) in &ev.pages {
        if inspect.page_sha256(path, off)?.as_ref() != Some(sha) {
            return Ok(Verdict::Differs);
        }
        checked = true;
    }
    if let Some(ends) = &ev.ends {
        if inspect.ends_sha256(path, size)? != *ends {
            return Ok(Verdict::Differs);
        }
        checked = true;
    }
    if let Some(whole) = &ev.whole {
        if size <= HASH_CHECK_MAX {
            if inspect.whole_sha256(path, size)? != Some(*whole) {
                return Ok(Verdict::Differs);
            }
            checked = true;
        }
    }
    Ok(if checked {
        Verdict::Holds
    } else {
        Verdict::Cannot
    })
}

/// What Restore may do with a generation (§3 Recovery).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restore {
    /// The content checks passed: apply the edits; `notes` say what was checked or changed.
    Apply { notes: Vec<String> },
    /// The content differs: nothing is applied and the journal stays.
    Refuse { why: String },
    /// `seen: pending`: ask ([`ASK_PENDING`]) before applying anything.
    Ask,
    /// A journal of another kind: named, never applied.
    OtherKind(Kind),
    /// The file could not be read: nothing is applied.
    Unchecked { why: String },
}

/// Restore's check of `gen` against the file as it is now, for a window of kind `opened_as`
/// (§3): content, never only metadata — hex reads every `seen` page even when `(size, mtime)`
/// equals the baseline; image compares the whole SHA-256; media the whole SHA-256 at most
/// 256 MiB, above that size, mtime and the first and last MiB.
pub fn restore_check(gen: &Generation, opened_as: Kind, inspect: &mut dyn Inspect) -> Restore {
    if gen.head.kind != opened_as {
        return Restore::OtherKind(gen.head.kind);
    }
    let Seen::Known(ev) = &gen.seen else {
        return Restore::Ask;
    };
    match check_restore(gen, ev, inspect) {
        Ok(decision) => decision,
        Err(e) => Restore::Unchecked {
            why: format!("the file could not be checked ({e:#})"),
        },
    }
}

fn check_restore(gen: &Generation, ev: &Evidence, inspect: &mut dyn Inspect) -> Result<Restore> {
    let path = gen.head.path.as_str();
    let base = &gen.head.baseline;
    let changed = || Restore::Refuse {
        why: "the file's content changed since these edits were made".to_string(),
    };
    if gen.head.kind == Kind::Hex {
        // Every page the edits touch needs its evidence, or nothing is applied without asking.
        match edited_pages(&gen.edits) {
            Ok(pages) if pages.iter().all(|p| ev.pages.contains_key(p)) => {}
            Ok(_) => return Ok(Restore::Ask),
            Err(e) => {
                return Ok(Restore::Refuse {
                    why: format!("the edits cannot be read ({e:#})"),
                })
            }
        }
    }
    let Some(now) = inspect.lstat(path)?.filter(|s| s.is_file && !s.is_symlink) else {
        return Ok(Restore::Refuse {
            why: format!("{path} is no longer a regular file there"),
        });
    };
    let same_meta = now.size == base.size && now.mtime == base.mtime;
    Ok(match gen.head.kind {
        Kind::Hex => {
            for (&off, sha) in &ev.pages {
                if inspect.page_sha256(path, off)?.as_ref() != Some(sha) {
                    return Ok(Restore::Refuse {
                        why: format!(
                            "the bytes under your edits at 0x{off:X} changed since they were made"
                        ),
                    });
                }
            }
            let notes = if same_meta {
                Vec::new()
            } else {
                vec![NOTE_PAGES.to_string()]
            };
            Restore::Apply { notes }
        }
        Kind::Image | Kind::Media if ev.whole.is_some() => {
            if now.size != base.size {
                return Ok(changed());
            }
            if gen.head.kind == Kind::Media && now.size > HASH_CHECK_MAX {
                // Media is never hashed whole above 256 MiB.
                return Ok(Restore::Ask);
            }
            match ev.whole {
                Some(whole) if inspect.whole_sha256(path, now.size)? == Some(whole) => {
                    Restore::Apply { notes: Vec::new() }
                }
                _ => changed(),
            }
        }
        Kind::Media if ev.ends.is_some() => {
            if !same_meta {
                return Ok(changed());
            }
            match ev.ends {
                Some(ends) if inspect.ends_sha256(path, now.size)? == ends => Restore::Apply {
                    notes: vec![NOTE_ENDS.to_string()],
                },
                _ => changed(),
            }
        }
        // Known but empty of what the kind needs: as good as pending.
        Kind::Image | Kind::Media => Restore::Ask,
    })
}

// ---------------------------------------------------------------------------------------------
// Reconciliation.

/// Whether a cut-off Save landed (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The target matches the reconciliation evidence: the journal is retired.
    Landed,
    /// The target still matches the pre-save evidence: the edits stay, offered by Restore.
    NotLanded,
    /// Write in place: some runs new, the others old. Nothing is removed or written.
    InPart,
    /// Anything else: nothing is removed.
    Unknown,
}

/// What may be done with a cut-off Save's temp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Temp {
    /// The route has no temp.
    None,
    /// Nothing is at the temp's name.
    Gone(String),
    /// Provably the temp this Save made, and the Save did not land: remove it.
    Remove(String),
    /// Its ownership is uncertain: keep it and name it.
    Keep { path: String, why: String },
}

/// [`reconcile`]'s verdict: the outcome, what to do with the temp, the next baseline when it
/// landed, and the row's line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub outcome: Outcome,
    pub temp: Temp,
    pub next: Option<(Fingerprint, Seen)>,
    pub line: String,
}

/// `2026-11-20 11:42 UTC`.
fn when(t: SystemTime) -> String {
    let r = crate::cache::rfc3339(t);
    format!("{} {} UTC", &r[..10], &r[11..16])
}

/// Decides whether the Save `intent` records landed, from fresh reads of the remote (§3.1):
/// landed when the target matches the reconciliation evidence, not landed when it still matches
/// the pre-save evidence, unknown otherwise; a write in place run by run. A temp is removed only
/// when the Save did not land, the phase reached `created`, its `lstat` equals the recorded stat
/// exactly (size, modification time, owner, mode, a regular file) and, from `staged` on, its
/// post-save evidence matches. Anything else — a temp a Save was cut off filling — is kept and
/// named: its name, owner, mode and size do not prove whose it is. Reads nothing it does not
/// need.
pub fn reconcile(intent: &SaveIntent, host: &str, inspect: &mut dyn Inspect) -> Reconciled {
    if let Phase::Writing { undo, runs } = &intent.phase {
        return reconcile_in_place(intent, host, undo, runs, inspect);
    }
    let target = intent.target.as_str();
    let temp = intent.phase.temp().unwrap_or_default().to_string();
    const UNTOLD: &str =
        "the evidence kept cannot tell it within what reconciliation reads (at most 256 MiB whole)";
    type Decided = (Outcome, Option<(Fingerprint, Seen)>, Option<String>);
    let decided = (|| -> Result<Decided> {
        let Some(now) = inspect
            .lstat(target)?
            .filter(|s| s.is_file && !s.is_symlink)
        else {
            return Ok((Outcome::Unknown, None, None));
        };
        if let Some((staged, post)) = intent.phase.staged() {
            if staged.matches(&now) {
                match holds(inspect, target, now.size, &post.check)? {
                    Verdict::Holds => {
                        let next = (post.next_baseline, post.next_seen.clone());
                        return Ok((Outcome::Landed, Some(next), None));
                    }
                    Verdict::Cannot => return Ok((Outcome::Unknown, None, Some(UNTOLD.into()))),
                    Verdict::Differs => {}
                }
            }
        }
        Ok(match matches_pre(intent, &now, inspect)? {
            Verdict::Holds => (Outcome::NotLanded, None, None),
            Verdict::Differs => (Outcome::Unknown, None, None),
            Verdict::Cannot => (Outcome::Unknown, None, Some(UNTOLD.into())),
        })
    })();
    let (outcome, next, why) = match decided {
        Ok(decided) => decided,
        Err(e) => (
            Outcome::Unknown,
            None,
            Some(format!("the remote could not be read ({e:#})")),
        ),
    };
    let verdict = temp_verdict(intent, &temp, outcome, inspect);
    let named = |v: &Temp| match v {
        Temp::Keep { path, why } => format!("; {host}:{path} is kept ({why})"),
        _ => String::new(),
    };
    let line = match outcome {
        Outcome::Landed => format!(
            "the save at {} landed (found on reopen){}",
            when(intent.started),
            named(&verdict)
        ),
        Outcome::NotLanded => format!(
            "the save at {} did not land; your edits are kept{}",
            when(intent.started),
            named(&verdict)
        ),
        Outcome::InPart | Outcome::Unknown => format!(
            "a save was cut off{}; check {host}:{target} and {host}:{temp}",
            why.map_or(String::new(), |why| format!(" ({why})"))
        ),
    };
    Reconciled {
        outcome,
        temp: verdict,
        next,
        line,
    }
}

/// Whether the target still holds the file the Save started from: `(size, mtime)` of the
/// pre-save baseline, and its `seen` evidence (known whenever a Save is recorded).
fn matches_pre(intent: &SaveIntent, now: &Stat, inspect: &mut dyn Inspect) -> Result<Verdict> {
    if now.size != intent.baseline.size || now.mtime != intent.baseline.mtime {
        return Ok(Verdict::Differs);
    }
    match &intent.seen {
        Seen::Known(ev) => holds(inspect, &intent.target, now.size, ev),
        Seen::Pending => Ok(Verdict::Cannot),
    }
}

/// The temp rule of §3.1 "Owning a temp".
fn temp_verdict(
    intent: &SaveIntent,
    temp: &str,
    outcome: Outcome,
    inspect: &mut dyn Inspect,
) -> Temp {
    let keep = |why: &str| Temp::Keep {
        path: temp.to_string(),
        why: why.to_string(),
    };
    let now = match inspect.lstat(temp) {
        Ok(None) => return Temp::Gone(temp.to_string()),
        Ok(Some(now)) => now,
        Err(e) => return keep(&format!("it could not be checked: {e:#}")),
    };
    match outcome {
        Outcome::NotLanded => {}
        Outcome::Landed => return keep("the save landed, yet something has the temp's name"),
        Outcome::InPart | Outcome::Unknown => return keep("the outcome is unknown"),
    }
    match &intent.phase {
        Phase::Naming { .. } => {
            keep("the create's reply was never recorded, so it may not be sopen's")
        }
        // Only the exact recorded stat — a regular file of that size, modification time,
        // owner and mode — shows the temp to be the one this Save made. A random name, an
        // owner, a mode and a size below the target's are shared by any number of files, and
        // once the process is gone another file may have taken the name: a temp that grew
        // (cut off mid-copy) or whose mode moved on is kept and named, for the user to remove.
        Phase::Created { created, .. } => {
            if created.matches(&now) {
                Temp::Remove(temp.to_string())
            } else {
                // It may be someone else's file by now: the words claim nothing more.
                keep(&format!(
                    "a file at the interrupted save's temporary path {temp} ({} bytes); sopen \
                     could not verify it is its own partial copy, so it was not removed",
                    now.size
                ))
            }
        }
        Phase::Staged { staged, post, .. } | Phase::Renaming { staged, post, .. } => {
            let same = staged.matches(&now)
                && holds(inspect, temp, now.size, &post.check).ok() == Some(Verdict::Holds);
            if same {
                Temp::Remove(temp.to_string())
            } else {
                keep("it is not the file sopen staged")
            }
        }
        Phase::Writing { .. } => Temp::None,
    }
}

fn reconcile_in_place(
    intent: &SaveIntent,
    host: &str,
    undo: &Path,
    runs: &[Run],
    inspect: &mut dyn Inspect,
) -> Reconciled {
    let target = intent.target.as_str();
    let (mut all_new, mut all_old, mut neither) = (true, true, false);
    let mut unread = None;
    for run in runs {
        match inspect.read_at(target, run.offset, run.new.len()) {
            Ok(now) => {
                let (is_new, is_old) = (now == run.new, now == run.old);
                all_new &= is_new;
                all_old &= is_old;
                neither |= !is_new && !is_old;
            }
            Err(e) => {
                unread = Some(format!("{e:#}"));
                neither = true;
                break;
            }
        }
    }
    let outcome = match (neither, all_new, all_old) {
        (true, _, _) => Outcome::Unknown,
        (false, true, _) => Outcome::Landed,
        (false, false, true) => Outcome::NotLanded,
        (false, false, false) => Outcome::InPart,
    };
    let undo = undo.display();
    let file = format!("{host}:{target}");
    let next = if outcome == Outcome::Landed {
        next_after_in_place(target, runs, inspect).ok()
    } else {
        None
    };
    let line = match outcome {
        Outcome::Landed => format!(
            "the save at {} landed (found on reopen)",
            when(intent.started)
        ),
        Outcome::NotLanded => format!(
            "the save at {} did not land; your edits are kept",
            when(intent.started)
        ),
        Outcome::InPart => {
            format!("{file} holds some of your changes; the original bytes are at {undo}")
        }
        Outcome::Unknown => format!(
            "{file} may hold some of your changes — check it; the original bytes are at {undo}{}",
            unread.map_or(String::new(), |e| format!(" (it could not be read: {e})"))
        ),
    };
    Reconciled {
        outcome,
        temp: Temp::None,
        next,
        line,
    }
}

/// A landed write in place's next baseline: the target's `(size, mtime)` now, and the new
/// hashes of the pages its runs touched.
fn next_after_in_place(
    target: &str,
    runs: &[Run],
    inspect: &mut dyn Inspect,
) -> Result<(Fingerprint, Seen)> {
    let now = inspect
        .lstat(target)?
        .ok_or_else(|| anyhow!("{target} is gone"))?;
    let mut pages = BTreeMap::new();
    for run in runs {
        let end = run
            .offset
            .checked_add(run.new.len() as u64)
            .context("a run past the end of any file")?;
        let mut page = Some(run.offset / PAGE * PAGE);
        while let Some(at) = page.filter(|&at| at < end) {
            if let Some(sha) = inspect.page_sha256(target, at)? {
                pages.insert(at, sha);
            }
            page = at.checked_add(PAGE);
        }
    }
    let base = Fingerprint {
        size: now.size,
        mtime: now.mtime,
        sha256: None,
    };
    Ok((base, Seen::pages(pages)))
}

// ---------------------------------------------------------------------------------------------
// The schedule and the status line.

/// Where the window's edits are, for its status bar (§3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keep {
    /// No edit.
    Clean,
    /// An edit is not yet in a durable generation.
    Keeping,
    /// The latest edit is in a durable generation.
    Kept,
    /// A generation could not be written.
    NotKept(String),
}

impl Keep {
    /// The status bar's words, `journal` being the `~`-shortened path.
    pub fn line(&self, journal: &str) -> String {
        match self {
            Keep::Clean => String::new(),
            Keep::Keeping => "unsaved — keeping…".to_string(),
            Keep::Kept => format!("unsaved — kept at {journal}"),
            Keep::NotKept(why) => format!("unsaved — NOT kept on disk ({why})"),
        }
    }
}

/// When the next generation is due (§3): the first change after the last generation schedules
/// it [`DELAY`] later, holding every change up to then; focus loss, close and a Save's start take
/// one at once ([`Schedule::taking`] without waiting). Edits are counted by the window's own
/// revision number, which grows with each change.
#[derive(Debug, Default)]
pub struct Schedule {
    due: Option<Instant>,
    changed: u64,
    kept: u64,
    failure: Option<String>,
}

impl Schedule {
    /// The window's edits are now at revision `rev`.
    pub fn changed(&mut self, rev: u64, now: Instant) {
        self.changed = self.changed.max(rev);
        if self.due.is_none() {
            self.due = Some(now + DELAY);
        }
    }

    /// When the next generation is due (for a repaint request).
    pub fn due(&self) -> Option<Instant> {
        self.due
    }

    pub fn is_due(&self, now: Instant) -> bool {
        self.due.is_some_and(|due| now >= due)
    }

    /// A generation is being written now: returns the revision it holds.
    pub fn taking(&mut self) -> u64 {
        self.due = None;
        self.changed
    }

    /// The generation holding `rev` is durable ([`Journal::publish`] returned).
    pub fn kept(&mut self, rev: u64) {
        self.kept = self.kept.max(rev);
        self.failure = None;
    }

    /// A generation could not be written.
    pub fn failed(&mut self, why: String) {
        self.failure = Some(why);
    }

    pub fn status(&self) -> Keep {
        if self.changed == 0 {
            Keep::Clean
        } else if self.kept >= self.changed {
            Keep::Kept
        } else if let Some(why) = &self.failure {
            Keep::NotKept(why.clone())
        } else {
            Keep::Keeping
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The file formats: text, one field per line; free text escaped (`%XX`); the edits as a
// length-prefixed block; a generation and a save record end with `sha256 <hex of all above>`.

const GEN_MAGIC: &str = "sopen journal 1";
const SAVE_MAGIC: &str = "sopen save 1";

fn esc(value: &[u8]) -> Vec<u8> {
    let utf8 = std::str::from_utf8(value).is_ok();
    let mut out = Vec::with_capacity(value.len());
    for &b in value {
        if b == b'%' || b < 0x20 || b == 0x7f || (!utf8 && b >= 0x80) {
            out.extend_from_slice(format!("%{b:02X}").as_bytes());
        } else {
            out.push(b);
        }
    }
    out
}

fn unesc(value: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        match value[i] {
            b'%' => {
                let hex = value.get(i + 1..i + 3).context("a cut escape")?;
                let digit = |c: u8| match c {
                    b'0'..=b'9' => Ok(c - b'0'),
                    b'A'..=b'F' => Ok(c - b'A' + 10),
                    _ => Err(anyhow!("a bad escape")),
                };
                out.push(digit(hex[0])? << 4 | digit(hex[1])?);
                i += 3;
            }
            b if b < 0x20 || b == 0x7f => bail!("a raw control byte"),
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Ok(out)
}

fn parse_sha(hex: &[u8]) -> Result<Sha> {
    ensure!(hex.len() == 64, "a SHA-256 is 64 hex digits");
    let mut out = [0u8; 32];
    for (i, pair) in hex.chunks(2).enumerate() {
        out[i] = parse_hex_byte(pair)?;
    }
    Ok(out)
}

fn parse_hex_byte(pair: &[u8]) -> Result<u8> {
    let digit = |c: u8| match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(anyhow!("not a lowercase hex digit")),
    };
    Ok(digit(pair[0])? << 4 | digit(pair[1])?)
}

fn parse_hex_bytes(hex: &[u8]) -> Result<Vec<u8>> {
    ensure!(hex.len().is_multiple_of(2), "an odd number of hex digits");
    hex.chunks(2).map(parse_hex_byte).collect()
}

fn parse_u64(text: &[u8]) -> Result<u64> {
    ensure!(
        !text.is_empty() && text.iter().all(u8::is_ascii_digit),
        "not a number"
    );
    std::str::from_utf8(text)?.parse().context("not a number")
}

fn parse_offset(text: &[u8]) -> Result<u64> {
    let hex = text.strip_prefix(b"0x").context("an offset is 0x…")?;
    ensure!(
        !hex.is_empty()
            && hex
                .iter()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c)),
        "not a hex offset"
    );
    u64::from_str_radix(std::str::from_utf8(hex)?, 16).context("not a hex offset")
}

/// `<secs>.<9-digit nanos>`, negative before 1970.
fn fmt_time(t: SystemTime) -> String {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => format!("{}.{:09}", d.as_secs(), d.subsec_nanos()),
        Err(e) => {
            let d = e.duration();
            format!("-{}.{:09}", d.as_secs(), d.subsec_nanos())
        }
    }
}

fn parse_time(text: &[u8]) -> Result<SystemTime> {
    let (neg, text) = match text.strip_prefix(b"-") {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let dot = text
        .iter()
        .position(|&c| c == b'.')
        .context("a time is secs.nanos")?;
    let (secs, nanos) = (&text[..dot], &text[dot + 1..]);
    ensure!(nanos.len() == 9, "nanoseconds have 9 digits");
    let d = Duration::new(parse_u64(secs)?, parse_u64(nanos)? as u32);
    let t = if neg {
        UNIX_EPOCH.checked_sub(d)
    } else {
        UNIX_EPOCH.checked_add(d)
    };
    t.context("a time out of range")
}

fn fmt_sha(sha: &Option<Sha>) -> String {
    sha.map_or("-".to_string(), |s| crate::cache::hex(&s))
}

fn fmt_fingerprint(f: &Fingerprint) -> String {
    format!("{} {} {}", f.size, fmt_time(f.mtime), fmt_sha(&f.sha256))
}

fn fmt_stat(s: &FileStat) -> String {
    let id = |v: Option<u32>| v.map_or("-".to_string(), |v| v.to_string());
    format!(
        "{} {} {} {} {:o}",
        s.size,
        fmt_time(s.mtime),
        id(s.uid),
        id(s.gid),
        s.mode
    )
}

fn put(out: &mut Vec<u8>, key: &str, value: &[u8]) {
    out.extend_from_slice(key.as_bytes());
    out.push(b' ');
    out.extend_from_slice(value);
    out.push(b'\n');
}

fn put_evidence(out: &mut Vec<u8>, key: &str, seen: &Seen) {
    let Seen::Known(ev) = seen else {
        put(out, key, b"pending");
        return;
    };
    put(out, key, b"known");
    for (off, sha) in &ev.pages {
        put(
            out,
            "page",
            format!("0x{off:x} {}", crate::cache::hex(sha)).as_bytes(),
        );
    }
    if let Some(whole) = &ev.whole {
        put(out, "whole", crate::cache::hex(whole).as_bytes());
    }
    if let Some(e) = &ev.ends {
        let value = format!(
            "{} {}",
            crate::cache::hex(&e.first),
            crate::cache::hex(&e.last)
        );
        put(out, "ends", value.as_bytes());
    }
}

/// `body` with its `sha256` line appended.
fn signed(mut body: Vec<u8>) -> Vec<u8> {
    let sha = crate::cache::hex(&sha256(&body));
    put(&mut body, "sha256", sha.as_bytes());
    body
}

/// The body (all but the hash line) of a file and the hash it carries, checked.
fn verified(bytes: &[u8]) -> Result<(&[u8], Sha)> {
    const TAIL: usize = "sha256 ".len() + 64 + 1;
    ensure!(bytes.len() > TAIL, "cut short");
    let (body, tail) = bytes.split_at(bytes.len() - TAIL);
    ensure!(
        tail.starts_with(b"sha256 ") && tail.ends_with(b"\n") && body.ends_with(b"\n"),
        "cut short"
    );
    let sha = parse_sha(&tail[7..71])?;
    ensure!(sha256(body) == sha, "its SHA-256 does not match");
    Ok((body, sha))
}

fn generation_body(n: u64, head: &Head, seen: &Seen, edits: &str, pastes: &[Sha]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(GEN_MAGIC.as_bytes());
    out.push(b'\n');
    put(&mut out, "gen", n.to_string().as_bytes());
    put(&mut out, "kind", head.kind.word().as_bytes());
    put(&mut out, "host", &esc(head.host.as_bytes()));
    put(&mut out, "path", &esc(head.path.as_bytes()));
    put(&mut out, "version", &esc(head.version.as_bytes()));
    put(&mut out, "created", fmt_time(head.created).as_bytes());
    put(
        &mut out,
        "baseline",
        fmt_fingerprint(&head.baseline).as_bytes(),
    );
    put_evidence(&mut out, "seen", seen);
    for sha in pastes {
        put(&mut out, "paste", crate::cache::hex(sha).as_bytes());
    }
    put(&mut out, "edits", edits.len().to_string().as_bytes());
    out.extend_from_slice(edits.as_bytes());
    out.push(b'\n');
    out
}

fn current_text(at: &GenRef) -> String {
    format!("{} {}\n", gen_name(at.n), crate::cache::hex(&at.sha256))
}

fn intent_bytes(i: &SaveIntent) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(SAVE_MAGIC.as_bytes());
    out.push(b'\n');
    put(
        &mut out,
        "saved",
        format!("{} {}", i.saved.n, crate::cache::hex(&i.saved.sha256)).as_bytes(),
    );
    put(&mut out, "kind", i.kind.word().as_bytes());
    put(&mut out, "route", i.route.word().as_bytes());
    put(&mut out, "target", &esc(i.target.as_bytes()));
    put(&mut out, "started", fmt_time(i.started).as_bytes());
    put(
        &mut out,
        "baseline",
        fmt_fingerprint(&i.baseline).as_bytes(),
    );
    put_evidence(&mut out, "seen", &i.seen);
    put(&mut out, "phase", i.phase.word().as_bytes());
    match &i.phase {
        Phase::Naming { temp } => put(&mut out, "temp", &esc(temp.as_bytes())),
        Phase::Created { temp, created } => {
            put(&mut out, "temp", &esc(temp.as_bytes()));
            put(&mut out, "created", fmt_stat(created).as_bytes());
        }
        Phase::Staged {
            temp,
            created,
            staged,
            post,
        }
        | Phase::Renaming {
            temp,
            created,
            staged,
            post,
        } => {
            put(&mut out, "temp", &esc(temp.as_bytes()));
            put(&mut out, "created", fmt_stat(created).as_bytes());
            put(&mut out, "staged", fmt_stat(staged).as_bytes());
            put(&mut out, "post", post.kind.word().as_bytes());
            put_evidence(&mut out, "check", &Seen::Known(post.check.clone()));
            put(
                &mut out,
                "next-baseline",
                fmt_fingerprint(&post.next_baseline).as_bytes(),
            );
            put_evidence(&mut out, "next-seen", &post.next_seen);
        }
        Phase::Writing { undo, runs } => {
            put(&mut out, "undo", &esc(undo.as_os_str().as_bytes()));
            for run in runs {
                let value = format!(
                    "0x{:x} {} {}",
                    run.offset,
                    crate::cache::hex(&run.old),
                    crate::cache::hex(&run.new)
                );
                put(&mut out, "run", value.as_bytes());
            }
        }
    }
    signed(out)
}

/// A line-by-line reader over a verified body.
struct Lines<'a> {
    rest: &'a [u8],
}

impl<'a> Lines<'a> {
    fn line(&mut self) -> Result<&'a [u8]> {
        let end = self
            .rest
            .iter()
            .position(|&c| c == b'\n')
            .context("cut short")?;
        let line = &self.rest[..end];
        self.rest = &self.rest[end + 1..];
        Ok(line)
    }

    fn next_key(&self) -> Option<&'a [u8]> {
        let end = self.rest.iter().position(|&c| c == b' ' || c == b'\n')?;
        Some(&self.rest[..end])
    }

    /// The value of the next line, which must be `key value`.
    fn field(&mut self, key: &str) -> Result<&'a [u8]> {
        let line = self.line()?;
        line.strip_prefix(key.as_bytes())
            .and_then(|rest| rest.strip_prefix(b" "))
            .with_context(|| format!("expected {key}"))
    }

    fn words<const N: usize>(&mut self, key: &str) -> Result<[&'a [u8]; N]> {
        let value = self.field(key)?;
        let words: Vec<&[u8]> = value.split(|&c| c == b' ').collect();
        words.try_into().map_err(|_| anyhow!("{key} has {N} words"))
    }

    fn text(&mut self, key: &str) -> Result<String> {
        String::from_utf8(unesc(self.field(key)?)?).with_context(|| format!("{key} is not UTF-8"))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(self.rest.len() >= n, "cut short");
        let (out, rest) = self.rest.split_at(n);
        self.rest = rest;
        Ok(out)
    }

    fn end(&self) -> Result<()> {
        ensure!(self.rest.is_empty(), "trailing bytes");
        Ok(())
    }

    fn fingerprint(&mut self, key: &str) -> Result<Fingerprint> {
        let [size, mtime, sha] = self.words(key)?;
        Ok(Fingerprint {
            size: parse_u64(size)?,
            mtime: parse_time(mtime)?,
            sha256: match sha {
                b"-" => None,
                hex => Some(parse_sha(hex)?),
            },
        })
    }

    fn stat(&mut self, key: &str) -> Result<FileStat> {
        let [size, mtime, uid, gid, mode] = self.words(key)?;
        let id = |v: &[u8]| -> Result<Option<u32>> {
            Ok(match v {
                b"-" => None,
                n => Some(u32::try_from(parse_u64(n)?)?),
            })
        };
        ensure!(
            !mode.is_empty() && mode.iter().all(|c| (b'0'..=b'7').contains(c)),
            "a mode is octal"
        );
        Ok(FileStat {
            size: parse_u64(size)?,
            mtime: parse_time(mtime)?,
            uid: id(uid)?,
            gid: id(gid)?,
            mode: u32::from_str_radix(std::str::from_utf8(mode)?, 8)?,
        })
    }

    fn evidence(&mut self, key: &str) -> Result<Seen> {
        match self.field(key)? {
            b"pending" => return Ok(Seen::Pending),
            b"known" => {}
            _ => bail!("{key} is pending or known"),
        }
        let mut ev = Evidence::default();
        while self.next_key() == Some(b"page") {
            let [off, sha] = self.words("page")?;
            let off = parse_offset(off)?;
            ensure!(
                ev.pages.last_key_value().is_none_or(|(&k, _)| k < off),
                "pages in order"
            );
            ev.pages.insert(off, parse_sha(sha)?);
        }
        if self.next_key() == Some(b"whole") {
            ev.whole = Some(parse_sha(self.field("whole")?)?);
        }
        if self.next_key() == Some(b"ends") {
            let [first, last] = self.words("ends")?;
            ev.ends = Some(Ends {
                first: parse_sha(first)?,
                last: parse_sha(last)?,
            });
        }
        Ok(Seen::Known(ev))
    }
}

/// A `gen-<n>` file, refused unless whole — its last line's SHA-256 must match everything above
/// — in the form sopen writes, and holding the evidence its kind keeps.
pub fn parse_generation(bytes: &[u8]) -> Result<Generation> {
    let (body, sha) = verified(bytes)?;
    let mut r = Lines { rest: body };
    ensure!(
        r.line()? == GEN_MAGIC.as_bytes(),
        "not a journal generation"
    );
    let n = parse_u64(r.field("gen")?)?;
    let kind = Kind::parse(r.field("kind")?)?;
    let host = r.text("host")?;
    let path = r.text("path")?;
    let version = r.text("version")?;
    let created = parse_time(r.field("created")?)?;
    let baseline = r.fingerprint("baseline")?;
    let seen = r.evidence("seen")?;
    let mut pastes = Vec::new();
    while r.next_key() == Some(b"paste") {
        let sha = parse_sha(r.field("paste")?)?;
        ensure!(pastes.last().is_none_or(|&p| p < sha), "pastes in order");
        pastes.push(sha);
    }
    let len = parse_u64(r.field("edits")?)? as usize;
    let edits = String::from_utf8(r.take(len)?.to_vec()).context("edits are not UTF-8")?;
    ensure!(r.take(1)? == b"\n", "the edits end with a newline");
    r.end()?;
    let gen = Generation {
        at: GenRef { n, sha256: sha },
        head: Head {
            kind,
            host,
            path,
            version,
            created,
            baseline,
        },
        seen,
        edits,
        pastes,
    };
    ensure!(
        gen.bytes() == bytes,
        "not in the form sopen writes a generation"
    );
    ensure!(
        kind == Kind::Image || gen.pastes.is_empty(),
        "only image edits paste pictures"
    );
    check_seen(kind, &gen.head.baseline, &gen.seen, &gen.edits)?;
    Ok(gen)
}

/// `CURRENT`: `gen-<n> <sha256>` and a newline, nothing else.
pub fn parse_current(bytes: &[u8]) -> Result<GenRef> {
    let mut r = Lines { rest: bytes };
    let [name, sha] = {
        let line = r.line()?;
        let words: Vec<&[u8]> = line.split(|&c| c == b' ').collect();
        <[&[u8]; 2]>::try_from(words).map_err(|_| anyhow!("CURRENT is `gen-<n> <sha256>`"))?
    };
    r.end()?;
    let n = name
        .strip_prefix(b"gen-")
        .context("CURRENT names a generation")?;
    let at = GenRef {
        n: parse_u64(n)?,
        sha256: parse_sha(sha)?,
    };
    ensure!(
        current_text(&at).as_bytes() == bytes,
        "not in the form sopen writes CURRENT"
    );
    Ok(at)
}

/// A `save` file, refused unless whole, in the form sopen writes, and holding what a Save of its
/// kind and route records: a route the kind has, known pre-save evidence of its shape, the
/// post-save evidence §3.1's table gives, write-in-place runs that do not overlap.
pub fn parse_save(bytes: &[u8]) -> Result<SaveIntent> {
    let (body, _) = verified(bytes)?;
    let mut r = Lines { rest: body };
    ensure!(r.line()? == SAVE_MAGIC.as_bytes(), "not a save record");
    let [n, sha] = r.words("saved")?;
    let saved = GenRef {
        n: parse_u64(n)?,
        sha256: parse_sha(sha)?,
    };
    let kind = Kind::parse(r.field("kind")?)?;
    let route = Route::parse(r.field("route")?)?;
    let target = r.text("target")?;
    let started = parse_time(r.field("started")?)?;
    let baseline = r.fingerprint("baseline")?;
    let seen = r.evidence("seen")?;
    let phase = match r.field("phase")? {
        b"naming" => Phase::Naming {
            temp: r.text("temp")?,
        },
        b"created" => Phase::Created {
            temp: r.text("temp")?,
            created: r.stat("created")?,
        },
        word @ (b"staged" | b"renaming") => {
            let temp = r.text("temp")?;
            let created = r.stat("created")?;
            let staged = r.stat("staged")?;
            let post_kind = Kind::parse(r.field("post")?)?;
            ensure!(post_kind == kind, "post-save evidence of another kind");
            let Seen::Known(check) = r.evidence("check")? else {
                bail!("reconciliation evidence is known");
            };
            let post = PostEvidence {
                kind: post_kind,
                check,
                next_baseline: r.fingerprint("next-baseline")?,
                next_seen: r.evidence("next-seen")?,
            };
            if word == b"staged" {
                Phase::Staged {
                    temp,
                    created,
                    staged,
                    post,
                }
            } else {
                Phase::Renaming {
                    temp,
                    created,
                    staged,
                    post,
                }
            }
        }
        b"writing" => {
            let undo = PathBuf::from(<std::ffi::OsString as OsStringExt>::from_vec(unesc(
                r.field("undo")?,
            )?));
            let mut runs = Vec::new();
            while r.next_key() == Some(b"run") {
                let [off, old, new] = r.words("run")?;
                runs.push(Run {
                    offset: parse_offset(off)?,
                    old: parse_hex_bytes(old)?,
                    new: parse_hex_bytes(new)?,
                });
            }
            Phase::Writing { undo, runs }
        }
        _ => bail!("unknown phase"),
    };
    r.end()?;
    let intent = SaveIntent {
        saved,
        kind,
        route,
        target,
        started,
        baseline,
        seen,
        phase,
    };
    ensure!(
        intent_bytes(&intent) == bytes,
        "not in the form sopen writes a save record"
    );
    check_intent(&intent)?;
    Ok(intent)
}

impl Generation {
    fn bytes(&self) -> Vec<u8> {
        let body = generation_body(self.at.n, &self.head, &self.seen, &self.edits, &self.pastes);
        signed(body)
    }

    /// The generation as its file holds it (readable text).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }
}

impl SaveIntent {
    /// The record as the `save` file holds it.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&intent_bytes(self)).into_owned()
    }
}

/// Tests: the next [`Journal::create`] under a cache held between its directory and its lock —
/// where a listing ([`orphans`]) finds the directory without a lock and takes it (a slow disk
/// stretches that moment) — until released. Armed per cache: the tests of a process run side by
/// side.
#[cfg(test)]
pub mod gap {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    /// (a journal is held there, it may go on)
    type Shared = Arc<(Mutex<(bool, bool)>, Condvar)>;

    static ARMED: Mutex<Vec<(PathBuf, Shared)>> = Mutex::new(Vec::new());

    /// The hold of the next journal made under one cache. Dropped, it lets it go.
    pub struct Gap {
        cache: PathBuf,
        shared: Shared,
    }

    /// The next journal made under `cache` (exactly that path) waits at the gap.
    pub fn arm(cache: &Path) -> Gap {
        let shared = Shared::default();
        let mut armed = ARMED.lock().unwrap_or_else(|p| p.into_inner());
        armed.push((cache.to_path_buf(), shared.clone()));
        Gap {
            cache: cache.to_path_buf(),
            shared,
        }
    }

    impl Gap {
        /// Waits until a journal is held at the gap; panics after 30 s.
        pub fn reached(&self) {
            let deadline = Instant::now() + Duration::from_secs(30);
            let (lock, cv) = &*self.shared;
            let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
            while !state.0 {
                let left = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !left.is_zero(),
                    "no journal of {:?} came to the gap",
                    self.cache
                );
                state = cv
                    .wait_timeout(state, left)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
            }
        }

        /// The held journal goes on to its lock.
        pub fn release(&self) {
            let (lock, cv) = &*self.shared;
            lock.lock().unwrap_or_else(|p| p.into_inner()).1 = true;
            cv.notify_all();
        }
    }

    impl Drop for Gap {
        fn drop(&mut self) {
            self.release();
            let mut armed = ARMED.lock().unwrap_or_else(|p| p.into_inner());
            armed.retain(|(_, shared)| !Arc::ptr_eq(shared, &self.shared));
        }
    }

    /// [`super::Journal::create`], between the directory and its lock: held there once, when a
    /// gap is armed for `cache`.
    pub(super) fn hold(cache: &Path) {
        let shared = {
            let mut armed = ARMED.lock().unwrap_or_else(|p| p.into_inner());
            match armed.iter().position(|(armed, _)| armed == cache) {
                Some(at) => armed.remove(at).1,
                None => return,
            }
        };
        let (lock, cv) = &*shared;
        let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
        state.0 = true;
        cv.notify_all();
        while !state.1 {
            state = cv.wait(state).unwrap_or_else(|p| p.into_inner());
        }
    }
}

#[cfg(test)]
mod tests;
