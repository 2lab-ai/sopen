//! Where `sopen SOURCE` opens (spec §4a). On this machine, or — with `--target HOST`,
//! `SOPEN_TARGET` or the config's `target` — on HOST's screen: sopen runs on HOST over the system
//! `ssh` (the same multiplexed master the SFTP layer uses) with SOURCE rewritten as HOST sees it,
//! and `--here`, so HOST never forwards again. Only HOST needs sopen; a file's host still needs
//! nothing but sshd.
//!
//! sshd hands the command to the login shell on HOST as one string, so every word is quoted
//! here ([`quote`]) and the script on HOST is plain POSIX `sh`.

use crate::target::{self, Target};
use anyhow::{anyhow, bail, Context, Result};
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::Path;
use std::process::{Command, ExitStatus};

/// Receipt hooks (AGENTS.md "Receipts") a forwarded run carries to HOST (`SOPEN_STATE`'s git keys
/// and `SOPEN_LIST=git` among them, 02 §9). Settings that name local paths — `SOPEN_CODE`,
/// `SOPEN_ZED`, `SOPEN_FFMPEG`, `SOPEN_FFPROBE`, `SOPEN_GIT`, `SOPEN_CACHE_DIR`, `SOPEN_STATE_DIR`,
/// `SOPEN_CONFIG` — mean nothing on HOST and stay here.
pub const FORWARDED_ENV: [&str; 22] = [
    "SOPEN_STATE",
    "SOPEN_SCREENSHOT",
    "SOPEN_WATCHDOG",
    "SOPEN_AUTOSAVE",
    "SOPEN_AUTOSAVE_DELAY_MS",
    "SOPEN_AUTOEXPORT",
    "SOPEN_EXPORT_HOLD_MS",
    "SOPEN_RENDERER",
    "SOPEN_WINDOW_SIZE",
    "SOPEN_CHOICE",
    "SOPEN_SAVE_TO",
    "SOPEN_SAVE_HOLD_MS",
    // 07 §7: the hex and media windows' Save As, where the window is.
    "SOPEN_SAVE_AS",
    "SOPEN_SAVE_AS_HOLD_MS",
    "SOPEN_LIST",
    "SOPEN_LOG",
    "SOPEN_TARGET_BIN",
    // 06 §11: the stream's stats lines and its throttle, read where the file is viewed.
    "SOPEN_STREAM_STATS",
    "SOPEN_STREAM_THROTTLE",
    // `sopen URL`: what opens the address, and the listener table the tunnel's port is checked
    // in, on the machine that shows it.
    "SOPEN_BROWSER",
    "SOPEN_LISTENERS",
    // 09 §4: whether the page view has an engine, read where the page or the address shows.
    "SOPEN_WEB",
];

/// Where HOST's sopen is looked for when the non-interactive ssh session's PATH does not have it
/// (Homebrew on macOS and Linux, `cargo install`). Shell words: `$HOME` expands on HOST.
pub const INSTALL_PATHS: &str = "/opt/homebrew/bin/sopen /usr/local/bin/sopen \
     /home/linuxbrew/.linuxbrew/bin/sopen \"$HOME/.linuxbrew/bin/sopen\" \"$HOME/.cargo/bin/sopen\"";

/// Homebrew's bin directories, appended to HOST's PATH when they exist: a non-interactive ssh
/// session on macOS has only `/usr/bin:/bin:/usr/sbin:/sbin`, and sopen finds ffmpeg on PATH.
/// Shell words: `$HOME` expands on HOST.
const BREW_BIN_DIRS: &str =
    "/opt/homebrew/bin /usr/local/bin /home/linuxbrew/.linuxbrew/bin \"$HOME/.linuxbrew/bin\"";

/// The desktop session's variables a window needs, imported from the systemd user manager when
/// an ssh session on Linux has no display — with the desktop's name (KDE Plasma: sopen brings the
/// editor's window forward, [`crate::raise`]) and the session's own, KDE_SESSION_VERSION among
/// them: `xdg-open`, an address's opener, picks `kde-open` by it, and without it falls back to
/// `kfmclient`, missing on Plasma 6, and reports success having opened nothing.
const SESSION_ENV: &str = "DISPLAY WAYLAND_DISPLAY XAUTHORITY XDG_RUNTIME_DIR \
     DBUS_SESSION_BUS_ADDRESS XDG_SESSION_TYPE XDG_CURRENT_DESKTOP \
     KDE_SESSION_VERSION KDE_FULL_SESSION XDG_SESSION_DESKTOP DESKTOP_SESSION";

/// `[user@]host` split at the last `@`, as ssh does.
fn split_user(host: &str) -> (Option<&str>, &str) {
    match host.rfind('@') {
        Some(i) => (Some(&host[..i]), &host[i + 1..]),
        None => (None, host),
    }
}

/// A HOST from `--target`, `SOPEN_TARGET` or the config: `[user@]host` (an alias from
/// `~/.ssh/config` works). It comes before `--` on the ssh command line, so it may not look like
/// an option.
pub fn check_host(host: &str) -> std::result::Result<(), String> {
    if host.is_empty() {
        return Err("the host is empty".to_string());
    }
    if host.starts_with('-') {
        return Err(format!("'{host}' starts with '-'"));
    }
    if host.chars().any(char::is_whitespace) {
        return Err(format!("'{host}' contains whitespace"));
    }
    if host.contains(':') {
        return Err(format!("'{host}' contains ':' (a host, not host:path)"));
    }
    match split_user(host) {
        (_, "") => Err(format!("'{host}' has no host name")),
        (Some(""), _) => Err(format!("'{host}' has an empty user name")),
        _ => Ok(()),
    }
}

/// Where to open (`None` = this machine): `--here` or `--target` (never both — the command line
/// refuses that), else `SOPEN_TARGET`, else the config's `target`, else this machine. `env` and
/// `config` are read only when nothing before them decided, so a broken config file or an
/// unreadable `SOPEN_TARGET` does not stop `--here` or `--target`.
pub fn choose(
    here: bool,
    flag: Option<&str>,
    env: impl FnOnce() -> Result<Option<String>>,
    config: impl FnOnce() -> Result<Option<String>>,
) -> Result<Option<String>> {
    if here {
        return Ok(None);
    }
    if let Some(host) = flag {
        check_host(host).map_err(|problem| anyhow!("--target: {problem}"))?;
        return Ok(Some(host.to_string()));
    }
    if let Some(host) = env()?.filter(|h| !h.is_empty()) {
        check_host(&host).map_err(|problem| anyhow!("SOPEN_TARGET: {problem}"))?;
        return Ok(Some(host));
    }
    config()
}

/// `SOPEN_TARGET` from the environment; one that is not UTF-8 is an error.
pub fn target_env() -> Result<Option<String>> {
    match std::env::var("SOPEN_TARGET") {
        Ok(host) => Ok(Some(host)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => bail!("SOPEN_TARGET is not valid UTF-8"),
    }
}

/// This machine's host name as `gethostname` gives it (`box` or `box.lan`).
pub fn hostname() -> Option<String> {
    let name = gethostname::gethostname().into_string().ok()?;
    (!name.is_empty()).then_some(name)
}

/// This machine's host name up to the first `.` (`box.lan` → `box`) — the name HOST is told to
/// reach this machine by.
pub fn short_hostname() -> Option<String> {
    short_name(&hostname()?)
}

fn short_name(name: &str) -> Option<String> {
    let name = bare(name);
    if name.parse::<std::net::IpAddr>().is_ok() {
        return Some(name.to_string());
    }
    let short = name.split('.').next()?;
    (!short.is_empty()).then(|| short.to_string())
}

/// A name without its trailing dot (`box.lan.` → `box.lan`).
fn bare(name: &str) -> &str {
    name.strip_suffix('.').unwrap_or(name)
}

/// The loopback aliases common in `/etc/hosts` (Debian, Red Hat), matched whole.
const LOOPBACK_ALIASES: [&str; 5] = [
    "localhost.localdomain",
    "localhost6",
    "localhost6.localdomain6",
    "ip6-localhost",
    "ip6-loopback",
];

/// Loopback, so whoever resolves it means itself: exactly `localhost`, a name under
/// `.localhost` (RFC 6761) or one of [`LOOPBACK_ALIASES`], `127.0.0.0/8`, or `::1`.
/// `localhost.example.com` is not.
pub(crate) fn is_loopback(name: &str) -> bool {
    let name = bare(name);
    match name.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => {
            let name = name.to_ascii_lowercase();
            name == "localhost"
                || name.ends_with(".localhost")
                || LOOPBACK_ALIASES.contains(&name.as_str())
        }
    }
}

/// HOST is this machine: its host part (without `user@`) is loopback, or equals this machine's
/// full host name (`hostname`, as `gethostname` gives it) or that name up to the first `.` — in
/// any case, trailing dot ignored. Nothing else counts: opening on the wrong machine is worse
/// than an ssh hop to this one, so `box.lan` from a machine named just `box` is forwarded.
pub fn is_here(host: &str, hostname: Option<&str>) -> bool {
    let (_, name) = split_user(host);
    let name = bare(name);
    is_loopback(name)
        || hostname.is_some_and(|full| {
            name.eq_ignore_ascii_case(bare(full))
                || short_name(full).is_some_and(|short| name.eq_ignore_ascii_case(&short))
        })
}

/// SOURCE as HOST must be given it:
/// - a file on HOST itself (same host, and no user or HOST's user) → its path there, which HOST
///   opens as a local file (a relative path is under HOST's home: the script starts there);
/// - a file on any other machine → unchanged;
/// - a file on this machine → `<short>:<abs>` (`short` from `hostname`, this machine's host
///   name), `abs` from `local_abs` (which fails on a missing file exactly as a local open does);
///   a loopback host (`localhost:p`, `[::1]:p`) would mean HOST itself, so it becomes
///   `[user@]<short>:p`.
pub fn source_for(
    host: &str,
    source: &str,
    hostname: Option<&str>,
    local_abs: impl FnOnce(&str) -> Result<String>,
) -> Result<String> {
    let short = hostname.and_then(short_name);
    let short = short.as_deref();
    match target::parse(source).map_err(anyhow::Error::msg)? {
        Target::Remote(spec) if is_loopback(&spec.host) => {
            let user = spec.user.map(|u| format!("{u}@")).unwrap_or_default();
            Ok(format!("{user}{}:{}", this_machine(short)?, spec.path))
        }
        Target::Remote(spec) => {
            let (user, name) = split_user(host);
            let same_user = spec.user.is_none() || spec.user.as_deref() == user;
            if spec.host != name || !same_user {
                return Ok(source.to_string());
            }
            let path = spec.path.as_str();
            // `~other/…` is for SFTP's expand-path to resolve; HOST keeps the remote form.
            if path.starts_with('~') && path != "~" && !path.starts_with("~/") {
                return Ok(source.to_string());
            }
            if path.is_empty() {
                return Ok(".".to_string());
            }
            // `a:b/c` would read as host `a` on HOST; scp's `./` keeps it a path.
            match target::parse(path) {
                Ok(Target::Remote(_)) | Err(_) => Ok(format!("./{path}")),
                Ok(Target::Local(_)) => Ok(path.to_string()),
            }
        }
        Target::Local(_) => {
            let short = this_machine(short)?;
            Ok(format!("{short}:{}", local_abs(source)?))
        }
    }
}

/// The name HOST reaches this machine by.
fn this_machine(short: Option<&str>) -> Result<&str> {
    short.context(
        "cannot read this machine's host name, which the other machine needs to fetch a file from here",
    )
}

/// `K=V` for each of [`FORWARDED_ENV`] that is set here, in that order.
pub fn forwarded_env(get: impl Fn(&str) -> Option<OsString>) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for key in FORWARDED_ENV {
        if let Some(value) = get(key) {
            let value = value
                .into_string()
                .map_err(|_| anyhow!("{key} is not valid UTF-8"))?;
            out.push(format!("{key}={value}"));
        }
    }
    Ok(out)
}

/// One shell word that reads back as `word`. Single quotes around everything except `'` and `\`,
/// which go outside them as `\'` and `\\` — so the word means the same in sh, bash, zsh and dash,
/// and in fish, which also treats `\\` and `\'` as escapes inside single quotes:
/// `it's` → `'it'\''s'`, `a\b` → `'a'\\'b'`.
pub fn quote(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 2);
    out.push('\'');
    for c in word.chars() {
        match c {
            '\'' => out.push_str(r"'\''"),
            '\\' => out.push_str(r"'\\'"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// The `sh` script HOST runs, on one line (no shell reads it differently for a newline). Its
/// arguments are `[--app APP] [--gui] [--hex] [--hold-until-eof] [--browser] -- SOURCE`; it
/// always adds `--here`.
fn script(host: &str, install_paths: &str) -> String {
    let missing = quote(&format!(
        "sopen is not installed on {host} (brew install 2lab-ai/tap/sopen-preview)"
    ));
    [
        r#"cd "$HOME" || exit 1;"#.to_string(),
        r#"bin=${SOPEN_TARGET_BIN:-};"#.to_string(),
        r#"if [ -z "$bin" ]; then bin=$(command -v sopen 2>/dev/null) || bin=; fi;"#.to_string(),
        format!(
            r#"if [ -z "$bin" ]; then for c in {install_paths}; do if [ -f "$c" ] && [ -x "$c" ]; then bin=$c; break; fi; done; fi;"#
        ),
        format!(r#"if [ -z "$bin" ]; then printf '%s\n' {missing} >&2; exit 127; fi;"#),
        // ffmpeg is found on PATH: sopen's own directory first (brew's ffmpeg sits next to brew's
        // sopen), then Homebrew's bin directories after the session's own entries.
        r#"case $bin in */*) dir=${bin%/*}; PATH=${dir:-/}:$PATH;; esac;"#.to_string(),
        format!(
            r#"for d in {BREW_BIN_DIRS}; do if [ -d "$d" ]; then case ":$PATH:" in *":$d:"*) ;; *) PATH=$PATH:$d;; esac; fi; done;"#
        ),
        "export PATH;".to_string(),
        // An ssh session on Linux has no display: take the desktop session's from the systemd
        // user manager. Only these names, and only by `export "NAME=value"` — nothing is
        // evaluated. Values systemd prints C-quoted (`$'…'`) are left alone.
        r#"if [ "$(uname -s)" = Linux ] && [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then"#
            .to_string(),
        r#"session=$(systemctl --user show-environment 2>/dev/null);"#.to_string(),
        format!(r#"for key in {SESSION_ENV}; do"#),
        r#"line=$(printf '%s\n' "$session" | grep "^$key=" | head -n 1);"#.to_string(),
        r#"case $line in ''|"$key="|"$key=\$'"*) ;; *) export "$line";; esac;"#.to_string(),
        "done;".to_string(),
        // Still none: the usual places of a local session's sockets.
        r#"if [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then"#.to_string(),
        r#"XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}; export XDG_RUNTIME_DIR;"#
            .to_string(),
        r#"for s in "$XDG_RUNTIME_DIR"/wayland-*; do if [ -S "$s" ]; then WAYLAND_DISPLAY=${s##*/}; export WAYLAND_DISPLAY; break; fi; done;"#
            .to_string(),
        r#"if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -e /tmp/.X11-unix/X0 ]; then DISPLAY=:0; export DISPLAY; fi;"#
            .to_string(),
        "fi;".to_string(),
        "fi;".to_string(),
        // Not `exec`: sopen runs as a foreground child, with default signal handling (a trap's
        // handler does not carry over to it, as an ignored signal would), while this shell
        // catches INT, TERM and HUP and lives to report 128 + n — a signal that ended the shell
        // itself would reach ssh as a lost connection (255). Not in the background either: a
        // non-interactive shell starts background commands with SIGINT ignored. Ctrl+C under
        // `ssh -t` reaches the whole process group, sopen included; a signal sent to this shell
        // alone leaves sopen running.
        r#"trap ':' INT TERM HUP; "$bin" --here "$@"; exit $?"#.to_string(),
    ]
    .join(" ")
}

/// What a forward passes on to HOST's sopen besides `--here`, in this order before `--`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flags<'a> {
    /// `--app APP` (10 §11 row 1).
    pub app: Option<&'a str>,
    /// `--gui` (10 §11 row 2, T35): the owner gave it (from XF also when the kind this side sees
    /// needs a window: `AUTO_GUI` in `src/main.rs`); never with `app` nor for an address. An
    /// older sopen on HOST that refuses it at parse time gets the run again without it ([`run`]).
    pub gui: bool,
    /// `--hex`.
    pub hex: bool,
    /// An address's `--browser` (09 §7).
    pub browser: bool,
}

/// The command for HOST's sshd: `exec [env K=V…] sh -c <script> sopen [--app APP] [--gui]
/// [--hex] [--browser] -- SOURCE`,
/// every word but `exec` quoted. The hooks go in front of `sh`, so the script sees
/// `SOPEN_TARGET_BIN` and the sopen it starts sees them all. The bare `exec` (a constant, never
/// data) replaces the login shell that parses the line — dash and fish would otherwise stay as
/// its parent, in the same process group, and die of the Ctrl+C the script survives, which ssh
/// reports as a lost connection (255).
pub fn remote_words(
    host: &str,
    source: &str,
    flags: Flags,
    env: &[String],
    install_paths: &str,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if let Some(app) = flags.app {
        args.extend(["--app".to_string(), app.to_string()]);
    }
    if flags.gui {
        args.push(GUI_FLAG.to_string());
    }
    if flags.hex {
        args.push("--hex".to_string());
    }
    if flags.browser {
        args.push(BROWSER_FLAG.to_string());
    }
    words(host, &args, source, env, install_paths)
}

/// [`remote_words`] for an address held open through a tunnel ([`tunnel`]): HOST's sopen gets
/// `--here --hold-until-eof [--browser] -- URL` — it opens the address, then waits for its
/// input's end.
pub fn tunnel_words(
    host: &str,
    url: &str,
    browser: bool,
    env: &[String],
    install_paths: &str,
) -> Vec<String> {
    let mut flags = vec![HOLD_FLAG.to_string()];
    if browser {
        flags.push(BROWSER_FLAG.to_string());
    }
    words(host, &flags, url, env, install_paths)
}

/// HOST's sopen flag that keeps it (and the ssh session, and the tunnel) until its input ends.
pub const HOLD_FLAG: &str = "--hold-until-eof";

/// `sopen URL`'s flag for the system browser (09 §7), passed on to HOST's sopen as given: an
/// older one does not know it and exits 2 with its own line.
pub const BROWSER_FLAG: &str = "--browser";

/// The window on the machine SOURCE opens on (10 §11 row 2).
pub const GUI_FLAG: &str = "--gui";

/// `exec [env K=V…] sh -c <script> sopen <flags…> -- SOURCE`, every word but `exec` quoted.
fn words(
    host: &str,
    flags: &[String],
    source: &str,
    env: &[String],
    install_paths: &str,
) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    if !env.is_empty() {
        words.push("env".to_string());
        words.extend(env.iter().cloned());
    }
    words.extend([
        "sh".to_string(),
        "-c".to_string(),
        script(host, install_paths),
        "sopen".to_string(),
    ]);
    words.extend(flags.iter().cloned());
    words.extend(["--".to_string(), source.to_string()]);
    std::iter::once("exec".to_string())
        .chain(words.iter().map(|w| quote(w)))
        .collect()
}

/// Options for the command run only: a `RemoteCommand` in `~/.ssh/config` (a tmux auto-attach)
/// would make ssh refuse a command line, and "Shared connection to HOST closed." would follow
/// every terminal run (errors still print).
const RUN_OPTIONS: [&str; 4] = ["-o", "RemoteCommand=none", "-o", "LogLevel=ERROR"];

/// HOST's terminal and input: a terminal (`-t`) when this side has one; otherwise none (`-T`)
/// and no input (`-n`) — ssh would read this side's input to its end, eating the rest of a
/// `while read` loop's list — unless `--app` waits for Enter on it.
fn session_flags(tty: bool, app: bool) -> &'static [&'static str] {
    match (tty, app) {
        (true, _) => &["-t"],
        (false, true) => &["-T"],
        (false, false) => &["-T", "-n"],
    }
}

/// `ssh <connection> <session flags> <run options> HOST -- <words>`.
pub fn ssh_args(
    connection: Vec<OsString>,
    tty: bool,
    app: bool,
    host: &str,
    words: Vec<String>,
) -> Vec<OsString> {
    let mut args = connection;
    args.extend(session_flags(tty, app).iter().map(OsString::from));
    args.extend(RUN_OPTIONS.iter().map(OsString::from));
    args.push(host.into());
    args.push("--".into());
    args.extend(words.into_iter().map(OsString::from));
    args
}

/// Runs sopen on HOST for `source` (already as HOST sees it) with `flags` and waits; returns the
/// exit code to leave with. A run with `--gui` that HOST's sopen refuses as an argument it does
/// not know (exit 2 and clap's line, [`older`]) is run once more without it, after one line
/// saying so: clap refuses before anything runs there, so nothing is done twice.
pub fn run(host: &str, source: &str, flags: Flags, state_dir: &Path) -> Result<u8> {
    let env = forwarded_env(|key| std::env::var_os(key))?;
    let connection = crate::remote::connection_args(host, state_dir)?;
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let ssh = |flags: Flags| -> Command {
        let words = remote_words(host, source, flags, &env, INSTALL_PATHS);
        // An address is logged as sopen prints it, never whole (`url::redacted`).
        let logged = match crate::url::parse(source) {
            Some(_) => remote_words(host, &crate::url::redacted(source), flags, &env, ""),
            None => words.clone(),
        };
        let app = flags.app.is_some();
        crate::debug!(
            "{}",
            crate::render_command(
                std::ffi::OsStr::new("ssh"),
                &ssh_args(connection.clone(), tty, app, host, logged)
            )
        );
        let mut ssh = Command::new("ssh");
        ssh.args(ssh_args(connection.clone(), tty, app, host, words));
        ssh
    };
    if flags.gui {
        let out: Sink = || Box::new(std::io::stdout());
        let err: Sink = || Box::new(std::io::stderr());
        let (status, refused) = watched(ssh(flags), tty, out, err)?;
        if !older(status.code(), refused) {
            return exit_code(host, status);
        }
        eprintln!(
            "sopen: sopen on {host} is older and does not know {GUI_FLAG}; running it there \
             again without it"
        );
    }
    let without = Flags {
        gui: false,
        ..flags
    };
    let status = ssh(without)
        .status()
        .context("cannot run ssh (is OpenSSH installed?)")?;
    exit_code(host, status)
}

/// clap's first line for an argument it does not know, as a sopen on HOST older than `--gui`
/// says it (clap 4; coloured when its stderr is a terminal, as under `ssh -t`).
const UNKNOWN_GUI: &str = "error: unexpected argument '--gui' found";

/// HOST's answer to a run with `--gui` says its sopen is older, and refused it before doing
/// anything: exit 2 *and* `refused` — [`UNKNOWN_GUI`] the first line HOST wrote on its stderr,
/// nothing on its stdout ([`watched`]), as clap's rejection at parse time leaves it. Any other
/// exit 2 — a usage error of its own, a sopen older still that does not know `--here`, a run that
/// said something first — is HOST's answer as it is.
fn older(code: Option<i32>, refused: bool) -> bool {
    code == Some(2) && refused
}

/// Where one of HOST's streams goes on this side.
type Sink = fn() -> Box<dyn std::io::Write>;

/// Runs `ssh` with HOST's output piped through this side to `out` and `err` — every byte passed
/// on the moment it is read, and only watched ([`Scan`]) — and returns ssh's status and whether
/// HOST's sopen refused `--gui` at parse time: under `-t` (`tty`) the terminal there takes HOST's
/// stdout and stderr both, ssh's stdout, whose first line must be [`UNKNOWN_GUI`]; under `-T`
/// that must be the first line of ssh's stderr, and its stdout empty.
fn watched(mut ssh: Command, tty: bool, out: Sink, err: Sink) -> Result<(ExitStatus, bool)> {
    use std::process::Stdio;
    ssh.stdout(Stdio::piped());
    if !tty {
        ssh.stderr(Stdio::piped());
    }
    let mut child = ssh
        .spawn()
        .context("cannot run ssh (is OpenSSH installed?)")?;
    let (out_seen, out_done) = watch(child.stdout.take(), out);
    let on_err = (!tty).then(|| watch(child.stderr.take(), err));
    let status = child.wait().context("ssh did not run to its end")?;
    let (err_seen, err_done) = match on_err {
        Some((seen, done)) => (Some(seen), Some(done)),
        None => (None, None),
    };
    // Their ends, unless a descendant of ssh still holds one (a ProxyCommand keeps ssh's stderr).
    ended(err_done, out_done);
    let out_seen = *out_seen.lock().unwrap_or_else(|e| e.into_inner());
    let refused = match err_seen {
        None => out_seen.first == Some(true),
        Some(err_seen) => {
            let err_seen = *err_seen.lock().unwrap_or_else(|e| e.into_inner());
            err_seen.first == Some(true) && !out_seen.wrote
        }
    };
    Ok((status, refused))
}

/// What one of HOST's streams has shown so far.
#[derive(Debug, Clone, Copy, Default)]
struct Seen {
    /// Whether its first line that is not blank is [`UNKNOWN_GUI`] (`None`: none yet).
    first: Option<bool>,
    /// A byte came.
    wrote: bool,
}

/// Copies `pipe` to `sink()`, each read passed on as it is the moment it is read — a terminal's
/// query that a program on HOST then waits on (`ESC[6n`) cannot wait for a line's end — and only
/// then [`Scan`]ned; the shared [`Seen`] follows it, and the receiver gets `()` at the pipe's end.
fn watch(
    pipe: Option<impl std::io::Read + Send + 'static>,
    sink: Sink,
) -> (
    std::sync::Arc<std::sync::Mutex<Seen>>,
    std::sync::mpsc::Receiver<()>,
) {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Seen::default()));
    let shared = seen.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut scan = Scan::default();
            let mut bytes = [0u8; 8192];
            loop {
                let n = match pipe.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                let mut sink = sink();
                let _ = sink.write_all(&bytes[..n]);
                let _ = sink.flush();
                scan.take(&bytes[..n]);
                *shared.lock().unwrap_or_else(|e| e.into_inner()) = scan.seen;
            }
            scan.end();
            *shared.lock().unwrap_or_else(|e| e.into_inner()) = scan.seen;
        }
        let _ = tx.send(());
    });
    (seen, rx)
}

/// Watches one of HOST's streams, never holding any of it: whether a byte came, and whether its
/// first line that is not blank is [`UNKNOWN_GUI`] ([`Seen`]). A line ends at `\n` or `\r` (a
/// terminal's `\r\n`); ANSI escape sequences (clap's colours) do not count.
#[derive(Default)]
struct Scan {
    /// The current line so far, up to [`Scan::KEPT`] bytes (past that, coloured, it is longer
    /// than clap's line).
    line: Vec<u8>,
    seen: Seen,
}

impl Scan {
    const KEPT: usize = 512;

    fn take(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.seen.wrote = true;
            if b == b'\n' || b == b'\r' {
                self.line_end();
            } else if self.line.len() < Self::KEPT {
                self.line.push(b);
            }
        }
    }

    /// The current line has ended: the first that is not blank is clap's or not.
    fn line_end(&mut self) {
        if self.seen.first.is_none() {
            let plain = without_escapes(&self.line);
            if !plain.iter().all(u8::is_ascii_whitespace) {
                self.seen.first = Some(plain.starts_with(UNKNOWN_GUI.as_bytes()));
            }
        }
        self.line.clear();
    }

    /// The stream's end: its last line counts.
    fn end(&mut self) {
        self.line_end();
    }
}

/// `bytes` without their ANSI escape sequences: a CSI (`ESC [`, parameters, one final byte in
/// `@`–`~`) whole, any other `ESC` with the byte after it.
fn without_escapes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        if bytes.get(i) == Some(&b'[') {
            i += 1;
            while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
        }
        // The final byte, or the one after a lone ESC.
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The switch from `sopen --tui` to a window on HOST (10 §7.3, X2-switch): HOST's half,
// `sopen --here --gui --state-stdin -- FOLDER` (X2-host), over the forward's master, options,
// bare `exec` and quoting, with the state on its input and its cues read from its output. The
// terminal's side of it — the wait, the lines, the state given back — is `gui::term::remote`.

/// HOST's flag for the switch's state on its input (X2-host).
pub const STATE_STDIN_FLAG: &str = "--state-stdin";

/// The hooks of [`FORWARDED_ENV`] the switch leaves here, each for why — they script HOST's run
/// or make it one without a window; the other ten travel as a `--target` run carries them
/// (`SOPEN_WATCHDOG`, `SOPEN_RENDERER`, `SOPEN_WINDOW_SIZE`, `SOPEN_LOG`, `SOPEN_TARGET_BIN`,
/// the two stream hooks, `SOPEN_BROWSER`, `SOPEN_LISTENERS`, `SOPEN_WEB`: how HOST's sopen is
/// found, logs, draws and reads, nothing of what its window shows).
pub const SWITCH_LEFT_HERE: [(&str, &str); 12] = [
    // HOST's browser applies it on top of the state it is given (`open_on` → `scripted`): an
    // `open=` would open a viewer over the state's.
    ("SOPEN_STATE", "applied on top of the given state"),
    // The window would be shot and closed: a scripted run, never asked about its close.
    (
        "SOPEN_SCREENSHOT",
        "a shot's run closes its window by itself",
    ),
    ("SOPEN_AUTOSAVE", "a save without a window"),
    ("SOPEN_AUTOSAVE_DELAY_MS", "SOPEN_AUTOSAVE's"),
    ("SOPEN_AUTOEXPORT", "an export without a window"),
    // Starts an Export when a media window opens: a job that writes, which holds the return.
    ("SOPEN_EXPORT_HOLD_MS", "starts an export on open"),
    (
        "SOPEN_CHOICE",
        "answers the binary dialog: a run without a window",
    ),
    ("SOPEN_SAVE_TO", "SOPEN_CHOICE's"),
    ("SOPEN_SAVE_HOLD_MS", "SOPEN_CHOICE's"),
    // Saves the edits as a new file once the state is on screen.
    ("SOPEN_SAVE_AS", "a scripted Save As"),
    ("SOPEN_SAVE_AS_HOLD_MS", "SOPEN_SAVE_AS's"),
    // A listing printed instead of the window.
    ("SOPEN_LIST", "a run without a window"),
];

/// The switch's hooks for HOST: [`forwarded_env`] without [`SWITCH_LEFT_HERE`]; and, when the
/// owner set no `SOPEN_WINDOW_SIZE`, the terminal's grid in points (`grid`, T16: a window a
/// switch opens has the terminal's grid size, as the in-process switch's has).
pub fn switch_env(
    get: impl Fn(&str) -> Option<OsString>,
    grid: Option<[f32; 2]>,
) -> Result<Vec<String>> {
    let left: Vec<&str> = SWITCH_LEFT_HERE.iter().map(|(key, _)| *key).collect();
    let mut env = forwarded_env(|key| get(key).filter(|_| !left.contains(&key)))?;
    if let Some([w, h]) =
        grid.filter(|_| !env.iter().any(|kv| kv.starts_with("SOPEN_WINDOW_SIZE=")))
    {
        env.push(format!("SOPEN_WINDOW_SIZE={w}x{h}"));
    }
    Ok(env)
}

/// The command for HOST's sshd: `exec [env K=V…] sh -c <script> sopen --gui --state-stdin --
/// FOLDER` — the script adds `--here` ([`remote_words`]'s, every word but `exec` quoted).
pub fn switch_words(host: &str, folder: &str, env: &[String], install_paths: &str) -> Vec<String> {
    let flags = [GUI_FLAG.to_string(), STATE_STDIN_FLAG.to_string()];
    words(host, &flags, folder, env, install_paths)
}

/// `ssh <connection> -T <run options> -o BatchMode=yes HOST -- <words>`: no terminal there, this
/// side's input kept (the state, then held open), and nothing asked — the terminal is the TUI's
/// (10 §7.3).
pub fn switch_args(connection: Vec<OsString>, host: &str, words: Vec<String>) -> Vec<OsString> {
    let mut args = connection;
    args.extend(session_flags(false, true).iter().map(OsString::from));
    args.extend(RUN_OPTIONS.iter().map(OsString::from));
    args.extend(["-o", "BatchMode=yes"].map(OsString::from));
    args.push(host.into());
    args.push("--".into());
    args.extend(words.into_iter().map(OsString::from));
    args
}

/// A place HOST names, as this machine names it — [`source_for`]'s way back (10 §7.3): HOST's own
/// path (`/abs`) is `HOST:/abs`; a loopback place there (`localhost:/p`, `127.0.0.1:/p`,
/// `[::1]:/p`) is HOST itself too — `[its user, else HOST's]@HOST:/p`, as `source_for` turns this
/// machine's loopback into this machine's name; `<this machine>:/abs` (no user) is `/abs` again;
/// any other machine's place is unchanged. `host` is HOST as ssh is given it, `[user@]host`.
pub fn source_back(host: &str, source: &str, hostname: Option<&str>) -> Result<String> {
    let (host_user, host_name) = split_user(host);
    let host_label = match target::parse(&format!("{host_name}:/")) {
        Ok(Target::Remote(spec)) => spec.label(),
        _ => bail!("{host} is not a host"),
    };
    match target::parse(source).map_err(anyhow::Error::msg)? {
        Target::Local(path) => {
            let user = host_user.map(|u| format!("{u}@")).unwrap_or_default();
            Ok(format!("{user}{host_label}:{}", path.to_string_lossy()))
        }
        Target::Remote(spec) if is_loopback(&spec.host) => {
            let user = (spec.user.as_deref().or(host_user))
                .map(|u| format!("{u}@"))
                .unwrap_or_default();
            Ok(format!("{user}{host_label}:{}", spec.path))
        }
        Target::Remote(spec) => {
            // This machine by the name `source_for` gave it, or by its full one; never loopback
            // (that would be HOST's own there).
            let name = bare(&spec.host);
            let mine = spec.user.is_none()
                && hostname.is_some_and(|full| {
                    name.eq_ignore_ascii_case(bare(full))
                        || short_name(full).is_some_and(|short| name.eq_ignore_ascii_case(&short))
                });
            Ok(if mine { spec.path } else { source.to_string() })
        }
    }
}

/// The longest line of HOST's standard error kept (the status row shows about a hundred cells).
pub(crate) const ERR_LINE_MAX: usize = 4096;

/// What HOST's half of the switch said, in its order ([`SwitchRun::said`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SwitchSaid {
    /// `sopen-xtools-state <json>` ([`crate::gui::switch_host::STATE_CUE`]): its JSON.
    State(String),
    /// `sopen-xtools-kept` ([`crate::gui::switch_host::KEPT_CUE`]).
    Kept,
    /// A state's cue past [`STATE_LINE_MAX`]: dropped unread.
    StateTooLong,
    /// A line of its standard error (ssh's own among them), escapes taken out.
    Err(String),
}

impl SwitchSaid {
    /// The cue on a line of HOST's output, matched exactly as [`Cue::of`] matches the tunnel's —
    /// only its end taken off (a `\r` too); any other line is none.
    fn cue(line: &str) -> Option<SwitchSaid> {
        // Each taken off on its own: the reader may have taken the `\n` already (a CRLF line).
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let state = crate::gui::switch_host::STATE_CUE;
        if line == crate::gui::switch_host::KEPT_CUE {
            return Some(SwitchSaid::Kept);
        }
        let json = line.strip_prefix(state)?.strip_prefix(' ')?;
        Some(SwitchSaid::State(json.to_string()))
    }
}

/// What the switch runs: the ssh program, its connection, HOST, the folder as HOST is given it,
/// the hooks and the install paths HOST's script looks in.
pub(crate) struct Switch<'a> {
    pub ssh: &'a std::ffi::OsStr,
    pub connection: Vec<OsString>,
    pub host: &'a str,
    pub folder: &'a str,
    pub env: &'a [String],
    pub install_paths: &'a str,
}

/// HOST's half running ([`Switch::start`]): ssh, the way to end its input, what HOST said so
/// far, and its two readers' ends.
pub(crate) struct SwitchRun {
    child: std::process::Child,
    /// Dropped, ssh's input closes — after the state's line, whenever it is written.
    input: Option<std::sync::mpsc::Sender<()>>,
    heard: std::sync::Arc<std::sync::Mutex<SwitchHeard>>,
    readers: Vec<std::sync::mpsc::Receiver<()>>,
}

/// The longest line of HOST's standard output read that is not a state's cue; a longer one is
/// dropped whole.
const OUT_LINE_MAX: usize = 1 << 20;

/// What HOST said and was not looked at yet — bounded, never a queue of its output: its last
/// state's cue, its kept cue, its last standard-error line.
#[derive(Debug, Default)]
struct SwitchHeard {
    state: Option<String>,
    kept: bool,
    err: Option<String>,
    /// A state's cue past [`STATE_LINE_MAX`] came: dropped, said.
    too_long: bool,
}

/// The longest line read that is a state's cue (`sopen-xtools-state ` and its JSON): a state can
/// pass [`OUT_LINE_MAX`] (select-all in a folder of tens of thousands of names).
pub(crate) const STATE_LINE_MAX: usize = 64 << 20;

/// Reads `from` to its end a line at a time (`\n`; a line split across reads is one), each given
/// to `line` with whether it passed its bound: `max` of the line so far says how long it may be;
/// a line past it keeps its first `max` bytes, the rest until its end dropped.
fn bounded_lines(
    mut from: impl std::io::Read,
    max: impl Fn(&[u8]) -> usize,
    mut line: impl FnMut(&[u8], bool),
) {
    let mut now = Vec::new();
    let mut over = false;
    let mut chunk = [0u8; 8192];
    loop {
        let n = match from.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for &b in &chunk[..n] {
            if b == b'\n' {
                line(&now, over);
                now.clear();
                over = false;
            } else if !over {
                match now.len() < max(&now) {
                    true => now.push(b),
                    false => over = true,
                }
            }
        }
    }
    if !now.is_empty() {
        line(&now, over);
    }
}

/// HOST's standard output read for its cues alone ([`SwitchSaid::cue`]); any other line dropped.
/// A line past its bound is dropped whole, never read cut: [`STATE_LINE_MAX`] for a state's cue
/// (said: [`SwitchHeard::too_long`]), [`OUT_LINE_MAX`] for any other.
fn hear_out(from: impl std::io::Read, heard: &std::sync::Mutex<SwitchHeard>) {
    let prefix = format!("{} ", crate::gui::switch_host::STATE_CUE);
    let state = |line: &[u8]| line.starts_with(prefix.as_bytes());
    let max = |line: &[u8]| match state(line) {
        true => STATE_LINE_MAX,
        false => OUT_LINE_MAX,
    };
    bounded_lines(from, max, |line, over| {
        let mut heard = heard.lock().unwrap_or_else(|p| p.into_inner());
        if over {
            heard.too_long |= state(line);
            return;
        }
        match SwitchSaid::cue(&String::from_utf8_lossy(line)) {
            Some(SwitchSaid::State(json)) => heard.state = Some(json),
            Some(SwitchSaid::Kept) => heard.kept = true,
            _ => {}
        }
    });
}

/// HOST's standard error read for its last line that is not blank, escapes taken out, cut to
/// [`ERR_LINE_MAX`] (a status row's line, never parsed); a cue there is a plain line.
fn hear_err(from: impl std::io::Read, heard: &std::sync::Mutex<SwitchHeard>) {
    bounded_lines(
        from,
        |_| ERR_LINE_MAX,
        |line, _| {
            let plain = without_escapes(line);
            let plain = String::from_utf8_lossy(&plain).trim_end().to_string();
            if !plain.trim().is_empty() {
                heard.lock().unwrap_or_else(|p| p.into_inner()).err = Some(plain);
            }
        },
    );
}

/// `hear` on `pipe` on a thread of its own; the receiver gets `()` at the pipe's end.
fn hearing<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
    heard: std::sync::Arc<std::sync::Mutex<SwitchHeard>>,
    hear: fn(R, &std::sync::Mutex<SwitchHeard>),
) -> std::sync::mpsc::Receiver<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Some(pipe) = pipe {
            hear(pipe, &heard);
        }
        let _ = tx.send(());
    });
    rx
}

impl Switch<'_> {
    /// Starts HOST's half: ssh with [`switch_args`], `SSH_ASKPASS_REQUIRE=never` in its
    /// environment (as the ssh layer's commands and git's runner run it), `state` and a newline
    /// on its input — written on a thread of its own, so a full pipe never holds the terminal —
    /// which then stays open; its output read for the cues, its standard error line by line.
    pub(crate) fn start(&self, state: &str) -> Result<SwitchRun> {
        use std::io::Write as _;
        use std::process::Stdio;
        let words = switch_words(self.host, self.folder, self.env, self.install_paths);
        let args = switch_args(self.connection.clone(), self.host, words);
        crate::debug!("{}", crate::render_command(self.ssh, &args));
        let mut child = Command::new(self.ssh)
            .args(&args)
            .env("SSH_ASKPASS_REQUIRE", "never")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot run ssh (is OpenSSH installed?)")?;
        let (input, closed) = std::sync::mpsc::channel::<()>();
        let mut stdin = child.stdin.take();
        let line = format!("{state}\n");
        std::thread::spawn(move || {
            if let Some(to) = stdin.as_mut() {
                let _ = to.write_all(line.as_bytes());
                let _ = to.flush();
            }
            // Held open until the run ends it (or goes).
            let _ = closed.recv();
            drop(stdin);
        });
        let heard = std::sync::Arc::new(std::sync::Mutex::new(SwitchHeard::default()));
        let out = hearing(child.stdout.take(), heard.clone(), hear_out);
        let err = hearing(child.stderr.take(), heard.clone(), hear_err);
        Ok(SwitchRun {
            child,
            input: Some(input),
            heard,
            readers: vec![out, err],
        })
    }
}

impl SwitchRun {
    /// What HOST said since the last look: its state's cue (the last), its kept cue, its last
    /// standard-error line — each at most once.
    pub(crate) fn said(&self) -> Vec<SwitchSaid> {
        let mut heard = self.heard.lock().unwrap_or_else(|p| p.into_inner());
        let mut said = Vec::new();
        said.extend(heard.err.take().map(SwitchSaid::Err));
        said.extend(heard.state.take().map(SwitchSaid::State));
        if std::mem::take(&mut heard.kept) {
            said.push(SwitchSaid::Kept);
        }
        if std::mem::take(&mut heard.too_long) {
            said.push(SwitchSaid::StateTooLong);
        }
        said
    }

    /// HOST's input ends (Ctrl+C here, 10 §7.3): it closes its window, or keeps one with
    /// something to lose and says so.
    pub(crate) fn end_input(&mut self) {
        self.input.take();
    }

    /// Ends this run's ssh client — its own pid, nothing else: never `ssh -O exit`, which would
    /// end the master every other session of the host shares.
    pub(crate) fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// ssh's status once it has ended — what its output still held then read first, waited for
    /// at most 2 s (a descendant of ssh may hold a pipe open), so a cue written just before the
    /// end is in [`SwitchRun::said`] — else `None`.
    pub(crate) fn ended(&mut self) -> std::io::Result<Option<ExitStatus>> {
        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        self.input.take();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        for done in self.readers.drain(..) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let _ = done.recv_timeout(left);
        }
        Ok(Some(status))
    }
}

impl Drop for SwitchRun {
    /// A run given up (the terminal's signal): its ssh client ends and is reaped.
    fn drop(&mut self) {
        self.input.take();
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// `sopen URL` on another machine (spec §4a): a loopback address there would mean that machine,
// so it is served from here through an ssh tunnel that lives exactly as long as this sopen.

/// The tunnel's own connection: never sopen's multiplexed master (a `-R` asked through a master
/// belongs to the master and would outlive this sopen), with its keepalives and connect bound
/// (`ServerAlive*`, `ConnectTimeout` from `connection`, [`crate::remote::connection_args`]), and
/// `ExitOnForwardFailure`, so a remote port that is taken ends ssh at once.
pub fn tunnel_connection(connection: &[OsString]) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-S",
        "none",
        "-o",
        "ControlMaster=no",
        "-o",
        "ExitOnForwardFailure=yes",
    ]
    .map(OsString::from)
    .to_vec();
    for pair in connection.windows(2) {
        let value = pair[1].to_string_lossy();
        if pair[0] == "-o"
            && (value.starts_with("ServerAlive") || value.starts_with("ConnectTimeout="))
        {
            args.extend(pair.iter().cloned());
        }
    }
    args
}

/// `ssh <connection> -T -R <forward> <run options> HOST -- <words>`: no terminal there, and this
/// side's input (no `-n`) — HOST's sopen holds until it ends.
pub fn tunnel_args(
    connection: Vec<OsString>,
    host: &str,
    forward: &str,
    words: Vec<String>,
) -> Vec<OsString> {
    let mut args = connection;
    args.extend(["-T", "-R", forward].map(OsString::from));
    args.extend(RUN_OPTIONS.iter().map(OsString::from));
    args.push(host.into());
    args.push("--".into());
    args.extend(words.into_iter().map(OsString::from));
    args
}

/// The first of the dynamic ports (IANA): a fallback port is one of 49152–65535.
const DYNAMIC_PORTS: std::ops::RangeInclusive<u16> = 49152..=65535;

/// The remote ports a tunnel tries: the address's own first (its origin unchanged), then
/// `tries` others from the dynamic range 49152–65535, each different, picked by `random`.
pub fn tunnel_ports(first: u16, tries: usize, mut random: impl FnMut(usize) -> u64) -> Vec<u16> {
    let span = u64::from(DYNAMIC_PORTS.end() - DYNAMIC_PORTS.start()) + 1;
    let mut ports = vec![first];
    for i in 0..tries {
        let mut port = DYNAMIC_PORTS.start() + (random(i) % span) as u16;
        // A port already on the list: the next one up, wrapping in the range.
        while ports.contains(&port) {
            port = if port == *DYNAMIC_PORTS.end() {
                *DYNAMIC_PORTS.start()
            } else {
                port + 1
            };
        }
        ports.push(port);
    }
    ports
}

/// ssh's line when the remote port is taken (with `ExitOnForwardFailure`, then exit 255).
fn forward_failed(line: &str) -> bool {
    line.contains("remote port forwarding failed for listen port")
}

/// How many other ports a tunnel tries after the address's own: ssh's failure line is the same
/// for a port that is taken and for a server that refuses forwarding, so the tries stay few.
const TUNNEL_TRIES: usize = 2;

/// The tunnel's ssh runs under this `sh` guard, given the ssh command line as its arguments.
/// ssh reads the guard's input — the pipe this sopen holds — and a watcher reading the same
/// input kills ssh at its end: when this sopen closes the pipe, or dies, SIGKILL included. ssh
/// alone does not end there: once HOST's command ends it waits for the forwarded connections a
/// browser keeps open (ssh(1), `~&`), and new ones would still be served. The input goes
/// through fd 3: a shell without job control gives an asynchronous command `/dev/null` as its
/// input, and dash does so before the command's own redirections, so `<&0` would be `/dev/null`
/// there. ssh's output goes through fd 4 and the guard keeps none of it, so that output ends
/// when ssh's session does (OpenSSH hands its stdout to the session channel), which is how this
/// side sees HOST's command end. The guard reaps its watcher and exits with ssh's status.
const GUARD: &str = r#"exec 3<&0 4>&1 >/dev/null; "$@" <&3 >&4 3<&- 4>&- & ssh=$!; exec 4>&-; (read -r _ <&3; kill "$ssh" 2>/dev/null) 2>/dev/null & watch=$!; exec 3<&-; wait "$ssh"; code=$?; kill "$watch" 2>/dev/null; wait "$watch" 2>/dev/null; exit "$code""#;

/// What a tunnel runs: the ssh program, its connection, HOST, the address and its `--browser`,
/// the hooks and the install paths HOST's script looks in.
pub(crate) struct Tunnel<'a> {
    pub ssh: &'a std::ffi::OsStr,
    pub connection: Vec<OsString>,
    pub host: &'a str,
    pub url: &'a crate::url::Url,
    pub browser: bool,
    pub env: &'a [String],
    pub install_paths: &'a str,
}

/// How one ssh run of a tunnel ended.
enum Attempt {
    /// The remote port was taken: try the next.
    Taken,
    /// The run's result: HOST's exit code, or 0 once stopped after the address opened, or once
    /// HOST's window was closed.
    Done(Result<u8>),
}

/// A line of HOST's sopen that is a cue for this side (09 §7). Matched exactly, as HOST writes
/// it — only its end (`\n` or `\r\n`) taken off, never a space or a tab before it: any other
/// line — a later sopen's cue among them — is HOST's plain output, passed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cue {
    /// [`crate::url::TUNNEL_WINDOW`]: the address is in a window there.
    Window,
    /// [`crate::url::TUNNEL_OPEN`]: the address is open there.
    Open,
    /// [`crate::url::TUNNEL_CLOSED`]: its window there was closed.
    Closed,
}

impl Cue {
    fn of(line: &str) -> Option<Cue> {
        let line = match line.strip_suffix('\n') {
            Some(line) => line.strip_suffix('\r').unwrap_or(line),
            None => line,
        };
        match line {
            crate::url::TUNNEL_WINDOW => Some(Cue::Window),
            crate::url::TUNNEL_OPEN => Some(Cue::Open),
            crate::url::TUNNEL_CLOSED => Some(Cue::Closed),
            _ => None,
        }
    }
}

/// What HOST's cues have said so far in one attempt.
#[derive(Default)]
struct Heard {
    window: bool,
    /// The open line was said.
    open: bool,
    closed: bool,
}

impl Heard {
    /// Takes the cues that came, in their order: the open line (`open_line`, given whether the
    /// window cue came first — an older HOST, or one on the system browser, never says it) is
    /// said once.
    fn take(
        &mut self,
        cues: &std::sync::mpsc::Receiver<Cue>,
        open_line: &dyn Fn(bool) -> String,
        say: &mut dyn FnMut(&str),
    ) {
        for cue in cues.try_iter() {
            match cue {
                Cue::Window => self.window = true,
                Cue::Open if !self.open => {
                    self.open = true;
                    say(&open_line(self.window));
                }
                Cue::Open => {}
                Cue::Closed => self.closed = true,
            }
        }
    }
}

/// Runs the tunnel on `ports` in turn until one is not taken on HOST; `say` gets the lines that
/// say the address is open there and, if HOST says so, that its window there was closed (09
/// §7); `stop` is polled for a signal that ends it.
pub(crate) fn run_tunnel(
    t: &Tunnel,
    ports: &[u16],
    stop: &dyn Fn() -> Option<i32>,
    say: &mut dyn FnMut(&str),
) -> Result<u8> {
    use std::io::Write as _;
    for (i, &port) in ports.iter().enumerate() {
        if let Attempt::Done(result) = attempt(t, port, stop, say)? {
            return result;
        }
        if let Some(next) = ports.get(i + 1) {
            // Not `eprintln!`, which panics on a closed stderr: the tunnel goes on all the same.
            let _ = writeln!(
                std::io::stderr(),
                "sopen: port {port} could not be forwarded on {} (taken there, or forwarding \
                 refused) — trying {next}",
                t.host
            );
        }
    }
    bail!(
        "{} refused the tunnel: forwarding is off there, or no port was free \
         (AllowTcpForwarding?)",
        t.host
    )
}

/// How long ssh may take to exit by itself once its session's output has ended (HOST's command
/// is over) before the tunnel is closed under it.
const SESSION_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// One ssh run of the tunnel on remote port `port`.
fn attempt(
    t: &Tunnel,
    port: u16,
    stop: &dyn Fn() -> Option<i32>,
    say: &mut dyn FnMut(&str),
) -> Result<Attempt> {
    use std::io::Write as _;
    use std::process::Stdio;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let there = t.url.with_port(port);
    let forward = t.url.remote_forward(port);
    let words = tunnel_words(t.host, &there, t.browser, t.env, t.install_paths);
    let args = tunnel_args(t.connection.clone(), t.host, &forward, words);
    // Logged as sopen prints the address, never whole.
    let shown_there = crate::url::redacted(&there);
    let logged = tunnel_words(t.host, &shown_there, t.browser, t.env, "");
    crate::debug!(
        "{}",
        crate::render_command(
            t.ssh,
            &tunnel_args(t.connection.clone(), t.host, &forward, logged)
        )
    );
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(GUARD)
        .arg("sopen-tunnel")
        .arg(t.ssh)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run ssh (is OpenSSH installed?)")?;
    // Held, never written: its end is HOST's (and the guard's) cue.
    let mut input = child.stdin.take();
    // HOST's cues, in the order it said them; every other line of its output is passed on.
    let (cue_tx, cues) = std::sync::mpsc::channel();
    let out = relay(child.stdout.take(), std::io::stdout, move |line| {
        let cue = Cue::of(line);
        if let Some(cue) = cue {
            let _ = cue_tx.send(cue);
        }
        cue.is_none()
    });
    let taken = Arc::new(AtomicBool::new(false));
    let seen = taken.clone();
    let err = relay(child.stderr.take(), std::io::stderr, move |line| {
        let failed = forward_failed(line);
        if failed {
            seen.store(true, Ordering::SeqCst);
        }
        !failed
    });
    let shown = t.url.shown();
    let open_line = |window: bool| {
        let closes = match window {
            true => "closing its window there, or Ctrl-C here, closes it",
            false => "Ctrl-C closes it",
        };
        match port == t.url.port() {
            true => format!("{shown} is open on {} through a tunnel — {closes}", t.host),
            false => format!(
                "{shown} is open on {} as {shown_there} through a tunnel (port {} could not be \
                 forwarded there) — {closes}",
                t.host,
                t.url.port()
            ),
        }
    };
    let mut heard = Heard::default();
    let stopped = |signal: i32, is_open: bool| -> Attempt {
        if is_open {
            Attempt::Done(Ok(0))
        } else {
            let _ = writeln!(
                std::io::stderr(),
                "sopen: stopped before {shown} opened on {}",
                t.host
            );
            Attempt::Done(Ok(128u8.saturating_add(u8::try_from(signal).unwrap_or(0))))
        }
    };
    // HOST's window was closed (09 §7): said once the tunnel is closed, and the run is over.
    let window_closed = |say: &mut dyn FnMut(&str)| -> Attempt {
        say(&format!(
            "{shown}: its window on {} was closed; the tunnel is closed",
            t.host
        ));
        Attempt::Done(Ok(0))
    };
    let mut out = Some(out);
    // When ssh's output ended (HOST's command is over), and ssh still runs.
    let mut session_over: Option<std::time::Instant> = None;
    loop {
        if let Some(signal) = stop() {
            input.take();
            end(&mut child);
            ended(out.take(), err);
            return Ok(stopped(signal, heard.open));
        }
        if let Some(status) = child.try_wait()? {
            // A watcher left behind (the guard killed by a signal) ends ssh at this end.
            input.take();
            ended(out.take(), err);
            // A stop during the drain comes first, as before 09: an open cue not read yet is
            // not said, and the run was stopped before the address opened.
            if let Some(signal) = stop() {
                return Ok(stopped(signal, heard.open));
            }
            // Only now, the relay drained, are HOST's last cues all in: a closed cue counts
            // whether it came before ssh's exit or with it.
            heard.take(&cues, &open_line, say);
            if heard.closed {
                return Ok(window_closed(say));
            }
            if !heard.open && taken.load(Ordering::SeqCst) && status.code() == Some(255) {
                return Ok(Attempt::Taken);
            }
            return Ok(Attempt::Done(exit_code(t.host, status)));
        }
        heard.take(&cues, &open_line, say);
        // HOST's window was closed while ssh runs: the tunnel is closed at once, whether ssh
        // would stay (for the browser's connections) or not.
        if heard.closed {
            input.take();
            end(&mut child);
            ended(out.take(), err);
            return Ok(window_closed(say));
        }
        if out.as_ref().is_some_and(|o| o.try_recv().is_ok()) {
            out = None;
            session_over = Some(std::time::Instant::now());
        }
        // HOST's command is over but ssh stays — for the browser's forwarded connections, which
        // would keep serving the address: the tunnel is closed under it.
        if session_over.is_some_and(|at| at.elapsed() >= SESSION_GRACE) {
            input.take();
            end(&mut child);
            ended(None, err);
            heard.take(&cues, &open_line, say);
            bail!(
                "sopen on {} ended while ssh stayed for the browser's connections; the tunnel \
                 to {shown} is closed",
                t.host
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Ends the guard whose input was just closed: its watcher kills ssh and it exits with ssh;
/// one that has not within 3 s is killed (its watcher still ends ssh at the input's end).
fn end(child: &mut std::process::Child) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Waits at most 2 s for each relay still running to reach its pipe's end (a descendant of ssh,
/// such as a ProxyCommand, may still hold it).
fn ended(out: Option<std::sync::mpsc::Receiver<()>>, err: std::sync::mpsc::Receiver<()>) {
    for done in out.into_iter().chain([err]) {
        let _ = done.recv_timeout(std::time::Duration::from_secs(2));
    }
}

/// Copies `pipe` to `sink()` line by line, each line `keep` lets through; the receiver gets
/// `()` at the pipe's end.
fn relay<W: std::io::Write + 'static>(
    pipe: Option<impl std::io::Read + Send + 'static>,
    sink: fn() -> W,
    mut keep: impl FnMut(&str) -> bool + Send + 'static,
) -> std::sync::mpsc::Receiver<()> {
    use std::io::BufRead;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Some(pipe) = pipe {
            let mut pipe = std::io::BufReader::new(pipe);
            let mut line = Vec::new();
            while matches!(pipe.read_until(b'\n', &mut line), Ok(n) if n > 0) {
                if keep(&String::from_utf8_lossy(&line)) {
                    let mut sink = sink();
                    let _ = sink.write_all(&line);
                    let _ = sink.flush();
                }
                line.clear();
            }
        }
        let _ = tx.send(());
    });
    rx
}

// HOST's half: the tunnel's port must be loopback only there. sshd binds the `-R` to the
// loopback address asked for, unless its `GatewayPorts yes` binds it to every interface —
// then anyone who reaches HOST would reach this machine's page. HOST's sopen checks what
// listens on the port before it opens the browser, and refuses.

/// The address of each listening TCP socket on `port` in `table` — rows of Linux's
/// `/proc/net/tcp` / `tcp6`, or lines of macOS's `netstat -anv -p tcp`; anything else is
/// skipped. `*` (netstat's any address) is `0.0.0.0`.
pub fn listeners_on(table: &str, port: u16) -> Vec<std::net::IpAddr> {
    table
        .lines()
        .filter_map(|line| proc_listener(line).or_else(|| netstat_listener(line)))
        .filter(|&(_, p)| p == port)
        .map(|(ip, _)| ip)
        .collect()
}

/// A `/proc/net/tcp{,6}` row in state LISTEN (`0A`): its local address and port. The address
/// is the kernel's 32-bit words as hex, each in this machine's byte order.
fn proc_listener(line: &str) -> Option<(std::net::IpAddr, u16)> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [slot, local, _, state, ..] = fields.as_slice() else {
        return None;
    };
    if !slot.ends_with(':') || *state != "0A" {
        return None;
    }
    let (addr, port) = local.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let words: Vec<[u8; 4]> = (0..addr.len() / 8)
        .map(|i| u32::from_str_radix(addr.get(i * 8..i * 8 + 8)?, 16).ok())
        .collect::<Option<Vec<u32>>>()?
        .into_iter()
        .map(u32::to_ne_bytes)
        .collect();
    let ip = match (addr.len(), words.as_slice()) {
        (8, [w]) => std::net::IpAddr::from(*w),
        (32, [a, b, c, d]) => {
            let mut bytes = [0u8; 16];
            for (i, w) in [a, b, c, d].iter().enumerate() {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&w[..]);
            }
            std::net::IpAddr::from(bytes)
        }
        _ => return None,
    };
    Some((ip, port))
}

/// A `netstat -anv -p tcp` line (macOS) in state LISTEN: its local address (`addr.port`, `*`
/// for any, an IPv6 one maybe with `%zone`) and port.
fn netstat_listener(line: &str) -> Option<(std::net::IpAddr, u16)> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [proto, _, _, local, _, state, ..] = fields.as_slice() else {
        return None;
    };
    if !proto.starts_with("tcp") || *state != "LISTEN" {
        return None;
    }
    let (addr, port) = local.rsplit_once('.')?;
    let port = port.parse().ok()?;
    let ip = match addr {
        "*" => std::net::IpAddr::from([0u8; 4]),
        addr => addr.split('%').next()?.parse().ok()?,
    };
    Some((ip, port))
}

/// Whether an address a socket listens on is one the tunnel's own bind can be: `127.0.0.1`,
/// `::1`, or `::ffff:127.0.0.1`.
fn loopback_only(ip: std::net::IpAddr) -> bool {
    let v4 = std::net::Ipv4Addr::LOCALHOST;
    match ip {
        std::net::IpAddr::V4(ip) => ip == v4,
        std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.to_ipv4_mapped() == Some(v4),
    }
}

/// `Ok` only on positive evidence that the tunnel's port `port` is private on `here` (this
/// machine's name, for the message): `table` has at least one listener on it — the tunnel's
/// own, which sshd opened before this command started — and every one is `127.0.0.1`, `::1` or
/// `::ffff:127.0.0.1`. No listener seen (an empty table, none on the port, output that does not
/// parse) refuses as well: it cannot confirm the port is private.
pub fn private_in(table: &str, port: u16, here: &str) -> Result<()> {
    let listeners = listeners_on(table, port);
    if listeners.is_empty() {
        bail!(
            "cannot confirm the tunnel is private on {here} (no listener seen on port {port}); \
             refused, nothing was opened"
        );
    }
    match listeners.into_iter().find(|&ip| !loopback_only(ip)) {
        None => Ok(()),
        Some(ip) => bail!(
            "the tunnel's port {port} listens on {ip} on {here}, open to the network (sshd's \
             GatewayPorts yes binds a forward to every interface, or another program holds the \
             port); refused, nothing was opened"
        ),
    }
}

/// This machine's listening TCP sockets: `SOPEN_LISTENERS` (a receipt hook: a file holding such
/// a table) when set and not empty; else `/proc/net/tcp` and `/proc/net/tcp6` on Linux,
/// `netstat -anv -p tcp` elsewhere. Both list every user's sockets — the tunnel's belongs to
/// sshd: `/proc/net/tcp*` is the network namespace's whole table, with a uid column (proc(5)),
/// and macOS's netstat shows root's too (launchd's `*.22`). A table that cannot be read is an
/// error: the check fails closed.
fn listener_table() -> Result<String> {
    if let Some(file) = std::env::var_os("SOPEN_LISTENERS").filter(|f| !f.is_empty()) {
        return std::fs::read_to_string(&file).with_context(|| {
            format!(
                "cannot read SOPEN_LISTENERS ({})",
                Path::new(&file).display()
            )
        });
    }
    if cfg!(target_os = "linux") {
        let mut table = std::fs::read_to_string("/proc/net/tcp")
            .context("cannot read /proc/net/tcp to check the tunnel's port")?;
        match std::fs::read_to_string("/proc/net/tcp6") {
            Ok(v6) => table.push_str(&v6),
            // No IPv6 here: nothing listens on it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).context("cannot read /proc/net/tcp6 to check the tunnel's port")
            }
        }
        return Ok(table);
    }
    let out = Command::new("/usr/sbin/netstat")
        .args(["-anv", "-p", "tcp"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .context("cannot run netstat to check the tunnel's port")?;
    if !out.status.success() {
        bail!("netstat failed ({}) checking the tunnel's port", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// HOST's half of a tunnel, before the browser opens: `Ok` only when the tunnel's port `port`
/// is seen listening here on loopback alone ([`private_in`]); `Err` (refused) otherwise, or when
/// the table cannot be read.
pub fn tunnel_port_is_private(port: u16) -> Result<()> {
    let here = hostname().unwrap_or_else(|| "this machine".to_string());
    private_in(&listener_table()?, port, &here)
}

/// `sopen URL` with HOST: serves the loopback address `url` from this machine on HOST through a
/// tunnel and opens it in HOST's browser (`browser`: `--browser`, passed on); returns once
/// Ctrl-C or SIGTERM ends it, or HOST's window is closed (0), or ssh ends (HOST's exit code, as
/// [`run`]).
pub fn tunnel(host: &str, url: &crate::url::Url, browser: bool, state_dir: &Path) -> Result<u8> {
    use std::hash::BuildHasher;
    let env = forwarded_env(|key| std::env::var_os(key))?;
    let tunnel = Tunnel {
        ssh: std::ffi::OsStr::new("ssh"),
        connection: tunnel_connection(&crate::remote::connection_args(host, state_dir)?),
        host,
        url,
        browser,
        env: &env,
        install_paths: INSTALL_PATHS,
    };
    let seed = std::collections::hash_map::RandomState::new();
    let ports = tunnel_ports(url.port(), TUNNEL_TRIES, |i| seed.hash_one(i));
    let watch = signals::Watch::start();
    // This side's lines go out as HOST's plain ones do ([`relay`]), a write error ignored —
    // never `println!`, which panics once the output is gone (`| head -1`): the tunnel then
    // lives on until Ctrl-C or HOST's end, as HOST's half does with its cues (09 §7).
    run_tunnel(&tunnel, &ports, &|| watch.signal(), &mut |line| {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    })
}

/// Ctrl-C and SIGTERM while a tunnel runs: the handler only records the signal, and the tunnel
/// closes on its next poll. Installed for one delivery (`SA_RESETHAND`): the same signal again
/// ends sopen at once — its pipe closes, and the guard ends ssh. Put back when the watch ends.
mod signals {
    use std::sync::atomic::{AtomicI32, Ordering};

    static SIGNAL: AtomicI32 = AtomicI32::new(0);
    const SIGNALS: [libc::c_int; 2] = [libc::SIGINT, libc::SIGTERM];

    extern "C" fn record(signal: libc::c_int) {
        let _ = SIGNAL.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub(super) struct Watch {
        before: [Option<libc::sigaction>; 2],
    }

    impl Watch {
        pub(super) fn start() -> Self {
            SIGNAL.store(0, Ordering::SeqCst);
            let mut before = [None; 2];
            for (signal, before) in SIGNALS.into_iter().zip(&mut before) {
                // SAFETY: whole `sigaction`s; the handler only stores into an atomic. An
                // ignored signal (`nohup`, a background job without job control) stays ignored.
                unsafe {
                    let mut old: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(signal, std::ptr::null(), &mut old) != 0
                        || old.sa_sigaction == libc::SIG_IGN
                    {
                        continue;
                    }
                    let mut new: libc::sigaction = std::mem::zeroed();
                    new.sa_sigaction = record as extern "C" fn(libc::c_int) as libc::sighandler_t;
                    libc::sigemptyset(&mut new.sa_mask);
                    new.sa_flags = libc::SA_RESTART | libc::SA_RESETHAND;
                    if libc::sigaction(signal, &new, std::ptr::null_mut()) == 0 {
                        *before = Some(old);
                    }
                }
            }
            Self { before }
        }

        /// The first signal since the start, if any.
        pub(super) fn signal(&self) -> Option<i32> {
            Some(SIGNAL.load(Ordering::SeqCst)).filter(|&s| s != 0)
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            for (signal, before) in SIGNALS.into_iter().zip(&self.before) {
                if let Some(before) = before {
                    // SAFETY: the disposition found at the start.
                    unsafe { libc::sigaction(signal, before, std::ptr::null_mut()) };
                }
            }
        }
    }
}

/// HOST's exit code is ours: 0, 1 and 3 from sopen, 2 from an older sopen that does not know
/// `--here` (a usage error; one that does not know `--gui` gets the run again without it first,
/// [`run`]), 127 for no sopen there. ssh's own failure (255) is "could not
/// reach". sopen itself exits with nothing above 3, so a status above 128 is most likely a
/// signal (128 + n) — said as a guess, with the status as it came.
fn exit_code(host: &str, status: ExitStatus) -> Result<u8> {
    match status.code() {
        Some(255) => bail!("could not reach {host}"),
        Some(code @ 129..=254) => bail!(
            "sopen on {host} ended with status {code} (signal {}?)",
            code - 128
        ),
        Some(code) => Ok(u8::try_from(code).unwrap_or(1)),
        None => bail!("ssh to {host} ended without an exit code ({status})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn hosts() {
        for ok in ["box", "me@box", "a@b@box", "box.lan"] {
            assert_eq!(check_host(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "-oProxyCommand=x",
            "a box",
            "box\n",
            "box:",
            "box:/x",
            "::1",
            "me@",
            "@box",
        ] {
            assert!(check_host(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn precedence() {
        type Read = Result<Option<String>>;
        let none = || -> Read { Ok(None) };
        let host = |h: &'static str| move || -> Read { Ok(Some(h.to_string())) };
        let never_env = || -> Read { panic!("SOPEN_TARGET was read") };
        let never = || -> Read { panic!("the config was read") };
        let pick = |here, flag, env: &dyn Fn() -> Read, cfg: &dyn Fn() -> Read| {
            choose(here, flag, env, cfg).map(|h| h.unwrap_or_else(|| "here".to_string()))
        };
        assert_eq!(pick(true, None, &never_env, &never).unwrap(), "here");
        assert_eq!(pick(false, Some("f"), &never_env, &never).unwrap(), "f");
        assert_eq!(pick(false, None, &host("e"), &never).unwrap(), "e");
        assert_eq!(pick(false, None, &host(""), &host("c")).unwrap(), "c");
        assert_eq!(pick(false, None, &none, &host("c")).unwrap(), "c");
        assert_eq!(pick(false, None, &none, &none).unwrap(), "here");
        // A broken config or SOPEN_TARGET only matters when it is consulted.
        let broken = || -> Read { Err(anyhow!("/c/config.toml: line 1: bad")) };
        assert!(pick(false, None, &none, &broken).is_err());
        let bad_env = || -> Read { Err(anyhow!("SOPEN_TARGET is not valid UTF-8")) };
        assert!(pick(false, None, &bad_env, &none).is_err());
        assert_eq!(pick(true, None, &bad_env, &broken).unwrap(), "here");
        assert_eq!(pick(false, Some("f"), &bad_env, &broken).unwrap(), "f");
        let e = pick(false, Some("-x"), &none, &none)
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("--target: "), "{e}");
        let e = pick(false, None, &host("a b"), &none)
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("SOPEN_TARGET: "), "{e}");
    }

    #[test]
    fn this_machine() {
        assert_eq!(short_name("Box.local").as_deref(), Some("Box"));
        assert_eq!(short_name("box").as_deref(), Some("box"));
        assert_eq!(short_name("box.lan.").as_deref(), Some("box"));
        assert_eq!(short_name(".x"), None);
        assert_eq!(short_name(""), None);

        // This machine is `Box.lan`: its full name or its short name, any case, trailing dot.
        let full = Some("Box.lan");
        for here in ["box", "me@BOX", "box.LAN", "box.lan.", "box."] {
            assert!(is_here(here, full), "{here}");
        }
        for elsewhere in ["box.other", "box.lan.example", "boxer", "lan", "other"] {
            assert!(!is_here(elsewhere, full), "{elsewhere}");
        }
        // Named just `box`: there is no domain to compare `box.lan` with, so it is forwarded.
        assert!(is_here("box", Some("box")));
        assert!(!is_here("box.lan", Some("box")));
        assert!(!is_here("box", None));

        // Loopback is here: `localhost`, names under `.localhost`, 127/8 — nothing else.
        for here in [
            "localhost",
            "me@LocalHost",
            "localhost.",
            "a.localhost",
            "me@x.LOCALHOST",
            "127.0.0.1",
            "me@127.1.2.3",
            // The /etc/hosts aliases, whole, any case, trailing dot.
            "localhost.localdomain",
            "LocalHost.LocalDomain.",
            "localhost6",
            "me@localhost6.localdomain6",
            "IP6-LOCALHOST",
            "ip6-loopback.",
        ] {
            assert!(is_here(here, None), "{here}");
        }
        for elsewhere in [
            "localhost.example.com",
            "mylocalhost",
            "localhostx",
            "localhost6x",
            "a.localhost6",
            "ip6-loopback.example.com",
            "localhost.localdomain.example.com",
        ] {
            assert!(!is_here(elsewhere, None), "{elsewhere}");
        }
        // An IP literal is compared whole, never cut at a dot.
        // (192.0.2.0/24 is the range reserved for documentation, RFC 5737.)
        assert!(!is_here("192.0.2.7", Some("192")));
        assert_eq!(short_name("192.0.2.7").as_deref(), Some("192.0.2.7"));

        // The real machine.
        let mine = hostname().expect("this machine has a host name");
        let short = short_hostname().expect("this machine has a short host name");
        // As normalised: an IP literal stays whole (a runner may be named by one), a name is
        // cut at its first dot.
        let normalised = |full: &str, short: &str| match bare(full).parse::<std::net::IpAddr>() {
            Ok(_) => short == bare(full),
            Err(_) => !short.is_empty() && !short.contains('.'),
        };
        assert!(normalised("192.0.2.7", "192.0.2.7"));
        assert!(normalised("box.lan.", "box"));
        assert!(!normalised("box.lan", "box.lan"));
        assert!(!normalised("192.0.2.7", "192"));
        assert!(normalised(&mine, &short), "{mine} -> {short}");
        assert!(is_here(&mine.to_uppercase(), Some(&mine)));
        assert!(is_here(&short, Some(&mine)));
    }

    #[test]
    fn source_as_the_target_sees_it() {
        let no_local = |p: &str| -> Result<String> { panic!("{p} is not local") };
        let cases = [
            ("box", "box:/tmp/a.html", "/tmp/a.html"),
            ("box", "box:rel/a.png", "rel/a.png"),
            ("box", "box:~/a.png", "~/a.png"),
            ("box", "box:~", "~"),
            ("box", "box:", "."),
            ("box", "box:a:b", "./a:b"),
            ("box", "box:[x]:y", "./[x]:y"),
            ("box", "me@box:/x", "me@box:/x"),
            ("me@box", "me@box:/x", "/x"),
            ("me@box", "box:/x", "/x"),
            ("me@box", "you@box:/x", "you@box:/x"),
            ("box", "box:~other/x", "box:~other/x"),
            ("box", "other:/x", "other:/x"),
            ("box", "Box:/x", "Box:/x"),
            // Loopback means this machine, not HOST: named by this machine's short name.
            ("box", "localhost:/p", "here:/p"),
            ("box", "a.localhost:/p", "here:/p"),
            ("box", "localhost.localdomain:/p", "here:/p"),
            ("box", "me@ip6-localhost:/p", "me@here:/p"),
            (
                "box",
                "localhost.example.com:/p",
                "localhost.example.com:/p",
            ),
            ("box", "me@LocalHost:rel/p", "me@here:rel/p"),
            ("box", "127.0.0.1:/p", "here:/p"),
            ("box", "127.9.9.9:/p", "here:/p"),
            ("box", "[::1]:/p", "here:/p"),
            ("box", "me@[::1]:", "me@here:"),
        ];
        for (host, source, want) in cases {
            assert_eq!(
                source_for(host, source, Some("here.lan"), no_local).unwrap(),
                want,
                "{host} {source}"
            );
        }
        let abs = |p: &str| -> Result<String> {
            assert_eq!(p, "./a b.png");
            Ok("/home/u/a b.png".to_string())
        };
        assert_eq!(
            source_for("box", "./a b.png", Some("here.lan"), abs).unwrap(),
            "here:/home/u/a b.png"
        );
        let missing =
            |p: &str| -> Result<String> { Err(anyhow!("{p}: no such file or directory")) };
        let e = source_for("box", "nope.txt", Some("here.lan"), missing).unwrap_err();
        assert_eq!(e.to_string(), "nope.txt: no such file or directory");
        assert!(source_for("box", "a.png", None, |_| Ok("/a.png".into())).is_err());
        assert!(source_for("box", "localhost:/p", None, no_local).is_err());
        assert!(source_for("box", "@box:/x", Some("here"), no_local).is_err());
    }

    #[test]
    fn hooks_travel_local_paths_stay() {
        let all = |key: &str| Some(OsString::from(format!("v-{key}")));
        let env = forwarded_env(all).unwrap();
        assert_eq!(env.len(), FORWARDED_ENV.len());
        assert_eq!(env[0], "SOPEN_STATE=v-SOPEN_STATE");
        assert!(env.contains(&"SOPEN_TARGET_BIN=v-SOPEN_TARGET_BIN".to_string()));
        assert!(env.contains(&"SOPEN_LIST=v-SOPEN_LIST".to_string()));
        // 06 §11: a `--target` run carries the stream's hooks to HOST as it carries the others.
        assert!(env.contains(&"SOPEN_STREAM_STATS=v-SOPEN_STREAM_STATS".to_string()));
        assert!(env.contains(&"SOPEN_STREAM_THROTTLE=v-SOPEN_STREAM_THROTTLE".to_string()));
        // 09 §4: the page view's engine is HOST's, and so is its off switch.
        assert!(env.contains(&"SOPEN_WEB=v-SOPEN_WEB".to_string()));
        // 07 §7: the hex window's Save As hooks travel as "Save to…"'s do — the window, and the
        // folder it saves into, are on HOST.
        for key in [
            "SOPEN_SAVE_TO",
            "SOPEN_SAVE_HOLD_MS",
            "SOPEN_SAVE_AS",
            "SOPEN_SAVE_AS_HOLD_MS",
        ] {
            assert!(env.contains(&format!("{key}=v-{key}")), "{key}");
        }
        for local in [
            "SOPEN_CODE",
            "VSCODE_EXTENSIONS",
            "SOPEN_ZED",
            "SOPEN_FFMPEG",
            "SOPEN_FFPROBE",
            "SOPEN_GIT",
            "SOPEN_REQUIRE_GIT",
            "SOPEN_CACHE_DIR",
            "SOPEN_STATE_DIR",
            "SOPEN_CONFIG",
            "SOPEN_TARGET",
        ] {
            assert!(
                !env.iter().any(|kv| kv.starts_with(&format!("{local}="))),
                "{local}"
            );
        }
        let some = |key: &str| (key == "SOPEN_LOG").then(|| OsString::from("debug"));
        assert_eq!(forwarded_env(some).unwrap(), ["SOPEN_LOG=debug"]);
        assert!(forwarded_env(|_| None).unwrap().is_empty());
    }

    #[test]
    fn quoting() {
        assert_eq!(quote(""), "''");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(r"a\b"), r"'a'\\'b'");
        let words = [
            "it's",
            "two  spaces",
            "$HOME",
            "`id`",
            "$(id)",
            r"back\slash\\",
            "-n",
            "",
            "한글 ü ✓",
            "*",
            "a;b|c&d>e",
            "\"dq\"",
            "!x",
        ];
        let quoted: Vec<String> = words.iter().map(|w| quote(w)).collect();
        let want: String = words.iter().map(|w| format!("[{w}]\n")).collect();
        for sh in login_shells() {
            let out = Command::new(&sh.path)
                .arg("-c")
                .arg(format!("printf '[%s]\\n' {}", quoted.join(" ")))
                .env_clear()
                .env("HOME", std::env::temp_dir())
                .output()
                .unwrap();
            assert!(out.status.success(), "{sh:?}");
            assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{sh:?}");
        }
    }

    #[test]
    fn ssh_command_line() {
        let connection = ["-S", "/s/cm-x", "-o", "ControlMaster=auto"]
            .map(OsString::from)
            .to_vec();
        let app = |name| Flags {
            app: Some(name),
            ..Flags::default()
        };
        let words = remote_words("box", "/tmp/a b.png", app("ImHex"), &[], INSTALL_PATHS);
        // No terminal here, `--app` waiting for Enter: no terminal there, but this side's input.
        let args = ssh_args(connection.clone(), false, true, "box", words.clone());
        let mut want = connection.clone();
        want.extend(
            [
                "-T",
                "-o",
                "RemoteCommand=none",
                "-o",
                "LogLevel=ERROR",
                "box",
                "--",
            ]
            .map(OsString::from),
        );
        want.extend(words.iter().map(OsString::from));
        assert_eq!(args, want);
        // A bare `exec` first (it replaces the login shell), then every word quoted.
        assert_eq!(words[..3], ["exec", "'sh'", "'-c'"]);
        assert_eq!(
            words[4..],
            ["'sopen'", "'--app'", "'ImHex'", "'--'", "'/tmp/a b.png'"]
        );
        // The session flags: a terminal there only with one here; no input without `--app`.
        let flags = |tty, app| {
            let args = ssh_args(connection.clone(), tty, app, "box", vec![]);
            let rest = &args[connection.len()..];
            rest[..rest.iter().position(|a| a == "-o").unwrap()].to_vec()
        };
        assert_eq!(flags(true, false), ["-t"]);
        assert_eq!(flags(true, true), ["-t"]);
        assert_eq!(flags(false, true), ["-T"]);
        assert_eq!(flags(false, false), ["-T", "-n"]);

        let env = ["SOPEN_LOG=debug".to_string()];
        let words = remote_words("box", "/x", Flags::default(), &env, INSTALL_PATHS);
        assert_eq!(words[..4], ["exec", "'env'", "'SOPEN_LOG=debug'", "'sh'"]);
        assert_eq!(words[words.len() - 3..], ["'sopen'", "'--'", "'/x'"]);
        // `--hex` travels after `--app`, before `--`.
        let hex = Flags {
            hex: true,
            ..Flags::default()
        };
        let words = remote_words("box", "/x", hex, &[], INSTALL_PATHS);
        assert_eq!(
            words[words.len() - 4..],
            ["'sopen'", "'--hex'", "'--'", "'/x'"]
        );
        let app_hex = Flags {
            hex: true,
            ..app("ImHex")
        };
        let words = remote_words("box", "/x", app_hex, &[], "");
        assert_eq!(
            words[words.len() - 6..],
            ["'sopen'", "'--app'", "'ImHex'", "'--hex'", "'--'", "'/x'"]
        );
        // `--gui` (10 §11 row 2) right after `--here`, before `--hex`: `--here --gui -- DIR`.
        let gui = Flags {
            gui: true,
            ..Flags::default()
        };
        let words = remote_words("box", "/srv/dir", gui, &[], INSTALL_PATHS);
        assert_eq!(
            words[words.len() - 4..],
            ["'sopen'", "'--gui'", "'--'", "'/srv/dir'"]
        );
        let words = remote_words("box", "/x", Flags { hex: true, ..gui }, &[], "");
        assert_eq!(
            words[words.len() - 5..],
            ["'sopen'", "'--gui'", "'--hex'", "'--'", "'/x'"]
        );
        // An address's `--browser` (09 §7) travels as given, before `--`.
        let browser = Flags {
            browser: true,
            ..Flags::default()
        };
        let words = remote_words("box", "https://x/", browser, &[], INSTALL_PATHS);
        assert_eq!(
            words[words.len() - 4..],
            ["'sopen'", "'--browser'", "'--'", "'https://x/'"]
        );

        let script = script("box", INSTALL_PATHS);
        assert!(!script.contains('\n'), "{script}");
        assert!(
            script.ends_with(r#"trap ':' INT TERM HUP; "$bin" --here "$@"; exit $?"#),
            "{script}"
        );
        for place in [
            "/opt/homebrew/bin/sopen",
            "/usr/local/bin/sopen",
            "/home/linuxbrew/.linuxbrew/bin/sopen",
            "\"$HOME/.linuxbrew/bin/sopen\"",
            "\"$HOME/.cargo/bin/sopen\"",
        ] {
            assert!(script.contains(place), "{place}");
        }
        assert!(!script.contains("eval"), "{script}");
        // The desktop session's names imported on Linux, each a whole word of the loop: the
        // display's, and the session's own (`xdg-open` picks `kde-open` by KDE_SESSION_VERSION;
        // without it, `kfmclient`, missing on Plasma 6, and a success that opened nothing).
        let keys = script
            .split_once("for key in ")
            .and_then(|(_, rest)| rest.split_once("; do"))
            .map(|(keys, _)| keys.split_whitespace().collect::<Vec<_>>())
            .unwrap_or_else(|| panic!("no session import in {script}"));
        for name in [
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XAUTHORITY",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
            "XDG_SESSION_TYPE",
            "XDG_CURRENT_DESKTOP",
            "KDE_SESSION_VERSION",
            "KDE_FULL_SESSION",
            "XDG_SESSION_DESKTOP",
            "DESKTOP_SESSION",
        ] {
            assert!(keys.contains(&name), "{name} not in {keys:?}");
        }
    }

    /// X2-switch: HOST's half of the switch is `--gui --state-stdin -- FOLDER` after the script's
    /// `--here`, over `-T` with this side's input kept (no `-n`: the state goes on it), the run
    /// options and `BatchMode=yes` — nothing asked, the terminal is the TUI's.
    #[test]
    fn the_switch_runs_hosts_half_with_its_input_and_nothing_asked() {
        let connection = ["-S", "/s/cm-x", "-o", "ControlMaster=auto"]
            .map(OsString::from)
            .to_vec();
        let env = ["SOPEN_LOG=debug".to_string()];
        let words = switch_words("box", "here:/a b", &env, INSTALL_PATHS);
        assert_eq!(words[..4], ["exec", "'env'", "'SOPEN_LOG=debug'", "'sh'"]);
        assert_eq!(
            words[words.len() - 5..],
            [
                "'sopen'",
                "'--gui'",
                "'--state-stdin'",
                "'--'",
                "'here:/a b'"
            ]
        );
        let args = switch_args(connection.clone(), "box", words.clone());
        let mut want = connection;
        want.extend(
            [
                "-T",
                "-o",
                "RemoteCommand=none",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "BatchMode=yes",
                "box",
                "--",
            ]
            .map(OsString::from),
        );
        want.extend(words.iter().map(OsString::from));
        assert_eq!(args, want);
    }

    /// X2-switch: the switch carries the hooks that say how HOST's sopen is found, logs, draws and
    /// reads, and leaves here those that script HOST's run or make it one without a window — every
    /// one of the 22 decided; the terminal's grid is the window's size (T16) unless the owner set
    /// one.
    #[test]
    fn the_switch_leaves_the_scripting_hooks_here() {
        let all = |key: &str| Some(OsString::from(format!("v-{key}")));
        let env = switch_env(all, Some([800.0, 480.0])).unwrap();
        let left: Vec<&str> = SWITCH_LEFT_HERE.iter().map(|(k, _)| *k).collect();
        for key in FORWARDED_ENV {
            let carried = env.contains(&format!("{key}=v-{key}"));
            assert_eq!(carried, !left.contains(&key), "{key}");
        }
        assert_eq!(env.len(), FORWARDED_ENV.len() - SWITCH_LEFT_HERE.len());
        assert_eq!(SWITCH_LEFT_HERE.len() + 10, FORWARDED_ENV.len());
        for (key, _) in SWITCH_LEFT_HERE {
            assert!(
                FORWARDED_ENV.contains(&key),
                "{key} is one of the forwarded"
            );
        }
        assert!(env.contains(&"SOPEN_WINDOW_SIZE=v-SOPEN_WINDOW_SIZE".to_string()));
        // The owner set none: the terminal's grid.
        let some = |key: &str| (key == "SOPEN_LOG").then(|| OsString::from("debug"));
        assert_eq!(
            switch_env(some, Some([800.0, 480.0])).unwrap(),
            ["SOPEN_LOG=debug", "SOPEN_WINDOW_SIZE=800x480"]
        );
        assert!(switch_env(|_| None, None).unwrap().is_empty());
    }

    /// X2-switch: a place HOST names comes back as this machine names it — HOST's own path as
    /// `HOST:/abs` (its label: a user kept), this machine's as its path (no user, its short or full
    /// name, any case), any other machine's unchanged.
    #[test]
    fn places_come_back_as_this_machine_names_them() {
        let back = |host: &str, place: &str| source_back(host, place, Some("here.lan")).unwrap();
        assert_eq!(back("box", "/srv/x"), "box:/srv/x");
        assert_eq!(back("me@box", "/srv/x"), "me@box:/srv/x");
        assert_eq!(back("box", "here:/home/u/a b"), "/home/u/a b");
        assert_eq!(back("box", "HERE.lan:/x"), "/x");
        assert_eq!(back("box", "me@here:/x"), "me@here:/x");
        assert_eq!(back("box", "other:/x"), "other:/x");
        assert_eq!(source_back("box", "here:/x", None).unwrap(), "here:/x");
        // A loopback place on HOST is HOST itself: never this machine.
        assert_eq!(back("box", "localhost:/y"), "box:/y");
        assert_eq!(back("box", "127.0.0.1:/y"), "box:/y");
        assert_eq!(back("box", "[::1]:/y"), "box:/y");
        assert_eq!(back("box", "me@localhost:/y"), "me@box:/y");
        assert_eq!(back("you@box", "localhost:/y"), "you@box:/y");
        assert_eq!(back("you@box", "me@localhost:/y"), "me@box:/y");
    }

    /// X2-switch's review: what HOST says is bounded — no queue grows with its output. Only the
    /// cues of its standard output are read (exactly: a leading space, the state's JSON without
    /// its space, a cue inside a longer line are none), any other line dropped, a cue split
    /// across two reads one line; of its standard error only the last line is kept, cut to
    /// [`ERR_LINE_MAX`]; a cue there is a plain line.
    #[test]
    fn what_host_says_is_bounded_and_its_cues_exact() {
        let tmp = tempfile::tempdir().unwrap();
        let ssh = tmp.path().join("ssh");
        executable(
            &ssh,
            "#!/bin/sh\n\
             i=0; while [ $i -lt 3000 ]; do echo \"line $i\" >&2; i=$((i+1)); done\n\
             echo sopen-xtools-kept >&2\n\
             echo ' sopen-xtools-kept'\n\
             echo 'sopen-xtools-state{\"v\":1}'\n\
             echo 'x sopen-xtools-kept'\n\
             head -c 2000000 /dev/zero | tr '\\000' a; echo\n\
             printf 'sopen-xtools-st'; sleep 0.2; printf 'ate {\"v\":1}\\n'\n\
             head -c 10000 /dev/zero | tr '\\000' b >&2; echo >&2\n",
        );
        // Run once now (it writes no file): a just-written stand-in can be "busy" for a moment on
        // Linux (testutil), and the switch would fail to start it.
        crate::testutil::run_fresh(&mut Command::new(&ssh));
        let switch = Switch {
            ssh: ssh.as_os_str(),
            connection: vec![],
            host: "box",
            folder: "/x",
            env: &[],
            install_paths: "",
        };
        let mut run = switch.start("{}").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut said = Vec::new();
        // Each look gives at most the last of each: a state, a kept cue, a stderr line.
        let look = |run: &SwitchRun, said: &mut Vec<SwitchSaid>| {
            let now = run.said();
            let errs = now
                .iter()
                .filter(|s| matches!(s, SwitchSaid::Err(_)))
                .count();
            assert!(
                now.len() <= 3 && errs <= 1,
                "not a queue of HOST's lines: {now:?}"
            );
            said.extend(now);
        };
        loop {
            look(&run, &mut said);
            if run.ended().unwrap().is_some() {
                look(&run, &mut said);
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the stand-in ended");
            std::thread::sleep(Duration::from_millis(5));
        }
        let states: Vec<&SwitchSaid> = (said.iter())
            .filter(|s| matches!(s, SwitchSaid::State(_)))
            .collect();
        assert_eq!(
            states,
            [&SwitchSaid::State("{\"v\":1}".into())],
            "one cue, whole"
        );
        assert!(
            !said.contains(&SwitchSaid::Kept),
            "no kept cue: {:?}",
            &said[..3]
        );
        let errs: Vec<&String> = (said.iter())
            .filter_map(|s| match s {
                SwitchSaid::Err(line) => Some(line),
                _ => None,
            })
            .collect();
        let last = errs.last().expect("its last stderr line");
        assert_eq!(last.len(), ERR_LINE_MAX, "cut");
        assert!(last.chars().all(|c| c == 'b'));
    }

    /// A reader of `prefix`, then `fill` bytes of `pad`, then `tail` — made as it is read, so a
    /// 65 MiB line costs the test no 65 MiB input.
    struct Line {
        parts: Vec<(Vec<u8>, usize)>,
    }

    impl Line {
        fn new(prefix: &[u8], pad: u8, fill: usize, tail: &[u8]) -> Line {
            Line {
                parts: vec![(prefix.to_vec(), 1), (vec![pad], fill), (tail.to_vec(), 1)],
            }
        }
    }

    impl std::io::Read for Line {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let Some((bytes, times)) = self.parts.first_mut() else {
                return Ok(0);
            };
            let mut n = 0;
            while *times > 0 && n + bytes.len() <= buf.len() {
                buf[n..n + bytes.len()].copy_from_slice(bytes);
                n += bytes.len();
                *times -= 1;
            }
            if *times == 0 {
                self.parts.remove(0);
            }
            Ok(n)
        }
    }

    /// What HOST's standard output said, read whole.
    fn heard_out(from: impl std::io::Read) -> SwitchHeard {
        let heard = std::sync::Mutex::new(SwitchHeard::default());
        hear_out(from, &heard);
        heard.into_inner().unwrap()
    }

    /// X2-switch's fix-check: a cue ended by CRLF is the cue (its `\r` taken off whether or not
    /// the reader took the `\n`).
    #[test]
    fn cues_ended_by_crlf_are_read() {
        let heard = heard_out(&b"sopen-xtools-kept\r\nsopen-xtools-state {\"v\":1}\r\n"[..]);
        assert!(heard.kept, "the kept cue");
        assert_eq!(heard.state.as_deref(), Some("{\"v\":1}"));
        assert_eq!(
            SwitchSaid::cue("sopen-xtools-kept\r"),
            Some(SwitchSaid::Kept),
            "a line its reader took the `\\n` off"
        );
    }

    /// X2-switch's fix-check: a line of HOST's output past its bound is dropped whole, never
    /// delivered cut — a valid state padded past the bound with junk after it is no state, and is
    /// said too long; a state line has a bound of its own (a real state past 1 MiB — select-all in
    /// a folder of tens of thousands — is read), past which it is dropped too.
    #[test]
    fn a_line_past_its_bound_is_dropped_whole() {
        let state = b"sopen-xtools-state {\"v\":1}";
        let padded = Line::new(state, b' ', STATE_LINE_MAX, b"junk\n");
        let heard = heard_out(padded);
        let cut = heard.state.as_ref().map(String::len);
        assert_eq!(
            cut, None,
            "the valid start of a line past its bound is no state"
        );
        assert!(heard.too_long, "said too long");
        // 2 MiB of a real state: read.
        let big = Line::new(
            b"sopen-xtools-state {\"v\":1,\"p\":\"",
            b'a',
            2 << 20,
            b"\"}\n",
        );
        let heard = heard_out(big);
        assert_eq!(
            heard.state.map(|s| s.len()),
            Some((2 << 20) + 14),
            "read whole"
        );
        assert!(!heard.too_long);
        // 65 MiB: past the state's bound — dropped, said too long.
        let huge = Line::new(
            b"sopen-xtools-state {\"v\":1,\"p\":\"",
            b'a',
            65 << 20,
            b"\"}\n",
        );
        let heard = heard_out(huge);
        assert_eq!(heard.state.map(|s| s.len()), None);
        assert!(heard.too_long);
        // Any other line past the small bound: dropped whole, nothing said of it.
        let other = Line::new(b"sopen-xtools-kept", b' ', OUT_LINE_MAX, b"\n");
        let heard = heard_out(other);
        assert!(!heard.kept && !heard.too_long);
    }

    #[test]
    fn exit_codes() {
        use std::os::unix::process::ExitStatusExt;
        let code = |c: i32| exit_code("box", ExitStatus::from_raw(c << 8));
        assert_eq!(code(0).unwrap(), 0);
        assert_eq!(code(3).unwrap(), 3);
        // An older sopen on HOST without `--here`: its usage error, as it is.
        assert_eq!(code(2).unwrap(), 2);
        assert_eq!(code(127).unwrap(), 127);
        assert_eq!(code(255).unwrap_err().to_string(), "could not reach box");
        assert_eq!(
            code(130).unwrap_err().to_string(),
            "sopen on box ended with status 130 (signal 2?)"
        );
        assert_eq!(
            code(143).unwrap_err().to_string(),
            "sopen on box ended with status 143 (signal 15?)"
        );
        assert!(exit_code("box", ExitStatus::from_raw(9)).is_err());
    }

    /// X2-gui: a sopen on HOST older than `--gui` refuses it as clap refuses any argument it does
    /// not know. The line is the locked clap's own, rendered from a command without `--gui` —
    /// plain, as `ssh -T` passes HOST's stderr, and coloured with `\r\n`, as a terminal there
    /// (`ssh -t`) gets it — and counts only as the first line that is not blank, in chunks of any
    /// size: after another line it is not parse-time evidence. The same line for another flag,
    /// or not at a line's start, is not it; and the answer is an older sopen only with exit 2.
    #[test]
    fn hosts_unknown_gui_line_counts_only_as_its_first() {
        let before = clap::Command::new("sopen")
            .arg(
                clap::Arg::new("here")
                    .long("here")
                    .action(clap::ArgAction::SetTrue),
            )
            .arg(clap::Arg::new("source"));
        let refused = |args: &[&str]| before.clone().try_get_matches_from(args).unwrap_err();
        let e = refused(&["sopen", "--here", "--gui", "--", "/srv/dir"]);
        assert_eq!(e.kind(), clap::error::ErrorKind::UnknownArgument);
        assert_eq!(e.exit_code(), 2);
        let plain = e.render().to_string();
        let coloured = e.render().ansi().to_string().replace('\n', "\r\n");
        assert!(coloured.contains('\x1b'), "{coloured:?}");
        let first = |bytes: &[u8], chunk: usize| {
            let mut scan = Scan::default();
            for part in bytes.chunks(chunk) {
                scan.take(part);
            }
            scan.end();
            scan.seen.first == Some(true)
        };
        for text in [&plain, &coloured] {
            for chunk in [1, 7, 4096] {
                assert!(first(text.as_bytes(), chunk), "{text:?} by {chunk}");
                let blank_first = format!("\n \r\n\x1b[0m\n{text}");
                assert!(first(blank_first.as_bytes(), chunk), "{blank_first:?}");
                let after = format!("fetching a.png\n{text}");
                assert!(!first(after.as_bytes(), chunk), "{after:?} by {chunk}");
                let progress = format!("progress 10%\r{text}");
                assert!(!first(progress.as_bytes(), chunk), "{progress:?}");
            }
        }
        let other = refused(&["sopen", "--here", "--state-stdin", "--", "/x"]);
        let other = other.render().to_string();
        assert!(!first(other.as_bytes(), 4096), "{other}");
        assert!(!first(
            b"note: error: unexpected argument '--gui' found\n",
            9
        ));
        assert!(!first(b"error: unexpected argument '--guide' found\n", 9));
        assert!(!first(b"", 1));
        assert!(older(Some(2), true));
        for (code, refused) in [
            (Some(2), false),
            (Some(1), true),
            (Some(0), true),
            (None, true),
        ] {
            assert!(!older(code, refused), "{code:?} {refused}");
        }
    }

    /// `watched` reads the right stream on each side of `tty` (X2-gui): under `-t` ssh's stdout
    /// (HOST's terminal takes both of its streams), under `-T` ssh's stderr with its stdout
    /// empty. Swapping the two arms, or taking a line on HOST's stdout under `-T` for a refusal,
    /// fails here.
    #[test]
    fn watched_reads_hosts_stderr_where_it_comes() {
        let sink: Sink = || Box::new(std::io::sink());
        let clap = r#"printf "error: unexpected argument '--gui' found\r\n""#;
        let run = |script: String, tty: bool| {
            let mut sh = Command::new("/bin/sh");
            sh.arg("-c").arg(script).stdin(std::process::Stdio::null());
            if tty {
                // Not watched under `-t`: kept off the test's own stderr.
                sh.stderr(std::process::Stdio::null());
            }
            let (status, refused) = watched(sh, tty, sink, sink).unwrap();
            assert_eq!(status.code(), Some(2));
            refused
        };
        assert!(run(format!("{clap}; exit 2"), true));
        assert!(!run(format!("{clap} >&2; exit 2"), true));
        assert!(run(format!("{clap} >&2; exit 2"), false));
        assert!(!run(format!("{clap}; exit 2"), false));
        assert!(!run(
            format!("printf 'did it\\n'; {clap} >&2; exit 2"),
            false
        ));
        assert!(!run(format!("printf 'did it\\n'; {clap}; exit 2"), true));
    }

    /// What reached this side's sink of one of HOST's streams in
    /// [`hosts_bytes_pass_while_it_waits`].
    static GOT: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

    struct Got;

    impl std::io::Write for Got {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            GOT.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// HOST's bytes reach this side the moment they are read, on both branches of `watched`: a
    /// terminal query (`ESC[6n`, the cursor's place) that a program there writes and then waits
    /// on is passed on while it waits — never held for a line's end, which would never come
    /// (the X2-gui fix-check, 2026-11-26).
    #[test]
    fn hosts_bytes_pass_while_it_waits() {
        let tmp = tempfile::tempdir().unwrap();
        let got: Sink = || Box::new(Got);
        let sink: Sink = || Box::new(std::io::sink());
        // (tty, where the query goes, the sinks of ssh's stdout and stderr)
        for (tty, to, out, err) in [
            (true, "", got, sink),
            (false, " >&2", sink, got),
            (false, "", got, sink),
        ] {
            GOT.lock().unwrap().clear();
            let done = tmp.path().join(format!("done-{tty}-{}", to.len()));
            let mut sh = Command::new("/bin/sh");
            sh.arg("-c")
                .arg(format!(
                    r#"printf '\033[6n'{to}; while [ ! -e "$1" ]; do sleep 0.05; done"#
                ))
                .arg("sh")
                .arg(&done)
                .stdin(std::process::Stdio::null());
            if tty {
                sh.stderr(std::process::Stdio::null());
            }
            let run = std::thread::spawn(move || watched(sh, tty, out, err).unwrap());
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let passed = loop {
                if GOT.lock().unwrap().as_slice() == b"\x1b[6n" {
                    break true;
                }
                if std::time::Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            std::fs::write(&done, "").unwrap();
            let (status, refused) = run.join().unwrap();
            assert!(passed, "tty={tty}{to}: the query was held");
            assert_eq!(status.code(), Some(0));
            assert!(!refused);
        }
    }

    // -----------------------------------------------------------------------------------------
    // `sopen URL` through a tunnel. The ssh run is a stand-in: a script that records its
    // arguments, plays "port taken" for one port, and stands for HOST's sopen by printing the
    // opened line.

    fn address(arg: &str) -> crate::url::Url {
        crate::url::parse(arg).unwrap().unwrap()
    }

    #[test]
    fn the_tunnel_has_its_own_connection_and_holds_this_sides_input() {
        let master: Vec<OsString> = [
            "-S",
            "/s/cm-x",
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=60s",
            "-o",
            "ServerAliveInterval=10",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ConnectTimeout=15",
        ]
        .map(OsString::from)
        .to_vec();
        let connection = tunnel_connection(&master);
        assert_eq!(
            connection,
            [
                "-S",
                "none",
                "-o",
                "ControlMaster=no",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=10",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "ConnectTimeout=15",
            ]
            .map(OsString::from)
        );
        // A ConnectTimeout from ~/.ssh/config: the master's options leave it to the config.
        assert!(!tunnel_connection(&master[..master.len() - 2])
            .iter()
            .any(|a| a.to_string_lossy().starts_with("ConnectTimeout")));

        let url = address("http://127.0.0.1:1234/a b?x#y".replace(' ', "%20").as_str());
        let words = tunnel_words("box", url.as_str(), false, &[], INSTALL_PATHS);
        let args = tunnel_args(
            connection.clone(),
            "box",
            &url.remote_forward(1234),
            words.clone(),
        );
        let mut want = connection.clone();
        want.extend(
            [
                "-T",
                "-R",
                "127.0.0.1:1234:127.0.0.1:1234",
                "-o",
                "RemoteCommand=none",
                "-o",
                "LogLevel=ERROR",
                "box",
                "--",
            ]
            .map(OsString::from),
        );
        want.extend(words.iter().map(OsString::from));
        assert_eq!(args, want);
        // This side's input reaches HOST's sopen: no `-n`, and no terminal.
        assert!(!args.iter().any(|a| a == "-n" || a == "-t"), "{args:?}");
        // HOST's sopen holds the address open until its input ends.
        assert_eq!(
            words[words.len() - 4..],
            [
                "'sopen'",
                "'--hold-until-eof'",
                "'--'",
                "'http://127.0.0.1:1234/a%20b?x#y'"
            ]
        );
        // `--browser` (09 §7) goes to HOST's half with it.
        let words = tunnel_words("box", url.as_str(), true, &[], INSTALL_PATHS);
        assert_eq!(
            words[words.len() - 5..],
            [
                "'sopen'",
                "'--hold-until-eof'",
                "'--browser'",
                "'--'",
                "'http://127.0.0.1:1234/a%20b?x#y'"
            ]
        );
    }

    #[test]
    fn the_tunnel_tries_the_addresss_port_first_then_high_ones() {
        let mut next = 0u64;
        let ports = tunnel_ports(1234, 4, |_| {
            next += 1;
            next * 7919
        });
        assert_eq!(ports.len(), 5);
        assert_eq!(ports[0], 1234);
        for (i, port) in ports[1..].iter().enumerate() {
            assert!((49152..=65535).contains(port), "{port}");
            assert!(!ports[..i + 1].contains(port), "{port} twice in {ports:?}");
        }
        // A random source that repeats itself still gives different ports, bounded.
        let ports = tunnel_ports(50000, 3, |_| 50000 - 49152);
        assert_eq!(ports.len(), 4);
        assert_eq!(ports[0], 50000);
        let mut sorted = ports.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "{ports:?}");
        assert!(forward_failed(
            "Error: remote port forwarding failed for listen port 1234"
        ));
        assert!(!forward_failed(
            "ssh: connect to host box port 22: Connection refused"
        ));
    }

    /// A stand-in for the tunnel's ssh in `dir`: it appends its arguments to `calls` (one call
    /// a block, ended by `---`) and `<its pid> <its parent's, the guard's>` to `pids`; a `-R` on
    /// port `taken` fails as ssh does (`ExitOnForwardFailure`: the error line, exit 255);
    /// otherwise it prints [`crate::url::TUNNEL_OPEN`], as HOST's sopen does, and then runs
    /// `then`.
    fn stand_in_ssh(dir: &Path, taken: u16, then: &str) -> PathBuf {
        stand_in_host(
            dir,
            taken,
            &format!("echo {}\n{then}", crate::url::TUNNEL_OPEN),
        )
    }

    /// [`stand_in_ssh`] whose HOST half runs `script` (shell lines) in place of the open cue and
    /// `then`: a HOST sopen of any version, the cues of 09 §7 included.
    fn stand_in_host(dir: &Path, taken: u16, script: &str) -> PathBuf {
        let ssh = dir.join("ssh");
        let file = |name: &str| quote(dir.join(name).to_str().unwrap());
        executable(
            &ssh,
            &format!(
                "#!/bin/sh\n\
                 [ \"$1\" = warm ] && exit 0\n\
                 printf '%s %s\\n' \"$$\" \"$PPID\" >> {pids}\n\
                 for a in \"$@\"; do printf '%s\\n' \"$a\"; done >> {calls}\n\
                 echo --- >> {calls}\n\
                 fwd=; prev=; for a in \"$@\"; do if [ \"$prev\" = -R ]; then fwd=$a; fi; prev=$a; done\n\
                 case $fwd in 127.0.0.1:{taken}:*) echo 'Error: remote port forwarding failed for listen port {taken}' >&2; exit 255;; esac\n\
                 {script}\n",
                pids = file("pids"),
                calls = file("calls"),
            ),
        );
        // Run once now (`warm` ends it before it records anything): a just-written stand-in can
        // be "busy" for a moment on Linux (testutil), and the tunnel would fail to start it.
        crate::testutil::run_fresh(Command::new(&ssh).arg("warm"));
        ssh
    }

    /// The calls the stand-in recorded, each its arguments.
    fn calls(dir: &Path) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(dir.join("calls")).unwrap_or_default();
        text.split("---\n")
            .filter(|c| !c.is_empty())
            .map(|c| c.lines().map(str::to_string).collect())
            .collect()
    }

    /// The `-R` value of a call.
    fn forward_of(call: &[String]) -> &str {
        let at = call.iter().position(|a| a == "-R").expect("a -R");
        &call[at + 1]
    }

    fn tunnel_for<'a>(ssh: &'a Path, url: &'a crate::url::Url) -> Tunnel<'a> {
        Tunnel {
            ssh: ssh.as_os_str(),
            connection: tunnel_connection(&[]),
            host: "box",
            url,
            // As the tests of `sopen URL` run it (09 §7).
            browser: true,
            env: &[],
            install_paths: INSTALL_PATHS,
        }
    }

    #[test]
    fn a_taken_port_is_tried_again_on_a_high_one() {
        let tmp = tempfile::tempdir().unwrap();
        let ssh = stand_in_ssh(tmp.path(), 1234, "exit 0");
        let url = address("http://127.0.0.1:1234/a?b#c");
        let mut said = Vec::new();
        let code = run_tunnel(
            &tunnel_for(&ssh, &url),
            &[1234, 50001, 50002],
            &|| None,
            &mut |line| said.push(line.to_string()),
        )
        .unwrap();
        assert_eq!(code, 0);
        let made = calls(tmp.path());
        assert_eq!(made.len(), 2, "{made:#?}");
        assert_eq!(forward_of(&made[0]), "127.0.0.1:1234:127.0.0.1:1234");
        assert_eq!(made[0].last().unwrap(), "'http://127.0.0.1:1234/a?b#c'");
        // The next port, the address rewritten to it, the origin's path kept.
        assert_eq!(forward_of(&made[1]), "127.0.0.1:50001:127.0.0.1:1234");
        assert_eq!(made[1].last().unwrap(), "'http://127.0.0.1:50001/a?b#c'");
        // Said without the query or fragment (they can be secrets).
        assert_eq!(
            said,
            [
                "http://127.0.0.1:1234/a?…#… is open on box as http://127.0.0.1:50001/a?…#… \
              through a tunnel (port 1234 could not be forwarded there) — Ctrl-C closes it"
            ]
        );

        // Its own port free: the address as written, said so.
        let tmp = tempfile::tempdir().unwrap();
        let ssh = stand_in_ssh(tmp.path(), 1, "exit 0");
        let mut said = Vec::new();
        let code = run_tunnel(
            &tunnel_for(&ssh, &url),
            &[1234, 50001],
            &|| None,
            &mut |l| said.push(l.to_string()),
        )
        .unwrap();
        assert_eq!(code, 0);
        assert_eq!(calls(tmp.path()).len(), 1);
        assert_eq!(
            said,
            ["http://127.0.0.1:1234/a?…#… is open on box through a tunnel — Ctrl-C closes it"]
        );
    }

    #[test]
    fn only_a_taken_port_is_retried_and_the_tries_are_bounded() {
        let url = address("http://localhost:1234/");
        // ssh failing for any other reason: once, as "could not reach".
        let tmp = tempfile::tempdir().unwrap();
        let ssh = stand_in_ssh(tmp.path(), 1, "exit 0");
        executable(
            &ssh,
            &format!(
                "#!/bin/sh\necho --- >> {}\necho 'ssh: connect to host box port 22: refused' >&2\nexit 255\n",
                quote(tmp.path().join("calls").to_str().unwrap())
            ),
        );
        let e = run_tunnel(
            &tunnel_for(&ssh, &url),
            &[1234, 50001],
            &|| None,
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "could not reach box");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("calls")).unwrap(),
            "---\n"
        );
        // Every port refused (taken, or forwarding off there — ssh says the same): each tried
        // once, then the server's refusal, said as such. The production list is 1 + 2.
        let tmp = tempfile::tempdir().unwrap();
        let ssh = stand_in_ssh(tmp.path(), 1234, "exit 0");
        let e = run_tunnel(
            &tunnel_for(&ssh, &url),
            &[1234, 1234],
            &|| None,
            &mut |_| {},
        )
        .unwrap_err()
        .to_string();
        assert_eq!(calls(tmp.path()).len(), 2);
        assert_eq!(
            e,
            "box refused the tunnel: forwarding is off there, or no port was free \
             (AllowTcpForwarding?)"
        );
        assert_eq!(TUNNEL_TRIES, 2);
        // HOST's own exit code comes back as a forwarded run's does.
        let tmp = tempfile::tempdir().unwrap();
        let ssh = stand_in_ssh(tmp.path(), 1, "exit 3");
        let code = run_tunnel(&tunnel_for(&ssh, &url), &[1234], &|| None, &mut |_| {}).unwrap();
        assert_eq!(code, 3);
    }

    /// A pid that is gone: reaped, not even a zombie.
    fn reaped(pid: i32) -> bool {
        // SAFETY: signal 0 only asks whether the process (a zombie included) exists.
        unsafe { libc::kill(pid, 0) != 0 }
    }

    /// The children of `pid` now.
    fn children(pid: i32) -> Vec<i32> {
        let out = Command::new("ps")
            .args(["-A", "-o", "pid=", "-o", "ppid="])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let mut f = l.split_whitespace().map(|n| n.parse::<i32>());
                match (f.next(), f.next()) {
                    (Some(Ok(child)), Some(Ok(parent))) if parent == pid => Some(child),
                    _ => None,
                }
            })
            .collect()
    }

    /// Every pid in `pids` is reaped within 10 s (else they are all killed, and it fails).
    fn all_reaped(pids: &[i32]) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        for &pid in pids {
            while !reaped(pid) {
                if std::time::Instant::now() >= deadline {
                    for &pid in pids {
                        // SAFETY: processes this test started (the stand-ins, the guard).
                        unsafe { libc::kill(pid, libc::SIGKILL) };
                    }
                    panic!("{pid} is still there (running, or a zombie no one reaped)");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    /// The `<ssh> <guard>` pids the stand-in recorded, and the guard's children (its watcher)
    /// as they are now.
    fn tunnel_pids(dir: &Path) -> Vec<i32> {
        let text = std::fs::read_to_string(dir.join("pids")).unwrap();
        let mut pids: Vec<i32> = text
            .split_whitespace()
            .map(|p| p.parse().unwrap())
            .collect();
        let guards: Vec<i32> = pids.iter().skip(1).step_by(2).copied().collect();
        for guard in guards {
            pids.extend(children(guard));
        }
        pids.sort_unstable();
        pids.dedup();
        pids
    }

    /// Stopped (Ctrl-C, SIGTERM), the tunnel's ssh is ended even when it would stay: once
    /// HOST's command ends, a real ssh waits for the forwarded connections a browser keeps open
    /// (ssh(1), `~&`). Here it ignores its input's end and SIGINT and lingers, its output open
    /// (HOST's command still runs); the stop is seen once the address is open, and the run
    /// says 0. ssh, the guard and its watcher are all reaped, none left a zombie.
    #[test]
    fn a_stop_ends_a_lingering_ssh() {
        let tmp = tempfile::tempdir().unwrap();
        let ssh = stand_in_ssh(tmp.path(), 1, "trap '' INT HUP; exec sleep 60");
        let url = address("http://127.0.0.1:1234/");
        let opened = std::sync::atomic::AtomicBool::new(false);
        let pids = std::sync::Mutex::new(Vec::new());
        let started = std::time::Instant::now();
        let code = run_tunnel(
            &tunnel_for(&ssh, &url),
            &[1234],
            &|| {
                opened
                    .load(std::sync::atomic::Ordering::SeqCst)
                    .then_some(15)
            },
            &mut |_| {
                *pids.lock().unwrap() = tunnel_pids(tmp.path());
                opened.store(true, std::sync::atomic::Ordering::SeqCst)
            },
        )
        .unwrap();
        assert_eq!(code, 0);
        assert!(started.elapsed() < Duration::from_secs(20));
        let pids = pids.into_inner().unwrap();
        // ssh, the guard and the watcher.
        assert!(pids.len() >= 3, "{pids:?}");
        all_reaped(&pids);
    }

    /// HOST's command ends (ssh's output ends, as OpenSSH's does with the session) while ssh
    /// stays for the browser's forwarded connections: the tunnel is closed under it within the
    /// grace, and the run is an error (no exit code came from HOST).
    #[test]
    fn hosts_end_closes_a_tunnel_ssh_would_keep() {
        let tmp = tempfile::tempdir().unwrap();
        let ssh = stand_in_ssh(
            tmp.path(),
            1,
            "exec >/dev/null; trap '' INT HUP; exec sleep 60",
        );
        let url = address("http://me:s3cret@127.0.0.1:1234/?token=abc");
        let pids = std::sync::Mutex::new(Vec::new());
        let mut said = Vec::new();
        let started = std::time::Instant::now();
        let e = run_tunnel(&tunnel_for(&ssh, &url), &[1234], &|| None, &mut |line| {
            *pids.lock().unwrap() = tunnel_pids(tmp.path());
            said.push(line.to_string())
        })
        .unwrap_err()
        .to_string();
        assert!(started.elapsed() < Duration::from_secs(15));
        assert_eq!(said.len(), 1, "{said:?}");
        assert_eq!(
            e,
            "sopen on box ended while ssh stayed for the browser's connections; the tunnel to \
             http://me:***@127.0.0.1:1234/?… is closed"
        );
        let pids = pids.into_inner().unwrap();
        assert!(pids.len() >= 3, "{pids:?}");
        all_reaped(&pids);
        // ssh ending by itself in the grace gives HOST's code, as before.
        let tmp = tempfile::tempdir().unwrap();
        let ssh = stand_in_ssh(tmp.path(), 1, "exec >/dev/null; sleep 0.3; exit 4");
        let code = run_tunnel(&tunnel_for(&ssh, &url), &[1234], &|| None, &mut |_| {}).unwrap();
        assert_eq!(code, 4);
    }

    /// HOST's window (09 §7): the open line names it only after `sopen-tunnel-window`, on the
    /// address's own port and on a fallback one; `sopen-tunnel-closed` ends the run with 0 and
    /// its line — read after ssh's exit (whatever HOST's code), and while ssh stays, which closes
    /// the tunnel under it at once, everything reaped. A HOST without the window cue gets
    /// today's line, and its closed cue counts the same.
    #[test]
    fn hosts_window_cues_set_the_lines_and_its_close_ends_the_run() {
        use crate::url::{TUNNEL_CLOSED, TUNNEL_OPEN, TUNNEL_WINDOW};
        let url = address("http://me:s3cret@127.0.0.1:1234/x?token=abc");
        let shown = "http://me:***@127.0.0.1:1234/x?…";
        let window = format!(
            "{shown} is open on box through a tunnel — closing its window there, or Ctrl-C \
             here, closes it"
        );
        let closed = format!("{shown}: its window on box was closed; the tunnel is closed");

        // The cues, then HOST's command ends at once, with another code: the cue counts.
        for (say, opened) in [
            (format!("echo {TUNNEL_WINDOW}; echo {TUNNEL_OPEN}"), &window),
            (
                format!("echo {TUNNEL_OPEN}"),
                &format!("{shown} is open on box through a tunnel — Ctrl-C closes it"),
            ),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let ssh = stand_in_host(
                tmp.path(),
                1,
                &format!("{say}; echo {TUNNEL_CLOSED}; exit 4"),
            );
            let mut said = Vec::new();
            let code = run_tunnel(&tunnel_for(&ssh, &url), &[1234], &|| None, &mut |l| {
                said.push(l.to_string())
            })
            .unwrap();
            assert_eq!(code, 0, "{say}");
            assert_eq!(said, [opened.as_str(), closed.as_str()], "{say}");
        }

        // A fallback port, and ssh staying after the cue (its output open): closed under it.
        let tmp = tempfile::tempdir().unwrap();
        let ssh = stand_in_host(
            tmp.path(),
            1234,
            &format!(
                "echo {TUNNEL_WINDOW}; echo {TUNNEL_OPEN}; echo {TUNNEL_CLOSED}; trap '' INT HUP; \
                 exec sleep 60"
            ),
        );
        let pids = std::sync::Mutex::new(Vec::new());
        let mut said = Vec::new();
        let started = std::time::Instant::now();
        let code = run_tunnel(
            &tunnel_for(&ssh, &url),
            &[1234, 50001],
            &|| None,
            &mut |line| {
                // At the first line: ssh, the guard and its watcher, all still there.
                let mut pids = pids.lock().unwrap();
                if pids.is_empty() {
                    *pids = tunnel_pids(tmp.path());
                }
                said.push(line.to_string())
            },
        )
        .unwrap();
        assert_eq!(code, 0);
        assert!(started.elapsed() < Duration::from_secs(15));
        assert_eq!(
            said,
            [
                format!(
                    "{shown} is open on box as http://me:***@127.0.0.1:50001/x?… through a \
                     tunnel (port 1234 could not be forwarded there) — closing its window there, \
                     or Ctrl-C here, closes it"
                ),
                closed
            ]
        );
        let pids = pids.into_inner().unwrap();
        assert!(pids.len() >= 3, "{pids:?}");
        all_reaped(&pids);
    }

    /// A cue is the whole line but its end (`\n`, `\r\n`, or none at the output's end): a space
    /// or a tab after it, or anything before it, makes it HOST's plain output (09 §7).
    #[test]
    fn cues_are_matched_exactly() {
        use crate::url::{TUNNEL_CLOSED, TUNNEL_OPEN, TUNNEL_WINDOW};
        for (cue, want) in [
            (TUNNEL_WINDOW, Cue::Window),
            (TUNNEL_OPEN, Cue::Open),
            (TUNNEL_CLOSED, Cue::Closed),
        ] {
            for line in [format!("{cue}\n"), format!("{cue}\r\n"), cue.to_string()] {
                assert_eq!(Cue::of(&line), Some(want), "{line:?}");
            }
            for line in [
                format!("{cue} \n"),
                format!("{cue}\t\n"),
                format!("{cue} \r\n"),
                format!("{cue}\r\r\n"),
                format!(" {cue}\n"),
                format!("{cue}x\n"),
                cue.to_uppercase(),
            ] {
                assert_eq!(Cue::of(&line), None, "{line:?}");
            }
        }
    }

    /// A stop (Ctrl-C, SIGTERM) that comes while this side drains HOST's output after ssh's
    /// exit, an open cue in that output not yet read: stopped before the address opened — the
    /// stop is checked before the cues are, as before 09; the open line is not said.
    #[test]
    fn a_stop_during_the_drain_comes_before_an_open_cue_in_it() {
        use crate::url::{TUNNEL_OPEN, TUNNEL_WINDOW};
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("stopped");
        // ssh ends at once; what it left running says the cues later, once this side drains,
        // and the stop comes before that drain is over.
        let ssh = stand_in_host(
            tmp.path(),
            1,
            &format!(
                "(sleep 1; echo {TUNNEL_WINDOW}; echo {TUNNEL_OPEN}; : > {}) &\nexit 0",
                quote(marker.to_str().unwrap())
            ),
        );
        let url = address("http://127.0.0.1:1234/");
        let mut said = Vec::new();
        let code = run_tunnel(
            &tunnel_for(&ssh, &url),
            &[1234],
            &|| marker.exists().then_some(15),
            &mut |l| said.push(l.to_string()),
        )
        .unwrap();
        assert_eq!(code, 128 + 15);
        assert_eq!(said, Vec::<String>::new());
    }

    /// The listener tables HOST's sopen reads: Linux's `/proc/net/tcp{,6}` rows (this machine's
    /// byte order: little-endian, as every runner is) and macOS's `netstat -anv -p tcp` lines.
    #[cfg(target_endian = "little")]
    #[test]
    fn listeners_are_read_from_proc_and_netstat() {
        let table = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:04D2 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1 1
   1: 00000000:04D2 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 2 1
   2: 0100007F:04D2 0100007F:C000 01 00000000:00000000 00:00000000 00000000  1000        0 3 1
   3: 126433C6:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4 1
  sl  local_address                         remote_address                        st
   0: 00000000000000000000000001000000:04D2 00000000000000000000000000000000:0000 0A 0 0
   1: 00000000000000000000000000000000:04D2 00000000000000000000000000000000:0000 0A 0 0
   2: 0000000000000000FFFF00000100007F:04D2 00000000000000000000000000000000:0000 0A 0 0
Active Internet connections (including servers)
Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)      rxbytes
tcp4       0      0  127.0.0.1.4321         *.*                    LISTEN            0
tcp4       0      0  *.4321                 *.*                    LISTEN            0
tcp6       0      0  ::1.4321               *.*                    LISTEN            0
tcp6       0      0  fe80::1%lo0.4321       *.*                    LISTEN            0
tcp46      0      0  *.4322                 *.*                    LISTEN            0
tcp4       0      0  198.51.100.18.4321      192.0.2.7.443          ESTABLISHED       0
tcp4       0      0  198.51.100.18.4323      *.*                    LISTEN            0
";
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        assert_eq!(
            listeners_on(table, 1234),
            [
                ip("127.0.0.1"),
                ip("0.0.0.0"),
                ip("::1"),
                ip("::"),
                ip("::ffff:127.0.0.1")
            ]
        );
        assert_eq!(listeners_on(table, 8080), [ip("198.51.100.18")]);
        assert_eq!(
            listeners_on(table, 4321),
            [ip("127.0.0.1"), ip("0.0.0.0"), ip("::1"), ip("fe80::1")]
        );
        assert_eq!(listeners_on(table, 4322), [ip("0.0.0.0")]);
        assert!(listeners_on(table, 9).is_empty());
        // Only on positive evidence: the tunnel's listener seen, on 127.0.0.1, ::1 or
        // ::ffff:127.0.0.1 alone.
        let only_loopback = "\
   0: 0100007F:04D2 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1 1
   0: 00000000000000000000000001000000:04D2 00000000000000000000000000000000:0000 0A 0 0
   0: 0000000000000000FFFF00000100007F:04D2 00000000000000000000000000000000:0000 0A 0 0
tcp4       0      0  127.0.0.1.1234         *.*                    LISTEN            0
";
        assert!(private_in(only_loopback, 1234, "box").is_ok());
        assert!(private_in("tcp6 0 0 ::1.1234 *.* LISTEN 0", 1234, "box").is_ok());
        // Nothing seen on the port — an empty table, none on it, only a connection on it,
        // output that does not parse — cannot confirm it: refused.
        for unseen in [
            "",
            only_loopback
                .replace("04D2", "04D3")
                .replace(".1234", ".1235")
                .as_str(),
            "tcp4 0 0 127.0.0.1.1234 192.0.2.7.443 ESTABLISHED 0",
            "   2: 0100007F:04D2 0100007F:C000 01 00000000:00000000 00:00000000 0 1000 0 3 1",
            "netstat: sysctl: net.inet.tcp.pcblist_n: Operation not permitted",
            "garbage 127.0.0.1:1234 LISTEN",
        ] {
            let e = private_in(unseen, 1234, "box").unwrap_err().to_string();
            assert!(
                e.starts_with(
                    "cannot confirm the tunnel is private on box (no listener seen on port 1234)"
                ),
                "{unseen:?}: {e}"
            );
        }
        // Anything else listening on the port — the network, or another loopback address
        // than the tunnel binds — refuses, said.
        let e = private_in(table, 1234, "box").unwrap_err().to_string();
        assert!(e.contains("port 1234 listens on 0.0.0.0 on box"), "{e}");
        assert!(e.contains("GatewayPorts"), "{e}");
        assert!(private_in(table, 4321, "box").is_err());
        assert!(private_in(table, 8080, "box").is_err());
        let other_loopback = "tcp4 0 0 127.0.0.2.1234 *.* LISTEN 0";
        assert!(private_in(other_loopback, 1234, "box")
            .unwrap_err()
            .to_string()
            .contains("listens on 127.0.0.2"));
    }

    /// The live table: this machine's own (`/proc` on Linux, netstat on macOS) is read and
    /// parsed — a socket listening on loopback here is found in it.
    #[test]
    fn this_machines_listener_table_is_read() {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        let table = listener_table().unwrap();
        assert!(
            listeners_on(&table, port).contains(&"127.0.0.1".parse().unwrap()),
            "port {port} not found"
        );
        assert!(private_in(&table, port, "here").is_ok());
    }

    /// The shells `/bin/sh` may be where the guard runs: this machine's, dash (Debian's and
    /// Ubuntu's `/bin/sh`; where it is not `/bin/sh` already) and bash (macOS's) — as
    /// [`login_shells`] picks them: a missing bash is skipped with a note, or fails under
    /// SOPEN_REQUIRE_SHELLS=1.
    fn sh_lanes() -> Vec<PathBuf> {
        login_shells()
            .into_iter()
            .map(|sh| sh.path)
            .filter(|p| p.file_name().is_some_and(|n| n != "zsh" && n != "fish"))
            .collect()
    }

    /// The guard alone, as a SIGKILL of this sopen leaves it: nothing kills it, only its input
    /// ends. The ssh under it ignores that end (a real one waits for the browser's forwarded
    /// connections, ssh(1) `~&`), and still goes, and the guard exits with its status. Before
    /// that, with the input open, ssh runs on and reads that input, not `/dev/null` (which a
    /// shell gives an asynchronous command first — dash before its redirections).
    #[test]
    fn the_guard_ends_ssh_when_its_input_ends() {
        for sh in sh_lanes() {
            let tmp = tempfile::tempdir().unwrap();
            let pidfile = tmp.path().join("pid");
            let ssh = tmp.path().join("ssh");
            // It reads its input to the end (marking `eof`), then lingers.
            let eof = tmp.path().join("eof");
            executable(
                &ssh,
                &format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$$\" > {pid}\ncat > /dev/null\n: > {eof}\n\
                     trap '' INT HUP\nexec sleep 60\n",
                    pid = quote(pidfile.to_str().unwrap()),
                    eof = quote(eof.to_str().unwrap()),
                ),
            );
            let mut guard = Command::new(&sh)
                .arg("-c")
                .arg(GUARD)
                .arg("sopen-tunnel")
                .arg(&ssh)
                .arg("-T")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            let pid: i32 = loop {
                if let Some(pid) = std::fs::read_to_string(&pidfile)
                    .ok()
                    .and_then(|p| p.trim().parse().ok())
                {
                    break pid;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "{sh:?}: ssh did not start"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            // Running, input open: it stays, and its input is the guard's, still open.
            std::thread::sleep(Duration::from_millis(300));
            let alive = !reaped(pid);
            let early_eof = eof.exists();
            // ssh and the watcher, the guard's children.
            let kids = children(guard.id() as i32);
            drop(guard.stdin.take());
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let status = loop {
                if let Some(status) = guard.try_wait().unwrap() {
                    break Some(status);
                }
                if std::time::Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let left = !reaped(pid);
            if status.is_none() || left {
                let _ = guard.kill();
                let _ = guard.wait();
                // SAFETY: the stand-in this test started (its pid was read from it).
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            assert!(alive, "{sh:?}: ssh ended while its input was open");
            assert!(
                !early_eof,
                "{sh:?}: ssh's input was not the guard's (it ended at once)"
            );
            assert!(!left, "{sh:?}: ssh {pid} outlived its input's end");
            // ssh's status: SIGTERM, as `wait` reports it.
            assert_eq!(status.and_then(|s| s.code()), Some(143), "{sh:?}");
            // The guard reaped ssh and its watcher: neither is left, not even as a zombie.
            assert_eq!(kids.len(), 2, "{sh:?}: {kids:?}");
            all_reaped(&kids);
        }
    }

    // -----------------------------------------------------------------------------------------
    // The command run as OpenSSH runs it: sshd joins the words with spaces into one string and
    // hands it to the login shell (`$SHELL -c "<string>"`), which parses the whole line first.
    // Each lane is one login shell: sh, dash (Debian's sh), bash, zsh (macOS's) and fish (the
    // default of some Linux distributions). A missing bash, zsh or fish is skipped with a note —
    // or fails, under SOPEN_REQUIRE_SHELLS=1 (CI).

    /// A login shell lane (its binary).
    #[derive(Debug)]
    struct Shell {
        path: PathBuf,
    }

    /// `name` on the test process's PATH.
    /// `name` on the test process's PATH: the first absolute, executable file, canonicalized.
    fn which(name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .filter_map(|dir| dir.join(name).canonicalize().ok())
            .find(|p| {
                std::fs::metadata(p)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    }

    fn login_shells() -> Vec<Shell> {
        let require = std::env::var_os("SOPEN_REQUIRE_SHELLS").is_some_and(|v| v == "1");
        let mut lanes = vec![Shell {
            path: PathBuf::from("/bin/sh"),
        }];
        let dash = Path::new("/bin/dash");
        // Where /bin/sh is dash already, one lane covers both.
        if dash.exists() && dash.canonicalize().ok() != Path::new("/bin/sh").canonicalize().ok() {
            lanes.push(Shell {
                path: dash.to_path_buf(),
            });
        }
        for name in ["bash", "zsh", "fish"] {
            match which(name) {
                Some(path) => lanes.push(Shell { path }),
                None if require => panic!("SOPEN_REQUIRE_SHELLS=1 but {name} is not on PATH"),
                None => eprintln!("note: {name} is not on PATH; its login-shell lane is skipped"),
            }
        }
        lanes
    }

    /// The tools the script and the fakes run, besides shell builtins.
    const TOOLS: [&str; 6] = ["env", "sh", "cat", "grep", "head", "id"];

    struct Host {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        bin: PathBuf,
        home: PathBuf,
    }

    fn executable(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A stand-in for HOST: a home, and a `bin` that is the whole PATH — links to [`TOOLS`], a
    /// fake `uname` (reporting `os`) and a fake `systemctl` that records its arguments in
    /// `systemctl-args`, prints `session-env` and exits with the code in `systemctl-exit`, if
    /// any. No real sopen can be on that PATH.
    fn host(os: &str, session: &str) -> Host {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let bin = root.join("bin");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        for tool in TOOLS {
            let real = which(tool).unwrap_or_else(|| panic!("{tool} is not on PATH"));
            std::os::unix::fs::symlink(real, bin.join(tool)).unwrap();
        }
        executable(&bin.join("uname"), &format!("#!/bin/sh\necho {os}\n"));
        std::fs::write(root.join("session-env"), session).unwrap();
        let file = |name: &str| quote(root.join(name).to_str().unwrap());
        executable(
            &bin.join("systemctl"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {args}\ncat {session}\n\
                 if [ -s {exit} ]; then echo 'Failed to connect to bus' >&2; exit \"$(cat {exit})\"; fi\n",
                args = file("systemctl-args"),
                session = file("session-env"),
                exit = file("systemctl-exit"),
            ),
        );
        Host {
            _tmp: tmp,
            root,
            bin,
            home,
        }
    }

    /// The variables a run on HOST depends on; the fake sopen prints each.
    const SEEN: [&str; 16] = [
        "SOPEN_STATE",
        "SOPEN_LOG",
        "SOPEN_TARGET_BIN",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_SESSION_TYPE",
        "XDG_CURRENT_DESKTOP",
        "KDE_SESSION_VERSION",
        "KDE_FULL_SESSION",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
        "EVIL",
        "PATH",
    ];

    /// A fake sopen that prints its arguments, where it runs, [`SEEN`] and which ffmpeg it
    /// would run.
    fn fake_sopen() -> String {
        let mut body = String::from(
            "#!/bin/sh\nfor a in \"$@\"; do printf 'arg [%s]\\n' \"$a\"; done\nprintf 'pwd %s\\n' \"$PWD\"\n\
             printf 'ffmpeg [%s]\\n' \"$(command -v ffmpeg)\"\n",
        );
        for var in SEEN {
            body.push_str(&format!("printf '{var} [%s]\\n' \"${{{var}-unset}}\"\n"));
        }
        body
    }

    /// `out` has exactly this line.
    fn has_line(out: &str, line: &str) -> bool {
        out.lines().any(|l| l == line)
    }

    /// The `[…]` value of the `name [value]` line the fake sopen printed.
    fn value<'a>(out: &'a str, name: &str) -> &'a str {
        out.lines()
            .find_map(|l| l.strip_prefix(&format!("{name} ["))?.strip_suffix(']'))
            .unwrap_or_else(|| panic!("no {name} line in {out}"))
    }

    /// The signals the lanes send, or a session's terminal does.
    const RESET: [libc::c_int; 4] = [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGHUP];

    /// Whether this process ignores any of `signals` (a child would start ignoring it too).
    fn ignored(signals: &[libc::c_int]) -> bool {
        signals.iter().any(|&signal| {
            // SAFETY: a null new action only reads the current one into `old`.
            unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                libc::sigaction(signal, std::ptr::null(), &mut old) == 0
                    && old.sa_sigaction == libc::SIG_IGN
            }
        })
    }

    impl Host {
        /// Runs `words` as sshd does: joined with spaces, `<login shell> -c "<line>"`, with only
        /// HOST's PATH and HOME (and `extra_env`, which may replace them).
        fn run(
            &self,
            sh: &Shell,
            words: &[String],
            extra_env: &[(&str, &str)],
        ) -> (i32, String, String) {
            use std::os::unix::process::CommandExt;
            let mut cmd = Command::new(&sh.path);
            // A process group of its own, as a session on HOST has: a test can signal all of
            // it (Ctrl+C on a terminal) without reaching the test runner.
            cmd.process_group(0)
                .arg("-c")
                .arg(words.join(" "))
                .env_clear()
                .env("PATH", &self.bin)
                .env("HOME", &self.home);
            for (k, v) in extra_env {
                cmd.env(k, v);
            }
            // The signals' default dispositions, as a session sshd starts has them. An ignored
            // signal is handed on across exec: a test runner started with SIGINT ignored (in the
            // background of a non-interactive shell, or under `nohup` for SIGHUP) would hand it to
            // the line, and sopen would survive the Ctrl+C a test sends it. Put back in the child
            // only then: a `pre_exec` makes the spawn a fork, whose child holds a copy of every
            // descriptor of this process until its exec — other tests' journal locks among them.
            if ignored(&RESET) {
                // SAFETY: only `signal`, async-signal-safe, between fork and exec.
                unsafe {
                    cmd.pre_exec(|| {
                        for signal in RESET {
                            libc::signal(signal, libc::SIG_DFL);
                        }
                        Ok(())
                    });
                }
            }
            let out = cmd.output().unwrap();
            (
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        }

        /// The PATH `sh` starts the line with, given HOST's PATH (and `extra_env`): the shell's
        /// own startup files may have changed it (snapd's fish config appends /snap/bin; zsh
        /// reads /etc/zshenv, bash `$BASH_ENV`). Read from `env`, run the way the line is.
        fn start_path(&self, sh: &Shell, extra_env: &[(&str, &str)]) -> String {
            let (code, out, err) = self.run(sh, &["env".to_string()], extra_env);
            assert_eq!(code, 0, "{sh:?}: {err}");
            out.lines()
                .find_map(|l| l.strip_prefix("PATH="))
                .unwrap_or_else(|| panic!("{sh:?}: no PATH in {out}"))
                .to_string()
        }

        /// An install path candidate under this host's temp dir, where nothing is installed.
        fn not_installed(&self) -> String {
            quote(self.root.join("not-installed/sopen").to_str().unwrap())
        }

        fn systemctl_args(&self) -> String {
            std::fs::read_to_string(self.root.join("systemctl-args")).unwrap_or_default()
        }
    }

    #[test]
    fn the_script_runs_sopen_here_with_the_source_and_hooks() {
        let source = r#"it's a "b" $HOME `id` \x 한글 ü.png"#;
        let env = [
            "SOPEN_STATE=rect=1,2,3,4;op=crop|pixelize".to_string(),
            "SOPEN_LOG=debug".to_string(),
        ];
        for sh in login_shells() {
            let h = host("Darwin", "");
            executable(&h.bin.join("sopen"), &fake_sopen());
            let app = Flags {
                app: Some("Hex Fiend"),
                ..Flags::default()
            };
            let words = remote_words("box", source, app, &env, INSTALL_PATHS);
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert_eq!(err, "", "{sh:?}");
            let args: Vec<&str> = out.lines().filter_map(|l| l.strip_prefix("arg ")).collect();
            assert_eq!(
                args,
                [
                    "[--here]",
                    "[--app]",
                    "[Hex Fiend]",
                    "[--]",
                    &format!("[{source}]")
                ],
                "{sh:?}"
            );
            assert!(
                has_line(&out, &format!("pwd {}", h.home.display())),
                "{sh:?}: {out}"
            );
            assert_eq!(
                value(&out, "SOPEN_STATE"),
                "rect=1,2,3,4;op=crop|pixelize",
                "{sh:?}"
            );
            assert_eq!(value(&out, "SOPEN_LOG"), "debug", "{sh:?}");
            // `--gui` and `--hex` (as a forward with `--hex` passes them, X2-gui) reach HOST's
            // sopen after `--here`, before `--`.
            let hex = Flags {
                gui: true,
                hex: true,
                ..Flags::default()
            };
            let words = remote_words("box", source, hex, &env, INSTALL_PATHS);
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            let args: Vec<&str> = out.lines().filter_map(|l| l.strip_prefix("arg ")).collect();
            assert_eq!(
                args,
                [
                    "[--here]",
                    "[--gui]",
                    "[--hex]",
                    "[--]",
                    &format!("[{source}]")
                ],
                "{sh:?}"
            );
            // Not Linux: no display variables are made up, and systemd is not asked.
            assert_eq!(value(&out, "DISPLAY"), "unset", "{sh:?}");
            assert_eq!(value(&out, "WAYLAND_DISPLAY"), "unset", "{sh:?}");
            assert_eq!(h.systemctl_args(), "", "{sh:?}");
        }
    }

    #[test]
    fn the_script_finds_sopen_by_target_bin_or_install_path() {
        for sh in login_shells() {
            let h = host("Darwin", "");
            // Not on PATH and not installed: only SOPEN_TARGET_BIN names it.
            let elsewhere = h.root.join("build/sopen-dev");
            executable(&elsewhere, &fake_sopen());
            let env = [format!("SOPEN_TARGET_BIN={}", elsewhere.display())];
            let words = remote_words("box", "/x", Flags::default(), &env, &h.not_installed());
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert!(has_line(&out, "arg [--here]"), "{sh:?}: {out}");
            assert_eq!(
                value(&out, "SOPEN_TARGET_BIN"),
                elsewhere.to_str().unwrap(),
                "{sh:?}"
            );

            // Not on PATH: the install paths, in order, `$HOME` expanded on HOST. (Not the
            // production list: a sopen installed on the test machine would be found first.)
            executable(&h.home.join(".cargo/bin/sopen"), &fake_sopen());
            let paths = format!(r#"{} "$HOME/.cargo/bin/sopen""#, h.not_installed());
            let words = remote_words("box", "/x", Flags::default(), &[], &paths);
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert!(has_line(&out, "arg [/x]"), "{sh:?}: {out}");
        }
    }

    #[test]
    fn sopen_on_the_target_finds_the_ffmpeg_next_to_it() {
        for sh in login_shells() {
            ffmpeg_next_to_sopen(&sh, false);
        }
    }

    /// The PATH a login shell starts the line with is its own: its startup files may have
    /// changed the session's (snapd's fish config appends /snap/bin; zsh reads /etc/zshenv, bash
    /// `$BASH_ENV`). sopen keeps it. Here bash reads a `$BASH_ENV` that appends a dir.
    #[test]
    fn a_path_changed_by_the_login_shells_startup_is_kept() {
        let bash = login_shells()
            .into_iter()
            .find(|sh| sh.path.file_name().is_some_and(|n| n == "bash"));
        match bash {
            Some(bash) => ffmpeg_next_to_sopen(&bash, true),
            None => eprintln!("note: bash is not on PATH; its $BASH_ENV case is skipped"),
        }
    }

    /// What the script appended to `path` after `first` (sopen's dir, then the PATH the shell
    /// started with, `start`): Homebrew's bin dirs that exist and are not on `start`, once each.
    fn appended<'a>(sh: &Shell, path: &'a str, first: &str, start: &str) -> Vec<&'a str> {
        let rest = path
            .strip_prefix(first)
            .unwrap_or_else(|| panic!("{sh:?}: {path} does not start with {first}"));
        let entries: Vec<&str> = match rest {
            "" => Vec::new(),
            rest => rest
                .strip_prefix(':')
                .unwrap_or_else(|| panic!("{sh:?}: {path} does not start with {first}:"))
                .split(':')
                .collect(),
        };
        let on_start: Vec<&str> = start.split(':').collect();
        for (i, entry) in entries.iter().enumerate() {
            assert!(Path::new(entry).is_dir(), "{sh:?}: {entry} in {path}");
            assert!(!on_start.contains(entry), "{sh:?}: {entry} again in {path}");
            assert!(
                !entries[..i].contains(entry),
                "{sh:?}: {entry} twice in {path}"
            );
        }
        entries
    }

    /// One lane of [`sopen_on_the_target_finds_the_ffmpeg_next_to_it`]. With `startup`, bash
    /// reads a `$BASH_ENV` that appends a dir to PATH before it runs the line.
    fn ffmpeg_next_to_sopen(sh: &Shell, startup: bool) {
        let h = host("Darwin", "");
        let session = h.bin.to_str().unwrap().to_string();
        let added = h.root.join("snap/bin");
        let bash_env = h.root.join("bash-env");
        let mut base: Vec<(&str, &str)> = Vec::new();
        if startup {
            std::fs::create_dir_all(&added).unwrap();
            let line = format!("PATH=$PATH:{}\n", quote(added.to_str().unwrap()));
            std::fs::write(&bash_env, line).unwrap();
            base.push(("BASH_ENV", bash_env.to_str().unwrap()));
        }
        // The session's PATH as the login shell starts the line with it; the script builds on
        // this, not on the PATH the session handed the shell.
        let start = h.start_path(sh, &base);
        if startup {
            assert_eq!(
                start,
                format!("{session}:{}", added.display()),
                "{sh:?}: $BASH_ENV was not read"
            );
        }
        let fixed = [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/home/linuxbrew/.linuxbrew/bin",
        ];

        // A brew-style install outside the session's PATH, ffmpeg beside sopen: its dir first,
        // then the start PATH, then Homebrew's bin dirs that exist.
        let brew = h.root.join("brew/bin");
        executable(&brew.join("sopen"), &fake_sopen());
        executable(&brew.join("ffmpeg"), "#!/bin/sh\n");
        let paths = quote(brew.join("sopen").to_str().unwrap());
        let words = remote_words("box", "/x", Flags::default(), &[], &paths);
        let (code, out, err) = h.run(sh, &words, &base);
        assert_eq!(code, 0, "{sh:?}: {err}");
        let path = value(&out, "PATH");
        let first = format!("{}:{start}", brew.display());
        for entry in appended(sh, path, &first, &start) {
            assert!(fixed.contains(&entry), "{sh:?}: {entry} in {path}");
        }
        assert_eq!(
            value(&out, "ffmpeg"),
            brew.join("ffmpeg").to_str().unwrap(),
            "{sh:?}"
        );

        // SOPEN_TARGET_BIN elsewhere: its dir first, and Homebrew's bin dirs that exist are
        // appended after the session's own entries, once each. (`$HOME/.linuxbrew/bin` is
        // the one a test can create; the absolute ones count where this machine has them.)
        let linuxbrew = h.home.join(".linuxbrew/bin");
        std::fs::create_dir_all(&linuxbrew).unwrap();
        let elsewhere = h.root.join("build/sopen-dev");
        executable(&elsewhere, &fake_sopen());
        let env = [format!("SOPEN_TARGET_BIN={}", elsewhere.display())];
        let words = remote_words("box", "/x", Flags::default(), &env, &h.not_installed());
        let (code, out, err) = h.run(sh, &words, &base);
        assert_eq!(code, 0, "{sh:?}: {err}");
        let path = value(&out, "PATH");
        let first = format!("{}:{start}", h.root.join("build").display());
        let entries = appended(sh, path, &first, &start);
        assert_eq!(
            entries.last(),
            Some(&linuxbrew.to_str().unwrap()),
            "{sh:?}: {path}"
        );
        for entry in &entries[..entries.len() - 1] {
            assert!(fixed.contains(entry), "{sh:?}: {entry} in {path}");
        }

        // Already on the session's PATH: not appended again.
        let session_with = format!("{session}:{}", linuxbrew.display());
        let mut with = base.clone();
        with.push(("PATH", &session_with));
        let start_with = h.start_path(sh, &with);
        let (code, out, err) = h.run(sh, &words, &with);
        assert_eq!(code, 0, "{sh:?}: {err}");
        let path = value(&out, "PATH");
        let first = format!("{}:{start_with}", h.root.join("build").display());
        for entry in appended(sh, path, &first, &start_with) {
            assert!(fixed.contains(&entry), "{sh:?}: {entry} in {path}");
        }
        assert_eq!(
            path.matches(linuxbrew.to_str().unwrap()).count(),
            1,
            "{sh:?}: {path}"
        );
    }

    #[test]
    fn the_script_says_when_sopen_is_missing() {
        for sh in login_shells() {
            let h = host("Darwin", "");
            let words = remote_words("me@box", "/x", Flags::default(), &[], &h.not_installed());
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 127, "{sh:?}: {out} {err}");
            assert_eq!(
                err, "sopen is not installed on me@box (brew install 2lab-ai/tap/sopen-preview)\n",
                "{sh:?}"
            );
            assert_eq!(out, "", "{sh:?}");
        }
    }

    #[test]
    fn a_sopen_stopped_by_a_signal_comes_back_as_an_exit_code() {
        // Each lane runs the line as sshd does, `<login shell> -c "<line>"`, in a process group
        // of its own. A signal to the whole group (Ctrl+C or a hangup on HOST's terminal) must
        // leave only the script's shell to report it: the login shell is gone (`exec`), the
        // script survives on its trap and exits 128 + n, and sopen saw the default handling (it
        // stopped; "survived" would mean the signal was ignored). A shell that died of it would
        // show here as no exit code (-1) — and to ssh as a lost connection (255).
        for sh in login_shells() {
            let h = host("Darwin", "");
            let words = remote_words("box", "/x", Flags::default(), &[], INSTALL_PATHS);
            for (signal, code) in [("INT", 130), ("TERM", 143), ("HUP", 129)] {
                executable(
                    &h.bin.join("sopen"),
                    &format!("#!/bin/sh\nkill -{signal} 0\necho survived\n"),
                );
                let (got, out, err) = h.run(&sh, &words, &[]);
                assert_eq!(got, code, "{sh:?} {signal}: {out} {err}");
                assert!(!out.contains("survived"), "{sh:?} {signal}: {out}");
            }
            // An ordinary exit code is passed on as it is.
            executable(&h.bin.join("sopen"), "#!/bin/sh\nexit 3\n");
            let (code, _, _) = h.run(&sh, &words, &[]);
            assert_eq!(code, 3, "{sh:?}");
        }
    }

    #[test]
    fn linux_without_a_display_takes_the_session_display() {
        for sh in login_shells() {
            let h = host("Linux", "");
            let pwned = h.root.join("pwned");
            // A value with command substitutions, in a name that is imported: kept as text. The
            // sentinel is a redirect, a shell builtin, so it works on HOST's bare PATH.
            let marker = quote(pwned.to_str().unwrap());
            let hostile = format!("x $(: > {marker}) `: > {marker}`");
            // Positive control: evaluated, the value does leave the marker.
            let evaluated = Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("echo {hostile}"))
                .env_clear()
                .env("PATH", &h.bin)
                .output()
                .unwrap();
            assert!(evaluated.status.success());
            assert!(pwned.exists(), "the sentinel cannot record evaluation");
            std::fs::remove_file(&pwned).unwrap();
            let imported = [
                (
                    "DBUS_SESSION_BUS_ADDRESS",
                    "unix:path=/run/user/1/bus,guid=0a1b",
                ),
                ("DISPLAY", ":7"),
                ("WAYLAND_DISPLAY", "wayland-9"),
                ("XAUTHORITY", "/run/user/1/xauth_q"),
                ("XDG_RUNTIME_DIR", "/run/user/1"),
                ("XDG_SESSION_TYPE", hostile.as_str()),
                ("XDG_CURRENT_DESKTOP", "KDE"),
                // A Plasma 6 session's own: `xdg-open` (an address's opener) picks `kde-open` by
                // KDE_SESSION_VERSION.
                ("KDE_SESSION_VERSION", "6"),
                ("KDE_FULL_SESSION", "true"),
                ("XDG_SESSION_DESKTOP", "KDE"),
                ("DESKTOP_SESSION", "plasma"),
            ];
            let mut session = format!("EVIL=$(: > {marker})\nPATH=/nowhere\nNOT_DISPLAY=:3\n");
            for (key, val) in imported {
                session.push_str(&format!("{key}={val}\n"));
            }
            std::fs::write(h.root.join("session-env"), session).unwrap();
            executable(&h.bin.join("sopen"), &fake_sopen());
            let words = remote_words("box", "/x", Flags::default(), &[], INSTALL_PATHS);
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert!(!pwned.exists(), "{sh:?}: a session value was evaluated");
            assert_eq!(h.systemctl_args(), "--user\nshow-environment\n", "{sh:?}");
            for (key, val) in imported {
                assert_eq!(value(&out, key), val, "{sh:?} {key}");
            }
            assert_eq!(value(&out, "EVIL"), "unset", "{sh:?}");
            assert!(!value(&out, "PATH").contains("/nowhere"), "{sh:?}: {out}");

            // A value systemd prints C-quoted (`$'…'`) is left alone.
            let session = "DISPLAY=:7\nXAUTHORITY=$'/a b'\n";
            std::fs::write(h.root.join("session-env"), session).unwrap();
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert_eq!(value(&out, "DISPLAY"), ":7", "{sh:?}");
            assert_eq!(value(&out, "XAUTHORITY"), "unset", "{sh:?}");
        }
    }

    #[test]
    fn linux_without_any_session_uses_the_wayland_socket() {
        for sh in login_shells() {
            let h = host("Linux", "");
            executable(&h.bin.join("sopen"), &fake_sopen());
            let run = h.root.join("run");
            std::fs::create_dir_all(&run).unwrap();
            std::fs::write(run.join("wayland-0.lock"), "").unwrap();
            let _socket = std::os::unix::net::UnixListener::bind(run.join("wayland-3")).unwrap();
            let words = remote_words("box", "/x", Flags::default(), &[], INSTALL_PATHS);
            let runtime = [("XDG_RUNTIME_DIR", run.to_str().unwrap())];
            let (code, out, err) = h.run(&sh, &words, &runtime);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert_eq!(value(&out, "WAYLAND_DISPLAY"), "wayland-3", "{sh:?}");
            assert_eq!(value(&out, "DISPLAY"), "unset", "{sh:?}");
            assert_eq!(
                value(&out, "XDG_RUNTIME_DIR"),
                run.to_str().unwrap(),
                "{sh:?}"
            );

            // systemd failing (no user manager) is quiet and falls back the same way.
            std::fs::write(h.root.join("session-env"), "").unwrap();
            std::fs::write(h.root.join("systemctl-exit"), "1").unwrap();
            let (code, out, err) = h.run(&sh, &words, &runtime);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert_eq!(err, "", "{sh:?}");
            assert_eq!(h.systemctl_args(), "--user\nshow-environment\n", "{sh:?}");
            assert_eq!(value(&out, "WAYLAND_DISPLAY"), "wayland-3", "{sh:?}");

            // A display already set is left alone, and systemd is not asked.
            std::fs::remove_file(h.root.join("systemctl-args")).unwrap();
            let (code, out, err) = h.run(&sh, &words, &[("DISPLAY", ":1")]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert_eq!(value(&out, "DISPLAY"), ":1", "{sh:?}");
            assert_eq!(value(&out, "WAYLAND_DISPLAY"), "unset", "{sh:?}");
            assert_eq!(h.systemctl_args(), "", "{sh:?}");
        }
    }
}
