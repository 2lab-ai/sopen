//! Detection (03 §2): what a folder is to git ([`super::model::Detection`]), from one bounded
//! `rev-parse` run in it, cached per `(host, folder)` for the window's lifetime; which git, and
//! the 2.20 floor (§2.4). S2-B writes it.
//!
//! [`Detector`] holds the cache and runs detection through [`Git`]; [`Detection::reason`] is
//! the line that says why a folder keeps its directory list; [`which_git`] is §2.4's local git.

use super::history::{argv, first_line, Bound, Git, Ran, RunError};
use super::model::*;
use super::parse;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The version floor (03 §2.4).
pub const FLOOR: (u32, u32) = (2, 20);

/// A remote's wrapper found no git (03 §2.4: `sopen: no git` on stderr, exit 127).
fn no_git(ran: &Ran) -> bool {
    ran.code == Some(127) && String::from_utf8_lossy(&ran.stderr).contains("sopen: no git")
}

fn unavailable(e: RunError) -> Detection {
    Detection::Unavailable(match e {
        RunError::NoGit => Unavailable::NoGit,
        RunError::TimedOut => Unavailable::TimedOut,
        RunError::Failed(e) => Unavailable::Failed(e),
    })
}

/// Detection's rev-parse in `folder` and what it says, without the cache or the version check.
pub fn run(git: &dyn Git, folder: &str) -> Detection {
    match git.run(folder, &argv::detection(), &[], Bound::Detection) {
        Ok(ran) if no_git(&ran) => Detection::Unavailable(Unavailable::NoGit),
        Ok(ran) => parse::detection(ran.code, &ran.stdout, &ran.stderr, folder),
        Err(e) => unavailable(e),
    }
}

/// What a host's git is, once asked (03 §2.4).
#[derive(Debug, Clone, PartialEq, Eq)]
enum HostGit {
    Missing,
    Version(GitVersion),
    /// `git version` printed something without a version: detection goes on.
    Unknown,
}

/// `git version` in `folder`, or the detection result that ends detection here.
fn ask_version(git: &dyn Git, folder: &str) -> Result<HostGit, Detection> {
    match git.run(folder, &argv::version(), &[], Bound::Detection) {
        Ok(ran) if no_git(&ran) => Err(Detection::Unavailable(Unavailable::NoGit)),
        Ok(ran) if ran.code == Some(0) => Ok(parse::git_version(&ran.stdout)
            .map(HostGit::Version)
            .unwrap_or(HostGit::Unknown)),
        Ok(ran) => {
            let line = first_line(&ran.stderr);
            let why = match (ran.code, line.is_empty()) {
                (_, false) => line,
                (Some(c), true) => format!("git version exited with {c}"),
                (None, true) => "git version ended without an exit status".into(),
            };
            Err(Detection::Unavailable(Unavailable::Failed(why)))
        }
        Err(e) => Err(unavailable(e)),
    }
}

#[derive(Debug, Default)]
struct State {
    folders: HashMap<(String, String), Detection>,
    hosts: HashMap<String, HostGit>,
}

/// Detection with its cache (03 §2.2): results per `(host, folder)` for the window's lifetime, a
/// missing git and the version per host. Shared by the browser's workers: git runs outside the
/// lock. A `Failed` result is not cached (the next visit asks again).
#[derive(Debug, Default)]
pub struct Detector {
    state: Mutex<State>,
}

impl Detector {
    pub fn new() -> Self {
        Self::default()
    }

    /// What `folder` on `host` is to git: the cached result, else `git version` (once per host)
    /// and detection's rev-parse, through `git`.
    pub fn detect(&self, host: &str, folder: &str, git: &dyn Git) -> Detection {
        let key = (host.to_string(), folder.to_string());
        let known = {
            let state = self.state.lock().unwrap();
            if let Some(found) = state.folders.get(&key) {
                return found.clone();
            }
            state.hosts.get(host).cloned()
        };
        let version = match known {
            Some(v) => v,
            None => match ask_version(git, folder) {
                Ok(v) => {
                    self.state
                        .lock()
                        .unwrap()
                        .hosts
                        .insert(host.to_string(), v.clone());
                    v
                }
                Err(ended) => return self.keep(key, ended),
            },
        };
        let found = match version {
            HostGit::Missing => return Detection::Unavailable(Unavailable::NoGit),
            HostGit::Version(v) if !v.at_least(FLOOR.0, FLOOR.1) => {
                Detection::Unavailable(Unavailable::TooOld(v.text))
            }
            HostGit::Version(_) | HostGit::Unknown => run(git, folder),
        };
        self.keep(key, found)
    }

    /// Caches `found` as 03 §2.2 says (a missing git per host too) and hands it back.
    fn keep(&self, key: (String, String), found: Detection) -> Detection {
        let mut state = self.state.lock().unwrap();
        match &found {
            Detection::Unavailable(Unavailable::NoGit) => {
                state.hosts.insert(key.0.clone(), HostGit::Missing);
            }
            Detection::Unavailable(Unavailable::Failed(_)) => return found,
            _ => {}
        }
        state.folders.insert(key, found.clone());
        found
    }

    /// The cached result, without running anything.
    pub fn cached(&self, host: &str, folder: &str) -> Option<Detection> {
        let state = self.state.lock().unwrap();
        state
            .folders
            .get(&(host.to_string(), folder.to_string()))
            .cloned()
    }

    /// The host's `git version`, once asked.
    pub fn version(&self, host: &str) -> Option<GitVersion> {
        match self.state.lock().unwrap().hosts.get(host) {
            Some(HostGit::Version(v)) => Some(v.clone()),
            _ => None,
        }
    }

    /// Drops the folder's result (Cmd/Ctrl+R in the list, 03 §2.2), so the re-read detects
    /// again — and, when the host's git is what kept the panel away (missing, or below the
    /// floor), what is known of it too: git may have been installed since. A usable version
    /// stays known (`git version` runs once per host, 03 §2.4).
    pub fn forget(&self, host: &str, folder: &str) {
        let mut state = self.state.lock().unwrap();
        state
            .folders
            .remove(&(host.to_string(), folder.to_string()));
        let blocked = match state.hosts.get(host) {
            Some(HostGit::Missing) => true,
            Some(HostGit::Version(v)) => !v.at_least(FLOOR.0, FLOOR.1),
            _ => false,
        };
        if blocked {
            state.hosts.remove(host);
        }
    }

    /// Drops what is known of the host's git (the git panel's hard refresh, 03 §5: detection
    /// and `git version` again); call [`Detector::forget`] for the folder as well.
    pub fn forget_host(&self, host: &str) {
        self.state.lock().unwrap().hosts.remove(host);
    }
}

/// A line git printed, without git's `fatal: `.
fn plain(line: &str) -> &str {
    line.strip_prefix("fatal: ").unwrap_or(line)
}

impl Detection {
    /// Only a work tree shows the git panel (03 §2.3).
    pub fn shows_panel(&self) -> bool {
        matches!(self, Detection::WorkTree { .. })
    }

    /// The line that says why the folder keeps its directory list (`None` for a work tree);
    /// `host` names where git ran. 03 §2.3's status-row texts, and G-D1's reasons.
    pub fn reason(&self, host: &str) -> Option<String> {
        let why = match self {
            Detection::WorkTree { .. } => return None,
            Detection::NoWorkTree(NoWorkTree::Bare) => "a bare repository, not a work tree".into(),
            Detection::NoWorkTree(NoWorkTree::GitDir) => {
                "inside a .git directory, not a work tree".into()
            }
            Detection::NotARepo(None) => "not a repository".into(),
            Detection::NotARepo(Some(line)) => format!("not a repository: {}", plain(line)),
            Detection::Refused(line) => format!("git refuses this repository: {}", plain(line)),
            Detection::Unavailable(Unavailable::NoGit) => format!("no git on {host}"),
            Detection::Unavailable(Unavailable::TooOld(v)) => {
                format!("git {v} on {host}; it needs {}.{}", FLOOR.0, FLOOR.1)
            }
            Detection::Unavailable(Unavailable::TimedOut) => "git did not answer in 10 s".into(),
            Detection::Unavailable(Unavailable::NotUtf8(top)) => {
                format!("the repository's path is not UTF-8: {top}")
            }
            Detection::Unavailable(Unavailable::Failed(e)) => plain(e).to_string(),
        };
        Some(format!("git panel: {why}"))
    }

    /// The reason goes to the status row, once per folder: `Refused`, `Unavailable` and
    /// `NotARepo` at a filesystem boundary (03 §2.3).
    pub fn on_status_row(&self) -> bool {
        matches!(
            self,
            Detection::Refused(_) | Detection::Unavailable(_) | Detection::NotARepo(Some(_))
        )
    }

    /// What follows the top level in the panel's frame title (03 §2.3): ` (worktree of <main top
    /// level>)`, ` (submodule of <superproject>)`, ` (shallow)`; empty when none applies. The
    /// main top level is the common dir's folder (the common dir itself when the main
    /// repository is bare).
    pub fn title_notes(&self) -> String {
        let Detection::WorkTree {
            common_dir,
            linked,
            submodule_of,
            shallow,
            ..
        } = self
        else {
            return String::new();
        };
        let mut notes = String::new();
        if *linked {
            let main = common_dir.strip_suffix("/.git").unwrap_or(common_dir);
            notes.push_str(&format!(" (worktree of {main})"));
        }
        if let Some(superproject) = submodule_of {
            notes.push_str(&format!(" (submodule of {superproject})"));
        }
        if *shallow {
            notes.push_str(" (shallow)");
        }
        notes
    }
}

/// The local git (03 §2.4) from `SOPEN_GIT` and `PATH` as given: `SOPEN_GIT` when set (an empty
/// value is ignored; a path that does not exist is the runner's `NoGit`), else the first `git` on
/// `path`. On macOS (`macos`), `/usr/bin/git` is the developer-tools shim: it counts only when
/// `xcode_select_ok` (`xcode-select -p` succeeds), so sopen never makes macOS offer the tools.
pub fn local_git(
    sopen_git: Option<&OsStr>,
    path: Option<&OsStr>,
    macos: bool,
    xcode_select_ok: impl FnOnce() -> bool,
) -> Result<PathBuf, Unavailable> {
    if let Some(chosen) = sopen_git.filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(chosen));
    }
    let found = std::env::split_paths(path.ok_or(Unavailable::NoGit)?)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join("git"))
        .find(|candidate| executable(candidate))
        .ok_or(Unavailable::NoGit)?;
    if macos && found == Path::new("/usr/bin/git") && !xcode_select_ok() {
        return Err(Unavailable::NoGit);
    }
    Ok(found)
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// [`local_git`] for this process: `SOPEN_GIT`, `PATH`, this platform.
pub fn which_git() -> Result<PathBuf, Unavailable> {
    local_git(
        std::env::var_os("SOPEN_GIT").as_deref(),
        std::env::var_os("PATH").as_deref(),
        cfg!(target_os = "macos"),
        xcode_tools_present,
    )
}

/// `xcode-select -p` succeeds: the developer tools are installed, so `/usr/bin/git` runs git.
pub fn xcode_tools_present() -> bool {
    std::process::Command::new("xcode-select")
        .arg("-p")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
