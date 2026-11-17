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

/// Receipt hooks (AGENTS.md "Receipts") a forwarded run carries to HOST. Settings that name local
/// paths — `SOPEN_ZED`, `SOPEN_FFMPEG`, `SOPEN_FFPROBE`, `SOPEN_CACHE_DIR`, `SOPEN_STATE_DIR`,
/// `SOPEN_CONFIG` — mean nothing on HOST and stay here.
pub const FORWARDED_ENV: [&str; 11] = [
    "SOPEN_STATE",
    "SOPEN_SCREENSHOT",
    "SOPEN_WATCHDOG",
    "SOPEN_AUTOSAVE",
    "SOPEN_AUTOSAVE_DELAY_MS",
    "SOPEN_AUTOEXPORT",
    "SOPEN_EXPORT_HOLD_MS",
    "SOPEN_RENDERER",
    "SOPEN_WINDOW_SIZE",
    "SOPEN_LOG",
    "SOPEN_TARGET_BIN",
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
/// an ssh session on Linux has no display.
const SESSION_ENV: &str =
    "DISPLAY WAYLAND_DISPLAY XAUTHORITY XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS XDG_SESSION_TYPE";

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
fn is_loopback(name: &str) -> bool {
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
/// arguments are `[--app APP] -- SOURCE`; it always adds `--here`.
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

/// The command for HOST's sshd: `exec [env K=V…] sh -c <script> sopen [--app APP] -- SOURCE`,
/// every word but `exec` quoted. The hooks go in front of `sh`, so the script sees
/// `SOPEN_TARGET_BIN` and the sopen it starts sees them all. The bare `exec` (a constant, never
/// data) replaces the login shell that parses the line — dash and fish would otherwise stay as
/// its parent, in the same process group, and die of the Ctrl+C the script survives, which ssh
/// reports as a lost connection (255).
pub fn remote_words(
    host: &str,
    source: &str,
    app: Option<&str>,
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
    if let Some(app) = app {
        words.extend(["--app".to_string(), app.to_string()]);
    }
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

/// Runs sopen on HOST for `source` (already as HOST sees it) and waits; returns the exit code to
/// leave with.
pub fn run(host: &str, source: &str, app: Option<&str>, state_dir: &Path) -> Result<u8> {
    let env = forwarded_env(|key| std::env::var_os(key))?;
    let words = remote_words(host, source, app, &env, INSTALL_PATHS);
    let connection = crate::remote::connection_args(host, state_dir)?;
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let args = ssh_args(connection, tty, app.is_some(), host, words);
    crate::debug!(
        "{}",
        crate::render_command(std::ffi::OsStr::new("ssh"), &args)
    );
    let status = Command::new("ssh")
        .args(&args)
        .status()
        .context("cannot run ssh (is OpenSSH installed?)")?;
    exit_code(host, status)
}

/// HOST's exit code is ours: 0, 1 and 3 from sopen, 2 from an older sopen that does not know
/// `--here` (a usage error), 127 for no sopen there. ssh's own failure (255) is "could not
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
        for local in [
            "SOPEN_ZED",
            "SOPEN_FFMPEG",
            "SOPEN_FFPROBE",
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
        let words = remote_words("box", "/tmp/a b.png", Some("ImHex"), &[], INSTALL_PATHS);
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
        let words = remote_words("box", "/x", None, &env, INSTALL_PATHS);
        assert_eq!(words[..4], ["exec", "'env'", "'SOPEN_LOG=debug'", "'sh'"]);
        assert_eq!(words[words.len() - 3..], ["'sopen'", "'--'", "'/x'"]);

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
    const SEEN: [&str; 11] = [
        "SOPEN_STATE",
        "SOPEN_LOG",
        "SOPEN_TARGET_BIN",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_SESSION_TYPE",
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
            let out = cmd.output().unwrap();
            (
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
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
            let words = remote_words("box", source, Some("Hex Fiend"), &env, INSTALL_PATHS);
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
            assert_eq!(value(&out, "SOPEN_STATE"), "rect=1,2,3,4;op=crop|pixelize");
            assert_eq!(value(&out, "SOPEN_LOG"), "debug");
            // Not Linux: no display variables are made up, and systemd is not asked.
            assert_eq!(value(&out, "DISPLAY"), "unset", "{sh:?}");
            assert_eq!(value(&out, "WAYLAND_DISPLAY"), "unset");
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
            let words = remote_words("box", "/x", None, &env, &h.not_installed());
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert!(has_line(&out, "arg [--here]"), "{out}");
            assert_eq!(value(&out, "SOPEN_TARGET_BIN"), elsewhere.to_str().unwrap());

            // Not on PATH: the install paths, in order, `$HOME` expanded on HOST. (Not the
            // production list: a sopen installed on the test machine would be found first.)
            executable(&h.home.join(".cargo/bin/sopen"), &fake_sopen());
            let paths = format!(r#"{} "$HOME/.cargo/bin/sopen""#, h.not_installed());
            let words = remote_words("box", "/x", None, &[], &paths);
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert!(has_line(&out, "arg [/x]"), "{out}");
        }
    }

    #[test]
    fn sopen_on_the_target_finds_the_ffmpeg_next_to_it() {
        for sh in login_shells() {
            let h = host("Darwin", "");
            let session = h.bin.to_str().unwrap().to_string();
            // A brew-style install outside the session's PATH, ffmpeg beside sopen.
            let brew = h.root.join("brew/bin");
            executable(&brew.join("sopen"), &fake_sopen());
            executable(&brew.join("ffmpeg"), "#!/bin/sh\n");
            let paths = quote(brew.join("sopen").to_str().unwrap());
            let words = remote_words("box", "/x", None, &[], &paths);
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            let path = value(&out, "PATH");
            assert!(
                path.starts_with(&format!("{}:{session}", brew.display())),
                "{sh:?}: {path}"
            );
            assert_eq!(value(&out, "ffmpeg"), brew.join("ffmpeg").to_str().unwrap());

            // SOPEN_TARGET_BIN elsewhere: its dir first, and Homebrew's bin dirs that exist are
            // appended after the session's own entries, once each. (`$HOME/.linuxbrew/bin` is
            // the one a test can create; the absolute ones count where this machine has them.)
            let linuxbrew = h.home.join(".linuxbrew/bin");
            std::fs::create_dir_all(&linuxbrew).unwrap();
            let elsewhere = h.root.join("build/sopen-dev");
            executable(&elsewhere, &fake_sopen());
            let env = [format!("SOPEN_TARGET_BIN={}", elsewhere.display())];
            let words = remote_words("box", "/x", None, &env, &h.not_installed());
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            let path = value(&out, "PATH");
            let first = format!("{}:{session}:", h.root.join("build").display());
            let appended = path
                .strip_prefix(&first)
                .unwrap_or_else(|| panic!("{sh:?}: {path}"));
            let fixed = [
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/home/linuxbrew/.linuxbrew/bin",
            ];
            let entries: Vec<&str> = appended.split(':').collect();
            assert_eq!(entries.last(), Some(&linuxbrew.to_str().unwrap()), "{path}");
            for entry in &entries[..entries.len() - 1] {
                assert!(fixed.contains(entry), "{entry} in {path}");
            }

            // Already on the session's PATH: not appended again.
            let session_with = format!("{session}:{}", linuxbrew.display());
            let (code, out, err) = h.run(&sh, &words, &[("PATH", &session_with)]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            let path = value(&out, "PATH");
            assert_eq!(
                path.matches(linuxbrew.to_str().unwrap()).count(),
                1,
                "{path}"
            );
        }
    }

    #[test]
    fn the_script_says_when_sopen_is_missing() {
        for sh in login_shells() {
            let h = host("Darwin", "");
            let words = remote_words("me@box", "/x", None, &[], &h.not_installed());
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
            let words = remote_words("box", "/x", None, &[], INSTALL_PATHS);
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
            ];
            let mut session = format!("EVIL=$(: > {marker})\nPATH=/nowhere\nNOT_DISPLAY=:3\n");
            for (key, val) in imported {
                session.push_str(&format!("{key}={val}\n"));
            }
            std::fs::write(h.root.join("session-env"), session).unwrap();
            executable(&h.bin.join("sopen"), &fake_sopen());
            let words = remote_words("box", "/x", None, &[], INSTALL_PATHS);
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert!(!pwned.exists(), "{sh:?}: a session value was evaluated");
            assert_eq!(h.systemctl_args(), "--user\nshow-environment\n", "{sh:?}");
            for (key, val) in imported {
                assert_eq!(value(&out, key), val, "{sh:?} {key}");
            }
            assert_eq!(value(&out, "EVIL"), "unset");
            assert!(!value(&out, "PATH").contains("/nowhere"), "{out}");

            // A value systemd prints C-quoted (`$'…'`) is left alone.
            let session = "DISPLAY=:7\nXAUTHORITY=$'/a b'\n";
            std::fs::write(h.root.join("session-env"), session).unwrap();
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert_eq!(value(&out, "DISPLAY"), ":7");
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
            let words = remote_words("box", "/x", None, &[], INSTALL_PATHS);
            let runtime = [("XDG_RUNTIME_DIR", run.to_str().unwrap())];
            let (code, out, err) = h.run(&sh, &words, &runtime);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert_eq!(value(&out, "WAYLAND_DISPLAY"), "wayland-3", "{sh:?}");
            assert_eq!(value(&out, "DISPLAY"), "unset");
            assert_eq!(value(&out, "XDG_RUNTIME_DIR"), run.to_str().unwrap());

            // systemd failing (no user manager) is quiet and falls back the same way.
            std::fs::write(h.root.join("session-env"), "").unwrap();
            std::fs::write(h.root.join("systemctl-exit"), "1").unwrap();
            let (code, out, err) = h.run(&sh, &words, &runtime);
            assert_eq!(code, 0, "{sh:?}: {err}");
            assert_eq!(err, "", "{sh:?}");
            assert_eq!(h.systemctl_args(), "--user\nshow-environment\n");
            assert_eq!(value(&out, "WAYLAND_DISPLAY"), "wayland-3", "{sh:?}");

            // A display already set is left alone, and systemd is not asked.
            std::fs::remove_file(h.root.join("systemctl-args")).unwrap();
            let (code, out, _) = h.run(&sh, &words, &[("DISPLAY", ":1")]);
            assert_eq!(code, 0);
            assert_eq!(value(&out, "DISPLAY"), ":1");
            assert_eq!(value(&out, "WAYLAND_DISPLAY"), "unset");
            assert_eq!(h.systemctl_args(), "");
        }
    }
}
