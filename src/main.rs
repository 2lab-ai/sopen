//! `sopen` CLI. Where it opens per spec §4a, routing per spec §3; every error is one line on
//! stderr and a non-zero exit (1 = error, 3 = a write refused because the remote changed). A run
//! forwarded to another machine leaves with that machine's exit code.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use sopen::cache::{self, Dirs, Fetched};
use sopen::cut::{self, CutRequest};
use sopen::ffmpeg::{self, Rect};
use sopen::gui;
use sopen::human;
use sopen::kind::{self, Kind};
use sopen::remote::{self, LocalFs, RemoteFs, SftpRemote, Stat, WriteOutcome};
use sopen::target::{self, RemoteSpec, Target};
use sopen::usage::Record;
use sopen::{app, config, forward, raise, zed};
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
        SOURCE uses scp syntax: [user@]host:path (relative paths are under the remote home) or a \
        local path. Folders and text open in VS Code when present, otherwise Zed; images, video \
        and audio in sopen's own window. Any other file (a zip, a binary) asks first: open it in \
        sopen's hex editor (the default) or save it to a path you choose. --hex opens any file in \
        the hex editor.\n\n\
        It opens on this machine, or with --target HOST on HOST's screen (sopen runs there, \
        started over ssh). SOPEN_TARGET or `sopen config target HOST` make a HOST the default; \
        --here opens on this machine anyway.",
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// [user@]host:path, or a local path
    #[arg(
        value_name = "SOURCE",
        required_unless_present_any = ["selftest_audio", "raise_window"]
    )]
    source: Option<String>,
    /// Open on HOST's screen instead of this machine's (sopen runs on HOST, over ssh)
    #[arg(long, value_name = "HOST", conflicts_with = "here")]
    target: Option<String>,
    /// Open on this machine, whatever SOPEN_TARGET or the config say
    #[arg(long)]
    here: bool,
    /// Open a copy in this local app; Enter in the terminal uploads the changes
    #[arg(long, value_name = "APP")]
    app: Option<String>,
    /// Open the file in sopen's hex editor, whatever its type (no dialog first)
    #[arg(long, conflicts_with = "app")]
    hex: bool,
    /// Receipt check: play FILE on the real audio device at gain 0 for ~2 s and print the
    /// callback / underrun counts (AGENTS.md "Receipts").
    #[arg(
        long,
        hide = true,
        value_name = "FILE",
        conflicts_with_all = ["source", "app", "hex", "target", "here"]
    )]
    selftest_audio: Option<PathBuf>,
    /// Internal: bring the window of the editor just opened to the user on KDE Plasma (the
    /// detached child sopen starts after the hand-off; spec §3).
    #[arg(
        long,
        hide = true,
        value_name = "EDITOR",
        value_parser = ["code", "zed"],
        conflicts_with_all = ["source", "app", "hex", "target", "here", "selftest_audio"]
    )]
    raise_window: Option<String>,
    /// Internal: the opened folder's or file's name, which the editor window shows (with
    /// --raise-window).
    #[arg(
        long,
        hide = true,
        value_name = "NAME",
        requires = "raise_window",
        allow_hyphen_values = true
    )]
    raise_caption: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Cut IN..OUT of an audio/video file into a new file next to it (never overwrites)
    Cut {
        /// [user@]host:path, or a local path
        #[arg(value_name = "SOURCE")]
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
    /// Show the config file, or set where `sopen SOURCE` opens by default
    Config {
        #[command(subcommand)]
        key: Option<ConfigKey>,
    },
}

#[derive(Subcommand)]
enum ConfigKey {
    /// Open on HOST by default (--here and --target still decide one run)
    Target {
        /// [user@]host, as --target takes it
        #[arg(value_name = "HOST", required_unless_present = "unset")]
        host: Option<String>,
        /// Open on this machine by default again
        #[arg(long, conflicts_with = "host")]
        unset: bool,
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
    if let Some(editor) = cli
        .raise_window
        .as_deref()
        .and_then(raise::Editor::from_arg)
    {
        raise::window_now(editor, cli.raise_caption.as_deref().unwrap_or_default());
        return ExitCode::SUCCESS;
    }
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
    let with_dirs = || dirs.as_ref().map_err(|e| anyhow::anyhow!("{e:#}"));
    let result = match cli.cmd {
        // Not a run on a file: no usage line.
        Some(Cmd::Config { key }) => return exit(run_config(key).map(|()| 0)),
        Some(Cmd::Cut {
            target,
            start,
            end,
            crop,
        }) => with_dirs().and_then(|dirs| {
            run_cut(&target, &start, &end, crop.as_deref(), dirs, &mut rec).map(|()| 0)
        }),
        None => with_dirs().and_then(|dirs| {
            run_source(
                cli.source.as_deref().unwrap_or_default(),
                cli.target.as_deref(),
                cli.here,
                cli.app.as_deref(),
                cli.hex,
                dirs,
                &mut rec,
            )
        }),
    };
    rec.ok = matches!(result, Ok(0));
    if let Ok(dirs) = &dirs {
        if let Err(e) = sopen::usage::append(&dirs.state, &rec) {
            sopen::debug!("usage log not written: {e:#}");
        }
    }
    exit(result)
}

/// The exit code: a forwarded run's own, else 0, or 1 / 3 with the error on one stderr line.
fn exit(result: Result<u8>) -> ExitCode {
    match result {
        Ok(code) => ExitCode::from(code),
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

    /// Hands the connection over: the file layer, how to reopen it if the session dies while
    /// a window is open, and the host label (`None` for local files).
    fn into_parts(
        self,
        dirs: &Dirs,
    ) -> (
        Box<dyn RemoteFs + Send>,
        Option<gui::Reconnect>,
        Option<String>,
    ) {
        match self {
            Place::Remote(spec, fs) => {
                let label = spec.label();
                let state = dirs.state.clone();
                let reconnect: gui::Reconnect = Box::new(move || {
                    let fs = SftpRemote::connect(&spec, &state)?;
                    Ok(Box::new(fs) as Box<dyn RemoteFs + Send>)
                });
                (fs, Some(reconnect), Some(label))
            }
            Place::Local(fs) => (Box::new(fs), None, None),
        }
    }

    /// Hands the connection to a window ([`Place::into_parts`]).
    fn into_origin(self, abs: &str, at: Stat, dirs: &Dirs) -> gui::Origin {
        let (fs, reconnect, label) = self.into_parts(dirs);
        gui::Origin::new(fs, reconnect, label, abs.to_string(), at)
    }
}

/// Claims the cached copy of `abs` for this session, moves an earlier copy's unsaved edits
/// aside, fetches with a progress line, and records what the copy now holds; records size and
/// fetch time. Every fetch into the cache goes through here (or [`cache::fetch_claimed`], its
/// core), so no session replaces or moves a copy another one is editing.
fn fetch_to_cache(place: &mut Place, abs: &str, dirs: &Dirs, rec: &mut Record) -> Result<Fetched> {
    let (_, name) = remote::split_parent(abs);
    let mut progress = Progress::new(format!("fetching {name}"));
    let host = place.host_label();
    let fetched = cache::fetch_claimed(place.fs(), &host, abs, &dirs.cache, &mut |done, total| {
        progress.bytes(done, total)
    });
    progress.finish();
    let (fetched, ms) = fetched?;
    rec.fetch_ms = ms;
    rec.bytes = fetched.at.size;
    Ok(fetched)
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

/// `sopen config [target HOST | target --unset]`.
fn run_config(key: Option<ConfigKey>) -> Result<()> {
    let path = config::path()?;
    match key {
        None => {
            let current = config::load(&path)?;
            println!("{}", path.display());
            match current.target {
                Some(host) => println!("target = {host}"),
                None => println!("target is not set"),
            }
            if let Some(host) = std::env::var("SOPEN_TARGET").ok().filter(|h| !h.is_empty()) {
                println!("SOPEN_TARGET={host} is set in this environment and comes first");
            }
        }
        Some(ConfigKey::Target { unset: true, .. }) => {
            config::unset_target(&path)?;
            println!("target is not set ({})", path.display());
        }
        Some(ConfigKey::Target { host, .. }) => {
            let host = host.context("give a HOST, or --unset")?;
            config::set_target(&path, &host)?;
            println!("target = {host} ({})", path.display());
        }
    }
    Ok(())
}

/// `sopen SOURCE`: on this machine, or forwarded to the target HOST (spec §4a). Returns the exit
/// code — HOST's own when forwarded.
fn run_source(
    source: &str,
    flag: Option<&str>,
    here: bool,
    app_name: Option<&str>,
    hex: bool,
    dirs: &Dirs,
    rec: &mut Record,
) -> Result<u8> {
    let chosen = forward::choose(here, flag, forward::target_env, || {
        Ok(config::load(&config::path()?)?.target)
    })?;
    let hostname = forward::hostname();
    let host = match chosen {
        Some(host) if !forward::is_here(&host, hostname.as_deref()) => host,
        _ => return run_open(source, app_name, hex, dirs, rec).map(|()| 0),
    };
    let there = forward::source_for(&host, source, hostname.as_deref(), |path| {
        LocalFs::new().realpath(path)
    })?;
    if let Ok(Target::Remote(spec)) = target::parse(source) {
        rec.host = Some(spec.label());
    }
    rec.ext = kind::extension(remote::split_parent(&there).1);
    rec.action = Some("open");
    rec.target = Some(host.clone());
    sopen::debug!("opening {there} on {host}");
    let code = forward::run(&host, &there, app_name, hex, &dirs.state)?;
    // HOST refused a write because the file changed (spec §5): a conflict here too (§10).
    rec.conflict = code == EXIT_CONFLICT;
    Ok(code)
}

fn run_open(
    arg: &str,
    app_name: Option<&str>,
    hex: bool,
    dirs: &Dirs,
    rec: &mut Record,
) -> Result<()> {
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
    if hex {
        if stat.is_dir {
            bail!("--hex opens files; {} is a directory", place.display(&abs));
        }
        return open_binary(place, &abs, stat.size, None, true, dirs, rec);
    }
    match kind {
        Kind::Directory | Kind::Text => open_in_editor(&mut place, &abs, kind, dirs, rec),
        Kind::Image | Kind::Video | Kind::Audio => open_in_window(place, &abs, kind, dirs, rec),
        Kind::Other => open_binary(place, &abs, stat.size, None, false, dirs, rec),
    }
}

/// The terminal's progress line ([`Progress`]) for the binary route's windowless transfers.
#[derive(Default)]
struct Meter(Option<Progress>);

impl gui::binary::Meter for Meter {
    fn start(&mut self, label: &str) {
        self.0 = Some(Progress::new(label));
    }
    fn bytes(&mut self, done: u64, total: u64) {
        if let Some(p) = self.0.as_mut() {
            p.bytes(done, total);
        }
    }
    fn finish(&mut self) {
        if let Some(mut p) = self.0.take() {
            p.finish();
        }
    }
}

/// A file sopen has no window for, or any file with `--hex` (spec §6a): the library's route
/// ([`gui::binary::open_file`]) over this file's connection. `fetched` = already in the cache
/// (a `.ts` that turned out not to be media).
fn open_binary(
    place: Place,
    abs: &str,
    size: u64,
    fetched: Option<Fetched>,
    hex: bool,
    dirs: &Dirs,
    rec: &mut Record,
) -> Result<()> {
    let hooks = gui::Hooks::from_env()?;
    let (_, name) = remote::split_parent(abs);
    let info = gui::binary::Info {
        title: place.display(abs),
        name: name.to_string(),
        size,
        local: fetched.is_some() || matches!(place, Place::Local(_)),
    };
    let (fs, reconnect, label) = place.into_parts(dirs);
    let source = gui::binary::FileSource::new(fs, reconnect, label, abs, &dirs.cache, fetched);
    let out = gui::binary::open_file(
        info,
        source,
        &hooks,
        hex,
        gui::display_available(),
        &mut Meter::default(),
        Box::new(gui::binary::NativeChooser),
        &mut |window| gui::binary::run_on_screen(window, &hooks),
    )?;
    if let Some(ms) = out.fetch_ms {
        rec.fetch_ms = ms;
    }
    if let Some(action) = out.action {
        rec.action = Some(action);
    }
    rec.conflict = out.conflict;
    match out.refused {
        Some(message) => Err(Refused(message).into()),
        None => Ok(()),
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
    let mut fetched = None;
    let (local, at) = match (&place, kind) {
        (Place::Local(_), Kind::Video | Kind::Audio) => {
            let at = place.fs().stat(abs)?;
            (PathBuf::from(abs), at)
        }
        _ => {
            // A copy with unsaved edits (an earlier `--app` / editor session) is kept, not
            // replaced by this fetch; the window repeats where it went.
            let copy = fetch_to_cache(&mut place, abs, dirs, rec)?;
            let local = copy.claim.local.clone();
            let at = copy.at;
            fetched = Some(copy);
            (local, at)
        }
    };
    let kind = refine_kind(&local, abs, kind);
    rec.kind = Some(kind);
    if kind == Kind::Other {
        // A `.ts` ffprobe cannot read as media: the binary dialog, on the copy already here.
        return open_binary(place, abs, at.size, fetched, false, dirs, rec);
    }
    let notices = fetched
        .as_ref()
        .map(|f| f.notices.clone())
        .unwrap_or_default();
    // Held until the window closes (media plays from the copy).
    let _claim = fetched.map(|f| f.claim);
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
        gui::image::open(file, &hooks, notices)?
    } else {
        gui::media::open(origin, &local, &dirs.cache, &hooks, notices)?
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
    let is_remote = matches!(place, Place::Remote(..));
    let code_cannot_remote = match zed::choose(
        is_remote,
        zed::locate_code(),
        zed::remote_ssh_installed,
        zed::locate,
    ) {
        zed::Editor::Code(code) => {
            let remote = match &*place {
                Place::Remote(spec, _) => Some((spec.user.as_deref(), spec.host.as_str())),
                Place::Local(_) => None,
            };
            zed::open_args(&code, &zed::code_args(remote, abs, kind == Kind::Directory))?;
            println!("opened {} in VS Code", place.display(abs));
            raise::editor_window(raise::Editor::Code, &raise::opened_name(abs));
            return Ok(());
        }
        zed::Editor::Zed {
            cli,
            code_cannot_remote,
        } => {
            if code_cannot_remote {
                eprintln!("VS Code has no Remote - SSH extension; opening in Zed");
            }
            let arg = match place {
                Place::Remote(spec, _) => zed::remote_url(spec.user.as_deref(), &spec.host, abs),
                Place::Local(_) => abs.to_string(),
            };
            zed::open(&cli, arg.as_ref())?;
            println!("opened {} in Zed", place.display(abs));
            raise::editor_window(raise::Editor::Zed, &raise::opened_name(abs));
            return Ok(());
        }
        zed::Editor::Neither { code_cannot_remote } => code_cannot_remote,
    };
    let no_code = if code_cannot_remote {
        "VS Code has no Remote - SSH extension"
    } else {
        "VS Code was not found"
    };
    if kind == Kind::Directory {
        let (names, fixed) = zed::candidates(zed::Os::current());
        let mut looked: Vec<&str> = names.to_vec();
        looked.extend_from_slice(fixed);
        bail!(
            "folders open in VS Code or Zed; {no_code} and Zed was not found (looked for {}); install an editor",
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
                "{no_code} and Zed was not found; editing a copy with {}",
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
            upload_back(place.fs(), &shown, &claim, abs, &at, rec)
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
    upload_back(place.fs(), &shown, &claim, abs, &at, rec)
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
/// next fetch cannot overwrite it, and the error prints that path. A save that lands is what
/// the copy holds from now on (its record).
fn upload_back(
    fs: &mut dyn RemoteFs,
    shown: &str,
    copy: &cache::Claim,
    abs: &str,
    at: &Stat,
    rec: &mut Record,
) -> Result<()> {
    let local = &copy.local;
    // The run is a save attempt from here on, whether it lands, conflicts or fails.
    rec.action = Some("save");
    match fs.write_atomic(local, abs, at.fingerprint()) {
        Ok(WriteOutcome::Written { now, notes }) => {
            if let Some(sha) = &now.sha256 {
                if let Err(e) = copy.record(sha) {
                    sopen::debug!("not recorded: {e:#}");
                }
            }
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
        &cache::exports_root(&dirs.cache),
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
        assert_eq!(cli.source.as_deref(), Some("box:/a.bin"));
        assert_eq!(cli.app.as_deref(), Some("ImHex"));
        assert!(cli.target.is_none() && !cli.here && !cli.hex);
        let cli = Cli::try_parse_from(["sopen", "--hex", "box:/a.txt"]).unwrap();
        assert!(cli.hex && cli.app.is_none());
        assert_eq!(cli.source.as_deref(), Some("box:/a.txt"));
        // As HOST's sopen is given it.
        let cli = Cli::try_parse_from(["sopen", "--here", "--hex", "--", "-x.bin"]).unwrap();
        assert!(cli.hex && cli.here);
        assert_eq!(cli.source.as_deref(), Some("-x.bin"));
        assert!(Cli::try_parse_from(["sopen", "--hex", "--app", "ImHex", "a"]).is_err());
        let cli = Cli::try_parse_from(["sopen", "--target", "me@box", "src:/a.png"]).unwrap();
        assert_eq!(cli.target.as_deref(), Some("me@box"));
        assert_eq!(cli.source.as_deref(), Some("src:/a.png"));
        assert!(Cli::try_parse_from(["sopen", "--here", "--target", "box", "a"]).is_err());
        assert!(Cli::try_parse_from(["sopen", "--target", "box", "cut", "a", "1", "2"]).is_err());
        // What HOST's sopen is given: after `--`, even a subcommand's name or a leading `-` is
        // SOURCE, and so is an empty SOURCE (HOST's home).
        for source in ["cut", "config", "-x.png", ""] {
            let cli = Cli::try_parse_from(["sopen", "--here", "--", source]).unwrap();
            assert!(cli.cmd.is_none(), "{source}");
            assert_eq!(cli.source.as_deref(), Some(source));
            assert!(cli.here);
        }
        let cli = Cli::try_parse_from(["sopen", "config", "target", "box"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Config { key: Some(ConfigKey::Target { host: Some(ref h), unset: false }) }) if h == "box"
        ));
        let cli = Cli::try_parse_from(["sopen", "config", "target", "--unset"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Config {
                key: Some(ConfigKey::Target {
                    host: None,
                    unset: true
                })
            })
        ));
        assert!(Cli::try_parse_from(["sopen", "config", "target"]).is_err());
        assert!(Cli::try_parse_from(["sopen", "config", "target", "box", "--unset"]).is_err());
        let cli = Cli::try_parse_from(["sopen", "config"]).unwrap();
        assert!(matches!(cli.cmd, Some(Cmd::Config { key: None })));
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

    /// `<tmp>/a.txt` holding `body`, fetched into the cache under `<tmp>/cache` (claimed for
    /// as long as the returned claim lives) and edited to `edit`.
    fn edited_copy(tmp: &Path, body: &str, edit: &str) -> (PathBuf, cache::Claim, Stat) {
        let target = tmp.join("a.txt");
        std::fs::write(&target, body).unwrap();
        let dirs = Dirs {
            cache: tmp.join("cache"),
            state: tmp.join("state"),
        };
        let abs = target.to_str().unwrap();
        let mut place = Place::Local(LocalFs::new());
        let Fetched { claim, at, .. } =
            fetch_to_cache(&mut place, abs, &dirs, &mut Record::default()).unwrap();
        std::fs::write(&claim.local, edit).unwrap();
        (target, claim, at)
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
        // Its own copy lives outside the mirror, under `<cache>/.sessions`.
        let mirrored = first.strip_prefix(&dirs.cache).unwrap();
        let name = second.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            second.parent().unwrap(),
            dirs.cache
                .join(".sessions")
                .join(mirrored.parent().unwrap())
        );
        assert!(
            name.starts_with("a.txt.") && name.ends_with(".txt"),
            "{name}"
        );
        let note = two.claim.note.as_deref().unwrap();
        assert!(note.starts_with("another sopen session has"), "{note}");
        assert!(one.claim.note.is_none());
        // The lock lasts as long as the session: once the first one ends, the copy is free
        // again (its unsaved edit is moved aside by the next fetch, as before).
        drop(one);
        let three = fetch_to_cache(&mut place, abs, &dirs, &mut rec).unwrap();
        assert_eq!(three.claim.local, first);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "v1");
        let kept = conflict_copies(first.parent().unwrap());
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(
            std::fs::read_to_string(&kept[0]).unwrap(),
            "the first session's edit in progress"
        );
        assert_eq!(
            three.notices,
            [format!(
                "the previous copy had unsaved edits; kept them at {}",
                kept[0].display()
            )]
        );
    }

    /// The cache mirrors remote paths, so a remote file may be named like anything sopen keeps
    /// about a copy (here: the old in-tree record and lock names). Such a file is just another
    /// copy: fetching it replaces neither its sibling's record nor its lock.
    #[test]
    fn a_remote_file_named_like_sopen_bookkeeping_is_just_another_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote");
        std::fs::create_dir(&remote).unwrap();
        let named = [
            ("a.txt", "v1"),
            (".a.txt.sopen-baseline", "a remote file 1"),
            (".a.txt.sopen-lock", "a remote file 2"),
        ];
        for (name, body) in named {
            std::fs::write(remote.join(name), body).unwrap();
        }
        let abs = |name: &str| remote.join(name).to_str().unwrap().to_string();
        let dirs = Dirs {
            cache: tmp.path().join("cache"),
            state: tmp.path().join("state"),
        };
        let mut place = Place::Local(LocalFs::new());
        let mut rec = Record::default();
        let mut fetch =
            |name: &str| fetch_to_cache(&mut place, &abs(name), &dirs, &mut rec).unwrap();
        let one = fetch("a.txt");
        let two = fetch(".a.txt.sopen-baseline");
        let three = fetch(".a.txt.sopen-lock");
        assert_eq!(
            std::fs::read_to_string(&two.claim.local).unwrap(),
            "a remote file 1"
        );
        assert_eq!(
            std::fs::read_to_string(&three.claim.local).unwrap(),
            "a remote file 2"
        );
        // a.txt's lock still holds: another session on it gets a copy of its own.
        let four = fetch("a.txt");
        assert_ne!(four.claim.local, one.claim.local, "the lock was replaced");
        // a.txt's record still says what its copy holds: once its sessions end, the next fetch
        // finds nothing unsaved to move aside.
        let first = one.claim.local.clone();
        drop((one, four));
        let five = fetch("a.txt");
        assert_eq!(five.claim.local, first);
        assert!(
            five.notices.is_empty(),
            "the record was replaced: {:?}",
            five.notices
        );
    }

    /// A save that lands is what the copy holds from then on: the next fetch finds nothing
    /// unsaved to move aside.
    #[test]
    fn a_saved_copy_is_recorded_so_the_next_fetch_keeps_nothing_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let (target, copy, at) = edited_copy(tmp.path(), "v1", "my edit");
        let abs = target.to_str().unwrap();
        upload_back(
            &mut LocalFs::new(),
            "box:/a.txt",
            &copy,
            abs,
            &at,
            &mut Record::default(),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "my edit");
        drop(copy);
        let dirs = Dirs {
            cache: tmp.path().join("cache"),
            state: tmp.path().join("state"),
        };
        let mut place = Place::Local(LocalFs::new());
        let again = fetch_to_cache(&mut place, abs, &dirs, &mut Record::default()).unwrap();
        assert!(again.notices.is_empty(), "{:?}", again.notices);
        assert_eq!(
            std::fs::read_to_string(&again.claim.local).unwrap(),
            "my edit"
        );
    }

    /// A copy edited outside a session (or by one that ended without saving) is moved aside by
    /// the next fetch, not overwritten, and the fetch says where it went.
    #[test]
    fn a_fetch_keeps_unsaved_edits_of_the_previous_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let (target, copy, _) = edited_copy(tmp.path(), "v1", "unsaved edit");
        let abs = target.to_str().unwrap().to_string();
        drop(copy);
        std::fs::write(&target, "v2").unwrap();
        let dirs = Dirs {
            cache: tmp.path().join("cache"),
            state: tmp.path().join("state"),
        };
        let mut place = Place::Local(LocalFs::new());
        let again = fetch_to_cache(&mut place, &abs, &dirs, &mut Record::default()).unwrap();
        assert_eq!(std::fs::read_to_string(&again.claim.local).unwrap(), "v2");
        let kept = conflict_copies(again.claim.local.parent().unwrap());
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(std::fs::read_to_string(&kept[0]).unwrap(), "unsaved edit");
        assert!(
            again.notices[0].ends_with(&*kept[0].to_string_lossy()),
            "{:?}",
            again.notices
        );
    }

    #[test]
    fn a_remote_file_named_like_a_session_copy_is_just_another_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote");
        std::fs::create_dir(&remote).unwrap();
        std::fs::write(remote.join("a.txt"), "v1").unwrap();
        std::fs::write(remote.join("a.session-1a2b3c4d.txt"), "a remote file").unwrap();
        let abs = |name: &str| remote.join(name).to_str().unwrap().to_string();
        let dirs = Dirs {
            cache: tmp.path().join("cache"),
            state: tmp.path().join("state"),
        };
        let mut place = Place::Local(LocalFs::new());
        let mut fetch =
            |name: &str| fetch_to_cache(&mut place, &abs(name), &dirs, &mut Record::default());
        let alike = fetch("a.session-1a2b3c4d.txt").unwrap();
        let alike_copy = alike.claim.local.clone();
        std::fs::write(&alike_copy, "my unsaved edit of it").unwrap();
        drop(alike);
        // Sessions on a.txt come and go, sharing its copy and getting their own.
        let one = fetch("a.txt").unwrap();
        let two = fetch("a.txt").unwrap();
        drop((one, two));
        let three = fetch("a.txt").unwrap();
        drop(three);
        assert_eq!(
            std::fs::read_to_string(&alike_copy).unwrap(),
            "my unsaved edit of it",
            "another file's copy was moved or removed"
        );
    }

    /// What the notice claims matches what sopen knows: a copy that differs from its record
    /// "had unsaved edits"; a copy with no record (its fetch was never recorded) is only kept in
    /// case it holds edits.
    #[test]
    fn a_kept_copy_is_described_by_what_is_known_about_it() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "v1").unwrap();
        let abs = target.to_str().unwrap().to_string();
        let dirs = Dirs {
            cache: tmp.path().join("cache"),
            state: tmp.path().join("state"),
        };
        let mut place = Place::Local(LocalFs::new());
        let mut fetch = || fetch_to_cache(&mut place, &abs, &dirs, &mut Record::default()).unwrap();

        let one = fetch();
        let copy = one.claim.local.clone();
        std::fs::write(&copy, "an edit").unwrap();
        drop(one);
        let two = fetch();
        let kept = conflict_copies(copy.parent().unwrap());
        assert_eq!(
            two.notices,
            [format!(
                "the previous copy had unsaved edits; kept them at {}",
                kept[0].display()
            )]
        );
        drop(two);

        // No record of what was fetched (as if never recorded): only "in case".
        let record = dirs
            .cache
            .join(".meta/record")
            .join(copy.strip_prefix(&dirs.cache).unwrap());
        std::fs::remove_file(&record).unwrap();
        let three = fetch();
        let kept: Vec<_> = conflict_copies(copy.parent().unwrap())
            .into_iter()
            .filter(|k| !kept.contains(k))
            .collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(
            three.notices,
            [format!(
                "no record of what was fetched for {}; kept it at {} in case it holds edits",
                copy.display(),
                kept[0].display()
            )]
        );
    }

    #[test]
    fn w8_a_conflict_keeps_the_edit_aside_and_exits_3() {
        let tmp = tempfile::tempdir().unwrap();
        let (target, copy, at) = edited_copy(tmp.path(), "v1", "my edit");
        let local = copy.local.clone();
        std::fs::write(&target, "v2, someone else").unwrap();
        let mut rec = Record::default();
        let err = upload_back(
            &mut LocalFs::new(),
            "box:/a.txt",
            &copy,
            target.to_str().unwrap(),
            &at,
            &mut rec,
        )
        .unwrap_err();
        assert!(err.chain().any(|c| c.is::<Refused>()));
        assert!(rec.conflict);
        let kept = conflict_copies(local.parent().unwrap());
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
        let (target, copy, at) = edited_copy(tmp.path(), "v1", "my edit");
        let local = copy.local.clone();
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
            &copy,
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
        let kept = conflict_copies(local.parent().unwrap());
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
            like: Option<remote::Like>,
        ) -> std::result::Result<remote::Uploaded, remote::CreateError> {
            self.0.create_from(local, path, like)
        }
        fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
            self.0.set_mode(path, mode)
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
        let (_, copy, at) = edited_copy(tmp.path(), "v1", "half-done edit");
        let err = keep_edits(&copy.local, &at, anyhow::anyhow!("vi exited with 1"));
        let kept = conflict_copies(copy.local.parent().unwrap());
        assert_eq!(kept.len(), 1);
        assert!(err.to_string().contains("your edited copy is at"), "{err}");

        // An untouched copy stays where it is; there is nothing to keep.
        drop(copy);
        let (_, copy, at) = edited_copy(tmp.path(), "v1", "v1");
        let err = keep_edits(&copy.local, &at, anyhow::anyhow!("vi exited with 1"));
        assert_eq!(err.to_string(), "vi exited with 1");
        assert!(copy.local.exists());
    }

    impl Cli {
        fn command_for_test() -> clap::Command {
            <Cli as clap::CommandFactory>::command()
        }
    }
}
