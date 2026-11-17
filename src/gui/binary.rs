//! Binary files (spec §6a): a file sopen has no window for (a zip, any binary) opens a small
//! dialog first — from the stat alone, nothing downloaded yet — that offers the hex window (the
//! default, Enter) or "Save to…" a chosen path; Esc or closing it downloads nothing. Choosing hex
//! fetches into the cache with the progress in the dialog, then the same window becomes the hex
//! window. Choosing save downloads straight to the chosen path, created exclusively: an existing
//! file is never replaced — the dialog says so and offers the next free `<stem>-N<ext>`.
//!
//! How the file is made local and saved is the caller's ([`Source`]); the native save dialog is
//! behind [`Chooser`]. Both are traits so the tests drive the real window with stand-ins.

use super::hex::{self, HexFile, HexWindow};
use super::{Hooks, Outcome, Window};
use crate::remote::RemoteFs;
use anyhow::{anyhow, bail, Context as _, Result};
use eframe::egui::{self, Color32, Key};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The dialog's size.
pub const WINDOW: [f32; 2] = [560.0, 320.0];
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
        Some(dirs) => dirs
            .download_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dirs.home_dir().to_path_buf()),
        None => PathBuf::from("."),
    }
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

/// Where "Save to…" saves first: `SOPEN_SAVE_TO`, else `<download folder>/<name>`, or the next
/// free name there when that is taken (said in the dialog).
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

/// Creates `dest` exclusively — never over, or through, anything already there (a file, a
/// link) — with the mode any new file there gets, and has `fill` write it. When `fill` fails
/// the file this call created is removed (if the path still names it).
pub fn save_new(
    dest: &Path,
    fill: impl FnOnce(&mut File) -> Result<u64>,
) -> std::result::Result<u64, SaveError> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(dest) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(SaveError::Exists {
                next: next_free(dest),
            })
        }
        Err(e) => {
            return Err(SaveError::Failed(
                anyhow::Error::new(e).context(format!("cannot create {}", dest.display())),
            ))
        }
    };
    let written = fill(&mut file).and_then(|n| {
        file.sync_all()
            .with_context(|| format!("cannot write {}", dest.display()))?;
        Ok(n)
    });
    match written {
        Ok(n) => Ok(n),
        Err(e) => {
            use std::os::unix::fs::MetadataExt;
            let ours = file.metadata().ok().map(|m| (m.dev(), m.ino()));
            let there = std::fs::symlink_metadata(dest)
                .ok()
                .map(|m| (m.dev(), m.ino()));
            drop(file);
            if ours.is_some() && ours == there {
                let _ = std::fs::remove_file(dest);
            }
            Err(SaveError::Failed(
                e.context(format!("{} was not saved", dest.display())),
            ))
        }
    }
}

/// Downloads `abs` into a new file at `dest` ([`save_new`]); a file that changes during the
/// transfer is not saved.
pub fn download_to(
    fs: &mut dyn RemoteFs,
    abs: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> std::result::Result<u64, SaveError> {
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

/// Copies the local file `src` into a new file at `dest` ([`save_new`]).
pub fn copy_to(
    src: &Path,
    dest: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> std::result::Result<u64, SaveError> {
    let mut input = File::open(src)
        .with_context(|| format!("cannot open {}", src.display()))
        .map_err(SaveError::Failed)?;
    let total = input
        .metadata()
        .map(|m| m.len())
        .with_context(|| format!("cannot stat {}", src.display()))
        .map_err(SaveError::Failed)?;
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
            progress(done, total);
        }
        Ok(done)
    })
}

/// What the hex window opens: the file made local, where earlier copies' unsaved edits went,
/// and how long a fetch took (`None`: nothing was fetched).
pub struct Opened {
    pub file: HexFile,
    pub notices: Vec<String>,
    pub fetch_ms: Option<u64>,
}

/// How the dialog's answers are carried out — by the caller, who holds the connection. Each
/// runs on a worker thread; `progress` gets `(bytes done, total)`.
pub trait Source: Send {
    /// Hex: the file made local (fetched into the cache, or the local file itself).
    fn open_hex(&mut self, progress: &mut dyn FnMut(u64, u64)) -> Result<Opened>;
    /// Save: the file's bytes in a new file at `dest`, created exclusively ([`save_new`]).
    fn save_to(
        &mut self,
        dest: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> std::result::Result<u64, SaveError>;
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
    Saved(PathBuf, std::result::Result<u64, SaveError>),
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
    /// Saved: the line stays a moment, then the window closes.
    Saved {
        line: String,
        at: Instant,
    },
    Hex(Box<HexWindow>),
}

pub(super) struct BinaryWindow {
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
    pub(super) fn new(
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
            prepicked: hooks.choice == Some(super::Choice::Hex),
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

    /// Save to the path in the field, on a worker. A folder gets the file's name inside it.
    fn start_save(&mut self, ctx: &egui::Context) {
        let text = self.path.trim().to_string();
        if text.is_empty() {
            self.notes = vec![("type a path to save to".to_string(), true)];
            return;
        }
        let mut dest = untilde(&text);
        if dest.is_dir() {
            dest = dest.join(&self.info.name);
        }
        let Some(mut source) = self.source.take() else {
            return;
        };
        self.notes.clear();
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
        let suggested = untilde(self.path.trim());
        let suggested = if suggested.is_dir() {
            suggested.join(&self.info.name)
        } else {
            suggested
        };
        self.notes.clear();
        self.stage = Stage::Choosing(self.chooser.choose(&suggested));
    }

    fn cancel(&mut self, ctx: &egui::Context) {
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
                    self.start_save(ctx);
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
                    self.stage = Stage::Ask;
                    self.notes = vec![("the transfer stopped without a result".to_string(), true)];
                }
                Err(mpsc::TryRecvError::Empty) => {}
            },
            Stage::Saved { at, .. } => {
                if at.elapsed() >= SAVED_LINGER {
                    self.close_ok = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    ctx.request_repaint_after(SAVED_LINGER - at.elapsed());
                }
            }
            Stage::Ask | Stage::Hex(_) => {}
        }
    }

    /// A fetch or a download came back.
    fn landed(&mut self, ctx: &egui::Context, moved: Moved) {
        self.source = Some(moved.source);
        self.stage = Stage::Ask;
        match moved.result {
            MovedResult::Saved(dest, Ok(_)) => {
                println!("saved {}", dest.display());
                self.ended.lock().unwrap().saved_to = Some(dest.clone());
                self.stage = Stage::Saved {
                    line: format!("saved {}", tilde(&dest)),
                    at: Instant::now(),
                };
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
                self.close_after_failure(ctx);
            }
            MovedResult::Saved(_, Err(SaveError::Failed(e))) => {
                eprintln!("sopen: {e:#}");
                self.notes = vec![(format!("{e:#}"), true)];
                self.close_after_failure(ctx);
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
                    self.close_ok = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
                            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                                hex::WINDOW.into(),
                            ));
                        }
                        self.stage = Stage::Hex(Box::new(window));
                    }
                    Err(e) => self.notes = vec![(format!("{e:#}"), true)],
                }
            }
            Err(e) => {
                eprintln!("sopen: {e:#}");
                self.notes = vec![(format!("{e:#}"), true)];
                self.close_after_failure(ctx);
            }
        }
    }

    /// Closing was asked for during a transfer that did not land: close now (the note is on
    /// the terminal too).
    fn close_after_failure(&mut self, ctx: &egui::Context) {
        if self.close_when_done {
            self.close_ok = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn dialog(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let asking = self.asking();
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
                .on_hover_text("Enter — fetches it into sopen's cache and opens the hex editor");
            if hex.clicked() {
                self.start_hex(&ctx);
            }
            let save = ui
                .add_enabled(asking, egui::Button::new("Save to…"))
                .on_hover_text("downloads it to the path below; never over an existing file");
            if save.clicked() {
                self.start_save(&ctx);
            }
            let cancel = ui
                .add_enabled(asking, egui::Button::new("Cancel"))
                .on_hover_text("Esc — nothing is downloaded");
            if cancel.clicked() {
                self.cancel(&ctx);
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
        ui.label(
            egui::RichText::new(
                "Nothing is downloaded until you choose. Enter opens the hex editor; Esc cancels.",
            )
            .weak(),
        );
        ui.add_space(8.0);
        match &self.stage {
            Stage::Moving {
                dest, done, total, ..
            } => {
                let (d, t) = (done.load(Ordering::Relaxed), total.load(Ordering::Relaxed));
                let frac = if t > 0 { d as f32 / t as f32 } else { 0.0 };
                ui.label(match dest {
                    Some(dest) => format!("saving to {}…", tilde(dest)),
                    None => "fetching for the hex editor…".to_string(),
                });
                ui.add(egui::ProgressBar::new(frac).text(format!(
                    "{:.0}% · {} / {}",
                    frac * 100.0,
                    crate::human(d),
                    crate::human(t)
                )));
            }
            Stage::Saved { line, .. } => {
                ui.label(line.as_str());
            }
            Stage::Choosing(_) => {
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
        super::disclosures(ui, &lines);
        if self.close_when_done {
            super::disclosures(ui, &[(CLOSES_AFTER.to_string(), plain)]);
        }
        // Keys, only while asking: Enter in the path field saves; Enter with nothing focused
        // opens the hex editor (a focused button takes Enter as its own click); Esc cancels.
        if self.asking() {
            if path_enter {
                self.start_save(&ctx);
            } else if ctx.memory(|m| m.focused()).is_none()
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter))
            {
                self.start_hex(&ctx);
            }
            if self.asking() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
            {
                self.cancel(&ctx);
            }
        }
    }
}

impl Window for BinaryWindow {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.prepicked) && self.asking() {
            self.start_hex(&ctx);
        }
        self.poll(&ctx);
        if let Stage::Hex(window) = &mut self.stage {
            return window.ui(ui);
        }
        egui::CentralPanel::default().show(ui, |ui| self.dialog(ui));
        self.painted = true;
    }

    fn ready(&self) -> bool {
        match &self.stage {
            Stage::Ask | Stage::Saved { .. } => self.painted,
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
            Stage::Ask | Stage::Choosing(_) | Stage::Saved { .. } => true,
        }
    }
}

/// Opens the dialog (one window: the dialog, the transfer, then the hex window if chosen) and
/// returns how it ended once it closed.
pub fn open(
    info: Info,
    source: Box<dyn Source>,
    chooser: Box<dyn Chooser>,
    hooks: &Hooks,
) -> Result<Ended> {
    let title = info.title.clone();
    let window = BinaryWindow::new(info, source, chooser, hooks);
    let (ended, hex_outcome, hex_leftover, leftover) = (
        window.ended.clone(),
        window.hex_outcome.clone(),
        window.hex_leftover.clone(),
        window.leftover.clone(),
    );
    let run = super::run_window(&title, WINDOW, window, hooks);
    hex::finish(&hex_leftover, &hex_outcome);
    if let Some(rx) = leftover.lock().unwrap().take() {
        eprintln!("sopen: the window closed during a transfer; waiting for it to finish");
        match rx.recv() {
            Ok(Moved {
                result: MovedResult::Saved(dest, Ok(_)),
                ..
            }) => {
                println!("saved {}", dest.display());
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
    run?;
    let mut ended = ended.lock().unwrap().clone();
    if ended.hex.is_some() {
        ended.hex = Some(hex_outcome.lock().unwrap().clone());
    }
    Ok(ended)
}

/// "Save to…" answered without a window (`SOPEN_CHOICE=save`): saves to `SOPEN_SAVE_TO`, else
/// the download folder (a folder gets the file's name inside it), never over anything — a taken
/// path is an error naming the next free name. Returns where it saved.
pub fn save_without_window(
    info: &Info,
    source: &mut dyn Source,
    hooks: &Hooks,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf> {
    let dest = first_path(&info.name, hooks);
    let dest = if dest.is_dir() {
        dest.join(&info.name)
    } else {
        dest
    };
    match source.save_to(&dest, progress) {
        Ok(_) => Ok(dest),
        Err(SaveError::Exists { next }) => bail!(
            "{} exists; not replaced (the next free name is {})",
            dest.display(),
            next.display()
        ),
        Err(SaveError::Failed(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests;
