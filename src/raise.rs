//! The editor's window, brought to the user on KDE Plasma (spec §3, owner message 11).
//!
//! The editor's CLI hands the folder or file to the running editor, which asks the window manager
//! to activate its window. KWin grants that only with an activation token, which a CLI started by
//! sopen — over ssh (`--target`) or from a terminal — does not carry: the window only demands
//! attention, maybe on another virtual desktop or screen, and nothing visible happens. So after
//! the hand-off sopen asks KWin itself, over its D-Bus scripting interface: a small script takes
//! the newest window of the editor's class that demands attention and brings it to where the user
//! is — the active window's desktops and screen (without one, the current desktop and the active
//! screen) — un-minimized, and activates it. The user is never moved to another desktop. It runs a
//! few times over about 3 s: the demand arrives after the CLI returns, and it clears once the
//! window is active, so later runs do nothing. Windows of other apps, and the editor's windows
//! that ask for nothing, are never touched. The runs happen in a detached `sopen --raise-window`,
//! so sopen itself (and the ssh session of a `--target` run) ends at once. Every failure is
//! silent: the open itself has already happened.

use crate::zed::Os;
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// When the script runs, counted from the end of the hand-off.
const RUNS: [Duration; 4] = [
    Duration::from_millis(300),
    Duration::from_millis(800),
    Duration::from_millis(1500),
    Duration::from_millis(3000),
];
/// The longest one D-Bus call may take (KWin answers in milliseconds); a call still running then
/// is killed and ends the runs.
const CALL: Duration = Duration::from_millis(500);

/// The editor sopen handed the folder or file to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Editor {
    Code,
    Zed,
}

impl Editor {
    /// Its windows' resource class (KWin's name for the app), as a JavaScript regular expression:
    /// VS Code's is `code`; Zed's is `zed`, or under Wayland its app id `dev.zed.Zed` (a
    /// channel's: `dev.zed.Zed-Preview`).
    fn class(self) -> &'static str {
        match self {
            Editor::Code => r"/^code$/i",
            Editor::Zed => r"/^(dev\.zed\.)?zed(-[a-z]+)?$/i",
        }
    }

    /// The value of `--raise-window`.
    pub fn arg(self) -> &'static str {
        match self {
            Editor::Code => "code",
            Editor::Zed => "zed",
        }
    }

    pub fn from_arg(arg: &str) -> Option<Self> {
        [Editor::Code, Editor::Zed]
            .into_iter()
            .find(|e| e.arg() == arg)
    }
}

/// After `editor` was handed a folder or file: on KDE Plasma (Linux, `XDG_CURRENT_DESKTOP` names
/// KDE), unless `SOPEN_RAISE=0`, starts a detached `sopen --raise-window` that brings its window
/// to the user, and returns at once. `SOPEN_RAISE=wait` does it here instead, about 3 s (tests and
/// receipts wait for it). Never fails.
pub fn editor_window(editor: Editor) {
    let var = std::env::var_os;
    let mode = var("SOPEN_RAISE");
    let desktop = var("XDG_CURRENT_DESKTOP");
    if !wanted(Os::current(), desktop.as_deref(), mode.as_deref()) {
        return;
    }
    let Some(tool) = find_tool(var("PATH").as_deref(), crate::zed::is_executable) else {
        crate::debug!("raise: no busctl, qdbus6 or qdbus on PATH");
        return;
    };
    if mode.as_deref() == Some(OsStr::new("wait")) {
        return raise(&tool, editor, &script_dir(), &RUNS, CALL);
    }
    if let Err(e) = detach(editor) {
        crate::debug!("raise: cannot start sopen --raise-window: {e}");
    }
}

/// `sopen --raise-window <editor>` (hidden): the runs, in the foreground of this process — the
/// detached child [`editor_window`] starts. Never fails.
pub fn window_now(editor: Editor) {
    let path = std::env::var_os("PATH");
    if let Some(tool) = find_tool(path.as_deref(), crate::zed::is_executable) {
        raise(&tool, editor, &script_dir(), &RUNS, CALL);
    }
}

/// Where the script file goes: `$XDG_RUNTIME_DIR` (the user's own, the desktop session's too),
/// else the temp dir.
fn script_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute() && dir.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// Starts this sopen as `sopen --raise-window <editor>` in a session of its own (a hangup of this
/// terminal, or the end of a `--target` run's ssh session, does not reach it), with no input or
/// output, and does not wait for it.
fn detach(editor: Editor) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["--raise-window", editor.arg()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe, as `pre_exec` requires.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().map(drop)
}

/// On Linux, when `desktop` (`XDG_CURRENT_DESKTOP`, a colon-separated list) names KDE in any
/// case, unless `raise` (`SOPEN_RAISE`) is `0`.
fn wanted(os: Os, desktop: Option<&OsStr>, raise: Option<&OsStr>) -> bool {
    os == Os::Linux
        && raise != Some(OsStr::new("0"))
        && desktop
            .and_then(OsStr::to_str)
            .is_some_and(|d| d.split(':').any(|name| name.eq_ignore_ascii_case("KDE")))
}

/// The D-Bus command-line client sopen calls KWin with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tool {
    path: PathBuf,
    /// `busctl` (systemd); else `qdbus6` / `qdbus` (Qt).
    busctl: bool,
}

impl Tool {
    /// `loadScript(file, name)`; the reply is the script's id.
    fn load(&self, file: &Path, name: &str) -> Vec<OsString> {
        let mut args = self.method("/Scripting", "org.kde.kwin.Scripting", "loadScript");
        if self.busctl {
            args.push("ss".into());
        }
        args.extend([file.as_os_str().to_os_string(), name.into()]);
        args
    }

    /// `run()` on the script loaded as `id`. KWin answers once the script has been evaluated.
    fn run(&self, id: i32) -> Vec<OsString> {
        self.method(
            &format!("/Scripting/Script{id}"),
            "org.kde.kwin.Script",
            "run",
        )
    }

    /// `unloadScript(name)`.
    fn unload(&self, name: &str) -> Vec<OsString> {
        let mut args = self.method("/Scripting", "org.kde.kwin.Scripting", "unloadScript");
        if self.busctl {
            args.push("s".into());
        }
        args.push(name.into());
        args
    }

    /// `busctl --user call org.kde.KWin OBJECT INTERFACE METHOD` or
    /// `qdbus6 org.kde.KWin OBJECT INTERFACE.METHOD`, before the method's arguments.
    fn method(&self, object: &str, interface: &str, method: &str) -> Vec<OsString> {
        if self.busctl {
            ["--user", "call", "org.kde.KWin", object, interface, method]
                .map(OsString::from)
                .to_vec()
        } else {
            vec![
                "org.kde.KWin".into(),
                object.into(),
                format!("{interface}.{method}").into(),
            ]
        }
    }
}

/// `busctl`, then `qdbus6`, then `qdbus`, each looked for on all of PATH.
fn find_tool(path_var: Option<&OsStr>, exists: impl Fn(&Path) -> bool) -> Option<Tool> {
    ["busctl", "qdbus6", "qdbus"].into_iter().find_map(|name| {
        let path = path_var
            .map(std::env::split_paths)
            .into_iter()
            .flatten()
            .map(|dir| dir.join(name))
            .find(|p| exists(p))?;
        Some(Tool {
            path,
            busctl: name == "busctl",
        })
    })
}

/// The KWin script (KWin 6's scripting API): the newest window of `editor`'s class that demands
/// attention comes to where the user is — the active window's desktops and screen, else the
/// current desktop and the active screen — un-minimized, and is activated.
fn script(editor: Editor) -> String {
    const SCRIPT: &str = "\
// sopen: bring the editor window that asked for attention (the newest one) to where the user is,
// and activate it. The user is never moved to another desktop.
const editor = CLASS;
const windows = workspace.windowList();
let w = null;
for (let i = windows.length - 1; i >= 0 && !w; i--) {
    if (windows[i].demandsAttention && editor.test(windows[i].resourceClass)) {
        w = windows[i];
    }
}
if (w) {
    // Where the user is: the active window's desktops (objects: names need not be unique) and
    // screen; without one (or on all desktops), the current desktop and the active screen.
    const here = workspace.activeWindow;
    let desktops = [workspace.currentDesktop];
    let output = workspace.activeScreen;
    if (here && here !== w) {
        if (!here.onAllDesktops) {
            desktops = here.desktops;
        }
        if (here.output) {
            output = here.output;
        }
    }
    w.minimized = false;
    if (!w.onAllDesktops) {
        w.desktops = desktops;
    }
    if (output && (!w.output || w.output.name !== output.name)) {
        workspace.sendClientToScreen(w, output);
    }
    workspace.activeWindow = w;
}
";
    SCRIPT.replace("CLASS", editor.class())
}

/// The id in a `loadScript` reply: `i 7` (busctl) or `7` (qdbus). A negative one means KWin did
/// not load it.
fn script_id(reply: &str) -> Option<i32> {
    reply
        .split_whitespace()
        .last()?
        .parse()
        .ok()
        .filter(|id| *id >= 0)
}

/// The script in a new file in `dir` that only this user can read (0600); removed when dropped.
struct ScriptFile(PathBuf);

impl ScriptFile {
    fn new(dir: &Path, text: &str) -> Option<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = dir.join(format!("sopen-raise-{}.js", crate::remote::random_hex()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .ok()?;
        let made = ScriptFile(path);
        file.write_all(text.as_bytes()).ok()?;
        Some(made)
    }
}

impl Drop for ScriptFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// At each of `runs` (from now): the script loaded, run and unloaded — unloaded even when the
/// load or the run failed, since a load that failed may have half-happened. The first failure ends
/// the runs. Each call gets `budget`. The script file is removed at the end.
fn raise(tool: &Tool, editor: Editor, dir: &Path, runs: &[Duration], budget: Duration) {
    let start = Instant::now();
    let Some(file) = ScriptFile::new(dir, &script(editor)) else {
        return;
    };
    let name = format!("sopen-raise-{}", crate::remote::random_hex());
    for &at in runs {
        std::thread::sleep((start + at).saturating_duration_since(Instant::now()));
        let reply = call(tool, &tool.load(&file.0, &name), budget);
        let ran = reply
            .as_deref()
            .and_then(script_id)
            .is_some_and(|id| call(tool, &tool.run(id), budget).is_some());
        let unloaded = call(tool, &tool.unload(&name), budget).is_some();
        if !(ran && unloaded) {
            break;
        }
    }
    drop(file);
}

/// `tool args`, with no input and its errors dropped, waited for `budget` and no longer: its output
/// when it exits 0; `None` when it cannot start, fails, or is still running (then it is killed).
fn call(tool: &Tool, args: &[OsString], budget: Duration) -> Option<String> {
    crate::debug!(
        "raise: {}",
        crate::render_command(tool.path.as_os_str(), args)
    );
    let deadline = Instant::now() + budget;
    let mut child = Command::new(&tool.path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut out = String::new();
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                return Some(out);
            }
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<&OsStr> {
        Some(OsStr::new(s))
    }

    const RUNS: [Duration; 3] = [
        Duration::ZERO,
        Duration::from_millis(20),
        Duration::from_millis(40),
    ];
    /// A test's budget per call: the stand-in client is a shell script, slower than busctl.
    const BUDGET: Duration = Duration::from_secs(5);

    #[test]
    fn only_under_kde_on_linux_unless_turned_off() {
        for desktop in ["KDE", "kde", "ubuntu:KDE", "KDE:Plasma"] {
            assert!(wanted(Os::Linux, os(desktop), None), "{desktop}");
            assert!(wanted(Os::Linux, os(desktop), os("1")), "{desktop}");
            assert!(!wanted(Os::Linux, os(desktop), os("0")), "{desktop}");
            assert!(!wanted(Os::MacOs, os(desktop), None), "{desktop}");
        }
        for desktop in ["GNOME", "ubuntu:GNOME", "X-Cinnamon", "KDEX", "XFCE", ""] {
            assert!(!wanted(Os::Linux, os(desktop), None), "{desktop}");
        }
        assert!(!wanted(Os::Linux, None, None));
    }

    #[test]
    fn busctl_first_then_qdbus6_then_qdbus() {
        let path = std::env::join_paths(["/a", "/b"]).unwrap();
        let only =
            |hits: &'static [&'static str]| move |p: &Path| hits.iter().any(|h| p == Path::new(h));
        let tool = |path: &str, busctl| {
            Some(Tool {
                path: PathBuf::from(path),
                busctl,
            })
        };
        assert_eq!(
            find_tool(Some(&path), only(&["/a/qdbus", "/b/busctl", "/a/qdbus6"])),
            tool("/b/busctl", true)
        );
        assert_eq!(
            find_tool(Some(&path), only(&["/a/qdbus", "/b/qdbus6"])),
            tool("/b/qdbus6", false)
        );
        assert_eq!(
            find_tool(Some(&path), only(&["/b/qdbus"])),
            tool("/b/qdbus", false)
        );
        assert_eq!(find_tool(Some(&path), only(&[])), None);
        assert_eq!(find_tool(None, only(&["/a/busctl"])), None);
    }

    /// A stand-in D-Bus client in `dir`: writes each call's arguments as one line to `calls`,
    /// copies a `.js` argument (the script) to `script` with its `ls -l` line in `mode`, then
    /// runs `then` (a call's reply and exit status).
    struct Fake {
        tool: Tool,
        calls: PathBuf,
        script: PathBuf,
        mode: PathBuf,
    }

    fn fake(dir: &Path, busctl: bool, then: &str) -> Fake {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(if busctl { "busctl" } else { "qdbus6" });
        let calls = dir.join("calls");
        let script = dir.join("script");
        let mode = dir.join("mode");
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{calls}'\n\
             for a in \"$@\"; do case $a in *.js) cat \"$a\" > '{script}'; ls -l \"$a\" > '{mode}';; esac; done\n\
             {then}\n",
            calls = calls.display(),
            script = script.display(),
            mode = mode.display(),
        );
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Run once first: the first start of a new executable can be slow (macOS checks it), and
        // a call has a deadline.
        Command::new(&path).arg("warm").output().unwrap();
        std::fs::remove_file(&calls).unwrap();
        Fake {
            tool: Tool { path, busctl },
            calls,
            script,
            mode,
        }
    }

    fn lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The script file and the name it was loaded under, from the first call (a load).
    fn loaded(calls: &[String], at: usize) -> (String, String) {
        let words: Vec<&str> = calls[0].split(' ').collect();
        (words[at].to_string(), words[at + 1].to_string())
    }

    #[test]
    fn busctl_loads_runs_and_unloads_the_script_at_each_run() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = fake(
            tmp.path(),
            true,
            "case \"$*\" in *loadScript*) printf 'i 7\\n';; *unloadScript*) printf 'b true\\n';; esac",
        );
        raise(&fake.tool, Editor::Code, tmp.path(), &RUNS, BUDGET);
        let calls = lines(&fake.calls);
        assert!(!calls.is_empty(), "no call");
        let (file, name) = loaded(&calls, 7);
        assert!(file.starts_with(tmp.path().to_str().unwrap()), "{file}");
        assert!(name.starts_with("sopen-raise-"), "{name}");
        let load = format!(
            "--user call org.kde.KWin /Scripting org.kde.kwin.Scripting loadScript ss {file} {name}"
        );
        let run = "--user call org.kde.KWin /Scripting/Script7 org.kde.kwin.Script run".to_string();
        let unload = format!(
            "--user call org.kde.KWin /Scripting org.kde.kwin.Scripting unloadScript s {name}"
        );
        let once = [load, run, unload];
        let every: Vec<String> = once.iter().cycle().take(3 * RUNS.len()).cloned().collect();
        assert_eq!(calls, every);
        // The script, private to this user while it exists, and gone after.
        let text = std::fs::read_to_string(&fake.script).unwrap();
        assert_eq!(text, script(Editor::Code));
        let mode = std::fs::read_to_string(&fake.mode).unwrap();
        assert_eq!(mode.get(..10), Some("-rw-------"), "{mode}");
        assert!(!Path::new(&file).exists(), "{file} left behind");
    }

    #[test]
    fn qdbus_takes_the_same_calls_in_its_own_form() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = fake(
            tmp.path(),
            false,
            "case \"$*\" in *loadScript*) printf '7\\n';; *unloadScript*) printf 'true\\n';; esac",
        );
        raise(&fake.tool, Editor::Zed, tmp.path(), &RUNS[..1], BUDGET);
        let calls = lines(&fake.calls);
        assert!(!calls.is_empty(), "no call");
        let (file, name) = loaded(&calls, 3);
        assert_eq!(
            calls,
            [
                format!("org.kde.KWin /Scripting org.kde.kwin.Scripting.loadScript {file} {name}"),
                "org.kde.KWin /Scripting/Script7 org.kde.kwin.Script.run".to_string(),
                format!("org.kde.KWin /Scripting org.kde.kwin.Scripting.unloadScript {name}"),
            ]
        );
        assert_eq!(
            std::fs::read_to_string(&fake.script).unwrap(),
            script(Editor::Zed)
        );
        assert!(!Path::new(&file).exists(), "{file} left behind");
    }

    /// The script picks only a window of the editor's class that demands attention, the newest
    /// first, and brings it to where the user is: the active window's desktops and screen (else
    /// the current desktop and the active screen), un-minimized, then activated. It never sets
    /// the current desktop, and never looks at a desktop's name.
    #[test]
    fn the_script_brings_only_the_editor_window_that_asks_to_the_user() {
        for (editor, class) in [
            (Editor::Code, r"/^code$/i"),
            (Editor::Zed, r"/^(dev\.zed\.)?zed(-[a-z]+)?$/i"),
        ] {
            let text = script(editor);
            assert!(text.contains(&format!("const editor = {class};")), "{text}");
            for line in [
                "const windows = workspace.windowList();",
                "for (let i = windows.length - 1; i >= 0 && !w; i--) {",
                "if (windows[i].demandsAttention && editor.test(windows[i].resourceClass)) {",
                "let desktops = [workspace.currentDesktop];",
                "let output = workspace.activeScreen;",
                "if (here && here !== w) {",
                "if (!here.onAllDesktops) {",
                "desktops = here.desktops;",
                "output = here.output;",
                "if (!w.onAllDesktops) {",
                "if (output && (!w.output || w.output.name !== output.name)) {",
            ] {
                assert!(text.contains(line), "{line}: {text}");
            }
            let steps = [
                "const here = workspace.activeWindow;",
                "w.minimized = false;",
                "w.desktops = desktops;",
                "workspace.sendClientToScreen(w, output);",
                "workspace.activeWindow = w;",
            ];
            let at: Vec<usize> = steps
                .iter()
                .map(|step| {
                    assert_eq!(text.matches(step).count(), 1, "{step}: {text}");
                    text.find(step).unwrap()
                })
                .collect();
            assert!(at.windows(2).all(|p| p[0] < p[1]), "in this order: {text}");
            // Reading the current desktop is fine; setting it would move the user.
            assert!(!text.contains("currentDesktop ="), "{text}");
            assert!(!text.contains("currentDesktop="), "{text}");
            // Names only of screens (unique); desktops' need not be.
            assert_eq!(text.matches(".name").count(), 2, "{text}");
        }
    }

    /// A client that fails ends the runs at once: the load, then one unload in case the load
    /// half-happened; quiet and quick, and the script file is removed.
    #[test]
    fn a_failing_client_stops_quietly() {
        for then in ["exit 1", "printf 'i -1\\n'"] {
            let tmp = tempfile::tempdir().unwrap();
            let fake = fake(tmp.path(), true, then);
            let started = Instant::now();
            raise(&fake.tool, Editor::Code, tmp.path(), &RUNS, BUDGET);
            assert!(started.elapsed() < Duration::from_secs(2), "{then}");
            let calls = lines(&fake.calls);
            assert_eq!(calls.len(), 2, "{then}: {calls:?}");
            assert!(calls[0].contains(" loadScript ss "), "{then}: {calls:?}");
            assert!(calls[1].contains(" unloadScript s "), "{then}: {calls:?}");
            let (file, _) = loaded(&calls, 7);
            assert!(!Path::new(&file).exists(), "{then}: {file} left behind");
        }
    }

    /// A client that hangs is killed at its budget: sopen never waits on KWin longer.
    #[test]
    fn a_hanging_client_is_cut_off() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = fake(
            tmp.path(),
            true,
            "case \"$*\" in *loadScript*) exec sleep 30;; esac",
        );
        let started = Instant::now();
        raise(
            &fake.tool,
            Editor::Code,
            tmp.path(),
            &RUNS,
            Duration::from_millis(300),
        );
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(300), "{took:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
        let calls = lines(&fake.calls);
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[1].contains(" unloadScript s "), "{calls:?}");
    }
}
