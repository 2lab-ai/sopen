//! The editor's window, brought to the user on KDE Plasma (spec §3, owner message 11).
//!
//! The editor's CLI hands the folder or file to the running editor, which asks the window manager
//! to activate its window. KWin grants that only with an activation token, which a CLI started by
//! sopen — over ssh (`--target`) or from a terminal — does not carry: the window only demands
//! attention, maybe on another virtual desktop or screen, and nothing visible happens. So after
//! the hand-off sopen asks KWin itself, over its D-Bus scripting interface: one script per open,
//! loaded and run once and unloaded about 3 s later, brings the editor window of this open to where
//! the user is — the active window's desktops and screen (without one, the current desktop and the
//! active screen) — un-minimized, and activates it, once. The user is never moved to another
//! desktop. "The window of this open" is the first editor window that asks for attention and
//! shows the opened name in its title, or that starts asking, or appears, while the script is
//! loaded; a window that already asked before and shows another name asked for something else, and
//! is left alone, as are other apps' windows. The script runs in a detached
//! `sopen --raise-window`, so sopen itself (and the ssh session of a `--target` run) ends at once.
//! Every failure is silent: the open itself has already happened.

use crate::zed::Os;
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long the script stays loaded: the editor's window may ask for attention, change its title
/// or appear only after the CLI returned.
const HOLD: Duration = Duration::from_secs(3);
/// How much longer than the hold the script lives on its own clock: past it, it stops acting and
/// lets go of every signal even if the unload never reached KWin.
const LIFE_MARGIN: Duration = Duration::from_millis(500);
/// The longest one D-Bus call may take (KWin answers in milliseconds); a call still running then
/// is killed, and the script is unloaded.
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

/// The name an editor window shows in its title for `path` (a folder's or a file's last
/// component; nothing for `/`).
pub fn opened_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_string()
}

/// After `editor` was handed a folder or file named `opened` ([`opened_name`]): on KDE Plasma
/// (Linux, `XDG_CURRENT_DESKTOP` names KDE), unless `SOPEN_RAISE=0`, starts a detached
/// `sopen --raise-window` that brings its window to the user, and returns at once.
/// `SOPEN_RAISE=wait` does it here instead, about 3 s (tests and receipts wait for it). Never
/// fails.
pub fn editor_window(editor: Editor, opened: &str) {
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
        return raise(&tool, editor, opened, &script_dir(), HOLD, CALL);
    }
    if let Err(e) = detach(editor, opened) {
        crate::debug!("raise: cannot start sopen --raise-window: {e}");
    }
}

/// `sopen --raise-window <editor> --raise-caption=<opened>` (hidden): the script, in the
/// foreground of this process — the detached child [`editor_window`] starts. Never fails.
pub fn window_now(editor: Editor, opened: &str) {
    let path = std::env::var_os("PATH");
    if let Some(tool) = find_tool(path.as_deref(), crate::zed::is_executable) {
        raise(&tool, editor, opened, &script_dir(), HOLD, CALL);
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

/// Starts this sopen as `sopen --raise-window <editor> --raise-caption=<opened>` in a session of
/// its own (a hangup of this terminal, or the end of a `--target` run's ssh session, does not reach
/// it), with no input or output, and does not wait for it.
fn detach(editor: Editor, opened: &str) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["--raise-window", editor.arg()])
        .arg(format!("--raise-caption={opened}"))
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

/// The KWin script (KWin 6's scripting API) for an open of `opened` in `editor`. It brings one
/// window to where the user is, once, and then does nothing more: the first editor window that
/// (a) asks for attention and shows `opened` in its title — at load, newest first, or when its title
/// changes later — or (b) starts asking for attention, or (c) appears, after the script started.
/// The user is never moved to another desktop. It lives `hold` + [`LIFE_MARGIN`] on its own clock:
/// past that it does nothing and lets go of every signal (a KWin timer ends it then; failing that,
/// the first signal after it does), whether or not KWin is ever told to unload it.
fn script(editor: Editor, opened: &str, hold: Duration) -> String {
    const SCRIPT: &str = "\
// sopen: bring the editor window of this open to where the user is, once, and activate it. The
// user is never moved to another desktop. After `life` ms it does nothing more.
const editor = @CLASS@;
const opened = @OPENED@;
const life = @LIFE@;
const deadline = Date.now() + life;
let done = false;
const hooks = [];

function hook(signal, handler) {
    signal.connect(handler);
    hooks.push([signal, handler]);
}

// Done: every handler disconnected, nothing more happens.
function stop() {
    done = true;
    for (const [signal, handler] of hooks) {
        signal.disconnect(handler);
    }
    hooks.length = 0;
}

// Whether the script may still act: not done, and its deadline not passed (else it stops now).
function live() {
    if (!done && Date.now() >= deadline) {
        stop();
    }
    return !done;
}

function isEditor(w) {
    return !!w && editor.test(w.resourceClass);
}

function shows(w) {
    return opened !== \"\" && String(w.caption).indexOf(opened) !== -1;
}

function bring(w) {
    if (!live()) {
        return;
    }
    stop();
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

// (b) an editor window that starts asking for attention now; (a) one that asks and shows the
// name once its title changes (a new window's title comes later).
function watch(w) {
    hook(w.demandsAttentionChanged, () => {
        if (live() && w.demandsAttention) {
            bring(w);
        }
    });
    hook(w.captionChanged, () => {
        if (live() && w.demandsAttention && shows(w)) {
            bring(w);
        }
    });
}

const windows = workspace.windowList();
// (a) at load, newest first. A window that asks but shows another name asked before this open.
for (let i = windows.length - 1; i >= 0 && live(); i--) {
    const w = windows[i];
    if (isEditor(w) && w.demandsAttention && shows(w)) {
        bring(w);
    }
}
if (live()) {
    for (const w of windows) {
        if (isEditor(w)) {
            watch(w);
        }
    }
    // (c) a new editor window: a folder not open yet opens one.
    hook(workspace.windowAdded, (w) => {
        if (live() && isEditor(w)) {
            bring(w);
        }
    });
    // The deadline, kept by KWin's script timer as well: it stops the script then, with no signal
    // needed.
    if (typeof QTimer !== \"undefined\") {
        const timer = new QTimer();
        timer.singleShot = true;
        timer.interval = life;
        hook(timer.timeout, stop);
        timer.start();
    }
}
";
    SCRIPT
        .replacen("@CLASS@", editor.class(), 1)
        .replacen("@OPENED@", &js_string(opened), 1)
        .replacen("@LIFE@", &(hold + LIFE_MARGIN).as_millis().to_string(), 1)
}

/// `text` as a JavaScript string literal in plain ASCII: letters, digits, space and `-._` as they
/// are, every other UTF-16 unit as `\uXXXX` — so no quote, backslash, line break or comment marker
/// in a file name can end the literal or the script.
fn js_string(text: &str) -> String {
    let mut out = String::from("\"");
    for unit in text.encode_utf16() {
        match char::from_u32(u32::from(unit)) {
            Some(c) if c.is_ascii_alphanumeric() || " -._".contains(c) => out.push(c),
            _ => out.push_str(&format!("\\u{unit:04x}")),
        }
    }
    out.push('"');
    out
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

/// The script for `opened` loaded and run once, left loaded for `hold`, then unloaded — unloaded
/// also when the load or the run failed, since a load that failed may have half-happened. Each call
/// gets `budget`. The script file is removed at the end.
fn raise(tool: &Tool, editor: Editor, opened: &str, dir: &Path, hold: Duration, budget: Duration) {
    let Some(file) = ScriptFile::new(dir, &script(editor, opened, hold)) else {
        return;
    };
    let name = format!("sopen-raise-{}", crate::remote::random_hex());
    let reply = call(tool, &tool.load(&file.0, &name), budget);
    let ran = reply
        .as_deref()
        .and_then(script_id)
        .is_some_and(|id| call(tool, &tool.run(id), budget).is_some());
    if ran {
        std::thread::sleep(hold);
    }
    call(tool, &tool.unload(&name), budget);
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

    /// A test's budget per call: the stand-in client is a shell script, slower than busctl.
    const BUDGET: Duration = Duration::from_secs(5);
    /// A test's hold: long enough to see that it was (or was not) waited.
    const HOLD: Duration = Duration::from_millis(400);

    /// Opened names that must reach the script as they are.
    const ODD: [&str; 11] = [
        "it's \"quoted\"",
        r"back\slash\",
        "new\nline",
        "</script><script>alert(1)</script>",
        "한글 폴더",
        "*/ x /*",
        "${x} `y`",
        "\u{2028}line sep\u{2029}",
        "tab\there",
        "emoji 😀",
        "@OPENED@ @CLASS@",
    ];

    #[test]
    fn only_under_kde_on_linux_unless_turned_off() {
        for desktop in ["KDE", "kde", "ubuntu:KDE", "KDE:Plasma"] {
            assert!(wanted(Os::Linux, os(desktop), None), "{desktop}");
            assert!(wanted(Os::Linux, os(desktop), os("1")), "{desktop}");
            assert!(wanted(Os::Linux, os(desktop), os("wait")), "{desktop}");
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

    #[test]
    fn opened_names() {
        assert_eq!(opened_name("/home/u/project"), "project");
        assert_eq!(opened_name("/home/u/project/"), "project");
        assert_eq!(opened_name("/srv/a b/notes.md"), "notes.md");
        assert_eq!(opened_name("/"), "");
    }

    /// What a stand-in client answers to each method: a shell snippet (a reply, an exit status).
    struct Replies {
        load: &'static str,
        run: &'static str,
        unload: &'static str,
    }

    /// busctl's replies when KWin does what is asked: `i <id>`, nothing (`run` returns nothing),
    /// `b true`.
    const BUSCTL_OK: Replies = Replies {
        load: "printf 'i 7\\n'",
        run: ":",
        unload: "printf 'b true\\n'",
    };
    const FAILED: &str = "echo 'Call failed: No such object path' >&2; exit 1";

    /// A stand-in D-Bus client in `dir`: writes each call's arguments as one line to `calls`,
    /// copies a `.js` argument (the script) to `script` with its `ls -l` line in `mode`, and
    /// answers by the method's exact name.
    struct Fake {
        tool: Tool,
        calls: PathBuf,
        script: PathBuf,
        mode: PathBuf,
    }

    fn fake(dir: &Path, busctl: bool, replies: &Replies) -> Fake {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(if busctl { "busctl" } else { "qdbus6" });
        let calls = dir.join("calls");
        let script = dir.join("script");
        let mode = dir.join("mode");
        // busctl: `--user call SERVICE OBJECT INTERFACE METHOD …`; qdbus: `SERVICE OBJECT
        // INTERFACE.METHOD …`.
        let method = if busctl { "$6" } else { "${3##*.}" };
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{calls}'\n\
             for a in \"$@\"; do case $a in *.js) cat \"$a\" > '{script}'; ls -l \"$a\" > '{mode}';; esac; done\n\
             case \"{method}\" in\n\
             unloadScript) {unload};;\n\
             loadScript) {load};;\n\
             run) {run};;\n\
             esac\n",
            calls = calls.display(),
            script = script.display(),
            mode = mode.display(),
            load = replies.load,
            run = replies.run,
            unload = replies.unload,
        );
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Run once first: the first start of a new executable can be slow (macOS checks it), and
        // a call has a deadline.
        crate::testutil::run_fresh(Command::new(&path).arg("warm"));
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

    /// busctl's three calls for a load under `name` of `file` (id 7).
    fn busctl_calls(file: &str, name: &str) -> [String; 3] {
        [
            format!(
                "--user call org.kde.KWin /Scripting org.kde.kwin.Scripting loadScript ss {file} {name}"
            ),
            "--user call org.kde.KWin /Scripting/Script7 org.kde.kwin.Script run".to_string(),
            format!(
                "--user call org.kde.KWin /Scripting org.kde.kwin.Scripting unloadScript s {name}"
            ),
        ]
    }

    /// One script per open: loaded and run once, held, unloaded once; private while it exists,
    /// gone after.
    #[test]
    fn busctl_loads_runs_holds_and_unloads_one_script() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = fake(tmp.path(), true, &BUSCTL_OK);
        let started = Instant::now();
        raise(
            &fake.tool,
            Editor::Code,
            "project",
            tmp.path(),
            HOLD,
            BUDGET,
        );
        assert!(started.elapsed() >= HOLD, "{:?}", started.elapsed());
        let calls = lines(&fake.calls);
        assert!(!calls.is_empty(), "no call");
        let (file, name) = loaded(&calls, 7);
        assert!(file.starts_with(tmp.path().to_str().unwrap()), "{file}");
        assert!(name.starts_with("sopen-raise-"), "{name}");
        assert_eq!(calls, busctl_calls(&file, &name));
        let text = std::fs::read_to_string(&fake.script).unwrap();
        assert_eq!(text, script(Editor::Code, "project", HOLD));
        let mode = std::fs::read_to_string(&fake.mode).unwrap();
        assert_eq!(mode.get(..10), Some("-rw-------"), "{mode}");
        assert!(!Path::new(&file).exists(), "{file} left behind");
    }

    #[test]
    fn qdbus_takes_the_same_calls_in_its_own_form() {
        let tmp = tempfile::tempdir().unwrap();
        let replies = Replies {
            load: "printf '7\\n'",
            run: ":",
            unload: "printf 'true\\n'",
        };
        let fake = fake(tmp.path(), false, &replies);
        raise(&fake.tool, Editor::Zed, "proj", tmp.path(), HOLD, BUDGET);
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
            script(Editor::Zed, "proj", HOLD)
        );
        assert!(!Path::new(&file).exists(), "{file} left behind");
    }

    /// A load that fails or is refused: unloaded anyway (it may have half-happened), not run, not
    /// held; quiet, and the script file is removed.
    #[test]
    fn a_failed_load_is_unloaded_at_once() {
        for load in [FAILED, "printf 'i -1\\n'"] {
            let tmp = tempfile::tempdir().unwrap();
            let fake = fake(tmp.path(), true, &Replies { load, ..BUSCTL_OK });
            let started = Instant::now();
            raise(
                &fake.tool,
                Editor::Code,
                "project",
                tmp.path(),
                HOLD,
                BUDGET,
            );
            assert!(started.elapsed() < HOLD, "{load}: held");
            let calls = lines(&fake.calls);
            let (file, name) = loaded(&calls, 7);
            let [load_call, _, unload_call] = busctl_calls(&file, &name);
            assert_eq!(calls, [load_call, unload_call], "{load}");
            assert!(!Path::new(&file).exists(), "{load}: {file} left behind");
        }
    }

    /// A run that fails: unloaded at once, not held.
    #[test]
    fn a_failed_run_is_unloaded_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = fake(
            tmp.path(),
            true,
            &Replies {
                run: FAILED,
                ..BUSCTL_OK
            },
        );
        let started = Instant::now();
        raise(
            &fake.tool,
            Editor::Code,
            "project",
            tmp.path(),
            HOLD,
            BUDGET,
        );
        assert!(started.elapsed() < HOLD, "held");
        let calls = lines(&fake.calls);
        let (file, name) = loaded(&calls, 7);
        assert_eq!(calls, busctl_calls(&file, &name));
        assert!(!Path::new(&file).exists(), "{file} left behind");
    }

    /// An unload that fails changes nothing: quiet, and the script file is removed.
    #[test]
    fn a_failed_unload_is_quiet() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = fake(
            tmp.path(),
            true,
            &Replies {
                unload: FAILED,
                ..BUSCTL_OK
            },
        );
        raise(
            &fake.tool,
            Editor::Code,
            "project",
            tmp.path(),
            HOLD,
            BUDGET,
        );
        let calls = lines(&fake.calls);
        let (file, name) = loaded(&calls, 7);
        assert_eq!(calls, busctl_calls(&file, &name));
        assert!(!Path::new(&file).exists(), "{file} left behind");
    }

    /// A client that hangs is killed at its budget: sopen never waits on KWin longer.
    #[test]
    fn a_hanging_client_is_cut_off() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = fake(
            tmp.path(),
            true,
            &Replies {
                load: "exec sleep 30",
                ..BUSCTL_OK
            },
        );
        let started = Instant::now();
        let budget = Duration::from_millis(300);
        raise(
            &fake.tool,
            Editor::Code,
            "project",
            tmp.path(),
            HOLD,
            budget,
        );
        let took = started.elapsed();
        assert!(took >= budget && took < Duration::from_secs(2), "{took:?}");
        let calls = lines(&fake.calls);
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[1].contains(" unloadScript s "), "{calls:?}");
    }

    /// Any opened name stays inside its string literal: plain ASCII, no quote, backslash-letter,
    /// line break or `<` of its own, and it reads back as the name.
    #[test]
    fn an_opened_name_is_one_string_literal() {
        assert_eq!(js_string("project"), "\"project\"");
        assert_eq!(js_string(""), "\"\"");
        for name in ODD {
            let literal = js_string(name);
            assert!(literal.is_ascii(), "{literal}");
            let inner = &literal[1..literal.len() - 1];
            assert!(
                literal.starts_with('"') && literal.ends_with('"'),
                "{literal}"
            );
            for bad in ['"', '\n', '\r', '<', '>', '`', '$', '*', '/', '\''] {
                assert!(!inner.contains(bad), "{bad:?} in {literal}");
            }
            assert!(
                inner
                    .split("\\u")
                    .skip(1)
                    .all(|u| u.len() >= 4 && u[..4].chars().all(|c| c.is_ascii_hexdigit())),
                "{literal}"
            );
            // JSON reads `\uXXXX` (and surrogate pairs) as JavaScript does.
            let back: String = serde_json::from_str(&literal).unwrap();
            assert_eq!(back, name);
            let text = script(Editor::Code, name, HOLD);
            assert!(
                text.contains(&format!("const opened = {literal};")),
                "{text}"
            );
            assert!(
                !text.contains("@OPENED@") && !text.contains("@CLASS@"),
                "{text}"
            );
            assert!(text.contains("const editor = /^code$/i;"), "{text}");
        }
    }

    /// The script's shape: one window, once (a `done` flag; every handler disconnected), within its
    /// own lifetime (the hold and a margin; every handler and the load-time check ask `live()`
    /// first; a KWin timer stops it then), the three ways a window qualifies, and the steps that
    /// bring it — never setting the current desktop, never reading a desktop's name.
    #[test]
    fn the_script_brings_one_window_once() {
        for (editor, class) in [
            (Editor::Code, r"/^code$/i"),
            (Editor::Zed, r"/^(dev\.zed\.)?zed(-[a-z]+)?$/i"),
        ] {
            let text = script(editor, "project", Duration::from_secs(3));
            assert!(text.contains(&format!("const editor = {class};")), "{text}");
            for line in [
                "const opened = \"project\";",
                "const life = 3500;",
                "const deadline = Date.now() + life;",
                "if (!done && Date.now() >= deadline) {",
                "done = true;",
                "signal.disconnect(handler);",
                "return opened !== \"\" && String(w.caption).indexOf(opened) !== -1;",
                "for (let i = windows.length - 1; i >= 0 && live(); i--) {",
                "if (isEditor(w) && w.demandsAttention && shows(w)) {",
                "hook(w.demandsAttentionChanged, () => {",
                "if (live() && w.demandsAttention) {",
                "hook(w.captionChanged, () => {",
                "if (live() && w.demandsAttention && shows(w)) {",
                "hook(workspace.windowAdded, (w) => {",
                "if (live() && isEditor(w)) {",
                "const timer = new QTimer();",
                "timer.singleShot = true;",
                "timer.interval = life;",
                "hook(timer.timeout, stop);",
                "timer.start();",
                "let desktops = [workspace.currentDesktop];",
                "let output = workspace.activeScreen;",
                "if (here && here !== w) {",
                "desktops = here.desktops;",
                "output = here.output;",
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
            assert!(!text.contains("currentDesktop ="), "{text}");
            assert!(!text.contains("currentDesktop="), "{text}");
            assert_eq!(text.matches(".name").count(), 2, "{text}");
        }
    }

    /// `name` on the test process's PATH (absolute dirs only).
    fn on_path(name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(name))
            .find(|p| crate::zed::is_executable(p))
    }

    /// The script, run under node against a stand-in `workspace` with signals
    /// (tests/kwin-script.mjs): the window of this open comes, once; an older demand never does.
    /// Skipped with a note where node is missing — except under SOPEN_REQUIRE_NODE=1 (CI).
    #[test]
    fn the_script_under_a_stand_in_kwin() {
        let Some(node) = on_path("node") else {
            if std::env::var_os("SOPEN_REQUIRE_NODE").is_some_and(|v| v == "1") {
                panic!("SOPEN_REQUIRE_NODE=1 but node is not on PATH");
            }
            eprintln!("note: node is not on PATH; the KWin script's behaviour test is skipped");
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let hold = super::HOLD;
        std::fs::write(dir.join("code.js"), script(Editor::Code, "project", hold)).unwrap();
        std::fs::write(dir.join("zed.js"), script(Editor::Zed, "project", hold)).unwrap();
        std::fs::write(dir.join("names.json"), serde_json::to_string(&ODD).unwrap()).unwrap();
        for (i, name) in ODD.iter().enumerate() {
            std::fs::write(
                dir.join(format!("odd-{i}.js")),
                script(Editor::Code, name, hold),
            )
            .unwrap();
        }
        let harness = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/kwin-script.mjs");
        let out = Command::new(node).arg(harness).arg(dir).output().unwrap();
        let report = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{report}");
        assert!(report.ends_with("all ok\n"), "{report}");
    }
}
