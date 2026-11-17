//! `sopen` CLI. Routing per spec §3; every error is one line on stderr and a non-zero exit
//! (1 = error, 3 = a write refused because the remote changed).

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use sopen::cache::{self, Dirs};
use sopen::cut::{self, CutRequest};
use sopen::ffmpeg::{self, Rect};
use sopen::gui;
use sopen::kind::{self, Kind};
use sopen::remote::{self, LocalFs, RemoteFs, SftpRemote, Stat, WriteOutcome};
use sopen::target::{self, RemoteSpec, Target};
use sopen::usage::Record;
use sopen::{app, zed};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

const EXIT_ERROR: u8 = 1;
const EXIT_CONFLICT: u8 = 3;

#[derive(Parser)]
#[command(
    name = "sopen",
    version = sopen::version_text(),
    about = "Open a file or folder on another machine as if it were local",
    long_about = "Open a file or folder on another machine as if it were local.\n\n\
        TARGET uses scp syntax: [user@]host:path (relative paths are under the remote home) or a \
        local path. Folders and text open in Zed; other files are fetched to the local cache.",
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// [user@]host:path, or a local path
    #[arg(required_unless_present = "selftest_audio")]
    target: Option<String>,
    /// Open a copy in this local app; Enter in the terminal uploads the changes
    #[arg(long, value_name = "APP")]
    app: Option<String>,
    /// Receipt check: play FILE on the real audio device at gain 0 for ~2 s and print the
    /// callback / underrun counts (AGENTS.md "Receipts").
    #[arg(long, hide = true, value_name = "FILE", conflicts_with_all = ["target", "app"])]
    selftest_audio: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Cut IN..OUT of an audio/video file into a new file next to it (never overwrites)
    Cut {
        /// [user@]host:path, or a local path
        target: String,
        /// Start: SS, MM:SS or HH:MM:SS(.ms)
        #[arg(value_name = "IN")]
        start: String,
        /// End: SS, MM:SS or HH:MM:SS(.ms)
        #[arg(value_name = "OUT")]
        end: String,
        /// Crop the video to this rectangle in source pixels
        #[arg(long, value_name = "X,Y,W,H")]
        crop: Option<String>,
    },
}

/// A write that was refused because the remote changed since the fetch (exit 3).
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(file) = &cli.selftest_audio {
        return match gui::playback::selftest(file) {
            Ok(line) => {
                println!("{line}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("sopen: {}", format!("{e:#}").replace('\n', " "));
                ExitCode::from(EXIT_ERROR)
            }
        };
    }
    let mut rec = Record::default();
    let dirs = cache::dirs();
    let result = dirs
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e:#}"))
        .and_then(|dirs| match cli.cmd {
            Some(Cmd::Cut {
                target,
                start,
                end,
                crop,
            }) => run_cut(&target, &start, &end, crop.as_deref(), dirs, &mut rec),
            None => run_open(
                cli.target.as_deref().unwrap_or_default(),
                cli.app.as_deref(),
                dirs,
                &mut rec,
            ),
        });
    rec.ok = result.is_ok();
    if let Ok(dirs) = &dirs {
        if let Err(e) = sopen::usage::append(&dirs.state, &rec) {
            sopen::debug!("usage log not written: {e:#}");
        }
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let line = format!("{e:#}").replace('\n', " ");
            eprintln!("sopen: {line}");
            if e.chain().any(|c| c.is::<Refused>()) {
                ExitCode::from(EXIT_CONFLICT)
            } else {
                ExitCode::from(EXIT_ERROR)
            }
        }
    }
}

/// `\r`-updated progress on stderr, only when stderr is a terminal.
struct Progress {
    label: String,
    tty: bool,
    shown: bool,
    last: Option<Instant>,
}

impl Progress {
    fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            tty: std::io::stderr().is_terminal(),
            shown: false,
            last: None,
        }
    }

    fn show(&mut self, text: String, force: bool) {
        if !self.tty {
            return;
        }
        let due = self.last.is_none_or(|t| t.elapsed().as_millis() >= 100);
        if force || due {
            eprint!("\r\x1b[2K{} {text}", self.label);
            let _ = std::io::stderr().flush();
            self.shown = true;
            self.last = Some(Instant::now());
        }
    }

    fn bytes(&mut self, done: u64, total: u64) {
        let pct = (done * 100).checked_div(total).unwrap_or(100);
        self.show(
            format!("{pct:>3}% {} / {}", human(done), human(total)),
            done == total,
        );
    }

    fn fraction(&mut self, f: f64) {
        self.show(format!("{:>3}%", (f * 100.0).round() as u64), f >= 1.0);
    }

    fn finish(&mut self) {
        if self.shown {
            eprintln!();
            self.shown = false;
        }
    }
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Where a target lives: the remote connection or the local disk, plus labels for output.
enum Place {
    Remote(RemoteSpec, Box<SftpRemote>),
    Local(LocalFs),
}

impl Place {
    fn connect(target: &Target, dirs: &Dirs, rec: &mut Record) -> Result<(Self, String)> {
        match target {
            Target::Remote(spec) => {
                rec.host = Some(spec.label());
                let fs = SftpRemote::connect(spec, &dirs.state)?;
                Ok((Place::Remote(spec.clone(), Box::new(fs)), spec.path.clone()))
            }
            Target::Local(path) => {
                let path = path
                    .to_str()
                    .context("local path is not valid UTF-8")?
                    .to_string();
                Ok((Place::Local(LocalFs::new()), path))
            }
        }
    }

    fn fs(&mut self) -> &mut dyn RemoteFs {
        match self {
            Place::Remote(_, fs) => fs.as_mut(),
            Place::Local(fs) => fs,
        }
    }

    /// How a path on this place is printed: `host:/abs` or the local path.
    fn display(&self, abs: &str) -> String {
        match self {
            Place::Remote(spec, _) => spec.display(abs),
            Place::Local(_) => abs.to_string(),
        }
    }

    fn host_label(&self) -> String {
        match self {
            Place::Remote(spec, _) => spec.label(),
            Place::Local(_) => cache::LOCAL_HOST.to_string(),
        }
    }

    /// Hands the connection to a window: the file layer, how to reopen it if the session dies
    /// while the window is open, and the host label (`None` for local files).
    fn into_origin(self, abs: &str, at: Stat, dirs: &Dirs) -> gui::Origin {
        match self {
            Place::Remote(spec, fs) => {
                let label = spec.label();
                let state = dirs.state.clone();
                let reconnect: gui::Reconnect = Box::new(move || {
                    let fs = SftpRemote::connect(&spec, &state)?;
                    Ok(Box::new(fs) as Box<dyn RemoteFs + Send>)
                });
                gui::Origin::new(fs, Some(reconnect), Some(label), abs.to_string(), at)
            }
            Place::Local(fs) => gui::Origin::new(Box::new(fs), None, None, abs.to_string(), at),
        }
    }
}

/// A remote file fetched into this session's cached copy.
struct Fetched {
    /// The copy, locked for as long as this lives: another session gets a copy of its own.
    claim: cache::Claim,
    at: Stat,
    /// Where an earlier copy's unsaved edits were moved (already printed).
    kept: Option<PathBuf>,
}

/// Claims the cached copy of `abs` for this session, moves an earlier copy's unsaved edits
/// aside, and fetches with a progress line; records size and fetch time. Every fetch into the
/// cache goes through here, so no session replaces or moves a copy another one is editing.
fn fetch_to_cache(place: &mut Place, abs: &str, dirs: &Dirs, rec: &mut Record) -> Result<Fetched> {
    let cached = cache::cache_path(&dirs.cache, &place.host_label(), abs);
    let claim = cache::claim(&cached)?;
    if claim.local != cached {
        eprintln!(
            "sopen: another sopen session has {} open; this one works on its own copy {}",
            cached.display(),
            claim.local.display()
        );
    }
    let kept = cache::set_aside_unsaved(&claim.local)?;
    if let Some(kept) = &kept {
        eprintln!(
            "sopen: the previous copy had unsaved edits; kept them at {}",
            kept.display()
        );
    }
    let (_, name) = remote::split_parent(abs);
    let mut progress = Progress::new(format!("fetching {name}"));
    let started = Instant::now();
    let fetched = place.fs().fetch(abs, &claim.local, &mut |done, total| {
        progress.bytes(done, total)
    });
    progress.finish();
    let at = fetched?;
    rec.fetch_ms = started.elapsed().as_millis() as u64;
    rec.bytes = at.size;
    Ok(Fetched { claim, at, kept })
}

fn classify(fs: &mut dyn RemoteFs, abs: &str, stat: &Stat) -> Result<Kind> {
    if stat.is_dir {
        return Ok(Kind::Directory);
    }
    let (_, name) = remote::split_parent(abs);
    if let Some(kind) = kind::by_extension(name) {
        return Ok(kind);
    }
    let head = fs.read_head(abs, kind::SNIFF_BYTES)?;
    Ok(kind::classify(name, || head, stat.size))
}

fn run_open(arg: &str, app_name: Option<&str>, dirs: &Dirs, rec: &mut Record) -> Result<()> {
    let target = target::parse(arg).map_err(anyhow::Error::msg)?;
    let (mut place, path) = Place::connect(&target, dirs, rec)?;
    let abs = place.fs().realpath(&path)?;
    let stat = place.fs().stat(&abs)?;
    let kind = classify(place.fs(), &abs, &stat)?;
    rec.kind = Some(kind);
    rec.ext = kind::extension(remote::split_parent(&abs).1);
    rec.bytes = stat.size;
    rec.action = Some("open");
    sopen::debug!("{} is {}", place.display(&abs), kind.as_str());

    if let Some(app_name) = app_name {
        if stat.is_dir {
            bail!("--app opens files; {} is a directory", place.display(&abs));
        }
        return open_in_app(&mut place, &abs, app_name, dirs, rec);
    }
    match kind {
        Kind::Directory | Kind::Text => open_in_editor(&mut place, &abs, kind, dirs, rec),
        Kind::Image | Kind::Video | Kind::Audio => open_in_window(place, &abs, kind, dirs, rec),
        Kind::Other => {
            match place {
                Place::Local(_) => println!("{abs}"),
                Place::Remote(..) => {
                    let fetched = fetch_to_cache(&mut place, &abs, dirs, rec)?;
                    println!("{}", fetched.claim.local.display());
                }
            }
            Ok(())
        }
    }
}

/// Images and media open in sopen's own window (spec §6, §7). Images are always fetched (a
/// local one is copied into the cache: Save needs the bytes as opened); local media is played
/// in place. Without a display (Linux over ssh) the local copy's path is printed instead.
fn open_in_window(
    mut place: Place,
    abs: &str,
    kind: Kind,
    dirs: &Dirs,
    rec: &mut Record,
) -> Result<()> {
    let hooks = gui::Hooks::from_env()?;
    let mut notice = None;
    // Held until the window closes (media plays from the copy).
    let mut _claim = None;
    let (local, at) = match (&place, kind) {
        (Place::Local(_), Kind::Video | Kind::Audio) => {
            let at = place.fs().stat(abs)?;
            (PathBuf::from(abs), at)
        }
        _ => {
            // A copy with unsaved edits (an earlier `--app` / editor session) is kept, not
            // replaced by this fetch; the window repeats where it went.
            let fetched = fetch_to_cache(&mut place, abs, dirs, rec)?;
            notice = fetched.kept.map(|kept| {
                format!(
                    "the previous copy had unsaved edits; kept them at {}",
                    kept.display()
                )
            });
            let local = fetched.claim.local.clone();
            _claim = Some(fetched.claim);
            (local, fetched.at)
        }
    };
    let kind = refine_kind(&local, abs, kind);
    rec.kind = Some(kind);
    if kind == Kind::Other {
        // A `.ts` ffprobe cannot read as media.
        println!("{}", local.display());
        return Ok(());
    }
    if !gui::display_available() && !hooks.autosave && !hooks.autoexport {
        println!(
            "no display for a window (DISPLAY / WAYLAND_DISPLAY unset); {}",
            local.display()
        );
        return Ok(());
    }
    let origin = place.into_origin(abs, at, dirs);
    let outcome = if kind == Kind::Image {
        let bytes =
            std::fs::read(&local).with_context(|| format!("cannot read {}", local.display()))?;
        let file = gui::image::ImageFile::new(origin, bytes, &dirs.cache);
        gui::image::open(file, &hooks, notice)?
    } else {
        gui::media::open(origin, &local, &dirs.cache, &hooks, notice)?
    };
    rec.action = Some(outcome.action());
    rec.conflict = outcome.conflict;
    match outcome.refused {
        Some(message) => Err(Refused(message).into()),
        None => Ok(()),
    }
}

/// Video by extension but no video stream (per ffprobe) is audio; without ffprobe, keep it
/// (a `.ts` ffprobe cannot read is not media).
fn refine_kind(local: &Path, name: &str, kind: Kind) -> Kind {
    if kind != Kind::Video {
        return kind;
    }
    let probe = match ffmpeg::resolve().and_then(|t| ffmpeg::probe(&t, local)) {
        Ok(probe) => Some((probe.video.is_some(), probe.has_audio)),
        Err(e) => {
            sopen::debug!("not probed: {e:#}");
            None
        }
    };
    kind::refine_after_fetch(name, kind, probe)
}

fn open_in_editor(
    place: &mut Place,
    abs: &str,
    kind: Kind,
    dirs: &Dirs,
    rec: &mut Record,
) -> Result<()> {
    if let Some(zed_cli) = zed::locate() {
        let arg = match place {
            Place::Remote(spec, _) => zed::remote_url(spec.user.as_deref(), &spec.host, abs),
            Place::Local(_) => abs.to_string(),
        };
        zed::open(&zed_cli, arg.as_ref())?;
        println!("opened {} in Zed", place.display(abs));
        return Ok(());
    }
    if kind == Kind::Directory {
        let (names, fixed) = zed::candidates(zed::Os::current());
        let mut looked: Vec<&str> = names.to_vec();
        looked.extend_from_slice(fixed);
        bail!(
            "folders open in Zed, and Zed was not found (looked for {}); install Zed or set SOPEN_ZED",
            looked.join(", ")
        );
    }
    let editor = zed::terminal_editor();
    match place {
        Place::Local(_) => {
            zed::run_terminal_editor(Path::new(abs))?;
            rec.action = Some("save");
            Ok(())
        }
        Place::Remote(..) => {
            let shown = place.display(abs);
            let Fetched { claim, at, .. } = fetch_to_cache(place, abs, dirs, rec)?;
            let local = &claim.local;
            eprintln!(
                "Zed not found; editing a copy with {}",
                editor.to_string_lossy()
            );
            if let Err(e) = zed::run_terminal_editor(local) {
                return Err(keep_edits(local, &at, e.context("not uploaded")));
            }
            if !edited(local, &at)? {
                println!("no changes; {shown} untouched");
                rec.action = Some("discard");
                return Ok(());
            }
            upload_back(place.fs(), &shown, local, abs, &at, rec)
        }
    }
}

fn open_in_app(
    place: &mut Place,
    abs: &str,
    app_name: &str,
    dirs: &Dirs,
    rec: &mut Record,
) -> Result<()> {
    if let Place::Local(_) = place {
        // A local file is edited in place; there is nothing to upload.
        app::open_with(app_name, Path::new(abs))?;
        println!("opened {abs} in {app_name}");
        return Ok(());
    }
    let shown = place.display(abs);
    // The claim is held until this returns: the whole app session.
    let Fetched { claim, at, .. } = fetch_to_cache(place, abs, dirs, rec)?;
    let local = &claim.local;
    app::open_with(app_name, local)?;
    eprintln!(
        "opened {shown} in {app_name} (local copy {})",
        local.display()
    );
    let stdin = std::io::stdin();
    let decision = match app::prompt(&mut stdin.lock(), &mut std::io::stderr()) {
        Ok(decision) => decision,
        Err(e) => return Err(keep_edits(local, &at, e.context("not uploaded"))),
    };
    if !edited(local, &at)? {
        println!("no changes; {shown} untouched");
        rec.action = Some("discard");
        return Ok(());
    }
    if decision == app::Decision::Discard {
        // Out of the way of the next fetch, but not deleted.
        let keep = cache::set_aside(local)?;
        println!(
            "discarded; {shown} untouched (your edited copy is at {})",
            keep.display()
        );
        rec.action = Some("discard");
        return Ok(());
    }
    upload_back(place.fs(), &shown, local, abs, &at, rec)
}

/// Does the fetched copy differ from what was fetched?
fn edited(local: &Path, at: &Stat) -> Result<bool> {
    Ok(Some(app::file_hash(local)?) != at.sha256)
}

/// A step after the fetch failed: an edited copy moves aside (no later fetch can overwrite it)
/// and the error says where it is.
fn keep_edits(local: &Path, at: &Stat, e: anyhow::Error) -> anyhow::Error {
    match edited(local, at) {
        Ok(true) => match cache::set_aside(local) {
            Ok(keep) => anyhow::anyhow!("{e:#} — your edited copy is at {}", keep.display()),
            Err(move_err) => e.context(format!("{move_err:#}")),
        },
        _ => e,
    }
}

/// Conflict-safe upload of an edited copy (spec §5). Whenever it does not land — a conflict
/// (exit 3) or any failure — the edit is moved aside to `<copy>.conflict-<stamp>` first, so the
/// next fetch cannot overwrite it, and the error prints that path.
fn upload_back(
    fs: &mut dyn RemoteFs,
    shown: &str,
    local: &Path,
    abs: &str,
    at: &Stat,
    rec: &mut Record,
) -> Result<()> {
    // The run is a save attempt from here on, whether it lands, conflicts or fails.
    rec.action = Some("save");
    match fs.write_atomic(local, abs, at.fingerprint()) {
        Ok(WriteOutcome::Written { notes, .. }) => {
            println!("saved {shown}");
            for note in notes {
                eprintln!("sopen: {note}");
            }
            Ok(())
        }
        Ok(WriteOutcome::Conflict { reason, notes, .. }) => {
            rec.conflict = true;
            let keep = cache::set_aside(local)?;
            let mut msg = format!(
                "{shown} changed since it was fetched ({reason}); not uploaded — your copy is at {}",
                keep.display()
            );
            for note in notes {
                msg.push_str(&format!("; {note}"));
            }
            Err(Refused(msg).into())
        }
        Err(e) => {
            let keep = cache::set_aside(local).with_context(|| format!("{e:#}"))?;
            Err(anyhow::anyhow!(failed_upload(&e, &keep)))
        }
    }
}

/// The one line for a save that failed. A request that got no reply is "outcome unknown —
/// check <path>" — never "not uploaded".
fn failed_upload(e: &anyhow::Error, keep: &Path) -> String {
    match remote::outcome_unknown(e) {
        Some(unknown) => format!(
            "{unknown}; your copy is at {} (cause: {})",
            keep.display(),
            e.root_cause()
        ),
        None => format!("{e:#}; your copy is at {}", keep.display()),
    }
}

fn run_cut(
    arg: &str,
    start: &str,
    end: &str,
    crop: Option<&str>,
    dirs: &Dirs,
    rec: &mut Record,
) -> Result<()> {
    rec.action = Some("cut");
    let tools = ffmpeg::resolve()?;
    let req = CutRequest {
        in_s: ffmpeg::parse_time(start)?,
        out_s: ffmpeg::parse_time(end)?,
        crop: crop.map(str::parse::<Rect>).transpose()?,
    };
    if req.out_s <= req.in_s {
        bail!("OUT ({end}) must be after IN ({start})");
    }
    let target = target::parse(arg).map_err(anyhow::Error::msg)?;
    let (mut place, path) = Place::connect(&target, dirs, rec)?;
    let abs = place.fs().realpath(&path)?;
    let stat = place.fs().stat(&abs)?;
    let (dir, name) = remote::split_parent(&abs);
    let (dir, name) = (dir.to_string(), name.to_string());
    if stat.is_dir {
        bail!("{} is a directory", place.display(&abs));
    }
    rec.kind = Some(cut::media_kind_of(&name)?);
    rec.ext = kind::extension(&name);
    rec.bytes = stat.size;
    // A fetched source stays claimed until the cut is published.
    let (src, _claim) = match place {
        Place::Local(_) => (PathBuf::from(&abs), None),
        Place::Remote(..) => {
            let fetched = fetch_to_cache(&mut place, &abs, dirs, rec)?;
            (fetched.claim.local.clone(), Some(fetched.claim))
        }
    };
    let mut progress = Progress::new(format!("cutting {name}"));
    let result = cut::cut_and_publish(
        place.fs(),
        &tools,
        &src,
        &dir,
        &name,
        &req,
        &dirs.cache.join("exports"),
        &mut |f| progress.fraction(f),
    );
    progress.finish();
    let final_name = result?;
    println!("{}", place.display(&remote::join(&dir, &final_name.name)));
    // Never lost silently: what the export did not keep (also in the window before Export).
    for note in &final_name.notes {
        eprintln!("sopen: {note}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_shapes() {
        Cli::command_for_test().debug_assert();
        let cli = Cli::try_parse_from(["sopen", "box:/a.bin", "--app", "ImHex"]).unwrap();
        assert_eq!(cli.target.as_deref(), Some("box:/a.bin"));
        assert_eq!(cli.app.as_deref(), Some("ImHex"));
        let cli = Cli::try_parse_from([
            "sopen",
            "cut",
            "box:/t.mp3",
            "1",
            "0:03",
            "--crop",
            "0,0,2,2",
        ])
        .unwrap();
        assert!(matches!(cli.cmd, Some(Cmd::Cut { ref end, .. }) if end == "0:03"));
        assert!(Cli::try_parse_from(["sopen"]).is_err());
    }

    #[test]
    fn sizes() {
        assert_eq!(human(999), "999 B");
        assert_eq!(human(1_500_000), "1.5 MB");
    }

    /// A fetched copy of `body` at `<tmp>/cache/a.txt`, edited to `edit`.
    fn edited_copy(tmp: &Path, body: &str, edit: &str) -> (PathBuf, PathBuf, Stat) {
        let target = tmp.join("a.txt");
        std::fs::write(&target, body).unwrap();
        let local = tmp.join("cache/a.txt");
        let at = LocalFs::new()
            .fetch(target.to_str().unwrap(), &local, &mut |_, _| {})
            .unwrap();
        std::fs::write(&local, edit).unwrap();
        (target, local, at)
    }

    fn conflict_copies(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(".conflict-"))
            .collect()
    }

    /// Two sessions on one file never share its cached copy: while the first one's app or
    /// editor works on the copy, the second fetches into a copy of its own (and says so); the
    /// first one's copy is neither replaced nor moved aside.
    #[test]
    fn a_second_session_gets_its_own_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "v1").unwrap();
        let abs = target.to_str().unwrap();
        let dirs = Dirs {
            cache: tmp.path().join("cache"),
            state: tmp.path().join("state"),
        };
        let mut place = Place::Local(LocalFs::new());
        let mut rec = Record::default();
        let one = fetch_to_cache(&mut place, abs, &dirs, &mut rec).unwrap();
        let first = one.claim.local.clone();
        std::fs::write(&first, "the first session's edit in progress").unwrap();
        let two = fetch_to_cache(&mut place, abs, &dirs, &mut rec).unwrap();
        let second = two.claim.local.clone();
        assert_ne!(second, first);
        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            "the first session's edit in progress"
        );
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "v1");
        assert!(conflict_copies(first.parent().unwrap()).is_empty());
        let name = second.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("a.session-") && name.ends_with(".txt"),
            "{name}"
        );
        // The lock lasts as long as the session: once the first one ends, the copy is free
        // again (its unsaved edit is moved aside by the next fetch, as before).
        drop(one);
        let three = fetch_to_cache(&mut place, abs, &dirs, &mut rec).unwrap();
        assert_eq!(three.claim.local, first);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "v1");
        let kept = three.kept.expect("the first session's edit was kept");
        assert_eq!(
            std::fs::read_to_string(kept).unwrap(),
            "the first session's edit in progress"
        );
    }

    #[test]
    fn w8_a_conflict_keeps_the_edit_aside_and_exits_3() {
        let tmp = tempfile::tempdir().unwrap();
        let (target, local, at) = edited_copy(tmp.path(), "v1", "my edit");
        std::fs::write(&target, "v2, someone else").unwrap();
        let mut rec = Record::default();
        let err = upload_back(
            &mut LocalFs::new(),
            "box:/a.txt",
            &local,
            target.to_str().unwrap(),
            &at,
            &mut rec,
        )
        .unwrap_err();
        assert!(err.chain().any(|c| c.is::<Refused>()));
        assert!(rec.conflict);
        let kept = conflict_copies(&tmp.path().join("cache"));
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read_to_string(&kept[0]).unwrap(), "my edit");
        assert!(
            err.to_string().contains(&kept[0].display().to_string()),
            "{err}"
        );
        assert!(!local.exists(), "the next fetch has nothing to overwrite");
    }

    #[test]
    fn w8_any_failed_upload_keeps_the_edit_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let (target, local, at) = edited_copy(tmp.path(), "v1", "my edit");
        // The remote directory refuses new files (unless running as root, which ignores modes).
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if std::fs::metadata(&local).unwrap().uid() == 0 {
            eprintln!("skipping: root ignores directory permissions");
            return;
        }
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let mut rec = Record::default();
        let result = upload_back(
            &mut LocalFs::new(),
            "box:/a.txt",
            &local,
            target.to_str().unwrap(),
            &at,
            &mut rec,
        );
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = result.unwrap_err();
        assert!(
            !err.chain().any(|c| c.is::<Refused>()),
            "a failure, not a conflict"
        );
        let msg = format!("{err:#}");
        assert!(msg.contains("was not replaced"), "{msg}");
        let kept = conflict_copies(&tmp.path().join("cache"));
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read_to_string(&kept[0]).unwrap(), "my edit");
        assert!(msg.contains(&kept[0].display().to_string()), "{msg}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1");
    }

    #[test]
    fn w8_failure_wording() {
        let keep = Path::new("/c/a.txt.conflict-x");
        // As write_atomic reports a rename that landed but whose reply was lost.
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "v1").unwrap();
        let local = tmp.path().join("cache/a.txt");
        let at = LocalFs::new()
            .fetch(target.to_str().unwrap(), &local, &mut |_, _| {})
            .unwrap();
        std::fs::write(&local, "edit").unwrap();
        let e = LostRenameReply(LocalFs::new())
            .write_atomic(&local, target.to_str().unwrap(), at.fingerprint())
            .unwrap_err();
        let line = failed_upload(&e, keep);
        let want = format!(
            "outcome unknown — check {}; your copy is at /c/a.txt.conflict-x (cause: ",
            target.display()
        );
        assert!(line.contains(&want), "{line}");
        assert!(line.contains("connection closed"), "{line}");
        assert!(!line.contains("not replaced"), "{line}");
        let plain = failed_upload(&anyhow::anyhow!("disk full"), keep);
        assert_eq!(plain, "disk full; your copy is at /c/a.txt.conflict-x");
    }

    /// LocalFs whose rename lands but whose reply is lost.
    struct LostRenameReply(LocalFs);

    impl RemoteFs for LostRenameReply {
        fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
            self.0.stat_opt(path)
        }
        fn lstat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
            self.0.lstat_opt(path)
        }
        fn realpath(&mut self, path: &str) -> Result<String> {
            self.0.realpath(path)
        }
        fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>> {
            self.0.read_head(path, max)
        }
        fn list_dir(&mut self, path: &str) -> Result<Vec<String>> {
            self.0.list_dir(path)
        }
        fn read_to(
            &mut self,
            path: &str,
            out: &mut dyn Write,
            progress: &mut dyn FnMut(u64),
        ) -> Result<u64> {
            self.0.read_to(path, out, progress)
        }
        fn create_from(
            &mut self,
            local: &Path,
            path: &str,
            mode: Option<u32>,
        ) -> std::result::Result<remote::Uploaded, remote::CreateError> {
            self.0.create_from(local, path, mode)
        }
        fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
            self.0.set_mode(path, mode)
        }
        fn set_owner(&mut self, path: &str, uid: u32, gid: u32) -> Result<()> {
            self.0.set_owner(path, uid, gid)
        }
        fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
            self.0.rename_over(from, to)?;
            Err(remote::unknown(
                "rename: connection closed before the reply",
            ))
        }
        fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
            self.0.link_new(from, to)
        }
        fn remove(&mut self, path: &str) -> Result<()> {
            self.0.remove(path)
        }
    }

    #[test]
    fn w8_editor_failure_keeps_an_edited_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, local, at) = edited_copy(tmp.path(), "v1", "half-done edit");
        let err = keep_edits(&local, &at, anyhow::anyhow!("vi exited with 1"));
        let kept = conflict_copies(&tmp.path().join("cache"));
        assert_eq!(kept.len(), 1);
        assert!(err.to_string().contains("your edited copy is at"), "{err}");

        // An untouched copy stays where it is; there is nothing to keep.
        let (_, local, at) = edited_copy(tmp.path(), "v1", "v1");
        let err = keep_edits(&local, &at, anyhow::anyhow!("vi exited with 1"));
        assert_eq!(err.to_string(), "vi exited with 1");
        assert!(local.exists());
    }

    impl Cli {
        fn command_for_test() -> clap::Command {
            <Cli as clap::CommandFactory>::command()
        }
    }
}
