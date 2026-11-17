//! Binary files (spec §6a): a file sopen has no window for (a zip, any binary) opens a small
//! dialog first — from the stat alone, nothing downloaded yet — that offers the hex window (the
//! default, Enter) or "Save to…" a chosen path; Esc or closing it downloads nothing. Choosing hex
//! fetches into the cache with the progress in the dialog, then the same window becomes the hex
//! window. Choosing save downloads straight to the chosen path, created exclusively: an existing
//! file is never replaced — the dialog says so and offers the next free `<stem>-N<ext>`.
//!
//! [`open_file`] is the whole route main takes for such a file — the dialog, `--hex`, the
//! `SOPEN_CHOICE` answers, no display — over [`FileSource`], which carries the answers out on the
//! connection. The dialog's answers go through [`Source`] and the native save dialog through
//! [`Chooser`]; both are traits, so the tests drive the real window with stand-ins too.

use super::hex::{self, HexFile, HexWindow};
use super::{Choice, Hooks, Origin, Outcome, Window};
use crate::remote::RemoteFs;
use anyhow::{anyhow, bail, Context as _, Result};
use eframe::egui::{self, Color32, Key};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// The dialog's size when it opens (its height then follows its content).
pub const WINDOW: [f32; 2] = [560.0, 240.0];
/// The smallest the dialog may be made.
const MIN_SIZE: [f32; 2] = [420.0, 160.0];
/// How long "saved <path>" stays before the window closes.
const SAVED_LINGER: Duration = Duration::from_millis(1200);
/// Browse… came back without a path: cancelled, or no system dialog could be shown (rfd says
/// the same for both).
pub const BROWSE_NONE: &str =
    "Browse… gave no path (cancelled, or no system save dialog here) — the path field still works";
/// Closing was asked for during a download or a fetch.
pub const CLOSES_AFTER: &str = "the window closes when the transfer is done";

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
/// dangling link.
pub fn next_free(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for n in 1..10_000 {
        let candidate = path.with_file_name(crate::remote::suffixed(&name, n));
        if std::fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
    path.with_file_name(crate::remote::suffixed(&name, 10_000))
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
    let next = next_free(&path);
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

/// Removes the staging file (its random name is this save's own). A failure is said, never
/// dropped.
fn remove_staging(path: &Path) -> Option<String> {
    std::fs::remove_file(path).err().map(|e| {
        format!(
            "the staging file {} could not be removed ({e}); it is safe to delete",
            path.display()
        )
    })
}

fn exists(dest: &Path) -> SaveError {
    SaveError::Exists {
        next: next_free(dest),
    }
}

/// Saves new bytes at `dest` without ever putting an incomplete file, or anything over an
/// existing one, under that name: `fill` writes a staging file `.<name>.sopen-<random>.part` in
/// the same folder (created exclusively; owner-only while it fills), which is flushed to disk,
/// given the mode any new file there gets (read back from the staging file while it is still
/// empty) and published by a hard link to `dest` — which fails if anything is there — then
/// unlinked. When linking fails, publishing is refused and the complete staging file is kept
/// and named in the error. An interrupted save leaves only the hidden staging file; cleanup
/// only ever removes that.
pub fn save_new(
    dest: &Path,
    fill: impl FnOnce(&mut File) -> Result<u64>,
) -> std::result::Result<SavedTo, SaveError> {
    save_new_with(dest, fill, &mut |from, to| std::fs::hard_link(from, to))
}

/// [`save_new`] with `link` standing in for `hard_link` (tests: a folder without hard links,
/// a name taken at the last moment).
fn save_new_with(
    dest: &Path,
    fill: impl FnOnce(&mut File) -> Result<u64>,
    link: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>,
) -> std::result::Result<SavedTo, SaveError> {
    if std::fs::symlink_metadata(dest).is_ok() {
        return Err(exists(dest));
    }
    let failed = |e: anyhow::Error| SaveError::Failed(e);
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| failed(anyhow!("{} has no file name", dest.display())))?;
    let dir = match dest.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let (staging, mut file) = (|| -> Result<(PathBuf, File)> {
        for _ in 0..32 {
            let random = crate::remote::random_hex();
            let path = dir.join(format!(".{name}.sopen-{random}.part"));
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok((path, file)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("cannot create a file in {}", dir.display()))
                }
            }
        }
        bail!("no free staging name in {}", dir.display())
    })()
    .map_err(failed)?;
    // The mode a new file gets here (umask, default ACLs), read while the file is empty; then
    // owner-only until the bytes are complete.
    let mode = file
        .metadata()
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o644);
    let give_up = |e: anyhow::Error, staging: &Path| -> SaveError {
        let e = e.context(format!("{} was not saved", dest.display()));
        SaveError::Failed(match remove_staging(staging) {
            Some(left) => e.context(left),
            None => e,
        })
    };
    let filled = (|| -> Result<u64> {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("cannot make {} owner-only", staging.display()))?;
        let n = fill(&mut file)?;
        file.sync_all()
            .with_context(|| format!("cannot write {}", staging.display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("cannot set the mode of {}", staging.display()))?;
        Ok(n)
    })();
    let bytes = match filled {
        Ok(n) => n,
        Err(e) => return Err(give_up(e, &staging)),
    };
    let mut notes = Vec::new();
    match link(&staging, dest) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if let Some(left) = remove_staging(&staging) {
                eprintln!("sopen: {left}");
            }
            return Err(exists(dest));
        }
        Err(link_err) => {
            return Err(SaveError::Failed(anyhow!(
                "cannot publish {} without replacing an existing file ({link_err}); \
                 the complete staging file is kept at {}",
                dest.display(),
                staging.display()
            )));
        }
    }
    notes.extend(remove_staging(&staging));
    Ok(SavedTo { bytes, notes })
}

/// Downloads `abs` into a new file at `dest` ([`save_new`]). Best-effort change check: a file
/// whose size or mtime differs after the transfer, or that gave a different number of bytes,
/// is not saved.
pub fn download_to(
    fs: &mut dyn RemoteFs,
    abs: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> std::result::Result<SavedTo, SaveError> {
    let before = fs.stat(abs).map_err(SaveError::Failed)?;
    if before.is_dir {
        return Err(SaveError::Failed(anyhow!("{abs} is a directory")));
    }
    save_new(dest, |file| {
        let mut out = std::io::BufWriter::with_capacity(1 << 20, &mut *file);
        let n = fs.read_to(abs, &mut out, &mut |done| progress(done, before.size))?;
        out.flush()?;
        drop(out);
        let after = fs.stat_opt(abs)?;
        let same = after.is_some_and(|a| a.size == before.size && a.mtime == before.mtime);
        if !same || (before.missing != Some("size") && n != before.size) {
            bail!("{abs} changed while it was downloaded; try again when it is quiet");
        }
        Ok(n)
    })
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
pub fn copy_to(
    src: &Path,
    dest: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> std::result::Result<SavedTo, SaveError> {
    let failed = |e: anyhow::Error| SaveError::Failed(e);
    let mut input = File::open(src)
        .with_context(|| format!("cannot open {}", src.display()))
        .map_err(failed)?;
    let before = handle_stamp(&input)
        .with_context(|| format!("cannot stat {}", src.display()))
        .map_err(failed)?;
    save_new(dest, |file| {
        let mut buf = vec![0u8; 1 << 20];
        let mut done = 0u64;
        loop {
            let n = input
                .read(&mut buf)
                .with_context(|| format!("cannot read {}", src.display()))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, before.0);
        }
        let after =
            handle_stamp(&input).with_context(|| format!("cannot stat {}", src.display()))?;
        if after != before || done != before.0 {
            bail!(
                "{} changed while it was copied ({} bytes copied of {}); try again when it is quiet",
                src.display(),
                done,
                before.0
            );
        }
        Ok(done)
    })
}

// ---------------------------------------------------------------------------------------------
// Carrying the answers out.

/// What the hex window opens: the file made local, where earlier copies' unsaved edits went,
/// and how long a fetch took (`None`: nothing was fetched).
pub struct Opened {
    pub file: HexFile,
    pub notices: Vec<String>,
    pub fetch_ms: Option<u64>,
}

/// How the dialog's answers are carried out — by the caller, who holds the connection. Each
/// runs on a worker thread; `progress` gets `(bytes done, total)`. A failure leaves the source
/// as it was: the owner may try again, or choose the other answer.
pub trait Source: Send {
    /// Hex: the file made local (fetched into the cache, or the local file itself).
    fn open_hex(&mut self, progress: &mut dyn FnMut(u64, u64)) -> Result<Opened>;
    /// Save: the file's bytes in a new file at `dest` ([`save_new`]).
    fn save_to(
        &mut self,
        dest: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> std::result::Result<SavedTo, SaveError>;
}

/// The [`Source`] main uses: the file on its host (or the local disk). Hex fetches into the
/// cache as every fetch does (claimed, recorded) and hands the connection to the hex window;
/// a local file is read in place. Save downloads (or copies a local or cached file).
pub struct FileSource {
    /// Taken when the hex window opens (it holds the connection from then on).
    fs: Option<Box<dyn RemoteFs + Send>>,
    reconnect: Option<super::Reconnect>,
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
        Self {
            fs: Some(fs),
            reconnect,
            label,
            abs: abs.to_string(),
            cache: cache.to_path_buf(),
            fetched,
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
        let fetch_ms = self.make_local(progress)?;
        let (base_path, at, notices) = match &self.fetched {
            Some(f) => (f.claim.local.clone(), f.at, f.notices.clone()),
            None => {
                let fs = self.fs.as_mut().expect("checked above");
                (PathBuf::from(&self.abs), fs.stat(&self.abs)?, Vec::new())
            }
        };
        let base = hex::Base::open(&base_path)?;
        // Nothing below fails: hand everything over.
        let fs = self.fs.take().expect("checked above");
        let claim = self.fetched.take().map(|f| f.claim);
        let origin = Origin::new(
            fs,
            self.reconnect.take(),
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
    ) -> std::result::Result<SavedTo, SaveError> {
        if let Some(fetched) = &self.fetched {
            return copy_to(&fetched.claim.local, dest, progress);
        }
        match self.fs.as_mut() {
            Some(_) if self.label.is_none() => copy_to(Path::new(&self.abs), dest, progress),
            Some(fs) => download_to(fs.as_mut(), &self.abs, dest, progress),
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
    let cancelled = if info.local {
        "sopen: cancelled"
    } else {
        "sopen: cancelled; nothing was downloaded"
    };
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
        }
        Some(Choice::Save) => {
            meter.start(&format!("saving {}", info.name));
            let saved =
                save_without_window(&info, &mut source, hooks, &mut |d, t| meter.bytes(d, t));
            meter.finish();
            let (dest, saved) = saved?;
            println!("saved {}", dest.display());
            for note in saved.notes {
                eprintln!("sopen: {note}");
            }
            out.saved_to = Some(dest);
        }
        // `--hex`, and the windowless autosave: fetched with the progress on the terminal.
        Some(Choice::Hex) if hex_flag || hooks.autosave => {
            meter.start(&format!("fetching {}", info.name));
            let opened = source.open_hex(&mut |d, t| meter.bytes(d, t));
            meter.finish();
            let opened = opened?;
            out.fetch_ms = opened.fetch_ms;
            out.hex(hex::open(opened.file, hooks, opened.notices)?);
        }
        // The dialog; with SOPEN_CHOICE=hex already answered (the fetch starts at once, in the
        // window, which then becomes the hex window).
        Some(Choice::Hex) | None => {
            let ended = dialog(info, Box::new(source), chooser, hooks, run)?;
            out.fetch_ms = ended.fetch_ms;
            match ended.hex {
                Some(outcome) => out.hex(outcome),
                None if ended.saved_to.is_none() => eprintln!("{cancelled}"),
                None => {}
            }
            out.saved_to = ended.saved_to;
        }
    }
    Ok(out)
}

/// "Save to…" answered without a window (`SOPEN_CHOICE=save`): saves to `SOPEN_SAVE_TO`, else
/// the download folder (a folder gets the file's name inside it), never over anything — a taken
/// path is an error naming the next free name. Returns where it saved.
pub fn save_without_window(
    info: &Info,
    source: &mut dyn Source,
    hooks: &Hooks,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(PathBuf, SavedTo)> {
    let dest = first_path(&info.name, hooks);
    let dest = if dest.is_dir() {
        dest.join(&info.name)
    } else {
        dest
    };
    match source.save_to(&dest, progress) {
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
            Ok(Moved {
                result: MovedResult::Saved(dest, Ok(saved)),
                ..
            }) => {
                println!("saved {}", dest.display());
                for note in saved.notes {
                    eprintln!("sopen: {note}");
                }
                ended.lock().unwrap().saved_to = Some(dest);
            }
            Ok(Moved {
                result: MovedResult::Saved(dest, Err(e)),
                ..
            }) => eprintln!("sopen: {} not saved: {e}", dest.display()),
            Ok(Moved {
                result: MovedResult::Hex(result),
                ..
            }) => match *result {
                Ok(_) => eprintln!(
                    "sopen: fetched; the window was closed, so the hex editor did not open"
                ),
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
}

/// A fetch or a download, back from its worker with the source.
struct Moved {
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
    /// Fetching for the hex window (`dest` = `None`) or saving to `dest`.
    Moving {
        dest: Option<PathBuf>,
        done: Arc<AtomicU64>,
        total: Arc<AtomicU64>,
        rx: Receiver<Moved>,
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
    leftover: Arc<Mutex<Option<Receiver<Moved>>>>,
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
        }
    }

    fn asking(&self) -> bool {
        matches!(self.stage, Stage::Ask)
    }

    /// Hex: fetch (or open the local file) on a worker, then become the hex window.
    fn start_hex(&mut self, ctx: &egui::Context) {
        let Some(mut source) = self.source.take() else {
            return;
        };
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
            let _ = tx.send(Moved {
                source,
                result: MovedResult::Hex(Box::new(result)),
            });
            ctx2.request_repaint();
        });
        self.stage = Stage::Moving {
            dest: None,
            done,
            total,
            rx,
        };
    }

    /// Save to the path in the field — exactly as typed (only an empty field is refused) — on a
    /// worker. A folder gets the file's name inside it.
    fn start_save(&mut self, ctx: &egui::Context, from_field: bool) {
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
        self.notes.clear();
        self.field_save = from_field;
        let (done, total) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let (tx, rx) = mpsc::channel();
        let (d, t, ctx2, to) = (done.clone(), total.clone(), ctx.clone(), dest.clone());
        std::thread::spawn(move || {
            let result = source.save_to(&to, &mut |now, all| {
                d.store(now, Ordering::Relaxed);
                t.store(all, Ordering::Relaxed);
                ctx2.request_repaint();
            });
            let _ = tx.send(Moved {
                source,
                result: MovedResult::Saved(to, result),
            });
            ctx2.request_repaint();
        });
        self.stage = Stage::Moving {
            dest: Some(dest),
            done,
            total,
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
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
    fn landed(&mut self, ctx: &egui::Context, moved: Moved) {
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
                if self.close_when_done {
                    eprintln!(
                        "sopen: the window was closed during the fetch; the hex editor did not \
                         open"
                    );
                    self.close(ctx);
                    return;
                }
                self.ended.lock().unwrap().fetch_ms = opened.fetch_ms;
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
                .on_hover_text(if local {
                    "Enter — opens it in sopen's hex editor"
                } else {
                    "Enter — fetches it into sopen's cache and opens the hex editor"
                });
            if hex.clicked() {
                self.start_hex(&ctx);
            }
            let save = ui
                .add_enabled(asking, egui::Button::new("Save to…"))
                .on_hover_text(if local {
                    "copies it to the path below; never over an existing file"
                } else {
                    "downloads it to the path below; never over an existing file"
                });
            if save.clicked() {
                self.start_save(&ctx, false);
            }
            let cancel = ui
                .add_enabled(asking, egui::Button::new("Cancel"))
                .on_hover_text("Esc");
            if cancel.clicked() {
                self.close(&ctx);
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let room = (ui.available_width() - 90.0).max(120.0);
            let field = ui.add_enabled(
                asking,
                egui::TextEdit::singleline(&mut self.path)
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
                    (Some(dest), _) => format!("saving to {}…", tilde(dest)),
                    (None, true) => "opening it in the hex editor…".to_string(),
                    (None, false) => "fetching for the hex editor…".to_string(),
                });
                ui.add(egui::ProgressBar::new(frac).text(format!(
                    "{:.0}% · {} / {}",
                    frac * 100.0,
                    crate::human(d),
                    crate::human(t)
                )));
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
        // Keys, only while asking: Enter in the path field saves; Enter with nothing focused
        // opens the hex editor (a focused button takes Enter as its own click); Esc cancels.
        // During a transfer neither does anything (closing waits for it).
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

    /// The window as tall as its content (unless `SOPEN_WINDOW_SIZE` fixed it).
    fn fit(&mut self, ctx: &egui::Context, bottom: f32) {
        if self.hooks.window_size.is_some() {
            return;
        }
        let height = (bottom + 12.0).ceil().max(MIN_SIZE[1]);
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
        let bottom = egui::CentralPanel::default()
            .show(ui, |ui| self.dialog(ui))
            .inner;
        self.fit(&ctx, bottom);
        self.painted = true;
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
            Stage::Moving { .. } => {
                // Never close mid-transfer: a download into the chosen path is the owner's
                // file; a fetch is the cache's. Both finish, then the window closes.
                self.close_when_done = true;
                false
            }
            Stage::Ask | Stage::Choosing(_) | Stage::Saved { .. } | Stage::Failed(_) => true,
        }
    }
}

#[cfg(test)]
mod tests;
