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

/// Where to open, first match wins: `--here` > `--target` > `SOPEN_TARGET` > the config's
/// `target` > this machine (`None`). `config` is called only when nothing before it decided, so a
/// broken config file does not stop `--here`, `--target` or `SOPEN_TARGET`.
pub fn choose(
    here: bool,
    flag: Option<&str>,
    env: Option<&str>,
    config: impl FnOnce() -> Result<Option<String>>,
) -> Result<Option<String>> {
    if here {
        return Ok(None);
    }
    if let Some(host) = flag {
        check_host(host).map_err(|problem| anyhow!("--target: {problem}"))?;
        return Ok(Some(host.to_string()));
    }
    if let Some(host) = env.filter(|h| !h.is_empty()) {
        check_host(host).map_err(|problem| anyhow!("SOPEN_TARGET: {problem}"))?;
        return Ok(Some(host.to_string()));
    }
    config()
}

/// This machine's host name up to the first `.` (`box.local` → `box`).
pub fn short_hostname() -> Option<String> {
    short_name(gethostname::gethostname().to_str()?)
}

fn short_name(name: &str) -> Option<String> {
    let short = name.split('.').next()?;
    (!short.is_empty()).then(|| short.to_string())
}

/// HOST is this machine: its host part (without `user@`) is `localhost` or `short` (this
/// machine's short host name), in any case.
pub fn is_here(host: &str, short: Option<&str>) -> bool {
    let (_, name) = split_user(host);
    name.eq_ignore_ascii_case("localhost") || short.is_some_and(|s| name.eq_ignore_ascii_case(s))
}

/// SOURCE as HOST must be given it:
/// - a file on HOST itself (same host, and no user or HOST's user) → its path there, which HOST
///   opens as a local file (a relative path is under HOST's home: the script starts there);
/// - a file on any other machine → unchanged;
/// - a file on this machine → `<short>:<abs>`, `abs` from `local_abs` (which fails on a missing
///   file exactly as a local open does).
pub fn source_for(
    host: &str,
    source: &str,
    short: Option<&str>,
    local_abs: impl FnOnce(&str) -> Result<String>,
) -> Result<String> {
    match target::parse(source).map_err(anyhow::Error::msg)? {
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
            let short = short.context(
                "cannot read this machine's host name, which the other machine needs to fetch a local file",
            )?;
            Ok(format!("{short}:{}", local_abs(source)?))
        }
    }
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
        r#"exec "$bin" --here "$@""#.to_string(),
    ]
    .join(" ")
}

/// The command for HOST's sshd, as quoted words: `[env K=V…] sh -c <script> sopen [--app APP]
/// -- SOURCE`. The hooks go in front of `sh`, so the script sees `SOPEN_TARGET_BIN` and the
/// sopen it starts sees them all.
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
    words.iter().map(|w| quote(w)).collect()
}

/// `ssh <connection> -t|-T HOST -- <words>`: a terminal on HOST only when this one is one (an
/// `--app` or editor prompt reads it).
pub fn ssh_args(
    connection: Vec<OsString>,
    tty: bool,
    host: &str,
    words: Vec<String>,
) -> Vec<OsString> {
    let mut args = connection;
    args.push(if tty { "-t" } else { "-T" }.into());
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
    let args = ssh_args(connection, tty, host, words);
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

/// HOST's exit code (0, 1, 3, 127 for no sopen there, …) is ours; ssh's own failure (255) is
/// "could not reach".
fn exit_code(host: &str, status: ExitStatus) -> Result<u8> {
    match status.code() {
        Some(255) => bail!("could not reach {host}"),
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
        let none = || Ok(None);
        let cfg = || Ok(Some("c".to_string()));
        let never = || -> Result<Option<String>> { panic!("the config was read") };
        assert_eq!(choose(true, Some("f"), Some("e"), never).unwrap(), None);
        assert_eq!(
            choose(false, Some("f"), Some("e"), never)
                .unwrap()
                .as_deref(),
            Some("f")
        );
        assert_eq!(
            choose(false, None, Some("e"), never).unwrap().as_deref(),
            Some("e")
        );
        assert_eq!(
            choose(false, None, Some(""), cfg).unwrap().as_deref(),
            Some("c")
        );
        assert_eq!(
            choose(false, None, None, cfg).unwrap().as_deref(),
            Some("c")
        );
        assert_eq!(choose(false, None, None, none).unwrap(), None);
        // A broken config only matters when it is consulted.
        let broken = || Err(anyhow!("/c/config.toml: line 1: bad"));
        assert!(choose(false, None, None, broken).is_err());
        assert_eq!(choose(true, None, None, broken).unwrap(), None);
        let e = choose(false, Some("-x"), None, none)
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("--target: "), "{e}");
        let e = choose(false, None, Some("a b"), none)
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("SOPEN_TARGET: "), "{e}");
    }

    #[test]
    fn this_machine() {
        assert_eq!(short_name("Box.local").as_deref(), Some("Box"));
        assert_eq!(short_name("box").as_deref(), Some("box"));
        assert_eq!(short_name(".x"), None);
        assert_eq!(short_name(""), None);
        let short = Some("Box");
        assert!(is_here("box", short));
        assert!(is_here("me@BOX", short));
        assert!(is_here("localhost", None));
        assert!(is_here("me@LocalHost", short));
        assert!(!is_here("box.lan", short));
        assert!(!is_here("other", short));
        assert!(!is_here("box", None));
        assert!(short_hostname().is_none_or(|s| !s.is_empty() && !s.contains('.')));
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
        ];
        for (host, source, want) in cases {
            assert_eq!(
                source_for(host, source, Some("here"), no_local).unwrap(),
                want,
                "{host} {source}"
            );
        }
        let abs = |p: &str| -> Result<String> {
            assert_eq!(p, "./a b.png");
            Ok("/home/u/a b.png".to_string())
        };
        assert_eq!(
            source_for("box", "./a b.png", Some("here"), abs).unwrap(),
            "here:/home/u/a b.png"
        );
        let missing =
            |p: &str| -> Result<String> { Err(anyhow!("{p}: no such file or directory")) };
        let e = source_for("box", "nope.txt", Some("here"), missing).unwrap_err();
        assert_eq!(e.to_string(), "nope.txt: no such file or directory");
        assert!(source_for("box", "a.png", None, |_| Ok("/a.png".into())).is_err());
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
        for sh in shells() {
            let quoted: Vec<String> = words.iter().map(|w| quote(w)).collect();
            let out = Command::new(&sh)
                .arg("-c")
                .arg(format!("printf '[%s]\\n' {}", quoted.join(" ")))
                .output()
                .unwrap();
            assert!(out.status.success(), "{sh}");
            let want: String = words.iter().map(|w| format!("[{w}]\n")).collect();
            assert_eq!(String::from_utf8(out.stdout).unwrap(), want, "{sh}");
        }
    }

    #[test]
    fn ssh_command_line() {
        let connection = ["-S", "/s/cm-x", "-o", "ControlMaster=auto"]
            .map(OsString::from)
            .to_vec();
        let words = remote_words("box", "/tmp/a b.png", Some("ImHex"), &[], INSTALL_PATHS);
        let args = ssh_args(connection.clone(), false, "box", words.clone());
        let mut want = connection.clone();
        want.extend(["-T", "box", "--"].map(OsString::from));
        want.extend(words.iter().map(OsString::from));
        assert_eq!(args, want);
        assert_eq!(words[0], "'sh'");
        assert_eq!(words[1], "'-c'");
        assert_eq!(
            words[3..],
            ["'sopen'", "'--app'", "'ImHex'", "'--'", "'/tmp/a b.png'"]
        );
        let tty = ssh_args(connection, true, "box", vec![]);
        assert_eq!(tty[4], "-t");

        let env = ["SOPEN_LOG=debug".to_string()];
        let words = remote_words("box", "/x", None, &env, INSTALL_PATHS);
        assert_eq!(words[..3], ["'env'", "'SOPEN_LOG=debug'", "'sh'"]);
        assert_eq!(words[words.len() - 3..], ["'sopen'", "'--'", "'/x'"]);

        let script = script("box", INSTALL_PATHS);
        assert!(!script.contains('\n'), "{script}");
        assert!(script.ends_with(r#"exec "$bin" --here "$@""#), "{script}");
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
        assert_eq!(code(127).unwrap(), 127);
        assert_eq!(code(255).unwrap_err().to_string(), "could not reach box");
        assert!(exit_code("box", ExitStatus::from_raw(9)).is_err());
    }

    // -----------------------------------------------------------------------------------------
    // The command run as sshd runs it: the words joined into one string for `sh -c`.

    /// `/bin/sh`, and `/bin/dash` where it exists (the `sh` of Debian and Ubuntu).
    fn shells() -> Vec<String> {
        let mut shells = vec!["/bin/sh".to_string()];
        if Path::new("/bin/dash").exists() {
            shells.push("/bin/dash".to_string());
        }
        shells
    }

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

    /// A stand-in for HOST: a home, and a `bin` with a fake `uname` (reporting `os`) and a fake
    /// `systemctl` (printing `session`).
    fn host(os: &str, session: &str) -> Host {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let bin = root.join("bin");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        executable(&bin.join("uname"), &format!("#!/bin/sh\necho {os}\n"));
        let listing = root.join("session-env");
        std::fs::write(&listing, session).unwrap();
        executable(
            &bin.join("systemctl"),
            &format!("#!/bin/sh\ncat {}\n", quote(listing.to_str().unwrap())),
        );
        Host {
            _tmp: tmp,
            root,
            bin,
            home,
        }
    }

    /// A fake sopen that prints its arguments, where it runs and the variables a run depends on.
    fn fake_sopen() -> String {
        let mut body = String::from(
            "#!/bin/sh\nfor a in \"$@\"; do printf 'arg [%s]\\n' \"$a\"; done\nprintf 'pwd %s\\n' \"$PWD\"\n\
             printf 'PATH [%s]\\n' \"$PATH\"\nprintf 'ffmpeg [%s]\\n' \"$(command -v ffmpeg)\"\n",
        );
        for var in [
            "SOPEN_STATE",
            "SOPEN_LOG",
            "SOPEN_TARGET_BIN",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XAUTHORITY",
            "XDG_RUNTIME_DIR",
            "XDG_SESSION_TYPE",
            "EVIL",
        ] {
            body.push_str(&format!("printf '{var} [%s]\\n' \"${{{var}-unset}}\"\n"));
        }
        body
    }

    /// `out` has exactly this line.
    fn has_line(out: &str, line: &str) -> bool {
        out.lines().any(|l| l == line)
    }

    impl Host {
        /// Runs `words` the way sshd does: joined with spaces, one string for the login shell.
        fn run(
            &self,
            sh: &str,
            words: &[String],
            extra_env: &[(&str, &str)],
        ) -> (i32, String, String) {
            let mut cmd = Command::new(sh);
            cmd.arg("-c")
                .arg(words.join(" "))
                .env_clear()
                .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
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
    }

    #[test]
    fn the_script_runs_sopen_here_with_the_source_and_hooks() {
        let source = r#"it's a "b" $HOME `id` \x.png"#;
        let env = [
            "SOPEN_STATE=rect=1,2,3,4;op=crop|pixelize".to_string(),
            "SOPEN_LOG=debug".to_string(),
        ];
        for sh in shells() {
            let h = host("Darwin", "");
            executable(&h.bin.join("sopen"), &fake_sopen());
            let words = remote_words("box", source, Some("Hex Fiend"), &env, INSTALL_PATHS);
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh}: {err}");
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
                "{sh}"
            );
            assert!(
                has_line(&out, &format!("pwd {}", h.home.display())),
                "{sh}: {out}"
            );
            assert!(
                has_line(&out, "SOPEN_STATE [rect=1,2,3,4;op=crop|pixelize]"),
                "{out}"
            );
            assert!(has_line(&out, "SOPEN_LOG [debug]"), "{out}");
            // Not Linux: no display variables are made up.
            assert!(has_line(&out, "DISPLAY [unset]"), "{out}");
            assert!(has_line(&out, "WAYLAND_DISPLAY [unset]"), "{out}");
        }
    }

    #[test]
    fn the_script_finds_sopen_by_target_bin_or_install_path() {
        for sh in shells() {
            let h = host("Darwin", "");
            // Not on PATH and not installed: only SOPEN_TARGET_BIN names it.
            let elsewhere = h.root.join("build/sopen-dev");
            executable(&elsewhere, &fake_sopen());
            let env = [format!("SOPEN_TARGET_BIN={}", elsewhere.display())];
            let words = remote_words("box", "/x", None, &env, "/nonexistent-sopen-test/sopen");
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh}: {err}");
            assert!(has_line(&out, "arg [--here]"), "{out}");
            assert!(
                has_line(&out, &format!("SOPEN_TARGET_BIN [{}]", elsewhere.display())),
                "{out}"
            );

            // Not on PATH: the install paths, in order, `$HOME` expanded on HOST. (Not the
            // production list: a sopen installed on the test machine would be found first.)
            executable(&h.home.join(".cargo/bin/sopen"), &fake_sopen());
            let paths = r#"/nonexistent-sopen-test/sopen "$HOME/.cargo/bin/sopen""#;
            let words = remote_words("box", "/x", None, &[], paths);
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh}: {err}");
            assert!(has_line(&out, "arg [/x]"), "{out}");
        }
    }

    /// The `[…]` value of the `name [value]` line the fake sopen printed.
    fn value<'a>(out: &'a str, name: &str) -> &'a str {
        out.lines()
            .find_map(|l| l.strip_prefix(&format!("{name} ["))?.strip_suffix(']'))
            .unwrap_or_else(|| panic!("no {name} line in {out}"))
    }

    #[test]
    fn sopen_on_the_target_finds_the_ffmpeg_next_to_it() {
        for sh in shells() {
            let h = host("Darwin", "");
            // A brew-style install outside the session's minimal PATH, ffmpeg beside sopen.
            let brew = h.root.join("brew/bin");
            executable(&brew.join("sopen"), &fake_sopen());
            executable(&brew.join("ffmpeg"), "#!/bin/sh\n");
            let paths = quote(brew.join("sopen").to_str().unwrap());
            let words = remote_words("box", "/x", None, &[], &paths);
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh}: {err}");
            let session = format!("{}:/usr/bin:/bin", h.bin.display());
            let path = value(&out, "PATH");
            assert!(
                path.starts_with(&format!("{}:{session}", brew.display())),
                "{sh}: {path}"
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
            let words = remote_words("box", "/x", None, &env, "/nonexistent-sopen-test/sopen");
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh}: {err}");
            let path = value(&out, "PATH");
            let first = format!("{}:{session}:", h.root.join("build").display());
            let appended = path
                .strip_prefix(&first)
                .unwrap_or_else(|| panic!("{sh}: {path}"));
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
            let mut cmd_words = words.clone();
            cmd_words.insert(0, format!("PATH={}", quote(&session_with)));
            let (code, out, err) = h.run(&sh, &cmd_words, &[]);
            assert_eq!(code, 0, "{sh}: {err}");
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
        for sh in shells() {
            let h = host("Darwin", "");
            let words = remote_words("me@box", "/x", None, &[], "/nonexistent-sopen-test/sopen");
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 127, "{sh}: {out} {err}");
            assert_eq!(
                err, "sopen is not installed on me@box (brew install 2lab-ai/tap/sopen-preview)\n",
                "{sh}"
            );
            assert_eq!(out, "", "{sh}");
        }
    }

    #[test]
    fn linux_without_a_display_takes_the_session_display() {
        for sh in shells() {
            let h = host("Linux", "");
            let pwned = h.root.join("pwned");
            // A value with command substitutions, in a name that is imported: kept as text.
            let hostile = format!("x $(touch {p}) `touch {p}`", p = pwned.display());
            let session = format!(
                "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1/bus\nDISPLAY=:7\n\
                 EVIL=$(touch {p})\nPATH=/nowhere\nWAYLAND_DISPLAY=wayland-9\n\
                 XAUTHORITY=$'/a b'\nXDG_SESSION_TYPE={hostile}\nNOT_DISPLAY=:3\n",
                p = pwned.display()
            );
            std::fs::write(h.root.join("session-env"), session).unwrap();
            executable(&h.bin.join("sopen"), &fake_sopen());
            let words = remote_words("box", "/x", None, &[], INSTALL_PATHS);
            let (code, out, err) = h.run(&sh, &words, &[]);
            assert_eq!(code, 0, "{sh}: {err}");
            assert!(has_line(&out, "DISPLAY [:7]"), "{sh}: {out}");
            assert!(has_line(&out, "WAYLAND_DISPLAY [wayland-9]"), "{out}");
            assert!(
                has_line(&out, &format!("XDG_SESSION_TYPE [{hostile}]")),
                "{out}"
            );
            assert!(
                has_line(&out, "XAUTHORITY [unset]"),
                "C-quoted value skipped: {out}"
            );
            assert!(has_line(&out, "EVIL [unset]"), "{out}");
            assert!(!pwned.exists(), "{sh}: a session value was evaluated");
            assert!(has_line(&out, "arg [--here]"), "PATH kept: {out}");
        }
    }

    #[test]
    fn linux_without_any_session_uses_the_wayland_socket() {
        for sh in shells() {
            let h = host("Linux", "");
            executable(&h.bin.join("sopen"), &fake_sopen());
            let run = h.root.join("run");
            std::fs::create_dir_all(&run).unwrap();
            std::fs::write(run.join("wayland-0.lock"), "").unwrap();
            let _socket = std::os::unix::net::UnixListener::bind(run.join("wayland-3")).unwrap();
            let words = remote_words("box", "/x", None, &[], INSTALL_PATHS);
            let runtime = [("XDG_RUNTIME_DIR", run.to_str().unwrap())];
            let (code, out, err) = h.run(&sh, &words, &runtime);
            assert_eq!(code, 0, "{sh}: {err}");
            assert!(has_line(&out, "WAYLAND_DISPLAY [wayland-3]"), "{sh}: {out}");
            assert!(has_line(&out, "DISPLAY [unset]"), "{out}");
            assert!(
                has_line(&out, &format!("XDG_RUNTIME_DIR [{}]", run.display())),
                "{out}"
            );

            // A display already set is left alone.
            let (code, out, _) = h.run(&sh, &words, &[("DISPLAY", ":1")]);
            assert_eq!(code, 0);
            assert!(has_line(&out, "DISPLAY [:1]"), "{out}");
            assert!(has_line(&out, "WAYLAND_DISPLAY [unset]"), "{out}");
        }
    }
}
