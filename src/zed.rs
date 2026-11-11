//! Zed as the folder/text editor (spec §3 A2): locate the CLI, build the `ssh://` URL, spawn it
//! detached. When Zed is absent, text falls back to `$VISUAL` / `$EDITOR` in the terminal.

use anyhow::{bail, Context, Result};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAC_APP_CLI: &str = "/Applications/Zed.app/Contents/MacOS/cli";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

/// Names searched on PATH, in order, then fixed locations (macOS app bundle CLI).
pub fn candidates(os: Os) -> (&'static [&'static str], &'static [&'static str]) {
    match os {
        Os::MacOs => (&["zed"], &[MAC_APP_CLI]),
        Os::Linux => (&["zeditor", "zed"], &[]),
    }
}

/// The Zed CLI, or `None`. `SOPEN_ZED` overrides the search (an empty value = "no Zed").
pub fn locate() -> Option<PathBuf> {
    if let Some(forced) = std::env::var_os("SOPEN_ZED") {
        return (!forced.is_empty()).then(|| PathBuf::from(forced));
    }
    locate_in(Os::current(), std::env::var_os("PATH").as_deref(), |p| {
        is_executable(p)
    })
}

pub fn locate_in(
    os: Os,
    path_var: Option<&OsStr>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let (names, fixed) = candidates(os);
    for name in names {
        for dir in path_var.map(std::env::split_paths).into_iter().flatten() {
            let candidate = dir.join(name);
            if exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    fixed.iter().map(PathBuf::from).find(|p| exists(p))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `ssh://[user@]host/abs/path` (IPv6 hosts bracketed). `abs_path` comes from SFTP realpath.
pub fn remote_url(user: Option<&str>, host: &str, abs_path: &str) -> String {
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let user = user.map(|u| format!("{u}@")).unwrap_or_default();
    let path = if abs_path.starts_with('/') {
        abs_path.to_string()
    } else {
        format!("/{abs_path}")
    };
    format!("ssh://{user}{host}{path}")
}

/// Spawns `zed <arg>` in its own process group with no stdio, so it outlives sopen and the
/// terminal's Ctrl-C. Waits briefly: the CLI hands off to the app and exits; a quick non-zero
/// exit is reported. Returns the command line that was run (for the debug log and receipts).
pub fn open(zed: &Path, arg: &OsStr) -> Result<String> {
    use std::os::unix::process::CommandExt;
    let args = vec![arg.to_os_string()];
    let line = crate::render_command(zed.as_os_str(), &args);
    crate::debug!("spawn: {line}");
    let mut child = Command::new(zed)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .with_context(|| format!("cannot start {}", zed.display()))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("{} exited with {status}", zed.display());
            }
            return Ok(line);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(line)
}

/// `$VISUAL`, then `$EDITOR`, then `vi`.
pub fn terminal_editor() -> OsString {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| OsString::from("vi"))
}

/// Runs the terminal editor on `file` and waits. The editor value may carry arguments
/// (`code -w`), so it goes through `sh -c '<editor> "$@"'` the way git runs `$EDITOR`.
pub fn run_terminal_editor(file: &Path) -> Result<()> {
    let editor = terminal_editor();
    let mut script = editor.clone();
    script.push(" \"$@\"");
    let status = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .arg(&editor)
        .arg(file)
        .status()
        .with_context(|| format!("cannot run {}", editor.to_string_lossy()))?;
    if !status.success() {
        bail!("{} exited with {status}", editor.to_string_lossy());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(remote_url(None, "box", "/srv/app"), "ssh://box/srv/app");
        assert_eq!(
            remote_url(Some("me"), "box", "/home/me/a.rs"),
            "ssh://me@box/home/me/a.rs"
        );
        assert_eq!(remote_url(None, "::1", "/tmp/x"), "ssh://[::1]/tmp/x");
        assert_eq!(remote_url(None, "box", "rel"), "ssh://box/rel");
    }

    #[test]
    fn locate_order() {
        let path = std::env::join_paths(["/a", "/b"]).unwrap();
        let only = |hit: &'static str| move |p: &Path| p == Path::new(hit);
        assert_eq!(
            locate_in(Os::MacOs, Some(&path), only("/b/zed")),
            Some(PathBuf::from("/b/zed"))
        );
        assert_eq!(
            locate_in(Os::MacOs, Some(&path), only(MAC_APP_CLI)),
            Some(PathBuf::from(MAC_APP_CLI))
        );
        assert_eq!(locate_in(Os::MacOs, None, only("/a/zed")), None);
        // Linux prefers `zeditor` anywhere on PATH over `zed`.
        let both = |p: &Path| p == Path::new("/a/zed") || p == Path::new("/b/zeditor");
        assert_eq!(
            locate_in(Os::Linux, Some(&path), both),
            Some(PathBuf::from("/b/zeditor"))
        );
        assert_eq!(locate_in(Os::Linux, Some(&path), only(MAC_APP_CLI)), None);
    }
}
