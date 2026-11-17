//! Folder/text editors (spec §3 A2): VS Code first, then Zed. Locate the CLI, build its
//! arguments, spawn it detached. When neither is present, text falls back to `$VISUAL` /
//! `$EDITOR` in the terminal.

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

/// The VS Code CLI, or `None`. `SOPEN_CODE` overrides the search (an empty value = "no VS Code").
pub fn locate_code() -> Option<PathBuf> {
    if let Some(forced) = std::env::var_os("SOPEN_CODE") {
        return (!forced.is_empty()).then(|| PathBuf::from(forced));
    }
    locate_code_in(
        Os::current(),
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").as_deref(),
        is_executable,
    )
}

fn locate_code_in(
    os: Os,
    path_var: Option<&OsStr>,
    home: Option<&OsStr>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    for dir in path_var.map(std::env::split_paths).into_iter().flatten() {
        let candidate = dir.join("code");
        if exists(&candidate) {
            return Some(candidate);
        }
    }
    if os == Os::MacOs {
        let bundle = "Visual Studio Code.app/Contents/Resources/app/bin/code";
        let system = Path::new("/Applications").join(bundle);
        if exists(&system) {
            return Some(system);
        }
        if let Some(home) = home {
            let user = Path::new(home).join("Applications").join(bundle);
            if exists(&user) {
                return Some(user);
            }
        }
    }
    None
}

/// The editor a folder or text file opens in.
#[derive(Debug, PartialEq, Eq)]
pub enum Editor {
    Code(PathBuf),
    /// `code_cannot_remote`: VS Code is installed but has no Remote - SSH for this remote path.
    Zed {
        cli: PathBuf,
        code_cannot_remote: bool,
    },
    Neither {
        code_cannot_remote: bool,
    },
}

/// VS Code when found (a remote path also needs its Remote - SSH extension), else Zed.
pub fn choose(
    remote: bool,
    code: Option<PathBuf>,
    remote_ssh: impl FnOnce() -> bool,
    zed: impl FnOnce() -> Option<PathBuf>,
) -> Editor {
    let code_cannot_remote = match code {
        Some(code) if !remote || remote_ssh() => return Editor::Code(code),
        Some(_) => true,
        None => false,
    };
    match zed() {
        Some(cli) => Editor::Zed {
            cli,
            code_cannot_remote,
        },
        None => Editor::Neither { code_cannot_remote },
    }
}

/// Whether Remote - SSH is in VS Code's extensions directory: `$VSCODE_EXTENSIONS` when set (VS
/// Code's own override, also the test hook), else `~/.vscode/extensions`. Read from disk, without
/// starting VS Code.
pub fn remote_ssh_installed() -> bool {
    extensions_dir(
        std::env::var_os("VSCODE_EXTENSIONS"),
        std::env::var_os("HOME"),
    )
    .is_some_and(|dir| remote_ssh_in(&dir))
}

/// `$VSCODE_EXTENSIONS` when non-empty, else `$HOME/.vscode/extensions`.
fn extensions_dir(var: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    match var.filter(|d| !d.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => home.map(|h| Path::new(&h).join(".vscode").join("extensions")),
    }
}

/// Whether `dir` (a VS Code extensions directory) holds Remote - SSH: a directory
/// `ms-vscode-remote.remote-ssh-<version>` that is not a key of `.obsolete` (a JSON object of
/// uninstalled, not yet removed directories). `remote-ssh-edit-<version>` is a different extension.
pub fn remote_ssh_in(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let obsolete: serde_json::Map<String, serde_json::Value> = std::fs::read(dir.join(".obsolete"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return false;
        };
        let is_remote_ssh = name
            .to_ascii_lowercase()
            .strip_prefix("ms-vscode-remote.remote-ssh-")
            .is_some_and(|version| version.starts_with(|c: char| c.is_ascii_digit()));
        is_remote_ssh && entry.path().is_dir() && !obsolete.contains_key(name)
    })
}

/// `code` arguments. A local path is passed as it is (VS Code stats it). A remote one is a
/// `vscode-remote://ssh-remote+[user@]host/<path>` URI behind `--folder-uri` or `--file-uri`:
/// `--remote` with a bare path would make VS Code guess the kind from the name (a dot = file).
/// VS Code percent-decodes the URI path, so it is encoded the way [`remote_url`] encodes it.
pub fn code_args(remote: Option<(Option<&str>, &str)>, path: &str, is_dir: bool) -> Vec<OsString> {
    let Some((user, host)) = remote else {
        return vec![OsString::from(path)];
    };
    let user = user.map(|u| format!("{u}@")).unwrap_or_default();
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let flag = if is_dir { "--folder-uri" } else { "--file-uri" };
    let uri = format!(
        "vscode-remote://ssh-remote+{user}{host}{}",
        encode_path(&path)
    );
    vec![OsString::from(flag), OsString::from(uri)]
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `ssh://[user@]host/abs/path` (IPv6 hosts bracketed). `abs_path` comes from SFTP realpath.
/// Zed percent-decodes the path, so it is percent-encoded here: a literal `%41` would otherwise
/// open `A`, `#` and `?` would start a fragment or query, and a URL parser drops trailing spaces
/// and tabs.
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
    format!("ssh://{user}{host}{}", encode_path(&path))
}

/// RFC 3986 path characters stay (unreserved, sub-delims, `:`, `@`, `/`); every other byte of
/// the UTF-8 path becomes `%XX`.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        let keep = b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&b);
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Spawns `zed <arg>` in its own process group with no stdio, so it outlives sopen and the
/// terminal's Ctrl-C. Waits briefly: the CLI hands off to the app and exits; a quick non-zero
/// exit is reported. Returns the command line that was run (for the debug log and receipts).
pub fn open(zed: &Path, arg: &OsStr) -> Result<String> {
    open_args(zed, &[arg.to_os_string()])
}

/// Spawns an editor with argv-only arguments, without shell interpolation.
pub fn open_args(editor: &Path, args: &[OsString]) -> Result<String> {
    use std::os::unix::process::CommandExt;
    let line = crate::render_command(editor.as_os_str(), args);
    crate::debug!("spawn: {line}");
    let mut child = Command::new(editor)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .with_context(|| format!("cannot start {}", editor.display()))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("{} exited with {status}", editor.display());
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
    fn code_discovery_and_arguments() {
        let path = std::env::join_paths(["/a", "/b"]).unwrap();
        let bundle = "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code";
        let user = "/home/test/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code";
        assert_eq!(
            locate_code_in(
                Os::MacOs,
                Some(&path),
                Some(OsStr::new("/home/test")),
                |p| p == Path::new("/b/code") || p == Path::new(bundle)
            ),
            Some(PathBuf::from("/b/code"))
        );
        assert_eq!(
            locate_code_in(Os::MacOs, None, Some(OsStr::new("/home/test")), |p| p
                == Path::new(bundle)),
            Some(PathBuf::from(bundle))
        );
        assert_eq!(
            locate_code_in(Os::MacOs, None, Some(OsStr::new("/home/test")), |p| p
                == Path::new(user)),
            Some(PathBuf::from(user))
        );
        assert_eq!(
            locate_code_in(Os::Linux, None, Some(OsStr::new("/home/test")), |p| p
                == Path::new(user)),
            None
        );
        // A local path is passed as it is: VS Code stats it.
        for path in ["/folder # % 한글", "/folder # % 한글/file $(touch x).txt"] {
            assert_eq!(code_args(None, path, true), [OsString::from(path)]);
            assert_eq!(code_args(None, path, false), [OsString::from(path)]);
        }
    }

    #[test]
    fn remote_code_args_name_the_kind() {
        let os = OsString::from;
        let remote = Some((Some("user"), "alias"));
        // A bare `--remote` path's kind is guessed from its name (a dot = file); the URI flags
        // say it: a folder named like a file, a file without an extension.
        assert_eq!(
            code_args(remote, "/srv/proj.v2", true),
            [
                os("--folder-uri"),
                os("vscode-remote://ssh-remote+user@alias/srv/proj.v2")
            ]
        );
        assert_eq!(
            code_args(Some((None, "alias")), "/srv/Makefile", false),
            [
                os("--file-uri"),
                os("vscode-remote://ssh-remote+alias/srv/Makefile")
            ]
        );
        // VS Code percent-decodes the URI path: space, `#`, `?`, `%` and UTF-8 are encoded.
        assert_eq!(
            code_args(remote, "/t/my dir #1 100%41 한글", true),
            [
                os("--folder-uri"),
                os("vscode-remote://ssh-remote+user@alias/t/my%20dir%20%231%20100%2541%20%ED%95%9C%EA%B8%80")
            ]
        );
        assert_eq!(
            code_args(remote, "/t/a?.txt", false),
            [
                os("--file-uri"),
                os("vscode-remote://ssh-remote+user@alias/t/a%3F.txt")
            ]
        );
    }

    #[test]
    fn extensions_dir_falls_back_when_unset_or_empty() {
        let home = || Some(OsString::from("/home/u"));
        let default = Some(PathBuf::from("/home/u/.vscode/extensions"));
        assert_eq!(
            extensions_dir(Some(OsString::from("/x/ext")), home()),
            Some(PathBuf::from("/x/ext"))
        );
        assert_eq!(extensions_dir(Some(OsString::new()), home()), default);
        assert_eq!(extensions_dir(None, home()), default);
        assert_eq!(extensions_dir(None, None), None);
    }

    #[test]
    fn remote_without_remote_ssh_falls_back_to_zed() {
        let code = || Some(PathBuf::from("/bin/code"));
        let zed = || Some(PathBuf::from("/bin/zed"));
        assert_eq!(
            choose(true, code(), || true, zed),
            Editor::Code(PathBuf::from("/bin/code"))
        );
        assert_eq!(
            choose(true, code(), || false, zed),
            Editor::Zed {
                cli: PathBuf::from("/bin/zed"),
                code_cannot_remote: true
            }
        );
        assert_eq!(
            choose(true, code(), || false, || None),
            Editor::Neither {
                code_cannot_remote: true
            }
        );
        // A local path never needs the extension (and does not look for it).
        let unused = || -> bool { panic!("extensions read for a local path") };
        assert_eq!(
            choose(false, code(), unused, zed),
            Editor::Code(PathBuf::from("/bin/code"))
        );
        assert_eq!(
            choose(true, None, || true, zed),
            Editor::Zed {
                cli: PathBuf::from("/bin/zed"),
                code_cannot_remote: false
            }
        );
        assert_eq!(
            choose(false, None, || true, || None),
            Editor::Neither {
                code_cannot_remote: false
            }
        );
    }

    #[test]
    fn remote_ssh_extension_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert!(!remote_ssh_in(&dir.join("missing")));
        assert!(!remote_ssh_in(dir));
        // Remote - SSH: Editing Configuration Files shares the prefix; it is not Remote - SSH.
        std::fs::create_dir(dir.join("ms-vscode-remote.remote-ssh-edit-0.87.0")).unwrap();
        assert!(!remote_ssh_in(dir));
        std::fs::create_dir(dir.join("ms-vscode-remote.remote-ssh-0.120.0")).unwrap();
        assert!(remote_ssh_in(dir));
        // Uninstalled but not yet removed: VS Code lists it in `.obsolete`.
        std::fs::write(
            dir.join(".obsolete"),
            r#"{"ms-vscode-remote.remote-ssh-0.120.0":true}"#,
        )
        .unwrap();
        assert!(!remote_ssh_in(dir));
        // Only `.obsolete`'s keys count, not a value that happens to spell the name.
        std::fs::write(
            dir.join(".obsolete"),
            r#"{"other":"ms-vscode-remote.remote-ssh-0.120.0"}"#,
        )
        .unwrap();
        assert!(remote_ssh_in(dir));
        // An unreadable `.obsolete` marks nothing obsolete.
        std::fs::write(dir.join(".obsolete"), "not json").unwrap();
        assert!(remote_ssh_in(dir));
    }

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
    fn url_paths_are_percent_encoded() {
        let cases = [
            ("/t/issue#12.md", "ssh://box/t/issue%2312.md"),
            ("/t/a?.txt", "ssh://box/t/a%3F.txt"),
            ("/t/100%41.txt", "ssh://box/t/100%2541.txt"),
            ("/t/my notes .txt", "ssh://box/t/my%20notes%20.txt"),
            ("/t/한글.md", "ssh://box/t/%ED%95%9C%EA%B8%80.md"),
            ("/t/tab\there", "ssh://box/t/tab%09here"),
            (
                "/srv/a-b_c.d~e/(x)+y=z,@:'!$&*;",
                "ssh://box/srv/a-b_c.d~e/(x)+y=z,@:'!$&*;",
            ),
        ];
        for (path, want) in cases {
            assert_eq!(remote_url(None, "box", path), want, "{path}");
        }
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
