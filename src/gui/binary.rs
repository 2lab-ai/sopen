//! Binary files (spec §6a): a file sopen has no window for (a zip, any binary) opens a small
//! dialog first — from the stat alone, nothing downloaded yet — that offers the hex window (the
//! default, Enter) or "Save to…" a chosen path; Esc or closing it downloads nothing. Choosing hex
//! opens a remote file as a stream on the dialog's connection (06 §7.5: nothing is fetched), and
//! the same window becomes the hex window; a local file is read in place. Choosing save downloads
//! straight to the chosen path, created exclusively: an existing file is never replaced — the
//! dialog says so and offers the next free `<stem>-N<ext>` — and Esc, Cancel or closing the
//! window cancels it, leaving nothing ([`Decision`]).
//!
//! [`open_file`] is the whole route main takes for such a file — the dialog, `--hex`, the
//! `SOPEN_CHOICE` answers, no display — over [`FileSource`], which carries the answers out on the
//! connection. The dialog's answers go through [`Source`] and the native save dialog through
//! [`Chooser`]; both are traits, so the tests drive the real window with stand-ins too.

use super::chrome;
use super::hex::{self, HexFile, HexWindow};
use super::{Choice, Hooks, Origin, Outcome, Window};
use crate::remote::RemoteFs;
use anyhow::{anyhow, bail, Context as _, Result};
use eframe::egui::{self, Color32, Key, Pos2, Rect, Vec2};
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

mod interrupt;

pub use interrupt::Interrupted;

/// The dialog's size when it opens (its height then follows its content).
pub const WINDOW: [f32; 2] = [704.0, 240.0];
/// The smallest the dialog may be made.
const MIN_SIZE: [f32; 2] = [420.0, 160.0];
/// The dialog box's title in its frame.
const TITLE: &str = "Binary File";
/// How far the box runs below its last line (its double line sits in the last half of it).
const BOX_BELOW: f32 = 10.0;
/// How long "saved <path>" stays before the window closes.
const SAVED_LINGER: Duration = Duration::from_millis(1200);
/// Browse… came back without a path: cancelled, or no system dialog could be shown (rfd says
/// the same for both).
pub const BROWSE_NONE: &str =
    "Browse… gave no path (cancelled, or no system save dialog here) — the path field still works";
/// Closing was asked for while the hex window was being opened.
pub const CLOSES_AFTER: &str = "the window closes when the transfer is done";
/// A "Save to…" stopped by Esc or by closing the window (06 §7.5): in the dialog, and on the
/// terminal.
pub const SAVE_CANCELLED: &str = "cancelled; nothing was saved";
const SAVE_CANCELLED_LINE: &str = "sopen: cancelled; nothing was saved";

/// What the dialog shows before anything is downloaded: from the stat alone.
#[derive(Debug, Clone)]
pub struct Info {
    /// `host:/abs` (or the local path): the window title.
    pub title: String,
    /// The file's name.
    pub name: String,
    pub size: u64,
    /// Nothing to download: a local file, or one already in the cache.
    pub local: bool,
}

impl Info {
    /// `12.3 MB (12,345,678 bytes) · ZIP archive`.
    pub fn describe(&self) -> String {
        let size = format!(
            "{} ({} bytes)",
            crate::human(self.size),
            hex::grouped(self.size)
        );
        match kind_name(&self.name) {
            Some(kind) => format!("{size} · {kind}"),
            None => size,
        }
    }
}

/// What a file is, when its extension says so cheaply.
pub fn kind_name(name: &str) -> Option<&'static str> {
    let ext = crate::kind::extension(name)?;
    Some(match ext.as_str() {
        "zip" => "ZIP archive",
        "gz" | "tgz" => "gzip archive",
        "tar" => "tar archive",
        "bz2" => "bzip2 archive",
        "xz" => "xz archive",
        "zst" => "Zstandard archive",
        "7z" => "7-Zip archive",
        "rar" => "RAR archive",
        "jar" => "Java archive",
        "apk" => "Android package",
        "deb" => "Debian package",
        "rpm" => "RPM package",
        "dmg" | "img" => "disk image",
        "iso" => "ISO disk image",
        "pdf" => "PDF document",
        "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" => {
            "office document"
        }
        "exe" => "Windows executable",
        "dll" => "Windows library",
        "so" | "dylib" => "shared library",
        "a" | "lib" => "static library",
        "o" | "obj" => "object file",
        "class" => "Java class file",
        "pyc" => "Python bytecode",
        "wasm" => "WebAssembly module",
        "sqlite" | "sqlite3" | "db" => "database",
        "ttf" | "otf" | "woff" | "woff2" => "font",
        "bin" | "dat" => "binary data",
        _ => return None,
    })
}

/// The platform's download folder, else the home folder, else the working directory.
pub fn download_dir() -> PathBuf {
    match directories::UserDirs::new() {
        Some(dirs) => download_dir_from(dirs.download_dir(), Some(dirs.home_dir())),
        None => download_dir_from(None, None),
    }
}

/// [`download_dir`] from the folders the platform names (tests give their own).
pub fn download_dir_from(download: Option<&Path>, home: Option<&Path>) -> PathBuf {
    download
        .or(home)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// `path` shown with the home folder as `~` (what the path field shows).
pub fn tilde(path: &Path) -> String {
    let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The path field's text as a path: a leading `~/` is the home folder.
pub fn untilde(text: &str) -> PathBuf {
    let home = || directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
    match text.strip_prefix("~/") {
        Some(rest) => match home() {
            Some(h) => h.join(rest),
            None => PathBuf::from(text),
        },
        None if text == "~" => home().unwrap_or_else(|| PathBuf::from(text)),
        None => PathBuf::from(text),
    }
}

/// The first `<stem>-N<ext>` next to `path` (N = 1, 2, …) that nothing has — not even a
/// dangling link. A name already in a counted series — `<stem>-N<ext>` beside its
/// `<stem><ext>` — counts on in that series (`model.edited-1.bin` gives `model.edited-2.bin`,
/// never `model.edited-1-1.bin`); a number that is part of a name (nothing at the name without
/// it, as `report-2024.pdf`) stays. An error when every `-N` up to [`NAME_TRIES`] is taken —
/// never a name it did not look at.
pub fn next_free(path: &Path) -> Result<PathBuf> {
    next_free_within(path, NAME_TRIES)
}

/// The `-N` names [`next_free`] looks at: `-1` to `-9999`.
const NAME_TRIES: u32 = 9_999;

/// [`next_free`] over `-1` to `-<limit>` (tests: a small limit).
fn next_free_within(path: &Path, limit: u32) -> Result<PathBuf> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = series_base(path, &name).unwrap_or(name);
    for n in 1..=limit {
        let candidate = path.with_file_name(fitted(&name, n));
        if std::fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }
    bail!(
        "every name from {} to {} is taken in {}",
        fitted(&name, 1),
        fitted(&name, limit),
        tilde(path.parent().unwrap_or(Path::new(".")))
    )
}

/// `<stem><ext>` of a name `<stem>-N<ext>` (N decimal digits) when something is at that name
/// beside it: the series the name is in.
fn series_base(path: &Path, name: &str) -> Option<String> {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    let (head, digits) = stem.rsplit_once('-')?;
    if head.is_empty() || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let base = format!("{head}{ext}");
    std::fs::symlink_metadata(path.with_file_name(&base))
        .is_ok()
        .then_some(base)
}

/// The longest name a folder takes, in bytes (macOS and Linux alike).
const NAME_MAX: usize = 255;

/// `<stem>-N<ext>` within [`NAME_MAX`] bytes: the stem gives way (cut on a character boundary)
/// when the name is already at the limit.
fn fitted(name: &str, n: u32) -> String {
    let full = crate::remote::suffixed(name, n);
    if full.len() <= NAME_MAX {
        return full;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    let tail = format!("-{n}{ext}");
    let mut room = NAME_MAX.saturating_sub(tail.len()).min(stem.len());
    while !stem.is_char_boundary(room) {
        room -= 1;
    }
    format!("{}{tail}", &stem[..room])
}

/// Where "Save to…" saves first: `SOPEN_SAVE_TO`, else `<download folder>/<name>`.
pub fn first_path(name: &str, hooks: &Hooks) -> PathBuf {
    match &hooks.save_to {
        Some(path) => path.clone(),
        None => download_dir().join(name),
    }
}

/// The default path as the field first shows it: itself when free, else the next free name and
/// a note saying so.
pub fn suggest(path: PathBuf) -> (PathBuf, Option<String>) {
    if std::fs::symlink_metadata(&path).is_err() {
        return (path, None);
    }
    let next = match next_free(&path) {
        Ok(next) => next,
        // Nothing free to suggest: the field keeps the name, and the note says why.
        Err(e) => return (path, Some(format!("{e:#}"))),
    };
    let note = format!(
        "{} is already in {}; suggesting {}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        tilde(path.parent().unwrap_or(Path::new("."))),
        next.file_name().unwrap_or_default().to_string_lossy()
    );
    (next, Some(note))
}

// ---------------------------------------------------------------------------------------------
// Save to…: a complete file under a new name, or nothing.

/// Why a save did not happen.
#[derive(Debug)]
pub enum SaveError {
    /// Something is at the path already; it was not touched. `next` is the next free name.
    Exists {
        next: PathBuf,
    },
    Failed(anyhow::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Exists { next } => write!(
                f,
                "something is already there; not replaced (the next free name is {})",
                next.display()
            ),
            SaveError::Failed(e) => write!(f, "{e:#}"),
        }
    }
}

/// What "Save to…" did: the bytes saved, and what the owner must hear about how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTo {
    pub bytes: u64,
    pub notes: Vec<String>,
}

fn exists(dest: &Path) -> SaveError {
    match next_free(dest) {
        Ok(next) => SaveError::Exists { next },
        Err(e) => SaveError::Failed(e.context(format!("{} exists", dest.display()))),
    }
}

/// A link a save makes (tests stand in for it, or fail it: the stand-in runs first, and its
/// error is the link's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinkStep {
    /// The still-empty `part` to a second name in the private folder: can this filesystem
    /// publish without replacing anything?
    Probe,
    /// The complete `part` to the chosen name.
    Publish,
}

/// The names a save makes inside its private folder.
const PART: &CStr = c"part";
const PROBE: &CStr = c"probe";
const MODE: &CStr = c"mode";
/// Held locked (`flock`) by the save for as long as its private folder is in use: a private
/// folder whose lock can be taken belongs to no running sopen ([`sweep_dead`]).
const LOCK: &CStr = c"lock";

fn sys(ret: libc::c_int) -> std::io::Result<libc::c_int> {
    if ret < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn c_name(bytes: &[u8]) -> std::io::Result<CString> {
    CString::new(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the name holds a NUL byte",
        )
    })
}

/// `openat(dir, name, flags | O_CLOEXEC, mode)` as an owned descriptor.
fn open_at(
    dir: libc::c_int,
    name: &CStr,
    flags: libc::c_int,
    mode: libc::c_uint,
) -> std::io::Result<OwnedFd> {
    // SAFETY: `name` is NUL-terminated; a descriptor returned here is ours alone.
    let fd = sys(unsafe { libc::openat(dir, name.as_ptr(), flags | libc::O_CLOEXEC, mode) })?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The kinds of failure that mean "this filesystem cannot make hard links here".
fn no_hard_links(e: &std::io::Error) -> bool {
    e.raw_os_error().is_some_and(|c| {
        c == libc::ENOTSUP
            || c == libc::EOPNOTSUPP
            || c == libc::EPERM
            || c == libc::EXDEV
            || c == libc::EMLINK
    })
}

/// The private folder `.sopen-<16 hex>` (0700) a save makes in the destination folder, and the
/// names it made inside it. Everything is reached through the two folders' descriptors — the
/// destination folder opened once — never by path again. The folder is the owner's only from
/// the call that makes it, so no other user can put or swap an entry in it; other processes of
/// the same user are trusted. Cleanup only ever removes sopen's own names inside it, then it.
/// For as long as the folder is in use the save holds a lock on `lock` inside it, so a later
/// save can tell a folder a dead sopen left from one in use ([`sweep_dead`]).
struct Staging {
    dir: OwnedFd,
    private: OwnedFd,
    /// The locked `lock` (`None` where the filesystem has no locks: then there is no `lock`
    /// file either, and the folder is never swept). Closing it lets the lock go.
    lock: Option<OwnedFd>,
    private_name: CString,
    /// `<destination folder>/.sopen-<16 hex>`, for messages.
    shown: PathBuf,
    /// Names made inside the private folder that are still there.
    made: Vec<&'static CStr>,
    /// Removed, or kept on purpose.
    done: bool,
}

impl Staging {
    /// Makes the private folder in `dir` (the destination folder's descriptor; `parent` is how
    /// it is shown).
    fn make(dir: OwnedFd, parent: &Path, expected: Option<u64>) -> Result<Self> {
        // A setgid folder (a shared one) gives what is made in it its group — on Linux only while
        // the private folder keeps the bit, so owner-only keeps it too.
        // SAFETY: `st` is written by the call; the descriptor is ours.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        sys(unsafe { libc::fstat(dir.as_raw_fd(), &mut st) })
            .with_context(|| format!("cannot stat the folder {}", parent.display()))?;
        let private_mode = if st.st_mode & libc::S_ISGID != 0 {
            0o2700
        } else {
            0o700
        };
        for _ in 0..32 {
            let hex = crate::remote::random_hex();
            let name = format!(".sopen-{hex}");
            let cname = c_name(name.as_bytes())?;
            // SAFETY: `cname` is NUL-terminated.
            match sys(unsafe { libc::mkdirat(dir.as_raw_fd(), cname.as_ptr(), 0o700) }) {
                Ok(_) => {}
                Err(e) if e.raw_os_error() == Some(libc::EEXIST) => continue,
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("cannot make a folder in {}", parent.display()))
                }
            }
            let private = open_at(
                dir.as_raw_fd(),
                &cname,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                0,
            );
            let remove = || {
                // SAFETY: `cname` is NUL-terminated; it names the folder just made.
                unsafe { libc::unlinkat(dir.as_raw_fd(), cname.as_ptr(), libc::AT_REMOVEDIR) };
            };
            let private = match private {
                Ok(fd) => fd,
                Err(e) => {
                    remove();
                    return Err(e)
                        .with_context(|| format!("cannot open {}", parent.join(&name).display()));
                }
            };
            let lock = match lock_new(&private, expected) {
                Ok(Locked::Held(lock)) => Some(lock),
                Ok(Locked::NoLocks) => None,
                // Another save's sweep took the lock in the instant between its creation and
                // its locking, and is removing the folder as a dead one: start under a new name.
                Ok(Locked::Swept) => {
                    // SAFETY: `LOCK` is NUL-terminated; the descriptor is this save's.
                    unsafe { libc::unlinkat(private.as_raw_fd(), LOCK.as_ptr(), 0) };
                    remove();
                    continue;
                }
                Err(e) => {
                    remove();
                    return Err(e).with_context(|| {
                        format!(
                            "cannot create {}",
                            parent.join(&name).join("lock").display()
                        )
                    });
                }
            };
            let mut staging = Self {
                made: lock.iter().map(|_| LOCK).collect(),
                private,
                lock,
                dir,
                private_name: cname,
                shown: parent.join(&name),
                done: false,
            };
            // SAFETY: a descriptor this save owns.
            if let Err(e) = sys(unsafe { libc::fchmod(staging.private.as_raw_fd(), private_mode) })
            {
                let left = staging.clean();
                return Err(with_left(
                    anyhow::Error::new(e).context(format!(
                        "cannot make {} owner-only",
                        staging.shown.display()
                    )),
                    left,
                ));
            }
            return Ok(staging);
        }
        bail!("no free name for a folder in {}", parent.display())
    }

    /// Creates `name` inside the private folder with `mode` in the creating call (never through
    /// a link, never an existing entry).
    fn create(&mut self, name: &'static CStr, mode: libc::c_uint) -> std::io::Result<File> {
        let fd = open_at(
            self.private.as_raw_fd(),
            name,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            mode,
        )?;
        self.made.push(name);
        Ok(File::from(fd))
    }

    fn unlink(&mut self, name: &'static CStr) -> std::io::Result<()> {
        // SAFETY: `name` is NUL-terminated and names an entry this save made in its folder.
        sys(unsafe { libc::unlinkat(self.private.as_raw_fd(), name.as_ptr(), 0) })?;
        self.made.retain(|n| *n != name);
        Ok(())
    }

    /// The mode a new file gets in this folder (the umask, any default ACL): a file created
    /// with 0666 shows it; it is removed at once.
    fn new_file_mode(&mut self) -> std::io::Result<u32> {
        let probe = self.create(MODE, 0o666)?;
        let mode = probe.metadata()?.permissions().mode() & 0o777;
        drop(probe);
        self.unlink(MODE)?;
        Ok(mode)
    }

    /// Links `part` to `name` in `to` (`None`: a second name inside the private folder).
    fn link(&self, to: Option<&CStr>) -> std::io::Result<()> {
        let (dir, name) = match to {
            Some(name) => (self.dir.as_raw_fd(), name),
            None => (self.private.as_raw_fd(), PROBE),
        };
        // SAFETY: both names are NUL-terminated; both descriptors are this save's.
        sys(unsafe {
            libc::linkat(
                self.private.as_raw_fd(),
                PART.as_ptr(),
                dir,
                name.as_ptr(),
                0,
            )
        })
        .map(|_| ())
    }

    /// Removes what this save made — its names inside the private folder, then the folder.
    /// What cannot be removed is said, never dropped.
    fn clean(&mut self) -> Option<String> {
        self.done = true;
        let mut problems = Vec::new();
        for name in std::mem::take(&mut self.made) {
            // SAFETY: as in `unlink`.
            if let Err(e) =
                sys(unsafe { libc::unlinkat(self.private.as_raw_fd(), name.as_ptr(), 0) })
            {
                problems.push(format!("{}: {e}", name.to_string_lossy()));
            }
        }
        // SAFETY: `private_name` is NUL-terminated and names the folder this save made.
        if let Err(e) = sys(unsafe {
            libc::unlinkat(
                self.dir.as_raw_fd(),
                self.private_name.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        }) {
            problems.push(e.to_string());
        }
        (!problems.is_empty()).then(|| {
            format!(
                "the staging folder {} could not be removed ({}); it is safe to delete",
                self.shown.display(),
                problems.join("; ")
            )
        })
    }

    /// Keeps the folder with the complete `part` in it; returns where that is, and what must
    /// be said about its `lock`. The lock is removed, best effort: the folder is no longer a
    /// save's. A sweep never needs that — a complete `part` is never swept ([`sweep_one`]) —
    /// so a lock that could not be removed costs only a stray file, which is said.
    fn keep(mut self) -> (PathBuf, Option<String>) {
        self.done = true;
        let mut note = None;
        if let Some(held) = self.lock.take() {
            // Unlinked while it is still held: no sweep looks at the folder in between.
            // SAFETY: as in `unlink`.
            if let Err(e) =
                sys(unsafe { libc::unlinkat(self.private.as_raw_fd(), LOCK.as_ptr(), 0) })
            {
                note = Some(format!(
                    "its lock file {} could not be removed ({e}); the download stays",
                    self.shown.join("lock").display()
                ));
            }
            drop(held);
        }
        (self.shown.join("part"), note)
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // Every return path cleans or keeps on purpose; this is for a panic in the transfer.
        if !self.done {
            if let Some(left) = self.clean() {
                eprintln!("sopen: {left}");
            }
        }
    }
}

/// What cleanup could not remove ([`Staging::clean`]'s words), on the error it came with.
#[derive(Debug)]
struct Left(String);

impl std::fmt::Display for Left {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `e`, with what cleanup could not remove.
fn with_left(e: anyhow::Error, left: Option<String>) -> anyhow::Error {
    match left {
        Some(left) => e.context(Left(left)),
        None => e,
    }
}

/// What became of a new private folder's `lock` ([`lock_new`]).
enum Locked {
    Held(OwnedFd),
    /// The filesystem has no `flock`: no `lock` file is left, so the folder is never swept.
    NoLocks,
    /// A sweep holds or has removed the lock: the folder is going, or gone.
    Swept,
}

/// `flock(fd, LOCK_EX | LOCK_NB)`.
fn lock_now(fd: &OwnedFd) -> std::io::Result<()> {
    // SAFETY: a descriptor the caller owns.
    sys(unsafe { libc::flock(fd.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }).map(|_| ())
}

fn stat_of(fd: &OwnedFd) -> std::io::Result<libc::stat> {
    // SAFETY: `st` is written by the call; the descriptor is the caller's.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    sys(unsafe { libc::fstat(fd.as_raw_fd(), &mut st) })?;
    Ok(st)
}

/// Creates `lock` in the private folder just made and locks it. Creating and locking are two
/// calls, and a sweep ([`sweep_dead`]) may come between them and take the folder for a dead
/// one: then the lock is busy, or — the sweep already done — taken but no longer in the folder
/// (no link left), and the caller starts again under a new name.
///
/// Once held, the lock records how many bytes the save expects (`expected`: the source's size
/// by its stat, in decimal, then a newline) — what a sweep needs to tell a partial `part` from
/// a complete one. Without it (a save of bytes made here, a source whose size is unknown, a
/// write that failed) the file stays empty and the folder is never swept.
fn lock_new(private: &OwnedFd, expected: Option<u64>) -> std::io::Result<Locked> {
    let lock = open_at(
        private.as_raw_fd(),
        LOCK,
        libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        0o600,
    )?;
    match lock_now(&lock) {
        Ok(()) if stat_of(&lock)?.st_nlink == 0 => Ok(Locked::Swept),
        Ok(()) => {
            let mut lock = File::from(lock);
            if let Some(expected) = expected {
                if lock.write_all(format!("{expected}\n").as_bytes()).is_err() {
                    // Half a number must not pass for a smaller one.
                    let _ = lock.set_len(0);
                }
            }
            Ok(Locked::Held(lock.into()))
        }
        Err(e) if e.raw_os_error() == Some(libc::EWOULDBLOCK) => Ok(Locked::Swept),
        Err(_) => {
            // SAFETY: `LOCK` is NUL-terminated; it names the file just made in this folder.
            unsafe { libc::unlinkat(private.as_raw_fd(), LOCK.as_ptr(), 0) };
            Ok(Locked::NoLocks)
        }
    }
}

/// `errno`, to tell the end of a folder from a failed read of it.
fn errno() -> *mut libc::c_int {
    // SAFETY: both return this thread's `errno`.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe {
        libc::__errno_location()
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    unsafe {
        libc::__error()
    }
}

/// The names in the folder `dir` is open on (without `.` and `..`), read through a descriptor
/// of its own. A read that fails mid-way is an error, never a shorter list.
fn names_in(dir: &OwnedFd) -> std::io::Result<Vec<CString>> {
    let again = open_at(dir.as_raw_fd(), c".", libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    let raw = again.into_raw_fd();
    // SAFETY: `raw` is an open folder; on success the stream owns it (`closedir` closes it).
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        let e = std::io::Error::last_os_error();
        // SAFETY: the stream did not take the descriptor.
        unsafe { libc::close(raw) };
        return Err(e);
    }
    let mut names = Vec::new();
    let read = loop {
        // SAFETY: `stream` is open; an entry is read before the next call; `errno` is this
        // thread's.
        unsafe { *errno() = 0 };
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break match unsafe { *errno() } {
                0 => Ok(()),
                code => Err(std::io::Error::from_raw_os_error(code)),
            };
        }
        // SAFETY: `d_name` is NUL-terminated within the entry.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if !matches!(name.to_bytes(), b"." | b"..") {
            names.push(name.to_owned());
        }
    };
    // SAFETY: opened above, closed once.
    unsafe { libc::closedir(stream) };
    read.map(|()| names)
}

/// `.sopen-<16 lower-case hex>`: a save's private folder, as [`Staging::make`] names it.
fn staging_name(name: &[u8]) -> bool {
    name.strip_prefix(b".sopen-").is_some_and(|hex| {
        hex.len() == 16 && hex.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Removes the private folders dead saves left in `dir` — a sopen that was killed outright
/// (SIGKILL, a crash, the power) cleans nothing up, and its `part` may hold gigabytes. A folder
/// goes only when it is provably no running save's, holds nothing but sopen's own staging
/// files, and its `part` is provably incomplete; anything else is left exactly as it is.
/// Nothing here fails the save that asked.
fn sweep_dead(dir: &OwnedFd, parent: &Path) {
    let Ok(names) = names_in(dir) else {
        return;
    };
    for name in names.iter().filter(|n| staging_name(n.to_bytes())) {
        if sweep_one(dir, name).is_some() {
            let gone = parent.join(std::ffi::OsStr::from_bytes(name.to_bytes()));
            crate::debug!(
                "removed {} (left by a sopen that was killed)",
                gone.display()
            );
        }
    }
}

/// [`sweep_dead`] for the folder `parent`, opened here: Save As runs it before it measures the
/// folder's free space, so what a killed save left never counts against the room
/// (`gui::save_as`). A folder that cannot be opened is left alone; nothing here fails.
pub(super) fn sweep_dead_in(parent: &Path) {
    if let Ok(dir) = open_folder(parent) {
        sweep_dead(&dir, parent);
    }
}

/// [`sweep_dead`] for one folder: `Some` when it was removed. It is dead when its `lock` can be
/// taken without waiting — every save holds its own for as long as it runs, and a lock dies
/// with its process, so a process id used again cannot pass for the owner. And it is a partial
/// when its `part` is shorter than the size its lock recorded. No `lock` (a folder of an older
/// sopen), a lock that is held, a lock without a readable size, a `part` as long as that size
/// (a complete download: kept after a failed publish, or about to get its name), another
/// user's folder, or anything inside that is not one of sopen's own names as a plain file:
/// left alone. Only those names are ever unlinked, the lock last, then the empty folder.
fn sweep_one(dir: &OwnedFd, name: &CStr) -> Option<()> {
    let private = open_at(
        dir.as_raw_fd(),
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        0,
    )
    .ok()?;
    // SAFETY: no arguments, no failure.
    if stat_of(&private).ok()?.st_uid != unsafe { libc::geteuid() } {
        return None;
    }
    let lock = open_at(
        private.as_raw_fd(),
        LOCK,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )
    .ok()?;
    if stat_of(&lock).ok()?.st_mode & libc::S_IFMT != libc::S_IFREG {
        return None;
    }
    // Held: its save is running.
    lock_now(&lock).ok()?;
    // Only a partial goes: `part` shorter than the size the lock recorded. A complete `part`
    // is the owner's download — kept after a failed publish, or killed an instant before its
    // name — and a lock that records no readable size says nothing: both stay.
    let expected = recorded_size(&lock)?;
    if part_len(&private)? >= expected {
        return None;
    }
    let inside = names_in(&private).ok()?;
    for entry in &inside {
        if ![PART, PROBE, MODE, LOCK].contains(&entry.as_c_str()) {
            return None;
        }
        // SAFETY: `entry` is NUL-terminated; `st` is written by the call.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        sys(unsafe {
            libc::fstatat(
                private.as_raw_fd(),
                entry.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        })
        .ok()?;
        if st.st_mode & libc::S_IFMT != libc::S_IFREG {
            return None;
        }
    }
    let lock_last = inside
        .iter()
        .filter(|n| n.as_c_str() != LOCK)
        .map(|n| n.as_c_str())
        .chain([LOCK]);
    for entry in lock_last {
        // SAFETY: `entry` is NUL-terminated and one of sopen's own names in this folder.
        sys(unsafe { libc::unlinkat(private.as_raw_fd(), entry.as_ptr(), 0) }).ok()?;
    }
    // SAFETY: `name` is NUL-terminated; only an empty folder goes.
    sys(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) }).ok()?;
    Some(())
}

/// The size a save recorded in its `lock` ([`lock_new`]): decimal digits and a newline,
/// nothing else. Anything else — empty, cut short, not a number — is `None`.
fn recorded_size(lock: &OwnedFd) -> Option<u64> {
    let mut text = [0u8; 32];
    // SAFETY: the buffer is ours, its length passed with it; the descriptor is the caller's.
    let n = unsafe { libc::pread(lock.as_raw_fd(), text.as_mut_ptr().cast(), text.len(), 0) };
    let n = usize::try_from(n).ok()?;
    let digits = text[..n].strip_suffix(b"\n")?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// How long `part` is in the private folder: 0 when there is none (the save died before it
/// made it); `None` when it cannot be looked at, or is not a plain file.
fn part_len(private: &OwnedFd) -> Option<u64> {
    // SAFETY: `PART` is NUL-terminated; `st` is written by the call.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    match sys(unsafe {
        libc::fstatat(
            private.as_raw_fd(),
            PART.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    }) {
        Ok(_) if st.st_mode & libc::S_IFMT == libc::S_IFREG => u64::try_from(st.st_size).ok(),
        Ok(_) => None,
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Some(0),
        Err(_) => None,
    }
}

/// Opens the destination's folder once, as a folder (a link to a folder is followed here, as
/// the path says); its failures are said as what the path names.
fn open_folder(parent: &Path) -> Result<OwnedFd> {
    let cpath = c_name(parent.as_os_str().as_bytes())?;
    match open_at(
        libc::AT_FDCWD,
        &cpath,
        libc::O_RDONLY | libc::O_DIRECTORY,
        0,
    ) {
        Ok(fd) => Ok(fd),
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
            bail!("the folder {} does not exist", parent.display())
        }
        Err(e) if e.raw_os_error() == Some(libc::ENOTDIR) => {
            bail!("{} is not a folder", parent.display())
        }
        Err(e) => Err(e).with_context(|| format!("cannot open the folder {}", parent.display())),
    }
}

/// Saves new bytes at `dest` without ever putting an incomplete file, or anything over an
/// existing one, under that name. The destination's folder is opened once and everything after
/// goes through its descriptor: a private folder `.sopen-<16 hex>` (0700) is made in it, and in
/// that `part`, created owner-only (0600) by the creating call. Before any byte moves, `part` is
/// linked to a second name in the private folder and unlinked again — a folder whose filesystem
/// has no hard links (or a path ending in '/', or a folder that does not exist) fails here, with
/// nothing transferred. Then `fill` writes `part`, which is flushed to disk, given the mode any
/// new file there gets, and published by a hard link to `dest` — which fails if anything is
/// there — then the private folder is removed. A publish that fails otherwise keeps the
/// complete `part` and names it. Cleanup only ever removes sopen's own names in its private
/// folder, then the folder. A save holds a lock on `lock` in its private folder while it runs;
/// before it makes its own, it removes the private folders dead saves left in the same folder
/// ([`sweep_dead`]).
pub fn save_new(
    dest: &Path,
    fill: impl FnOnce(&mut File) -> Result<u64>,
) -> std::result::Result<SavedTo, SaveError> {
    save_new_with(dest, fill, &mut |_, _| Ok(()))
}

/// [`save_new`] with `link` run before each link (tests: a folder without hard links, a name
/// taken at the last moment, something put in the private folder). Its error is the link's.
pub(super) fn save_new_with(
    dest: &Path,
    fill: impl FnOnce(&mut File) -> Result<u64>,
    link: &mut dyn FnMut(LinkStep, &Path) -> std::io::Result<()>,
) -> std::result::Result<SavedTo, SaveError> {
    save_new_deciding(dest, None, fill, link, &Decision::default())
}

/// [`save_new_with`] for a save that can be cancelled: `decision` is asked once, right before
/// the complete file gets its name — after the fill, its sync and its mode, and after `link`'s
/// stand-in for that step. Cancelled by then, nothing is published: the part and the private
/// folder are removed and the error is [`Cancelled`](crate::remote::Cancelled). Otherwise the
/// publish has won, and a cancel coming later is refused ([`Decision::cancel`]). `expected` is
/// how many bytes the fill is to write, when that is known before it starts (a download's or a
/// copy's source size): recorded in the private folder's lock, it is what lets a later save
/// tell a partial a dead sopen left from a complete download ([`sweep_dead`]).
pub(super) fn save_new_deciding(
    dest: &Path,
    expected: Option<u64>,
    fill: impl FnOnce(&mut File) -> Result<u64>,
    link: &mut dyn FnMut(LinkStep, &Path) -> std::io::Result<()>,
    decision: &Decision,
) -> std::result::Result<SavedTo, SaveError> {
    let nothing = |e: anyhow::Error| {
        SaveError::Failed(e.context(format!(
            "{} was not saved; nothing was transferred",
            dest.display()
        )))
    };
    if dest.as_os_str().as_bytes().ends_with(b"/") {
        return Err(nothing(anyhow!(
            "{} ends with '/', so it names a folder, not a file to save",
            dest.display()
        )));
    }
    let name = dest
        .file_name()
        .ok_or_else(|| nothing(anyhow!("{} has no file name", dest.display())))?;
    let cname = c_name(name.as_bytes()).map_err(|e| nothing(e.into()))?;
    let parent = match dest.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let dir = open_folder(&parent).map_err(nothing)?;
    // Anything already there — a file, a folder, a link (followed or not) — is taken.
    // SAFETY: `cname` is NUL-terminated; `st` is written by the call.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    match sys(unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            cname.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    }) {
        Ok(_) => return Err(exists(dest)),
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {}
        Err(e) => {
            return Err(nothing(
                anyhow::Error::new(e).context(format!("cannot look at {}", dest.display())),
            ))
        }
    }
    sweep_dead(&dir, &parent);
    let mut staging = Staging::make(dir, &parent, expected).map_err(nothing)?;
    let give_up = |staging: &mut Staging, e: anyhow::Error| {
        let left = staging.clean();
        with_left(e, left)
    };
    let prepared = (|| -> Result<(File, u32)> {
        let part = staging
            .create(PART, 0o600)
            .with_context(|| format!("cannot create {}", staging.shown.join("part").display()))?;
        let mode = staging
            .new_file_mode()
            .with_context(|| format!("cannot create a file in {}", staging.shown.display()))?;
        Ok((part, mode))
    })();
    let (mut file, mode) = match prepared {
        Ok(made) => made,
        Err(e) => return Err(nothing(give_up(&mut staging, e))),
    };
    let probed = link(LinkStep::Probe, &staging.shown)
        .and_then(|()| staging.link(None))
        .and_then(|()| {
            staging.made.push(PROBE);
            staging.unlink(PROBE)
        });
    if let Err(e) = probed {
        let e = if no_hard_links(&e) {
            anyhow!(
                "the filesystem of {} has no hard links ({e}); sopen can't publish there \
                 without risking a replace — choose another folder",
                parent.display()
            )
        } else {
            anyhow::Error::new(e).context(format!(
                "cannot test whether {} takes hard links",
                parent.display()
            ))
        };
        return Err(nothing(give_up(&mut staging, e)));
    }
    let filled = (|| -> Result<u64> {
        let n = fill(&mut file)?;
        file.sync_all()
            .with_context(|| format!("cannot write {}", staging.shown.join("part").display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .with_context(|| {
                format!(
                    "cannot set the mode of {}",
                    staging.shown.join("part").display()
                )
            })?;
        Ok(n)
    })();
    let bytes = match filled {
        Ok(n) => n,
        Err(e) => {
            let e = e.context(format!("{} was not saved", dest.display()));
            return Err(SaveError::Failed(give_up(&mut staging, e)));
        }
    };
    drop(file);
    let stood_in = link(LinkStep::Publish, &staging.shown);
    // The one decision (06 §7.5): the complete file gets its name now, or the cancel won.
    if stood_in.is_ok() && !decision.publish() {
        let e = anyhow!(crate::remote::Cancelled);
        return Err(SaveError::Failed(give_up(&mut staging, e)));
    }
    match stood_in.and_then(|()| staging.link(Some(&cname))) {
        Ok(()) => Ok(SavedTo {
            bytes,
            notes: staging.clean().into_iter().collect(),
        }),
        Err(e) if e.raw_os_error() == Some(libc::EEXIST) => {
            if let Some(left) = staging.clean() {
                eprintln!("sopen: {left}");
            }
            Err(exists(dest))
        }
        Err(e) => {
            let (part, lock_left) = staging.keep();
            let lock_left = lock_left.map(|l| format!(" ({l})")).unwrap_or_default();
            Err(SaveError::Failed(anyhow!(
                "cannot publish {} ({e}); nothing was put under that name — the complete \
                 download is kept at {}{lock_left}",
                dest.display(),
                part.display()
            )))
        }
    }
}

/// Whether a "Save to…" is cancelled or published (06 §7.5): decided once, at one point. A
/// cancel wins until the moment right before the complete file gets its name; from then on the
/// publish has won, the cancel is refused, and the save is reported as saved. So what the
/// owner is told is always what happened: `cancelled; nothing was saved` never comes with a
/// file under the name, and a file under the name is never called cancelled.
#[derive(Debug, Default)]
pub struct Decision(std::sync::atomic::AtomicU8);

impl Decision {
    const OPEN: u8 = 0;
    const CANCELLED: u8 = 1;
    const PUBLISHING: u8 = 2;

    pub const fn new() -> Self {
        Self(std::sync::atomic::AtomicU8::new(Self::OPEN))
    }

    /// Cancels the save, unless it is being published already: whether the cancel won.
    pub fn cancel(&self) -> bool {
        self.settle(Self::CANCELLED)
    }

    /// The save was cancelled (looked at between chunks, to stop the transfer early).
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst) == Self::CANCELLED
    }

    /// The complete file is about to get its name, unless the save was cancelled: whether
    /// the publish won.
    fn publish(&self) -> bool {
        self.settle(Self::PUBLISHING)
    }

    /// Open again, for the next save it decides (the one a signal cancels: [`interrupt`]).
    fn reopen(&self) {
        self.0.store(Self::OPEN, Ordering::SeqCst);
    }

    fn settle(&self, to: u8) -> bool {
        match self
            .0
            .compare_exchange(Self::OPEN, to, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => true,
            Err(now) => now == to,
        }
    }
}

/// What a transfer writes through until `stop` is cancelled: from then on every write fails,
/// which ends the read feeding it at its next response (06 §7.5: between chunks).
struct Stopping<'a, W> {
    inner: W,
    stop: &'a Decision,
}

impl<W: Write> Write for Stopping<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.stop.cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Whether a save ended because it was cancelled ([`download_to`], [`copy_to`]): nothing was
/// saved, and nothing is left of it.
pub fn save_cancelled(e: &SaveError) -> bool {
    matches!(e, SaveError::Failed(e) if crate::remote::is_cancelled(e))
}

/// Downloads `abs` into a new file at `dest` ([`save_new`]). Best-effort change check: a file
/// whose size or mtime differs after the transfer, or that gave a different number of bytes,
/// is not saved. `cancel` stops it between two responses (06 §7.5): the part and its private
/// folder are removed, nothing is published, and the error is
/// [`Cancelled`](crate::remote::Cancelled) ([`save_cancelled`]).
pub fn download_to(
    fs: &mut dyn RemoteFs,
    abs: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &Decision,
) -> std::result::Result<SavedTo, SaveError> {
    let before = fs.stat(abs).map_err(SaveError::Failed)?;
    if before.is_dir {
        return Err(SaveError::Failed(anyhow!("{abs} is a directory")));
    }
    let filled = |file: &mut File| {
        let mut out = Stopping {
            inner: std::io::BufWriter::with_capacity(1 << 20, &mut *file),
            stop: cancel,
        };
        let read = fs
            .read_to(abs, &mut out, &mut |done| progress(done, before.size))
            .and_then(|n| out.flush().map(|()| n).map_err(anyhow::Error::from));
        if cancel.cancelled() {
            return Err(anyhow!(crate::remote::Cancelled));
        }
        let n = read?;
        drop(out);
        let after = fs.stat_opt(abs)?;
        let same = after.is_some_and(|a| a.size == before.size && a.mtime == before.mtime);
        if !same || (before.missing != Some("size") && n != before.size) {
            bail!("{abs} changed while it was downloaded; try again when it is quiet");
        }
        Ok(n)
    };
    let expected = (before.missing != Some("size")).then_some(before.size);
    save_new_deciding(dest, expected, filled, &mut |_, _| Ok(()), cancel)
}

/// What a file's handle says about its bytes: size, mtime, change time.
fn handle_stamp(file: &File) -> std::io::Result<(u64, Option<SystemTime>, i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata()?;
    Ok((m.len(), m.modified().ok(), m.ctime(), m.ctime_nsec()))
}

/// Copies the local file `src` (the local file itself, or the cached copy) into a new file at
/// `dest` ([`save_new`]). Its handle's size, mtime and change time are compared before and
/// after the copy, and the bytes copied with its size: any difference and nothing is saved.
/// `cancel` stops it between two chunks, as [`download_to`]'s. `shown` is how the file is named
/// in what is said: the path itself for a local file, `host:/abs` when `src` is a remote file's
/// copy in sopen's cache — that path is never the owner's to see.
pub fn copy_to(
    src: &Path,
    shown: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &Decision,
) -> std::result::Result<SavedTo, SaveError> {
    let failed = |e: anyhow::Error| SaveError::Failed(e);
    let mut input = File::open(src)
        .with_context(|| format!("cannot open {shown}"))
        .map_err(failed)?;
    let before = handle_stamp(&input)
        .with_context(|| format!("cannot stat {shown}"))
        .map_err(failed)?;
    let filled = |file: &mut File| {
        let mut buf = vec![0u8; 1 << 20];
        let mut done = 0u64;
        loop {
            let n = input
                .read(&mut buf)
                .with_context(|| format!("cannot read {shown}"))?;
            if n == 0 {
                break;
            }
            if cancel.cancelled() {
                return Err(anyhow!(crate::remote::Cancelled));
            }
            file.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, before.0);
        }
        let after = handle_stamp(&input).with_context(|| format!("cannot stat {shown}"))?;
        if after != before || done != before.0 {
            bail!(
                "{shown} changed while it was copied ({done} bytes copied of {}); try again when it is quiet",
                before.0
            );
        }
        Ok(done)
    };
    let expected = Some(before.0);
    save_new_deciding(dest, expected, filled, &mut |_, _| Ok(()), cancel)
}

// ---------------------------------------------------------------------------------------------
// Carrying the answers out.

/// What the hex window opens: the file — a remote one as a stream, a local one read in place,
/// or the cached copy already there — what must be said whole in its rows, and how long a
/// fetch took (`None`: nothing was fetched, which is every open the production source makes).
pub struct Opened {
    pub file: HexFile,
    pub notices: Vec<String>,
    pub fetch_ms: Option<u64>,
}

/// How the dialog's answers are carried out — by the caller, who holds the connection. Each
/// runs on a worker thread; `progress` gets `(bytes done, total)`. A failure leaves the source
/// as it was: the owner may try again, or choose the other answer.
pub trait Source: Send {
    /// Hex: the file ready for the hex window — a remote one opened as a stream (06 §7.5:
    /// nothing fetched), a local one in place. A failure leaves the source as it was.
    fn open_hex(&mut self, progress: &mut dyn FnMut(u64, u64)) -> Result<Opened>;
    /// Save: the file's bytes in a new file at `dest` ([`save_new`]). `cancel` stops it
    /// between chunks (06 §7.5): nothing is saved, and the source is as it was.
    fn save_to(
        &mut self,
        dest: &Path,
        progress: &mut dyn FnMut(u64, u64),
        cancel: &Decision,
    ) -> std::result::Result<SavedTo, SaveError>;
}

/// The [`Source`] main uses: the file on its host (or the local disk). Hex opens a remote file
/// as a stream on this connection (06 §7.5: nothing is fetched); a local file is read in
/// place, and a copy already in the cache is opened as it is. Save downloads (or copies a
/// local or cached file).
pub struct FileSource {
    /// Taken when the hex window opens (it holds the connection from then on: a stream's
    /// transport, or a file read in place's `Origin`).
    fs: Option<Box<dyn RemoteFs + Send>>,
    /// Opens another connection to the file's host: a stream's retry, and its `Origin`'s
    /// first use.
    dial: Option<super::browser::Connect>,
    /// The kind sniff's bytes (06 §11), read on this connection before the dialog.
    sniff: u64,
    /// `[user@]host` for a remote file; `None` for a local one.
    label: Option<String>,
    abs: String,
    cache: PathBuf,
    /// Already in the cache, claimed (fetched for another reason, or by an earlier try).
    fetched: Option<crate::cache::Fetched>,
}

impl FileSource {
    pub fn new(
        fs: Box<dyn RemoteFs + Send>,
        reconnect: Option<super::Reconnect>,
        label: Option<String>,
        abs: &str,
        cache: &Path,
        fetched: Option<crate::cache::Fetched>,
    ) -> Self {
        let dial = reconnect.map(|reconnect| {
            let reconnect = Mutex::new(reconnect);
            Arc::new(move || (reconnect.lock().unwrap_or_else(|p| p.into_inner()))())
                as super::browser::Connect
        });
        Self {
            fs: Some(fs),
            dial,
            sniff: 0,
            label,
            abs: abs.to_string(),
            cache: cache.to_path_buf(),
            fetched,
        }
    }

    /// The bytes the kind sniff read of the file before the dialog (06 §11's `sniff`).
    pub fn sniffed(mut self, bytes: u64) -> Self {
        self.sniff = bytes;
        self
    }

    /// How the viewer's `Origin` connects again.
    fn reconnect(&self) -> Option<super::Reconnect> {
        let dial = self.dial.clone()?;
        Some(Box::new(move || dial()))
    }

    /// A remote file not in the cache yet opens through a stream (06 §7.1, §7.5): the
    /// connection becomes the stream's transport, nothing is fetched, and the hex window's
    /// `Origin` connects on its first use. Every step that can fail — the open, the handle's
    /// `fstat` — comes before the connection is handed over: a failure gives it back, and
    /// the source is as it was (never a quiet whole fetch instead).
    fn open_stream(&mut self, label: String) -> Result<Opened> {
        let fs = self.fs.take().context("the file was already opened")?;
        let repaint = hex::Repaint::default();
        let opening = crate::stream::Opening {
            label: format!("{label}:{}", self.abs),
            began: Instant::now(),
            sniff: self.sniff,
        };
        let dial = self.dial.clone();
        match super::streamed::open(fs, &self.abs, opening, dial, repaint.waker(), None) {
            Ok(opened) => {
                let origin = Origin::placeholder(
                    self.reconnect(),
                    label,
                    self.abs.clone(),
                    opened.at,
                    super::Placeholder::YES,
                );
                Ok(Opened {
                    file: HexFile::streamed(origin, opened.source, repaint, &self.cache),
                    notices: Vec::new(),
                    fetch_ms: None,
                })
            }
            Err(not) => {
                self.fs = not.conn;
                Err(anyhow!(crate::stream::explain(&not.error)))
            }
        }
    }

    /// The file as its owner knows it: `host:/abs`, or the local path. What fails on its copy
    /// in sopen's cache is said about this, never about the copy's path.
    fn shown(&self) -> String {
        match &self.label {
            Some(label) => format!("{label}:{}", self.abs),
            None => self.abs.clone(),
        }
    }

    fn host(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| crate::cache::LOCAL_HOST.to_string())
    }

    /// Fetches into the cache unless the copy is there or the file is local. Returns the fetch
    /// time when it fetched.
    fn make_local(&mut self, progress: &mut dyn FnMut(u64, u64)) -> Result<Option<u64>> {
        if self.fetched.is_some() || self.label.is_none() {
            return Ok(None);
        }
        let host = self.host();
        let fs = self.fs.as_mut().context("the file was already opened")?;
        let (fetched, ms) =
            crate::cache::fetch_claimed(fs.as_mut(), &host, &self.abs, &self.cache, progress)?;
        self.fetched = Some(fetched);
        Ok(Some(ms))
    }

    /// Without a window: the local file, or the cached copy (fetched now if it is not yet), and
    /// the fetch time when it fetched.
    pub fn local_path(
        &mut self,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(PathBuf, Option<u64>)> {
        let ms = self.make_local(progress)?;
        let path = match &self.fetched {
            Some(fetched) => fetched.claim.local.clone(),
            None => PathBuf::from(&self.abs),
        };
        Ok((path, ms))
    }
}

impl Source for FileSource {
    /// Every step that can fail comes before anything is handed over: on a failure the
    /// connection and any fetched copy stay here, for a retry or a Save to….
    fn open_hex(&mut self, progress: &mut dyn FnMut(u64, u64)) -> Result<Opened> {
        if self.fs.is_none() {
            bail!("the file was already opened");
        }
        if let (Some(label), None) = (self.label.clone(), &self.fetched) {
            return self.open_stream(label);
        }
        let fetch_ms = self.make_local(progress)?;
        let (base_path, at, notices) = match &self.fetched {
            Some(f) => (f.claim.local.clone(), f.at, f.notices.clone()),
            None => {
                let fs = self.fs.as_mut().expect("checked above");
                (PathBuf::from(&self.abs), fs.stat(&self.abs)?, Vec::new())
            }
        };
        let base = hex::Base::open_as(&base_path, &self.shown())?;
        // Nothing below fails: hand everything over.
        let fs = self.fs.take().expect("checked above");
        let claim = self.fetched.take().map(|f| f.claim);
        let origin = Origin::new(
            fs,
            self.reconnect(),
            self.label.clone(),
            self.abs.clone(),
            at,
        );
        Ok(Opened {
            file: HexFile::new(origin, base, claim, &self.cache),
            notices,
            fetch_ms,
        })
    }

    fn save_to(
        &mut self,
        dest: &Path,
        progress: &mut dyn FnMut(u64, u64),
        cancel: &Decision,
    ) -> std::result::Result<SavedTo, SaveError> {
        if let Some(fetched) = &self.fetched {
            // From the cached copy, said about the file: the copy's path is not the owner's.
            return copy_to(&fetched.claim.local, &self.shown(), dest, progress, cancel);
        }
        match self.fs.as_mut() {
            Some(_) if self.label.is_none() => {
                copy_to(Path::new(&self.abs), &self.abs, dest, progress, cancel)
            }
            Some(fs) => download_to(fs.as_mut(), &self.abs, dest, progress, cancel),
            None => Err(SaveError::Failed(anyhow!("the file was already opened"))),
        }
    }
}

/// The system's save dialog: the chosen path arrives on the receiver (`None` = no path).
pub trait Chooser {
    fn choose(&mut self, suggested: &Path) -> Receiver<Option<PathBuf>>;
}

/// [`Chooser`] over `rfd`: AppKit's save panel on macOS (on this thread, the main one, as
/// AppKit requires; it is modal), xdg-desktop-portal over D-Bus on Linux (on a thread of its
/// own, so the window keeps painting).
pub struct NativeChooser;

impl Chooser for NativeChooser {
    fn choose(&mut self, suggested: &Path) -> Receiver<Option<PathBuf>> {
        let (tx, rx) = mpsc::channel();
        let mut dialog = rfd::FileDialog::new().set_title("Save as");
        if let Some(dir) = suggested.parent().filter(|d| d.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(name) = suggested.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
        if cfg!(target_os = "macos") {
            let _ = tx.send(dialog.save_file());
        } else {
            std::thread::spawn(move || {
                let _ = tx.send(dialog.save_file());
            });
        }
        rx
    }
}

// ---------------------------------------------------------------------------------------------
// The route main takes.

/// The terminal's progress line for a transfer without a window.
pub trait Meter {
    fn start(&mut self, label: &str);
    fn bytes(&mut self, done: u64, total: u64);
    fn finish(&mut self);
}

/// How a binary file's run ended, for the usage log and the exit code.
#[derive(Debug, Clone, Default)]
pub struct FileOutcome {
    /// The usage log's action when the hex window opened (`None`: an open).
    pub action: Option<&'static str>,
    pub conflict: bool,
    /// The last save was refused (exit 3).
    pub refused: Option<String>,
    pub fetch_ms: Option<u64>,
    pub saved_to: Option<PathBuf>,
    /// The line said when the run ended without opening or saving anything.
    pub cancelled: Option<String>,
}

impl FileOutcome {
    fn hex(&mut self, outcome: Outcome) {
        self.action = Some(outcome.action());
        self.conflict = outcome.conflict;
        self.refused = outcome.refused;
    }
}

/// A file sopen has no window for, or any file with `--hex` (spec §6a): the dialog — the hex
/// window (the default) or Save to… — or, with `--hex` or `SOPEN_CHOICE`, that answer without
/// it. With no display the local copy's path is printed, as before the dialog existed. `run`
/// shows the dialog window ([`run_on_screen`]; tests drive it instead).
#[allow(clippy::too_many_arguments)]
pub fn open_file(
    info: Info,
    mut source: FileSource,
    hooks: &Hooks,
    hex_flag: bool,
    display: bool,
    meter: &mut dyn Meter,
    chooser: Box<dyn Chooser>,
    run: &mut dyn FnMut(BinaryWindow) -> Result<()>,
) -> Result<FileOutcome> {
    let choice = if hex_flag {
        Some(Choice::Hex)
    } else {
        hooks.choice
    };
    if choice.is_none() && hooks.screenshot.is_none() && (hooks.autosave || hooks.autoexport) {
        // A scripted run ignores input: the dialog would wait for an answer no one gives.
        bail!(
            "{} is a binary file: SOPEN_AUTOSAVE / SOPEN_AUTOEXPORT need SOPEN_CHOICE=hex (or \
             --hex) to say what to open",
            info.title
        );
    }
    let cancelled = cancel_line(info.local, Moved::Nothing);
    let windowless = match choice {
        Some(Choice::Save | Choice::Cancel) => true,
        Some(Choice::Hex) => hooks.autosave,
        None => false,
    };
    let mut out = FileOutcome::default();
    if !display && !windowless {
        meter.start(&format!("fetching {}", info.name));
        let local = source.local_path(&mut |d, t| meter.bytes(d, t));
        meter.finish();
        let (local, ms) = local?;
        out.fetch_ms = ms;
        eprintln!(
            "sopen: no display for a window (DISPLAY / WAYLAND_DISPLAY unset); printing the \
             local path instead"
        );
        println!("{}", local.display());
        return Ok(out);
    }
    match choice {
        Some(Choice::Cancel) => {
            eprintln!("{cancelled}");
            out.cancelled = Some(cancelled.to_string());
        }
        Some(Choice::Save) => {
            meter.start(&format!("saving {}", info.name));
            // No window, so no Esc: Ctrl-C and SIGTERM cancel this save while it runs, and
            // only while it runs ([`interrupt`]).
            let watch = interrupt::Watch::start();
            let cancel = watch.decision();
            let mut hold = hooks.save_hold;
            let mut progress = |d: u64, t: u64| {
                meter.bytes(d, t);
                if let Some(hold) = hold.take() {
                    // `SOPEN_SAVE_HOLD_MS`: mid-way, until the time is up or a cancel comes
                    // (`:stalled`: whatever comes, as a stalled remote).
                    let until = Instant::now() + hold;
                    while Instant::now() < until && (hooks.save_stalled || !cancel.cancelled()) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            };
            let saved = save_without_window(&info, &mut source, hooks, &mut progress, cancel);
            let interrupted = watch.end();
            meter.finish();
            let (dest, saved) = match (saved, interrupted) {
                // The cancel won, whatever the transfer said as it stopped (Ctrl-C reaches
                // ssh too, whose end may be the error): nothing has the name, the part and
                // its folder are gone, and main ends with 128 + the signal.
                (Err(e), Some(interrupted)) => {
                    // …unless cleanup could not remove it: that is still said.
                    if let Some(left) = e.downcast_ref::<Left>() {
                        eprintln!("sopen: {left}");
                    }
                    return Err(interrupted.into());
                }
                (saved, _) => saved?,
            };
            println!("saved {}", dest.display());
            for note in saved.notes {
                eprintln!("sopen: {note}");
            }
            out.saved_to = Some(dest);
        }
        // `--hex`, and the windowless autosave: a remote file opens as a stream on this
        // connection and a local one in place (06 §7.1, §7.5) — nothing is fetched, so the
        // terminal has no progress line to show.
        Some(Choice::Hex) if hex_flag || hooks.autosave => {
            let opened = source.open_hex(&mut |_, _| {})?;
            out.fetch_ms = opened.fetch_ms;
            out.hex(hex::open(opened.file, hooks, opened.notices)?);
        }
        // The dialog; with SOPEN_CHOICE=hex already answered (the hex window is opened at once,
        // in the dialog's window, which then becomes it: a remote file as a stream).
        Some(Choice::Hex) | None => {
            let local = info.local;
            let ended = dialog(info, Box::new(source), chooser, hooks, run)?;
            out.fetch_ms = ended.fetch_ms;
            match ended.hex {
                Some(outcome) => out.hex(outcome),
                None if ended.saved_to.is_none() => {
                    let line = cancel_line(local, ended.moved);
                    eprintln!("{line}");
                    out.cancelled = Some(line.to_string());
                }
                None => {}
            }
            out.saved_to = ended.saved_to;
        }
    }
    Ok(out)
}

/// What a run that opened and saved nothing transferred before it ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Moved {
    /// No fetch or download was started.
    #[default]
    Nothing,
    /// One was started and did not land (it may have moved some bytes).
    Started,
    /// A "Save to…" was cancelled (06 §7.5): its part and private folder are gone.
    Stopped,
}

/// The line said when a binary file's run ends without opening or saving it: it claims nothing
/// was downloaded only when that is so.
pub fn cancel_line(local: bool, moved: Moved) -> &'static str {
    match (local, moved) {
        (_, Moved::Stopped) => SAVE_CANCELLED_LINE,
        (true, _) | (false, Moved::Started) => "sopen: cancelled",
        (false, Moved::Nothing) => "sopen: cancelled; nothing was downloaded",
    }
}

/// "Save to…" answered without a window (`SOPEN_CHOICE=save`): saves to `SOPEN_SAVE_TO`, else
/// the download folder (a folder gets the file's name inside it), never over anything — a taken
/// path is an error naming the next free name. Returns where it saved. `cancel` stops it between
/// chunks: without a window that is a signal's to do ([`interrupt`]).
pub fn save_without_window(
    info: &Info,
    source: &mut dyn Source,
    hooks: &Hooks,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &Decision,
) -> Result<(PathBuf, SavedTo)> {
    let dest = first_path(&info.name, hooks);
    let dest = if dest.is_dir() {
        dest.join(&info.name)
    } else {
        dest
    };
    match source.save_to(&dest, progress, cancel) {
        Ok(saved) => Ok((dest, saved)),
        Err(SaveError::Exists { next }) => bail!(
            "{} exists; not replaced (the next free name is {})",
            dest.display(),
            next.display()
        ),
        Err(SaveError::Failed(e)) => Err(e),
    }
}

/// Shows the dialog window on the screen until it closes.
pub fn run_on_screen(window: BinaryWindow, hooks: &Hooks) -> Result<()> {
    let title = window.info.title.clone();
    super::run_window_sized(&title, WINDOW, MIN_SIZE, window, hooks)
}

/// The dialog (one window: the dialog, the transfer, then the hex window if chosen), shown by
/// `run`; returns how it ended once it closed. A transfer or a save still running when the
/// window went away is waited for, its result printed.
pub fn dialog(
    info: Info,
    source: Box<dyn Source>,
    chooser: Box<dyn Chooser>,
    hooks: &Hooks,
    run: &mut dyn FnMut(BinaryWindow) -> Result<()>,
) -> Result<Ended> {
    let window = BinaryWindow::new(info, source, chooser, hooks);
    let (ended, hex_outcome, hex_leftover, leftover) = (
        window.ended.clone(),
        window.hex_outcome.clone(),
        window.hex_leftover.clone(),
        window.leftover.clone(),
    );
    let ran = run(window);
    hex::finish(&hex_leftover, &hex_outcome);
    if let Some(rx) = leftover.lock().unwrap().take() {
        eprintln!("sopen: the window closed during a transfer; waiting for it to finish");
        match rx.recv() {
            Ok(Landed {
                result: MovedResult::Saved(dest, Ok(saved)),
                ..
            }) => {
                println!("saved {}", dest.display());
                for note in saved.notes {
                    eprintln!("sopen: {note}");
                }
                ended.lock().unwrap().saved_to = Some(dest);
            }
            Ok(Landed {
                result: MovedResult::Saved(_, Err(e)),
                ..
            }) if save_cancelled(&e) => ended.lock().unwrap().moved = Moved::Stopped,
            Ok(Landed {
                result: MovedResult::Saved(dest, Err(e)),
                ..
            }) => eprintln!("sopen: {} not saved: {e}", dest.display()),
            Ok(Landed {
                result: MovedResult::Hex(result),
                ..
            }) => match *result {
                Ok(opened) => {
                    eprintln!("sopen: the window was closed, so the hex editor did not open");
                    ended.lock().unwrap().fetch_ms = opened.fetch_ms;
                }
                Err(e) => eprintln!("sopen: {e:#}"),
            },
            Err(_) => eprintln!("sopen: the transfer stopped without a result"),
        }
    }
    ran?;
    let mut ended = ended.lock().unwrap().clone();
    if ended.hex.is_some() {
        ended.hex = Some(hex_outcome.lock().unwrap().clone());
    }
    Ok(ended)
}

// ---------------------------------------------------------------------------------------------
// The window.

/// How the dialog ended, for the usage log and the exit code.
#[derive(Debug, Default, Clone)]
pub struct Ended {
    /// Where "Save to…" saved the file.
    pub saved_to: Option<PathBuf>,
    /// The hex window's outcome, when it opened.
    pub hex: Option<Outcome>,
    pub fetch_ms: Option<u64>,
    /// What was transferred, for the closing line.
    pub moved: Moved,
}

/// A fetch or a download, back from its worker with the source.
struct Landed {
    source: Box<dyn Source>,
    result: MovedResult,
}

enum MovedResult {
    Hex(Box<Result<Opened>>),
    Saved(PathBuf, std::result::Result<SavedTo, SaveError>),
}

enum Stage {
    Ask,
    /// The system's save dialog is open.
    Choosing(Receiver<Option<PathBuf>>),
    /// Opening the hex window (`dest` = `None`) or saving to `dest`. `cancel` stops a save
    /// between chunks (06 §7.5: Esc, or closing the window).
    Moving {
        dest: Option<PathBuf>,
        done: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        cancel: Arc<Decision>,
        rx: Receiver<Landed>,
    },
    /// Saved: the line stays a moment, then the window closes — unless there are notes to read.
    Saved {
        line: String,
        notes: Vec<String>,
        at: Instant,
    },
    /// The transfer's worker ended without a result: nothing more can be done here.
    Failed(String),
    Hex(Box<HexWindow>),
}

pub struct BinaryWindow {
    info: Info,
    hooks: Hooks,
    stage: Stage,
    source: Option<Box<dyn Source>>,
    chooser: Box<dyn Chooser>,
    /// The path field's text.
    path: String,
    /// What the dialog must say whole: (text, is an error).
    notes: Vec<(String, bool)>,
    ended: Arc<Mutex<Ended>>,
    hex_outcome: Arc<Mutex<Outcome>>,
    hex_leftover: Arc<Mutex<Option<hex::Job>>>,
    /// A transfer still running when the window is dropped; its opener waits for it.
    leftover: Arc<Mutex<Option<Receiver<Landed>>>>,
    close_when_done: bool,
    close_ok: bool,
    painted: bool,
    /// `SOPEN_CHOICE=hex`: the answer is given; the first pass starts the fetch, as Enter would.
    prepicked: bool,
    /// The running save was started by Enter in the path field: if it does not land, the field
    /// gets the keyboard back (so the next Enter saves again rather than opening the hex editor).
    field_save: bool,
    /// Passes so far, and the height last asked of the window (and when).
    frames: u64,
    asked: Option<(f32, u64)>,
    /// A file at a revision (03 §6.3, S2-F): "Save to…" is off with the reason, and the hex
    /// window it becomes is read-only. The browser never routes such a file here (it opens
    /// anything that is not text, an image or media in the hex window); this keeps the dialog
    /// from being a way to save one.
    read_only: bool,
}

impl Drop for BinaryWindow {
    fn drop(&mut self) {
        if let Stage::Moving { .. } = self.stage {
            if let Stage::Moving { rx, .. } = std::mem::replace(&mut self.stage, Stage::Ask) {
                *self.leftover.lock().unwrap() = Some(rx);
            }
        }
    }
}

fn path_id() -> egui::Id {
    egui::Id::unique("sopen-binary-path")
}

impl BinaryWindow {
    pub fn new(
        info: Info,
        source: Box<dyn Source>,
        chooser: Box<dyn Chooser>,
        hooks: &Hooks,
    ) -> Self {
        let first = first_path(&info.name, hooks);
        // A taken SOPEN_SAVE_TO is said when Save is chosen (it is what the run asked for).
        let (path, note) = if hooks.save_to.is_some() {
            (first, None)
        } else {
            suggest(first)
        };
        let notes = note.map(|n| (n, false)).into_iter().collect();
        Self {
            info,
            hooks: hooks.clone(),
            stage: Stage::Ask,
            source: Some(source),
            chooser,
            path: tilde(&path),
            notes,
            ended: Arc::default(),
            hex_outcome: Arc::default(),
            hex_leftover: Arc::default(),
            leftover: Arc::default(),
            close_when_done: false,
            close_ok: false,
            painted: false,
            prepicked: hooks.choice == Some(Choice::Hex),
            field_save: false,
            frames: 0,
            asked: None,
            read_only: false,
        }
    }

    /// A fetch or a download is under way: from now on the closing line no longer claims that
    /// nothing was downloaded.
    fn started(&mut self) {
        let mut ended = self.ended.lock().unwrap();
        if ended.moved == Moved::Nothing {
            ended.moved = Moved::Started;
        }
    }

    fn asking(&self) -> bool {
        matches!(self.stage, Stage::Ask)
    }

    /// Hex: open the file on a worker — a remote one as a stream, a local one in place — then
    /// become the hex window.
    fn start_hex(&mut self, ctx: &egui::Context) {
        let Some(mut source) = self.source.take() else {
            return;
        };
        self.started();
        self.notes.clear();
        self.field_save = false;
        let (done, total) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let (tx, rx) = mpsc::channel();
        let (d, t, ctx2) = (done.clone(), total.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = source.open_hex(&mut |now, all| {
                d.store(now, Ordering::Relaxed);
                t.store(all, Ordering::Relaxed);
                ctx2.request_repaint();
            });
            let _ = tx.send(Landed {
                source,
                result: MovedResult::Hex(Box::new(result)),
            });
            ctx2.request_repaint();
        });
        self.stage = Stage::Moving {
            dest: None,
            done,
            total,
            cancel: Arc::default(),
            rx,
        };
    }

    /// Save to the path in the field — exactly as typed (only an empty field is refused) — on a
    /// worker. A folder gets the file's name inside it.
    fn start_save(&mut self, ctx: &egui::Context, from_field: bool) {
        if self.read_only {
            let why = super::image::AT_A_REVISION;
            self.notes = vec![(format!("cannot save: {why}"), true)];
            return;
        }
        if self.path.trim().is_empty() {
            self.notes = vec![("type a path to save to".to_string(), true)];
            if from_field {
                ctx.memory_mut(|m| m.request_focus(path_id()));
            }
            return;
        }
        let mut dest = untilde(&self.path);
        if dest.is_dir() {
            dest = dest.join(&self.info.name);
        }
        let Some(mut source) = self.source.take() else {
            return;
        };
        self.started();
        self.notes.clear();
        self.field_save = from_field;
        let (done, total) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let (tx, rx) = mpsc::channel();
        let (d, t, ctx2, to) = (done.clone(), total.clone(), ctx.clone(), dest.clone());
        let cancel = Arc::new(Decision::default());
        let stop = cancel.clone();
        std::thread::spawn(move || {
            let result = source.save_to(
                &to,
                &mut |now, all| {
                    d.store(now, Ordering::Relaxed);
                    t.store(all, Ordering::Relaxed);
                    ctx2.request_repaint();
                },
                &stop,
            );
            let _ = tx.send(Landed {
                source,
                result: MovedResult::Saved(to, result),
            });
            ctx2.request_repaint();
        });
        self.stage = Stage::Moving {
            dest: Some(dest),
            done,
            total,
            cancel,
            rx,
        };
    }

    fn browse(&mut self) {
        let suggested = untilde(&self.path);
        let suggested = if suggested.is_dir() {
            suggested.join(&self.info.name)
        } else {
            suggested
        };
        self.notes.clear();
        self.stage = Stage::Choosing(self.chooser.choose(&suggested));
    }

    fn close(&mut self, ctx: &egui::Context) {
        self.close_ok = true;
        super::request_close(ctx);
    }

    fn poll(&mut self, ctx: &egui::Context) {
        match &self.stage {
            Stage::Choosing(rx) => match rx.try_recv() {
                Ok(Some(path)) => {
                    self.path = tilde(&path);
                    self.stage = Stage::Ask;
                    // The system dialog's own Save button was the confirmation.
                    self.start_save(ctx, false);
                }
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.stage = Stage::Ask;
                    self.notes = vec![(BROWSE_NONE.to_string(), false)];
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            },
            Stage::Moving { rx, .. } => match rx.try_recv() {
                Ok(moved) => self.landed(ctx, moved),
                Err(mpsc::TryRecvError::Disconnected) => {
                    let message = "the transfer stopped without a result (its worker ended); \
                                   nothing more can be done in this window"
                        .to_string();
                    eprintln!("sopen: {message}");
                    self.stage = Stage::Failed(message);
                    if self.close_when_done {
                        self.close(ctx);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
            },
            Stage::Saved { at, notes, .. } => {
                if notes.is_empty() {
                    let elapsed = at.elapsed();
                    if elapsed >= SAVED_LINGER {
                        self.close(ctx);
                    } else {
                        ctx.request_repaint_after(SAVED_LINGER.saturating_sub(elapsed));
                    }
                }
            }
            Stage::Ask | Stage::Failed(_) | Stage::Hex(_) => {}
        }
    }

    /// A fetch or a download came back.
    fn landed(&mut self, ctx: &egui::Context, moved: Landed) {
        self.source = Some(moved.source);
        self.stage = Stage::Ask;
        match moved.result {
            MovedResult::Saved(dest, Ok(saved)) => {
                println!("saved {}", dest.display());
                for note in &saved.notes {
                    eprintln!("sopen: {note}");
                }
                self.ended.lock().unwrap().saved_to = Some(dest.clone());
                self.stage = Stage::Saved {
                    line: format!("saved {}", tilde(&dest)),
                    notes: saved.notes,
                    at: Instant::now(),
                };
                if self.close_when_done {
                    self.close(ctx);
                }
                ctx.request_repaint();
            }
            MovedResult::Saved(dest, Err(SaveError::Exists { next })) => {
                self.notes = vec![(
                    format!(
                        "{} exists — not replaced. Save to… again saves as {}",
                        tilde(&dest),
                        next.file_name().unwrap_or_default().to_string_lossy()
                    ),
                    true,
                )];
                self.path = tilde(&next);
                self.after_failure(ctx);
            }
            // Esc or closing stopped it (06 §7.5): nothing was saved and nothing is left of
            // it; the dialog asks again — or the window closes, and the run's last line says it.
            MovedResult::Saved(_, Err(e)) if save_cancelled(&e) => {
                self.ended.lock().unwrap().moved = Moved::Stopped;
                if !self.close_when_done {
                    eprintln!("{SAVE_CANCELLED_LINE}");
                }
                self.notes = vec![(SAVE_CANCELLED.to_string(), false)];
                self.after_failure(ctx);
            }
            MovedResult::Saved(_, Err(SaveError::Failed(e))) => {
                eprintln!("sopen: {e:#}");
                self.notes = vec![(format!("{e:#}"), true)];
                self.after_failure(ctx);
            }
            MovedResult::Hex(result) => self.opened(ctx, *result),
        }
    }

    /// The fetch for the hex window came back: become the hex window.
    fn opened(&mut self, ctx: &egui::Context, result: Result<Opened>) {
        match result {
            Ok(opened) => {
                self.ended.lock().unwrap().fetch_ms = opened.fetch_ms;
                if self.close_when_done {
                    eprintln!(
                        "sopen: the window was closed while the file was being opened; the hex \
                         editor did not open"
                    );
                    self.close(ctx);
                    return;
                }
                match hex::embedded(
                    opened.file,
                    &self.hooks,
                    opened.notices,
                    self.hex_outcome.clone(),
                    self.hex_leftover.clone(),
                ) {
                    Ok(window) => {
                        self.ended.lock().unwrap().hex = Some(Outcome::default());
                        if self.hooks.window_size.is_none() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(
                                [480.0, 320.0].into(),
                            ));
                            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                                hex::WINDOW.into(),
                            ));
                        }
                        let mut window = window;
                        if self.read_only {
                            window.set_read_only(true);
                        }
                        self.stage = Stage::Hex(Box::new(window));
                    }
                    Err(e) => {
                        self.notes = vec![(format!("{e:#}"), true)];
                        self.after_failure(ctx);
                    }
                }
            }
            Err(e) => {
                eprintln!("sopen: {e:#}");
                self.notes = vec![(format!("{e:#}"), true)];
                self.after_failure(ctx);
            }
        }
    }

    /// A transfer that did not land: closing asked for meanwhile closes now (the reason is on the
    /// terminal too); a save started from the path field gives it the keyboard back.
    fn after_failure(&mut self, ctx: &egui::Context) {
        if self.close_when_done {
            self.close(ctx);
        } else if std::mem::take(&mut self.field_save) {
            ctx.memory_mut(|m| m.request_focus(path_id()));
        }
    }

    /// The dialog's content; returns where it ends (its height decides the window's).
    fn dialog(&mut self, ui: &mut egui::Ui) -> f32 {
        let ctx = ui.ctx().clone();
        if let Stage::Failed(message) = &self.stage {
            let message = message.clone();
            ui.add_space(4.0);
            ui.add(egui::Label::new(egui::RichText::new(&self.info.name).heading()).truncate());
            let error = ui.visuals().error_fg_color;
            super::disclosures(ui, &[(message, error)]);
            ui.add_space(8.0);
            if ui.button("Close").clicked()
                || ctx.input_mut(|i| {
                    i.consume_key(egui::Modifiers::NONE, Key::Escape)
                        || i.consume_key(egui::Modifiers::NONE, Key::Enter)
                })
            {
                self.close(&ctx);
            }
            return ui.cursor().top();
        }
        let asking = self.asking();
        let local = self.info.local;
        let mut path_enter = false;
        ui.add_space(4.0);
        ui.add(egui::Label::new(egui::RichText::new(&self.info.name).heading()).truncate());
        ui.label(self.info.describe());
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let fill = ui.visuals().selection.bg_fill;
            let hex = ui
                .add_enabled(
                    asking,
                    egui::Button::new(egui::RichText::new("Open in hex editor").strong())
                        .fill(fill),
                )
                .on_hover_text("Enter — opens it in the xtools hex editor");
            if hex.clicked() {
                self.start_hex(&ctx);
            }
            let save = ui
                .add_enabled(asking && !self.read_only, egui::Button::new("Save to…"))
                .on_hover_text(if local {
                    "copies it to the path below; never over an existing file"
                } else {
                    "downloads it to the path below; never over an existing file"
                })
                .on_disabled_hover_text(super::image::AT_A_REVISION);
            if save.clicked() {
                self.start_save(&ctx, false);
            }
            // While "Save to…" runs it stops that (06 §7.5), as Esc does; else it closes.
            let saving = match &self.stage {
                Stage::Moving {
                    dest: Some(_),
                    cancel,
                    ..
                } => Some(cancel.clone()),
                _ => None,
            };
            let cancel = ui
                .add_enabled(asking || saving.is_some(), egui::Button::new("Cancel"))
                .on_hover_text("Esc");
            if cancel.clicked() {
                match saving {
                    Some(stop) => drop(stop.cancel()),
                    None => self.close(&ctx),
                }
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let room = (ui.available_width() - 90.0).max(120.0);
            let field = ui.add_enabled(
                asking,
                chrome::field(&mut self.path)
                    .id(path_id())
                    .desired_width(room),
            );
            // Enter in the path field saves there (it never opens the hex editor).
            if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                path_enter = true;
            }
            if ui
                .add_enabled(asking, egui::Button::new("Browse…"))
                .on_hover_text("the system's save dialog")
                .clicked()
            {
                self.browse();
            }
        });
        ui.add_space(4.0);
        ui.label(egui::RichText::new(hint(local)).weak());
        match &self.stage {
            Stage::Moving {
                dest, done, total, ..
            } => {
                ui.add_space(6.0);
                let (d, t) = (done.load(Ordering::Relaxed), total.load(Ordering::Relaxed));
                let frac = if t > 0 { d as f32 / t as f32 } else { 0.0 };
                ui.label(match (dest, local) {
                    (Some(dest), _) => format!("saving to {}… Esc cancels", tilde(dest)),
                    // A remote file opens as a stream (06 §7.5): nothing is fetched for it.
                    (None, _) => "opening it in the hex editor…".to_string(),
                });
                let label = format!(
                    "{:.0}% · {} / {}",
                    frac * 100.0,
                    crate::human(d),
                    crate::human(t)
                );
                let label = egui::RichText::new(label).color(chrome::PALETTE.field_text);
                ui.add(egui::ProgressBar::new(frac).text(label));
            }
            Stage::Saved { line, notes, .. } => {
                ui.add_space(6.0);
                ui.label(line.as_str());
                if !notes.is_empty() {
                    let plain = ui.visuals().text_color();
                    let lines: Vec<(String, Color32)> =
                        notes.iter().map(|n| (n.clone(), plain)).collect();
                    super::disclosures(ui, &lines);
                    if ui.button("Close").clicked() {
                        self.close(&ctx);
                    }
                }
            }
            Stage::Choosing(_) => {
                ui.add_space(6.0);
                ui.label("waiting for the system's save dialog…");
            }
            _ => {}
        }
        let (error, plain) = (ui.visuals().error_fg_color, ui.visuals().text_color());
        let lines: Vec<(String, Color32)> = self
            .notes
            .iter()
            .map(|(t, e)| (t.clone(), if *e { error } else { plain }))
            .collect();
        if !lines.is_empty() {
            ui.add_space(4.0);
            super::disclosures(ui, &lines);
        }
        if self.close_when_done {
            super::disclosures(ui, &[(CLOSES_AFTER.to_string(), plain)]);
        }
        // Esc during "Save to…" cancels it (06 §7.5); Enter does nothing then, and neither does
        // anything while the hex window is being opened.
        if let Stage::Moving {
            dest: Some(_),
            cancel,
            ..
        } = &self.stage
        {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
                cancel.cancel();
            }
        }
        // Keys while asking: Enter in the path field saves; Enter with nothing focused opens
        // the hex editor (a focused button takes Enter as its own click); Esc cancels.
        if self.asking() {
            if path_enter {
                self.start_save(&ctx, true);
            } else if ctx.memory(|m| m.focused()).is_none()
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter))
            {
                self.start_hex(&ctx);
            }
            if self.asking() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
            {
                self.close(&ctx);
            }
        }
        ui.cursor().top()
    }

    /// The window as tall as its content — `bottom` is where the dialog's shadow ends — unless
    /// `SOPEN_WINDOW_SIZE` fixed it.
    fn fit(&mut self, ctx: &egui::Context, bottom: f32) {
        if self.hooks.window_size.is_some() {
            return;
        }
        let height = bottom.ceil().max(MIN_SIZE[1]);
        if self.asked.is_some_and(|(h, _)| (h - height).abs() < 1.0) {
            return;
        }
        let width = ctx.viewport_rect().width().max(MIN_SIZE[0]);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(width, height)));
        self.asked = Some((height, self.frames));
    }

    /// The height the window was last asked to have.
    #[cfg(test)]
    fn asked_height(&self) -> Option<f32> {
        self.asked.map(|(h, _)| h)
    }
}

/// The dialog's one-line hint: what Enter and Esc do, and — when it is so — that nothing is
/// downloaded before a choice.
fn hint(local: bool) -> &'static str {
    if local {
        "Enter opens the hex editor; Esc cancels."
    } else {
        "Nothing is downloaded until you choose. Enter opens the hex editor; Esc cancels."
    }
}

impl Window for BinaryWindow {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.frames += 1;
        if std::mem::take(&mut self.prepicked) && self.asking() {
            self.start_hex(&ctx);
        }
        self.poll(&ctx);
        if let Stage::Hex(window) = &mut self.stage {
            return window.ui(ui);
        }
        // The F-key row at the bottom (F3 closes, as Cmd/Ctrl+W and Esc do).
        super::own_keys(ui, &[]);
        // A PC Shell dialog box on the blue screen: cyan, its double line a cell in, the file's
        // question in it, the shadow two cells right and one down — over the F-key row, which
        // shows through it. The box (and its shadow) are laid under the content, which decides
        // how tall they are.
        let cell = chrome::CELL;
        let margin = egui::Margin {
            left: (2.0 * cell.x) as i8,
            right: (4.0 * cell.x) as i8,
            top: (1.5 * cell.y) as i8,
            bottom: 0,
        };
        let bottom = egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(chrome::PALETTE.back)
                    .inner_margin(margin),
            )
            .show(ui, |ui| {
                let window = ui.ctx().viewport_rect();
                // The whole window, the F-key row's cells too (the shadow falls on them).
                let mut screen = ui.painter().clone();
                screen.set_clip_rect(window);
                let under = screen.add(egui::Shape::Noop);
                chrome::dialog_visuals(ui);
                // Where the last line ends (the cursor is a line's spacing below it).
                let end = self.dialog(ui) - ui.spacing().item_spacing.y;
                let dialog = Rect::from_min_max(
                    window.min,
                    Pos2::new(window.right() - 2.0 * cell.x, end + BOX_BELOW),
                );
                let p = chrome::PALETTE;
                screen.set(
                    under,
                    egui::Shape::Vec(vec![
                        egui::Shape::rect_filled(
                            dialog.translate(Vec2::new(2.0 * cell.x, cell.y)),
                            0.0,
                            p.shadow,
                        ),
                        egui::Shape::rect_filled(dialog, 0.0, p.dialog_back),
                    ]),
                );
                chrome::dialog_lines(&screen, dialog, TITLE);
                // The shadow's row is the F-key row's.
                dialog.bottom() + cell.y
            })
            .inner;
        self.fit(&ctx, bottom);
        self.painted = true;
    }

    /// A file at a revision: "Save to…" off, the hex window it becomes read-only (03 §6.3).
    fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
        if let Stage::Hex(window) = &mut self.stage {
            window.set_read_only(read_only);
        }
    }

    fn ready(&self) -> bool {
        // On screen once the window has its content's size (a pass or two after asking).
        let sized = self.hooks.window_size.is_some()
            || self.asked.is_some_and(|(_, at)| self.frames >= at + 2);
        match &self.stage {
            Stage::Ask | Stage::Saved { .. } | Stage::Failed(_) => self.painted && sized,
            Stage::Hex(window) => window.ready(),
            Stage::Choosing(_) | Stage::Moving { .. } => false,
        }
    }

    fn close_requested(&mut self) -> bool {
        if self.close_ok {
            return true;
        }
        match &mut self.stage {
            Stage::Hex(window) => window.close_requested(),
            Stage::Moving { dest, cancel, .. } => {
                // A "Save to…" is cancelled (06 §7.5): it stops between chunks and leaves
                // nothing, then the window closes. Opening the hex window is waited for.
                if dest.is_some() {
                    cancel.cancel();
                }
                self.close_when_done = true;
                false
            }
            Stage::Ask | Stage::Choosing(_) | Stage::Saved { .. } | Stage::Failed(_) => true,
        }
    }
}

#[cfg(test)]
mod tests;

/// S2-F: the dialog is never a way to save a file at a revision (S2-E's hand-over).
#[cfg(test)]
mod read_only_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A source that counts what is asked of it and does nothing.
    struct Counted(Arc<AtomicUsize>);

    impl Source for Counted {
        fn open_hex(&mut self, _: &mut dyn FnMut(u64, u64)) -> Result<Opened> {
            anyhow::bail!("not here")
        }

        fn save_to(
            &mut self,
            _: &Path,
            _: &mut dyn FnMut(u64, u64),
            _: &Decision,
        ) -> std::result::Result<SavedTo, SaveError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(SaveError::Failed(anyhow::anyhow!("not here")))
        }
    }

    struct Never;

    impl Chooser for Never {
        fn choose(&mut self, _: &Path) -> Receiver<Option<PathBuf>> {
            std::sync::mpsc::channel().1
        }
    }

    /// A click on "Save to…" (off) saves nothing, and the save itself (`start_save`, where the
    /// button and Enter in the path field both lead) refuses with the reason in the dialog's
    /// notes. The hover text and the Enter key are not driven here.
    #[test]
    fn a_file_at_a_revision_has_no_save_to() {
        let saves = Arc::new(AtomicUsize::new(0));
        let tmp = tempfile::tempdir().unwrap();
        let hooks = Hooks {
            save_to: Some(tmp.path().join("out.bin")),
            ..Hooks::default()
        };
        let info = Info {
            title: "/r/blob.bin @ 0123abcd".into(),
            name: "blob.bin".into(),
            size: 4,
            local: true,
        };
        let mut win = BinaryWindow::new(
            info,
            Box::new(Counted(saves.clone())),
            Box::new(Never),
            &hooks,
        );
        win.set_read_only(true);
        let mut drv = crate::gui::testkit::Driver::new(WINDOW[0], 320.0);
        drv.idle(&mut win, 3);
        drv.click_text(&mut win, "Save to…");
        drv.idle(&mut win, 2);
        let ctx = drv.ctx.clone();
        win.start_save(&ctx, true);
        drv.idle(&mut win, 2);
        assert_eq!(saves.load(Ordering::SeqCst), 0, "nothing saved");
        assert!(
            win.notes
                .iter()
                .any(|(n, e)| *e && n.contains(super::super::image::AT_A_REVISION)),
            "{:?}",
            win.notes
        );
        assert!(!tmp.path().join("out.bin").exists());
    }
}
