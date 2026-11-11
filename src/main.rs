//! `sopen` CLI. Routing per spec §3; every error is one line on stderr and a non-zero exit
//! (1 = error, 3 = a write refused because the remote changed).

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use sopen::cache::{self, Dirs};
use sopen::cut::{self, CutRequest};
use sopen::ffmpeg::{self, Rect};
use sopen::kind::{self, Kind};
use sopen::remote::{self, LocalFs, RemoteFs, SftpRemote, Stat, WriteOutcome};
use sopen::target::{self, RemoteSpec, Target};
use sopen::usage::Record;
use sopen::{app, zed};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Instant, SystemTime};

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
    #[arg(required = true)]
    target: Option<String>,
    /// Open a copy in this local app; Enter in the terminal uploads the changes
    #[arg(long, value_name = "APP")]
    app: Option<String>,
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
}

/// Fetches `abs` into the cache with a progress line; records size and fetch time.
fn fetch_to_cache(
    place: &mut Place,
    abs: &str,
    dirs: &Dirs,
    rec: &mut Record,
) -> Result<(PathBuf, Stat)> {
    let local = cache::cache_path(&dirs.cache, &place.host_label(), abs);
    let (_, name) = remote::split_parent(abs);
    let mut progress = Progress::new(format!("fetching {name}"));
    let started = Instant::now();
    let fetched = place
        .fs()
        .fetch(abs, &local, &mut |done, total| progress.bytes(done, total));
    progress.finish();
    let stat = fetched?;
    rec.fetch_ms = started.elapsed().as_millis() as u64;
    rec.bytes = stat.size;
    Ok((local, stat))
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
        Kind::Image | Kind::Video | Kind::Audio => {
            let (shown, kind) = match place {
                Place::Local(_) => (format!("file at {abs}"), kind),
                Place::Remote(..) => {
                    let (local, _) = fetch_to_cache(&mut place, &abs, dirs, rec)?;
                    let kind = refine_kind(&local, kind);
                    (format!("cached at {}", local.display()), kind)
                }
            };
            rec.kind = Some(kind);
            let what = if kind == Kind::Image {
                "image"
            } else {
                "media"
            };
            println!("{what} window lands in the next release — {shown}");
            Ok(())
        }
        Kind::Other => {
            match place {
                Place::Local(_) => println!("{abs}"),
                Place::Remote(..) => {
                    let (local, _) = fetch_to_cache(&mut place, &abs, dirs, rec)?;
                    println!("{}", local.display());
                }
            }
            Ok(())
        }
    }
}

/// Video by extension but no video stream (per ffprobe) is audio; without ffprobe, keep it.
fn refine_kind(local: &Path, kind: Kind) -> Kind {
    if kind != Kind::Video {
        return kind;
    }
    match ffmpeg::resolve().and_then(|t| ffmpeg::probe(&t, local)) {
        Ok(probe) => kind::refine_with_probe(kind, probe.video.is_some()),
        Err(e) => {
            sopen::debug!("not probed: {e:#}");
            kind
        }
    }
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
            let (local, at) = fetch_to_cache(place, abs, dirs, rec)?;
            let before = app::file_hash(&local)?;
            eprintln!(
                "Zed not found; editing a copy with {}",
                editor.to_string_lossy()
            );
            if let Err(e) = zed::run_terminal_editor(&local) {
                bail!("{e:#}; not uploaded — the copy is at {}", local.display());
            }
            if app::file_hash(&local)? == before {
                println!("no changes; {} untouched", place.display(abs));
                rec.action = Some("discard");
                return Ok(());
            }
            upload_back(place, &local, abs, at, rec)
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
    let (local, at) = fetch_to_cache(place, abs, dirs, rec)?;
    let before = app::file_hash(&local)?;
    app::open_with(app_name, &local)?;
    eprintln!(
        "opened {} in {app_name} (local copy {})",
        place.display(abs),
        local.display()
    );
    let stdin = std::io::stdin();
    let decision = app::prompt(&mut stdin.lock(), &mut std::io::stderr())?;
    if decision == app::Decision::Discard {
        println!("discarded; {} untouched", place.display(abs));
        rec.action = Some("discard");
        return Ok(());
    }
    if app::file_hash(&local)? == before {
        println!("no changes; {} untouched", place.display(abs));
        rec.action = Some("discard");
        return Ok(());
    }
    upload_back(place, &local, abs, at, rec)
}

fn describe_change(at: &Stat, now: Option<&Stat>) -> String {
    let Some(now) = now else {
        return "it was deleted".to_string();
    };
    let secs = |t: SystemTime| cache::rfc3339(t);
    let mut parts = Vec::new();
    if now.size != at.size {
        parts.push(format!("size {} → {}", at.size, now.size));
    }
    if now.mtime != at.mtime {
        parts.push(format!("modified {} → {}", secs(at.mtime), secs(now.mtime)));
    }
    parts.join(", ")
}

/// Conflict-safe upload of an edited copy (spec §5). On a conflict the edit is moved aside to
/// `<copy>.conflict-<stamp>` (so the next fetch cannot overwrite it) and the run fails with exit 3.
fn upload_back(
    place: &mut Place,
    local: &Path,
    abs: &str,
    at: Stat,
    rec: &mut Record,
) -> Result<()> {
    // The run is a save attempt from here on, whether it lands, conflicts or fails.
    rec.action = Some("save");
    match place.fs().write_atomic(local, abs, at.fingerprint())? {
        WriteOutcome::Written { .. } => {
            println!("saved {}", place.display(abs));
            Ok(())
        }
        WriteOutcome::Conflict { now } => {
            rec.conflict = true;
            let keep = cache::conflict_copy_path(local, SystemTime::now());
            std::fs::rename(local, &keep)
                .with_context(|| format!("cannot keep the edited copy at {}", keep.display()))?;
            Err(Refused(format!(
                "{} changed since it was fetched ({}); not uploaded — your copy is at {}",
                place.display(abs),
                describe_change(&at, now.as_ref()),
                keep.display()
            ))
            .into())
        }
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
    let src = match place {
        Place::Local(_) => PathBuf::from(&abs),
        Place::Remote(..) => fetch_to_cache(&mut place, &abs, dirs, rec)?.0,
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
    println!("{}", place.display(&remote::join(&dir, &final_name)));
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
    fn change_description() {
        let at = Stat {
            is_dir: false,
            size: 3,
            mtime: SystemTime::UNIX_EPOCH,
            mode: 0o644,
        };
        let now = Stat { size: 5, ..at };
        assert_eq!(describe_change(&at, Some(&now)), "size 3 → 5");
        assert_eq!(describe_change(&at, None), "it was deleted");
        assert_eq!(human(999), "999 B");
        assert_eq!(human(1_500_000), "1.5 MB");
    }

    impl Cli {
        fn command_for_test() -> clap::Command {
            <Cli as clap::CommandFactory>::command()
        }
    }
}
