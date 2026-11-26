//! The `sopen` binary on local targets: no ssh, no network. Cache, state and config go to a temp
//! dir; `--target` runs a fake `ssh`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The binary with cache, state and config under `home`, and nothing from the caller's
/// environment that would forward the run (a default target) or change it (a receipt hook).
fn sopen_command(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sopen"));
    cmd.env("SOPEN_CACHE_DIR", home.join("cache"))
        .env("SOPEN_STATE_DIR", home.join("state"))
        .env("SOPEN_CONFIG", home.join("config.toml"))
        .env("SOPEN_CODE", "")
        .env("VSCODE_EXTENSIONS", home.join("vscode-extensions"))
        // Never ask a real KWin (a test run inside a KDE session) to move a window.
        .env("SOPEN_RAISE", "0")
        .env_remove("SOPEN_TARGET");
    for key in sopen::forward::FORWARDED_ENV {
        cmd.env_remove(key);
    }
    cmd
}

/// How long a run that should end by itself may take — its exit and the end of its output
/// together — before the test gives up on it.
const RUN_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// A child that is killed and reaped when this goes, whatever way the test leaves (a panic
/// included), unless its exit was already seen.
struct Reaped(Option<std::process::Child>);

impl Drop for Reaped {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Everything `pipe` gives until its end, sent when the end comes. A pipe some descendant
/// keeps open never ends: then nothing is sent, and the receiver's deadline says so.
fn collected(
    pipe: Option<impl std::io::Read + Send + 'static>,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = tx.send(bytes);
    });
    rx
}

/// Runs sopen to its end and returns what it printed. One deadline, [`RUN_LIMIT`], covers the
/// child's exit and the end of both its outputs: a run that is still there, or whose output
/// never ends (a descendant holding a pipe open), fails the test within it instead of hanging
/// the suite. The child is under a kill-and-reap guard from the moment it is spawned.
fn sopen(home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = sopen_command(home);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut guard = Reaped(Some(cmd.spawn().unwrap()));
    let child = guard.0.as_mut().unwrap();
    // Both pipes are read while it runs, so a run that prints a lot is never stuck on a full
    // pipe.
    let stdout = collected(child.stdout.take());
    let stderr = collected(child.stderr.take());
    let deadline = std::time::Instant::now() + RUN_LIMIT;
    let status = loop {
        if let Some(status) = guard.0.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "sopen {args:?} did not end within {RUN_LIMIT:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    // Exited and reaped: nothing is left for the guard to kill.
    guard.0 = None;
    let until_end = |rx: std::sync::mpsc::Receiver<Vec<u8>>, what: &str| {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        rx.recv_timeout(left).unwrap_or_else(|_| {
            panic!("sopen {args:?} exited, but its {what} did not end within {RUN_LIMIT:?}")
        })
    };
    Output {
        status,
        stdout: until_end(stdout, "stdout"),
        stderr: until_end(stderr, "stderr"),
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn version_line() {
    let tmp = tempfile::tempdir().unwrap();
    let out = sopen(tmp.path(), &["--version"], &[]);
    assert!(out.status.success());
    let all = text(&out.stdout);
    let line = all.lines().next().unwrap_or_default();
    let want = format!("sopen {} (", env!("CARGO_PKG_VERSION"));
    assert!(line.starts_with(&want), "{all}");
    assert!(line.ends_with(')'), "{all}");
}

/// The windows' font is CC BY-SA 4.0: its credit travels with the binary (a Homebrew install
/// has nothing else), in `--version` and the long help; `-V` stays the one version line.
#[test]
fn version_and_help_credit_the_font() {
    let tmp = tempfile::tempdir().unwrap();
    let credit = "Font: PxPlus IBM VGA 8x16 by VileR (The Ultimate Oldschool PC Font Pack, \
                  https://int10h.org/oldschool-pc-fonts/), CC BY-SA 4.0";
    for args in [&["--version"][..], &["--help"][..]] {
        let out = sopen(tmp.path(), args, &[]);
        assert!(out.status.success(), "{args:?}");
        let said = text(&out.stdout);
        assert!(said.lines().any(|l| l.trim() == credit), "{args:?}: {said}");
    }
    let short = text(&sopen(tmp.path(), &["-V"], &[]).stdout);
    assert_eq!(short.lines().count(), 1, "{short}");
    assert!(!short.contains("VileR"), "{short}");
}

#[test]
fn missing_file_is_one_error_line() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope.txt");
    let out = sopen(tmp.path(), &[missing.to_str().unwrap()], &[]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.starts_with("sopen: "), "{err}");
    assert!(err.contains("no such file"), "{err}");
}

/// A binary file's dialog answered by `SOPEN_CHOICE` (no window): "save" copies it to
/// `SOPEN_SAVE_TO` and never over anything; "cancel" writes nothing.
#[test]
fn other_files_save_to_a_path_or_cancel_and_log_usage() {
    let tmp = tempfile::tempdir().unwrap();
    let blob = tmp.path().join("blob.bin");
    let body = [0u8, 1, 2, 3, 0xff];
    std::fs::write(&blob, body).unwrap();
    let out_dir = tmp.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let dest = out_dir.join("copy.bin");
    let save = [
        ("SOPEN_CHOICE", "save"),
        ("SOPEN_SAVE_TO", dest.to_str().unwrap()),
    ];
    let out = sopen(tmp.path(), &[blob.to_str().unwrap()], &save);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout).trim(),
        format!("saved {}", dest.display())
    );
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_eq!(
        files(&out_dir),
        std::slice::from_ref(&dest),
        "only the saved file (no staging left)"
    );
    let log = usage_lines(tmp.path());
    assert_eq!(log[0]["kind"], "other");
    assert_eq!(log[0]["ext"], "bin");
    assert_eq!(log[0]["action"], "open");
    assert_eq!(log[0]["host_hash"], "local");
    assert_eq!(log[0]["ok"], true);

    // Again: the path is taken now. Not replaced; the next free name is named.
    std::fs::write(&dest, b"mine").unwrap();
    let out = sopen(tmp.path(), &[blob.to_str().unwrap()], &save);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains("exists; not replaced"), "{err}");
    assert!(err.contains("copy-1.bin"), "{err}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"mine");
    assert!(!out_dir.join("copy-1.bin").exists());

    // A folder: the file's own name inside it.
    let out = sopen(
        tmp.path(),
        &[blob.to_str().unwrap()],
        &[
            ("SOPEN_CHOICE", "save"),
            ("SOPEN_SAVE_TO", out_dir.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(std::fs::read(out_dir.join("blob.bin")).unwrap(), body);

    // Cancel: nothing written.
    let before = files(&out_dir);
    let out = sopen(
        tmp.path(),
        &[blob.to_str().unwrap()],
        &[("SOPEN_CHOICE", "cancel")],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("sopen: cancelled"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(files(&out_dir), before);
    let out = sopen(
        tmp.path(),
        &[blob.to_str().unwrap()],
        &[("SOPEN_CHOICE", "maybe")],
    );
    assert_eq!(out.status.code(), Some(1));
}

/// A windowless "Save to…" (`SOPEN_CHOICE=save` into `out`) of a 3 MiB file, held at its first
/// megabyte by `SOPEN_SAVE_HOLD_MS`: the running sopen, and its private staging folder in `out`
/// once `part` holds bytes. The child is killed and reaped when this goes.
struct HeldSave {
    child: Option<std::process::Child>,
    staging: PathBuf,
}

impl HeldSave {
    fn start(home: &Path, out: &Path) -> Self {
        Self::start_held(home, out, "60000")
    }

    /// [`HeldSave::start`] with `hold` for `SOPEN_SAVE_HOLD_MS` (`<ms>:stalled`: a hold that
    /// no cancel ends).
    fn start_held(home: &Path, out: &Path, hold: &str) -> Self {
        let big = home.join("big.bin");
        let bytes: Vec<u8> = (0..=255u8).cycle().take(3 << 20).collect();
        std::fs::write(&big, bytes).unwrap();
        let mut cmd = sopen_command(home);
        cmd.arg(&big)
            .env("SOPEN_CHOICE", "save")
            .env("SOPEN_SAVE_TO", out)
            .env("SOPEN_SAVE_HOLD_MS", hold)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut held = Self {
            child: Some(cmd.spawn().unwrap()),
            staging: PathBuf::new(),
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let filling = staging_in(out)
                .into_iter()
                .find(|s| std::fs::metadata(s.join("part")).is_ok_and(|m| m.len() > 0));
            if let Some(staging) = filling {
                held.staging = staging;
                return held;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the save did not start filling its part in 20 s"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Sends `signal` to the sopen alone (not its group).
    fn signal(&self, signal: i32) {
        let child = self.child.as_ref().unwrap();
        // SAFETY: the child is ours and not reaped yet, so the id is still its own.
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
    }

    /// Whether the sopen is still running.
    fn running(&mut self) -> bool {
        self.child.as_mut().unwrap().try_wait().unwrap().is_none()
    }

    /// Sends `signal` to the sopen alone (not its group) and waits at most 20 s for its end.
    /// The child stays under this guard until its exit has been seen: a panic on the way
    /// (the 20 s, a failed wait) still kills and reaps it.
    fn signalled(mut self, signal: i32) -> Output {
        self.signal(signal);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while self.running() {
            assert!(
                std::time::Instant::now() < deadline,
                "sopen did not end within 20 s of signal {signal}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // It has exited: only its output is left to collect.
        let child = self.child.take().unwrap();
        child.wait_with_output().unwrap()
    }
}

impl Drop for HeldSave {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The names in `dir`, sorted.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The private staging folders (`.sopen-<16 hex>`) in `dir`.
fn staging_in(dir: &Path) -> Vec<PathBuf> {
    names_in(dir)
        .into_iter()
        .filter(|n| n.starts_with(".sopen-"))
        .map(|n| dir.join(n))
        .collect()
}

/// Ctrl-C (SIGINT) or SIGTERM during a "Save to…" without a window cancels it: the part and
/// its private folder are removed, nothing is saved, the terminal says so, and sopen ends with
/// 130 or 143 — it does not die mid-way with gigabytes left hidden in the folder.
#[test]
fn a_signal_cancels_a_windowless_save_and_leaves_nothing() {
    for (signal, code) in [(libc::SIGINT, 130), (libc::SIGTERM, 143)] {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().join("out");
        std::fs::create_dir(&out_dir).unwrap();
        let out = HeldSave::start(tmp.path(), &out_dir).signalled(signal);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(code), "signal {signal}: {err}");
        assert!(
            err.lines()
                .any(|l| l == "sopen: cancelled; nothing was saved"),
            "signal {signal}: {err}"
        );
        assert_eq!(text(&out.stdout), "", "signal {signal}");
        assert_eq!(
            names_in(&out_dir),
            Vec::<String>::new(),
            "signal {signal}: nothing saved, no staging left"
        );
    }
}

/// A transfer whose remote has stalled never gets to look at a cancel, so the first SIGTERM
/// changes nothing that can be seen — and must not make sopen unkillable: the same signal again
/// has its default disposition and ends sopen at once. What that leaves is what a SIGKILL
/// leaves, and the next save into the folder removes it.
#[test]
fn a_second_signal_ends_a_stalled_windowless_save_and_the_next_save_sweeps_it() {
    use std::os::unix::process::ExitStatusExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let mut held = HeldSave::start_held(tmp.path(), &out_dir, "60000:stalled");
    let staging = held.staging.clone();
    held.signal(libc::SIGTERM);
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(held.running(), "the first signal only asks for a cancel");
    assert!(staging.join("part").exists());
    let out = held.signalled(libc::SIGTERM);
    assert_eq!(
        (out.status.code(), out.status.signal()),
        (None, Some(libc::SIGTERM)),
        "the second signal ends it: {}",
        text(&out.stderr)
    );
    assert!(
        staging.join("part").exists(),
        "ended mid-way: nothing cleaned"
    );

    let blob = tmp.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1, 2, 3, 0xff]).unwrap();
    let out = sopen(
        tmp.path(),
        &[blob.to_str().unwrap()],
        &[
            ("SOPEN_CHOICE", "save"),
            ("SOPEN_SAVE_TO", out_dir.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(names_in(&out_dir), ["blob.bin"], "the staging left is gone");
}

/// A cancelled save whose staging cannot be removed (here its private folder was made
/// read-only meanwhile) still says so, with the cancel: the folder left is named, never
/// dropped in silence.
#[test]
fn a_cancelled_save_that_cannot_remove_its_staging_says_so() {
    use std::os::unix::fs::PermissionsExt as _;
    // SAFETY: no arguments, no failure.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: root removes from a read-only folder");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let held = HeldSave::start(tmp.path(), &out_dir);
    let staging = held.staging.clone();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o500)).unwrap();
    let out = held.signalled(libc::SIGINT);
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700)).unwrap();
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(130), "{err}");
    let lines: Vec<&str> = err.lines().filter(|l| l.starts_with("sopen: ")).collect();
    assert_eq!(lines.len(), 2, "{err}");
    assert!(
        lines[0].starts_with(&format!(
            "sopen: the staging folder {} could not be removed (",
            staging.display()
        )) && lines[0].ends_with("; it is safe to delete"),
        "{err}"
    );
    assert_eq!(lines[1], "sopen: cancelled; nothing was saved");
    assert!(!out_dir.join("big.bin").exists(), "nothing was saved");
    assert!(staging.join("part").exists(), "what could not be removed");
}

/// A save that was killed outright (SIGKILL: nothing can clean up) leaves its staging folder;
/// the next save into the same folder removes it — its owner is dead, and it holds only sopen's
/// own staging files.
#[test]
fn the_next_save_sweeps_the_staging_a_killed_save_left() {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let held = HeldSave::start(tmp.path(), &out_dir);
    let staging = held.staging.clone();
    let out = held.signalled(libc::SIGKILL);
    assert_eq!(out.status.code(), None, "killed");
    assert!(staging.join("part").exists(), "a kill cleans nothing up");

    let blob = tmp.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1, 2, 3, 0xff]).unwrap();
    let out = sopen(
        tmp.path(),
        &[blob.to_str().unwrap()],
        &[
            ("SOPEN_CHOICE", "save"),
            ("SOPEN_SAVE_TO", out_dir.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(names_in(&out_dir), ["blob.bin"], "the dead staging is gone");
}

/// The staging of a save that is still running belongs to it: another save into the same
/// folder leaves it alone (its lock is held), and the running one still ends cleanly.
#[test]
fn a_running_saves_staging_is_left_alone_by_another_save_there() {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let held = HeldSave::start(tmp.path(), &out_dir);
    let staging = held.staging.clone();

    let blob = tmp.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1, 2, 3, 0xff]).unwrap();
    let out = sopen(
        tmp.path(),
        &[blob.to_str().unwrap()],
        &[
            ("SOPEN_CHOICE", "save"),
            ("SOPEN_SAVE_TO", out_dir.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let part = std::fs::metadata(staging.join("part"));
    assert!(
        part.is_ok_and(|m| m.len() > 0),
        "the running save's part was taken from under it: {:?}",
        names_in(&out_dir)
    );

    let out = held.signalled(libc::SIGINT);
    assert_eq!(out.status.code(), Some(130), "{}", text(&out.stderr));
    assert_eq!(names_in(&out_dir), ["blob.bin"]);
}

/// No display (Linux over ssh): the binary file is not asked about; its local path is printed
/// as before, with one line saying why.
#[cfg(target_os = "linux")]
#[test]
fn other_files_without_a_display_print_their_path() {
    let tmp = tempfile::tempdir().unwrap();
    let blob = tmp.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1, 2, 3]).unwrap();
    let mut cmd = sopen_command(tmp.path());
    cmd.arg(&blob)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    let real = std::fs::canonicalize(&blob).unwrap();
    assert_eq!(text(&out.stdout).trim(), real.to_str().unwrap());
    assert!(
        text(&out.stderr).contains("no display for a window"),
        "{}",
        text(&out.stderr)
    );
}

/// `--hex` opens any file — here a text file — in the hex editor; with `SOPEN_STATE` patches
/// and `SOPEN_AUTOSAVE` it saves them without a window, after a backup.
#[test]
fn hex_flag_saves_patches_to_a_text_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("notes.txt");
    std::fs::write(&file, "hello, world\n").unwrap();
    let out = sopen(
        tmp.path(),
        &["--hex", file.to_str().unwrap()],
        &[
            ("SOPEN_STATE", "at=0x7;patch=0:48,0x7:574F"),
            ("SOPEN_AUTOSAVE", "1"),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(std::fs::read(&file).unwrap(), b"Hello, WOrld\n");
    let printed = text(&out.stdout);
    assert!(printed.contains("backup of "), "{printed}");
    assert!(printed.contains("(3 bytes changed)"), "{printed}");
    let backups = files(&sopen::cache::backup_root(&tmp.path().join("cache")));
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(std::fs::read(&backups[0]).unwrap(), b"hello, world\n");
    let log = usage_lines(tmp.path());
    assert_eq!(
        (log[0]["kind"].as_str(), log[0]["action"].as_str()),
        (Some("text"), Some("save"))
    );
    // A patch past the end is an error; nothing is written.
    let out = sopen(
        tmp.path(),
        &["--hex", file.to_str().unwrap()],
        &[("SOPEN_STATE", "patch=0xd:00"), ("SOPEN_AUTOSAVE", "1")],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(std::fs::read(&file).unwrap(), b"Hello, WOrld\n");
    // Folders are not files.
    let out = sopen(tmp.path(), &["--hex", tmp.path().to_str().unwrap()], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("--hex opens files"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        sopen(tmp.path(), &["--hex", "--app", "X", "a"], &[])
            .status
            .code()
            == Some(2)
    );
}

/// 06 §12 V17: a local file is untouched by streaming. It is read in place, so no stream is
/// made — `SOPEN_STREAM_STATS` prints no line — and the usage line says so: `bytes_fetched` 0
/// beside the file's size (06 §11).
#[test]
fn a_local_file_makes_no_stream() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("blob.bin");
    std::fs::write(&file, [0u8, 1, 2, 3, 0xff, 0xfe, 0, 0]).unwrap();
    let out = sopen(
        tmp.path(),
        &["--hex", file.to_str().unwrap()],
        &[
            ("SOPEN_STREAM_STATS", "1"),
            ("SOPEN_STATE", "patch=1:AA"),
            ("SOPEN_AUTOSAVE", "1"),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(std::fs::read(&file).unwrap()[..2], [0, 0xAA]);
    let said = text(&out.stderr);
    assert!(
        !said.lines().any(|line| line.starts_with("stream ")),
        "a local file got a stream: {said}"
    );
    let log = usage_lines(tmp.path());
    assert_eq!(log[0]["bytes_fetched"], 0, "{}", log[0]);
    assert_eq!(log[0]["bytes"], 8);
    assert_eq!(log[0]["fetch_ms"], 0);
}

/// A scripted save on a binary file must say what to open: the dialog would wait for an answer
/// no one gives (a scripted run ignores input). An error, not a window.
#[test]
fn autosave_on_a_binary_file_needs_the_hex_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let blob = tmp.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1, 2, 3]).unwrap();
    let out = sopen(
        tmp.path(),
        &[blob.to_str().unwrap()],
        &[("SOPEN_STATE", "patch=0:ff"), ("SOPEN_AUTOSAVE", "1")],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("need SOPEN_CHOICE=hex"), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
    assert_eq!(std::fs::read(&blob).unwrap(), [0, 1, 2, 3]);
}

/// The dialog answered "hex" for a binary file: the same hex save path.
#[test]
fn hex_choice_on_a_binary_file_saves_patches() {
    let tmp = tempfile::tempdir().unwrap();
    let blob = tmp.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1, 2, 3]).unwrap();
    let out = sopen(
        tmp.path(),
        &[blob.to_str().unwrap()],
        &[
            ("SOPEN_CHOICE", "hex"),
            ("SOPEN_STATE", "patch=3:ff"),
            ("SOPEN_AUTOSAVE", "1"),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(std::fs::read(&blob).unwrap(), [0, 1, 2, 0xff]);
}

/// Exit 3: the file changed while the autosave waited; nothing of ours is written.
#[test]
fn hex_autosave_refuses_a_file_changed_meanwhile() {
    let tmp = tempfile::tempdir().unwrap();
    let blob = tmp.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1, 2, 3]).unwrap();
    let mut child = sopen_command(tmp.path())
        .args(["--hex", blob.to_str().unwrap()])
        .env("SOPEN_STATE", "patch=0:ff")
        .env("SOPEN_AUTOSAVE", "1")
        .env("SOPEN_AUTOSAVE_DELAY_MS", "1000")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::BufRead;
    let mut lines = std::io::BufReader::new(child.stderr.take().unwrap()).lines();
    let first = lines.next().unwrap().unwrap();
    assert!(first.starts_with("waiting "), "{first}");
    std::fs::write(&blob, b"another writer").unwrap();
    let err: String = lines.map(|l| l.unwrap() + "\n").collect();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(3), "{err}");
    assert!(err.contains("changed since it was opened"), "{err}");
    assert_eq!(std::fs::read(&blob).unwrap(), b"another writer");
    // The edits are listed where the refusal says: one `offset: bytes` line.
    let lists: Vec<_> = files(&sopen::cache::edits_root(&tmp.path().join("cache")))
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("blob.bin.hexpatch-"))
        .collect();
    assert_eq!(lists.len(), 1, "{lists:?}");
    assert!(err.contains(lists[0].to_str().unwrap()), "{err}");
    let text = std::fs::read_to_string(&lists[0]).unwrap();
    assert!(text.lines().any(|l| l == "0x0: FF"), "{text}");
}

/// A folder opens in sopen's own file browser (owner message 12), not in an editor: VS Code
/// (`SOPEN_CODE`) is not run. `SOPEN_LIST` answers without a window, with what the browser
/// lists: the path, folders first, the footer. Text files still go to VS Code.
#[test]
fn a_folder_opens_in_the_browser_not_the_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let code = tmp.path().join("code");
    let record = tmp.path().join("code-record");
    executable(
        &code,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n",
            record.display()
        ),
    );
    let dir = tmp.path().join("project");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), "text").unwrap();
    let out = sopen(
        tmp.path(),
        &[dir.to_str().unwrap()],
        &[("SOPEN_CODE", code.to_str().unwrap()), ("SOPEN_LIST", "1")],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let real = std::fs::canonicalize(&dir).unwrap();
    let listed = text(&out.stdout);
    let lines: Vec<&str> = listed.lines().collect();
    assert_eq!(lines[0], real.to_str().unwrap(), "{listed}");
    assert!(
        lines[1].starts_with("..") && lines[1].ends_with("<UP>"),
        "{listed}"
    );
    assert!(
        lines[2].starts_with("sub ") && lines[2].contains("<DIR>"),
        "{listed}"
    );
    assert!(
        lines[3].starts_with("a ") && lines[3].contains(" txt "),
        "{listed}"
    );
    assert!(lines[4].starts_with("2 Listed = 4 bytes"), "{listed}");
    assert!(!record.exists(), "VS Code was run for a folder");
    let log = usage_lines(tmp.path());
    assert_eq!(
        (log[0]["kind"].as_str(), log[0]["action"].as_str()),
        (Some("dir"), Some("open"))
    );
    // A scripted save has nothing to act on in a folder: an error, not a window no one can use.
    let out = sopen(
        tmp.path(),
        &[dir.to_str().unwrap()],
        &[("SOPEN_AUTOSAVE", "1")],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("is a folder"),
        "{}",
        text(&out.stderr)
    );
    // A text file still opens in VS Code.
    let out = sopen(
        tmp.path(),
        &[dir.join("a.txt").to_str().unwrap()],
        &[("SOPEN_CODE", code.to_str().unwrap())],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    // The CLI is detached (and a new script's first run can be slow); give it a moment.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !record.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let seen = std::fs::read_to_string(&record).unwrap();
    assert_eq!(seen.trim(), real.join("a.txt").to_str().unwrap());
}

/// Runs sopen and waits at most 20 s for it: a run that is still there then (it opened a
/// window and waits for its owner) is killed and reaped, and `None` is returned.
fn sopen_within_20s(home: &Path, args: &[&str], env: &[(&str, &str)]) -> Option<Output> {
    let mut cmd = sopen_command(home);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if child.try_wait().unwrap().is_some() {
            return Some(child.wait_with_output().unwrap());
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The browser's receipt hooks are read before its window opens: a `SOPEN_KEYS` step that names
/// no key, and a `SOPEN_STATE` mode that is neither, end the run with the reason and no window.
/// A run that does not end (it went on to open the window) is killed after 20 s and fails here.
#[test]
fn the_browsers_hooks_are_checked_before_its_window() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    std::fs::create_dir(&dir).unwrap();
    // A display's name is enough to take the browser's route: the hooks are read before any
    // window is made.
    let display = ("DISPLAY", ":77");
    for (hook, value, want) in [
        (
            "SOPEN_KEYS",
            "F5,F99",
            "SOPEN_KEYS: 'F99': unknown key 'F99'",
        ),
        ("SOPEN_KEYS", "Hyper+F5", "unknown modifier 'Hyper'"),
        (
            "SOPEN_STATE",
            "mode=both",
            "mode must be 'dual' or 'preview'",
        ),
    ] {
        let out = sopen_within_20s(
            tmp.path(),
            &[dir.to_str().unwrap()],
            &[display, (hook, value)],
        )
        .unwrap_or_else(|| panic!("{hook}={value}: sopen went on to its window (killed)"));
        let err = text(&out.stderr);
        assert!(
            !out.status.success() && err.contains(want),
            "{hook}={value}: {err}"
        );
    }
}

/// No display (Linux over ssh): a folder cannot show the browser; it opens in the editor as
/// before message 12, and one line says why.
#[cfg(target_os = "linux")]
#[test]
fn a_folder_without_a_display_opens_in_the_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let code = tmp.path().join("code");
    let record = tmp.path().join("code-record");
    executable(
        &code,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n",
            record.display()
        ),
    );
    let dir = tmp.path().join("project");
    std::fs::create_dir(&dir).unwrap();
    let mut cmd = sopen_command(tmp.path());
    cmd.arg(&dir)
        .env("SOPEN_CODE", &code)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("no display"),
        "{}",
        text(&out.stderr)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !record.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let seen = std::fs::read_to_string(&record).unwrap();
    assert_eq!(
        seen.trim(),
        std::fs::canonicalize(&dir).unwrap().to_str().unwrap()
    );
}

#[test]
fn code_takes_priority_over_zed_for_text() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let code = tmp.path().join("code");
    let zed = tmp.path().join("zed");
    let code_record = tmp.path().join("code-record");
    let zed_record = tmp.path().join("zed-record");
    for (exe, record) in [(&code, &code_record), (&zed, &zed_record)] {
        std::fs::write(
            exe,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n",
                record.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let dir = tmp.path().join("project # % 한글");
    std::fs::create_dir(&dir).unwrap();
    let file = dir.join("hello world.txt");
    std::fs::write(&file, "text").unwrap();
    // `sopen_command`'s VSCODE_EXTENSIONS has no Remote - SSH: a local path uses VS Code anyway.
    let out = sopen(
        tmp.path(),
        &[file.to_str().unwrap()],
        &[
            ("SOPEN_CODE", code.to_str().unwrap()),
            ("SOPEN_ZED", zed.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("in VS Code"),
        "{}",
        text(&out.stdout)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !code_record.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let seen = std::fs::read_to_string(&code_record).unwrap();
    assert_eq!(
        seen.lines().collect::<Vec<_>>(),
        [std::fs::canonicalize(&file)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()]
    );
    assert!(!zed_record.exists());
}

/// VS Code's CLI failing is the error, and Zed is not tried instead. `SOPEN_EDITOR_WAIT=60`:
/// the failure is seen even when a loaded machine takes seconds to start the CLI and see it fail
/// (the usual 3 s once ran out first and the run "succeeded"); the wait ends at its exit.
#[test]
fn code_failure_does_not_open_zed() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let code = tmp.path().join("code");
    let zed = tmp.path().join("zed");
    let marker = tmp.path().join("zed-opened");
    std::fs::write(&code, "#!/bin/sh\nexit 42\n").unwrap();
    std::fs::write(&zed, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    for exe in [&code, &zed] {
        std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let file = tmp.path().join("notes.txt");
    std::fs::write(&file, "text").unwrap();
    let out = sopen(
        tmp.path(),
        &[file.to_str().unwrap()],
        &[
            ("SOPEN_CODE", code.to_str().unwrap()),
            ("SOPEN_ZED", zed.to_str().unwrap()),
            ("SOPEN_EDITOR_WAIT", "60"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("42"), "{}", text(&out.stderr));
    assert!(!marker.exists());
}

/// `SOPEN_EDITOR_WAIT` is a number of seconds, at most 120: no value makes sopen wait for an
/// editor CLI that never exits (`exit`, once "until it exits", is not a number: the usual 3 s).
#[test]
fn an_editor_cli_that_never_exits_is_waited_for_a_bounded_time() {
    let tmp = tempfile::tempdir().unwrap();
    // The stand-in runs for as long as this file exists: removed at the end (and with the temp
    // folder), so it never outlives the test, and no pid is ever signalled.
    let alive = tmp.path().join("alive");
    std::fs::write(&alive, b"").unwrap();
    let code = tmp.path().join("code");
    executable(
        &code,
        &format!(
            "#!/bin/sh\nwhile [ -e '{}' ]; do sleep 0.1; done\n",
            alive.display()
        ),
    );
    let file = tmp.path().join("notes.txt");
    std::fs::write(&file, "text").unwrap();
    let begun = std::time::Instant::now();
    let out = sopen(
        tmp.path(),
        &[file.to_str().unwrap()],
        &[
            ("SOPEN_CODE", code.to_str().unwrap()),
            ("SOPEN_EDITOR_WAIT", "exit"),
        ],
    );
    let took = begun.elapsed();
    std::fs::remove_file(&alive).unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(took < std::time::Duration::from_secs(30), "{took:?}");
}

#[test]
fn text_goes_to_the_editor_command() {
    let tmp = tempfile::tempdir().unwrap();
    // A stand-in Zed CLI that records its argument.
    let record = tmp.path().join("zed-args");
    let fake = tmp.path().join("fake-zed");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n",
            record.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let dir = tmp.path().join("proj");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("Makefile"), "all:\n\ttrue\n").unwrap();

    let zed = [("SOPEN_ZED", fake.to_str().unwrap())];
    let out = sopen(tmp.path(), &[dir.join("Makefile").to_str().unwrap()], &zed);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).starts_with("opened "),
        "{}",
        text(&out.stdout)
    );
    // The CLI is detached; give it a moment to write (a new script's first run can take long on a
    // loaded machine; the wait ends as soon as it has written).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut seen = String::new();
    while std::time::Instant::now() < deadline {
        seen = std::fs::read_to_string(&record).unwrap_or_default();
        if seen.lines().count() >= 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let real = std::fs::canonicalize(&dir).unwrap();
    assert_eq!(
        seen.lines().collect::<Vec<_>>(),
        [real.join("Makefile").to_str().unwrap().to_string()]
    );
    // No Zed: local text runs $VISUAL in place.
    let out = sopen(
        tmp.path(),
        &[dir.join("Makefile").to_str().unwrap()],
        &[("SOPEN_ZED", ""), ("VISUAL", "true")],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
}

/// A stand-in `busctl` in `<home>/raise-bin` that records each call's arguments as one line in
/// `<home>/busctl-calls`, copies the script it is asked to load to `<home>/busctl-script`, and
/// answers by the exact method name as KWin does; with `FAKE_FAIL` set, loadScript fails.
/// Returns that bin dir, the calls file and the script copy.
fn fake_busctl(home: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let bin = home.join("raise-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let calls = home.join("busctl-calls");
    let script = home.join("busctl-script");
    let busctl = bin.join("busctl");
    executable(
        &busctl,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{calls}'\n\
             case $6 in\n\
             unloadScript) printf 'b true\\n';;\n\
             loadScript) cat \"$8\" > '{script}'\n\
             if [ -n \"$FAKE_FAIL\" ]; then echo 'Call failed: Access denied' >&2; exit 1; fi\n\
             printf 'i 7\\n';;\n\
             esac\n",
            calls = calls.display(),
            script = script.display(),
        ),
    );
    // Run once first: the first start of a new executable can be slow (macOS checks it), and
    // each call has a deadline.
    run_fresh(Command::new(&busctl).arg("warm"));
    std::fs::remove_file(&calls).unwrap();
    (bin, calls, script)
}

fn lines_of(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// One script loaded, run and unloaded once under one name, and the script file gone.
fn assert_one_run(calls: &[String]) {
    assert_eq!(calls.len(), 3, "{calls:?}");
    let words: Vec<&str> = calls[0].split(' ').collect();
    let (file, name) = (words[7], words[8]);
    assert_eq!(
        calls,
        [
            format!(
                "--user call org.kde.KWin /Scripting org.kde.kwin.Scripting loadScript ss {file} {name}"
            ),
            "--user call org.kde.KWin /Scripting/Script7 org.kde.kwin.Script run".to_string(),
            format!(
                "--user call org.kde.KWin /Scripting org.kde.kwin.Scripting unloadScript s {name}"
            ),
        ]
    );
    assert!(!Path::new(file).exists(), "{file} left behind");
}

/// The detached child (`sopen --raise-window`): one script for the opened name, loaded, run,
/// held about 3 s and unloaded, silently; a failing load is unloaded at once, silently too.
#[test]
fn the_raise_child_asks_kwin_and_cleans_up() {
    let tmp = tempfile::tempdir().unwrap();
    let (bin, calls, script) = fake_busctl(tmp.path());
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let started = std::time::Instant::now();
    let out = sopen(
        tmp.path(),
        &["--raise-window", "code", "--raise-caption=-it's \"한글\"\n"],
        &[("PATH", &path)],
    );
    let took = started.elapsed();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        (text(&out.stdout), text(&out.stderr)),
        (String::new(), String::new())
    );
    assert_one_run(&lines_of(&calls));
    assert!(
        took >= std::time::Duration::from_secs(3),
        "not held: {took:?}"
    );
    // The name, as one escaped string literal.
    let loaded = std::fs::read_to_string(&script).unwrap();
    assert!(
        loaded.contains("const opened = \"-it\\u0027s \\u0022\\ud55c\\uae00\\u0022\\u000a\";"),
        "{loaded}"
    );

    std::fs::remove_file(&calls).unwrap();
    let started = std::time::Instant::now();
    let out = sopen(
        tmp.path(),
        &["--raise-window", "zed", "--raise-caption=proj"],
        &[("PATH", &path), ("FAKE_FAIL", "1")],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        (text(&out.stdout), text(&out.stderr)),
        (String::new(), String::new())
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    let seen = lines_of(&calls);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen[0].contains(" loadScript ss "), "{seen:?}");
    assert!(seen[1].contains(" unloadScript s "), "{seen:?}");
}

/// After VS Code took a text file (a folder opens in sopen's own browser, owner message 12; the
/// browser's F4 raises the same way): on KDE Plasma (Linux) sopen brings its window to the user; not
/// elsewhere, not with SOPEN_RAISE=0, and a failing client changes nothing sopen says or exits
/// with. By default the script runs detached and sopen returns at once.
#[test]
fn the_editor_window_is_brought_to_the_user_on_kde_only() {
    let tmp = tempfile::tempdir().unwrap();
    let (bin, calls, script) = fake_busctl(tmp.path());
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let code = tmp.path().join("code");
    executable(&code, "#!/bin/sh\nexit 0\n");
    let dir = tmp.path().join("project");
    std::fs::create_dir(&dir).unwrap();
    let file = dir.join("notes.txt");
    std::fs::write(&file, "text").unwrap();
    let kde = cfg!(target_os = "linux");
    let open = |desktop: &str, raise: Option<&str>, fail: bool| {
        let _ = std::fs::remove_file(&calls);
        let _ = std::fs::remove_file(&script);
        let mut cmd = sopen_command(tmp.path());
        cmd.arg(&file)
            .env("SOPEN_CODE", &code)
            .env("PATH", &path)
            .env("XDG_CURRENT_DESKTOP", desktop);
        match raise {
            Some(mode) => cmd.env("SOPEN_RAISE", mode),
            None => cmd.env_remove("SOPEN_RAISE"),
        };
        if fail {
            cmd.env("FAKE_FAIL", "1");
        }
        let started = std::time::Instant::now();
        let out = cmd.output().unwrap();
        (out, started.elapsed())
    };

    let (out, _) = open("KDE", Some("wait"), false);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let opened = text(&out.stdout);
    assert!(
        opened.starts_with("opened ") && opened.ends_with(" in VS Code\n"),
        "{opened}"
    );
    assert_eq!(text(&out.stderr), "");
    if kde {
        assert_one_run(&lines_of(&calls));
        let loaded = std::fs::read_to_string(&script).unwrap();
        assert!(loaded.contains("const opened = \"notes.txt\";"), "{loaded}");
    } else {
        assert_eq!(lines_of(&calls), Vec::<String>::new(), "not Linux: no call");
    }

    for (desktop, raise) in [
        ("GNOME", Some("wait")),
        ("ubuntu:GNOME", None),
        ("KDE", Some("0")),
    ] {
        let (out, _) = open(desktop, raise, false);
        assert!(out.status.success(), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), opened, "{desktop} {raise:?}");
        assert_eq!(
            lines_of(&calls),
            Vec::<String>::new(),
            "{desktop} {raise:?}"
        );
    }

    // KWin refusing: the same line, no error, exit 0.
    let (out, _) = open("KDE", Some("wait"), true);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), opened);
    assert_eq!(text(&out.stderr), "");
    if kde {
        assert_eq!(lines_of(&calls).len(), 2, "{:?}", lines_of(&calls));
    }

    // Detached (the default): sopen returns at once; the script is unloaded after it.
    let (out, took) = open("KDE", None, false);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), opened);
    assert!(took < std::time::Duration::from_secs(2), "{took:?}");
    if kde {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while lines_of(&calls).len() < 3 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // The script file's removal may lag the last line by a moment.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_one_run(&lines_of(&calls));
    } else {
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(lines_of(&calls), Vec::<String>::new(), "not Linux: no call");
    }
}

#[test]
fn app_selection_does_not_open_code() {
    let tmp = tempfile::tempdir().unwrap();
    let code = tmp.path().join("code");
    let marker = tmp.path().join("code-opened");
    executable(&code, &format!("#!/bin/sh\ntouch '{}'\n", marker.display()));
    let file = tmp.path().join("notes.txt");
    std::fs::write(&file, "text").unwrap();
    let out = sopen(
        tmp.path(),
        &[file.to_str().unwrap(), "--app", "NoSuchAppSopenTest"],
        &[("SOPEN_CODE", code.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(!marker.exists());
}

#[test]
fn cut_on_a_local_file_writes_a_sibling() {
    let tmp = tempfile::tempdir().unwrap();
    let ffmpeg = std::env::var("SOPEN_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let wav = tmp.path().join("a.wav");
    let generated = Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=4"])
        .arg(&wav)
        .status();
    match generated {
        Ok(s) if s.success() => {}
        _ if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but ffmpeg did not run")
        }
        _ => {
            eprintln!("skipping cut CLI test: ffmpeg not available");
            return;
        }
    }
    let out = sopen(
        tmp.path(),
        &["cut", wav.to_str().unwrap(), "1", "0:03"],
        &[],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let printed = text(&out.stdout);
    assert!(printed.trim().ends_with("/a.cut-1-3.wav"), "{printed}");
    assert!(Path::new(printed.trim()).is_file());
    let bad = sopen(tmp.path(), &["cut", wav.to_str().unwrap(), "3", "1"], &[]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(
        text(&bad.stderr).contains("must be after"),
        "{}",
        text(&bad.stderr)
    );
}

#[test]
fn app_that_does_not_exist_is_one_error_line() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("data.bin");
    std::fs::write(&file, [1u8, 2, 3]).unwrap();
    let out = sopen(
        tmp.path(),
        &[file.to_str().unwrap(), "--app", "NoSuchAppSopenTest"],
        &[],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.contains("NoSuchAppSopenTest"), "{err}");
}

fn small_png(path: &Path) {
    let img = image::RgbImage::from_fn(48, 32, |x, y| {
        image::Rgb([(x * 5) as u8, (y * 7) as u8, 90])
    });
    img.save(path).unwrap();
}

/// Files under `dir`, recursively; an absent `dir` has none, any other error fails the test.
fn files(dir: &Path) -> Vec<std::path::PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => panic!("cannot list {}: {e}", dir.display()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let p = entry
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
            .path();
        if p.is_dir() {
            out.extend(files(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn image_autosave_backs_up_then_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let png = tmp.path().join("shot.png");
    small_png(&png);
    let before = std::fs::read(&png).unwrap();
    let out = sopen(
        tmp.path(),
        &[png.to_str().unwrap()],
        &[
            ("SOPEN_STATE", "rect=4,4,24,16;op=pixelize;block=8"),
            ("SOPEN_AUTOSAVE", "1"),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let printed = text(&out.stdout);
    assert!(printed.contains("backup of "), "{printed}");
    assert!(printed.contains("saved "), "{printed}");
    let after = std::fs::read(&png).unwrap();
    assert_ne!(after, before);
    let saved = image::load_from_memory(&after).unwrap();
    assert_eq!((saved.width(), saved.height()), (48, 32));
    let backups = files(&sopen::cache::backup_root(&tmp.path().join("cache")));
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(std::fs::read(&backups[0]).unwrap(), before);
    let log = std::fs::read_to_string(tmp.path().join("state/usage.jsonl")).unwrap();
    let v: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(
        (v["kind"].as_str(), v["action"].as_str()),
        (Some("image"), Some("save"))
    );
}

#[test]
fn image_autosave_refuses_a_file_changed_meanwhile() {
    let tmp = tempfile::tempdir().unwrap();
    let png = tmp.path().join("shot.png");
    small_png(&png);
    let mut child = sopen_command(tmp.path())
        .arg(&png)
        .env("SOPEN_STATE", "rect=0,0,8,8;op=crop")
        .env("SOPEN_AUTOSAVE", "1")
        .env("SOPEN_AUTOSAVE_DELAY_MS", "1000")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Change the file once sopen has opened it and is waiting to save.
    use std::io::BufRead;
    let mut lines = std::io::BufReader::new(child.stderr.take().unwrap()).lines();
    let first = lines.next().unwrap().unwrap();
    assert!(first.starts_with("waiting "), "{first}");
    std::fs::write(&png, b"another writer").unwrap();
    let err: String = lines.map(|l| l.unwrap() + "\n").collect();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(3), "{err}");
    assert!(err.contains("changed since it was opened"), "{err}");
    assert_eq!(std::fs::read(&png).unwrap(), b"another writer");
    let kept: Vec<_> = files(&sopen::cache::edits_root(&tmp.path().join("cache")))
        .into_iter()
        .filter(|p| p.to_string_lossy().contains(".conflict-"))
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(err.contains(kept[0].to_str().unwrap()), "{err}");
}

#[test]
fn media_autoexport_writes_a_sibling() {
    let tmp = tempfile::tempdir().unwrap();
    let ffmpeg = std::env::var("SOPEN_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let wav = tmp.path().join("talk.wav");
    let generated = Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=4"])
        .arg(&wav)
        .status();
    match generated {
        Ok(s) if s.success() => {}
        _ if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but ffmpeg did not run")
        }
        _ => {
            eprintln!("skipping autoexport test: ffmpeg not available");
            return;
        }
    }
    let out = sopen(
        tmp.path(),
        &[wav.to_str().unwrap()],
        &[("SOPEN_STATE", "in=1;out=2.5"), ("SOPEN_AUTOEXPORT", "1")],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let printed = text(&out.stdout);
    assert!(printed.contains("/talk.cut-1-2.5.wav"), "{printed}");
    assert!(tmp.path().join("talk.cut-1-2.5.wav").is_file());
    // An image op on media is a hook error, not a silent no-op.
    let bad = sopen(
        tmp.path(),
        &[wav.to_str().unwrap()],
        &[("SOPEN_STATE", "op=crop"), ("SOPEN_AUTOEXPORT", "1")],
    );
    assert_eq!(bad.status.code(), Some(1));
}

// ---------------------------------------------------------------------------------------------
// Where it opens (spec §4a). `ssh` is a stand-in: it answers `ssh -G`, records the call that runs
// sopen on the target (`argv`; every call, in `calls`, and `<its pid> <its parent's>`, in
// `ssh-pids`), and then
// runs it as OpenSSH does — the words after the destination
// joined with spaces, `sh -c "<line>"` on the "target" (here, in a home of its own, with a
// PATH of only the tools the script uses and a display set; the script's Linux branch has its
// own tests). The target has nothing installed: no sopen on that PATH, and the absolute install
// paths in the command are pointed at a directory that does not exist, so a sopen installed on
// the test machine is never run. `FAKE_SSH_EXIT` stops it before the run, with that code;
// `FAKE_SSH_TAKEN=<port>` fails a `-R` on that port (bound to 127.0.0.1) as ssh does with
// `ExitOnForwardFailure`; `FAKE_SSH_LINGER=1` keeps it running after the target's command ends,
// its output closed, as a real ssh waits for the forwarded connections a browser keeps open (and
// hands its output to the session, which has ended). As sshd does, it opens the forward's
// listener before the target's command starts: a loopback LISTEN row for the `-R` port goes into
// the `SOPEN_LISTENERS` table, if one is set (`FAKE_SSH_NO_BIND=1`: no row, a listener HOST
// cannot see). The target's stand-in sopen prints `FAKE_SOPEN_SAY` (its lines), if set — with
// `FAKE_SOPEN_PACE=<seconds>`, one at a time, that long after each — and with
// `FAKE_SOPEN_HOLD=1` then reads its input to the end, as HOST's half of a tunnel holds, before
// it exits — so it plays a HOST sopen of any version, cues included (09 §7). With
// `FAKE_SOPEN_UNKNOWN=<flag>` it is a sopen that does not know that flag: given it, it prints
// clap's usage error for it on its stderr and exits 2 before anything else, as clap does.

/// A target name no test machine has.
const HOST: &str = "sopen-test-target";

/// What the fake `ssh -G` reports; nothing binds it. Short, so no temp dir is too long for it.
const CONTROL_PATH: &str = "/s/cm-test";

/// Where the target looks for sopen besides PATH (spec §4a), outside the target's home; the fake
/// ssh points them elsewhere.
const INSTALLED: [&str; 3] = [
    "/opt/homebrew/bin/sopen",
    "/usr/local/bin/sopen",
    "/home/linuxbrew/.linuxbrew/bin/sopen",
];

/// The tools the target's script runs besides shell builtins (its Linux display branch is not
/// taken: the fake target has a display).
const TARGET_TOOLS: [&str; 3] = ["env", "sh", "uname"];

/// `name` on the test process's PATH: the first absolute, executable file, canonicalized.
fn which(name: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .filter_map(|dir| dir.join(name).canonicalize().ok())
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

struct FakeSsh {
    dir: std::path::PathBuf,
    path: String,
    /// A sopen for the target that records its arguments (`FAKE_SOPEN_EXIT` = its exit code,
    /// `FAKE_SOPEN_SIGNAL` = a signal it sends itself).
    sopen: std::path::PathBuf,
}

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Runs `cmd` to its end, for a stub executable this test has just written. On Linux such a file
/// can be "busy" (ETXTBSY) for a moment: a child that another test thread forked meanwhile still
/// holds its write descriptor until that child execs. A busy start is retried, up to 50 times
/// 20 ms apart; any other error, or the last busy one, panics. (The lib's tests have the same
/// helper, tested, in src/testutil.rs.)
fn run_fresh(cmd: &mut Command) -> Output {
    retry_busy(|| cmd.output()).unwrap_or_else(|e| panic!("cannot run {cmd:?}: {e}"))
}

/// `attempt` until it is not refused as busy (ETXTBSY), at most 50 retries 20 ms apart; then
/// whatever it gave.
fn retry_busy<T>(mut attempt: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut retries = 0;
    loop {
        match attempt() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && retries < 50 => {
                retries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            other => return other,
        }
    }
}

impl FakeSsh {
    fn new(home: &Path) -> Self {
        let dir = home.join("fake-ssh");
        let target_path = dir.join("target-path");
        std::fs::create_dir_all(&target_path).unwrap();
        for tool in TARGET_TOOLS {
            let real = which(tool).unwrap_or_else(|| panic!("{tool} is not on PATH"));
            std::os::unix::fs::symlink(real, target_path.join(tool)).unwrap();
        }
        let unplug: String = INSTALLED
            .iter()
            .map(|p| format!(" -e 's|{p}|{}/not-installed{p}|g'", dir.display()))
            .collect();
        executable(
            &dir.join("ssh"),
            &format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = -G ]; then printf 'controlpath {CONTROL_PATH}\\nconnecttimeout none\\n'; exit 0; fi\n\
                 for a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{dir}/argv'\n\
                 printf '%s %s\\n' \"$$\" \"$PPID\" >> '{dir}/ssh-pids'\n\
                 {{ for a in \"$@\"; do printf '%s\\n' \"$a\"; done; echo ---; }} >> '{dir}/calls'\n\
                 if [ -n \"${{FAKE_SSH_EXIT:-}}\" ]; then exit \"$FAKE_SSH_EXIT\"; fi\n\
                 fwd=; prev=; for a in \"$@\"; do if [ \"$prev\" = -R ]; then fwd=$a; fi; prev=$a; done\n\
                 if [ -n \"${{FAKE_SSH_TAKEN:-}}\" ]; then case $fwd in \"127.0.0.1:$FAKE_SSH_TAKEN\":*) \
                 echo \"Error: remote port forwarding failed for listen port $FAKE_SSH_TAKEN\" >&2; exit 255;; esac; fi\n\
                 if [ -n \"${{SOPEN_LISTENERS:-}}\" ] && [ -z \"${{FAKE_SSH_NO_BIND:-}}\" ]; then case $fwd in \
                 127.0.0.1:*) p=${{fwd#127.0.0.1:}}; printf 'tcp4 0 0 127.0.0.1.%s *.* LISTEN 0\\n' \"${{p%%:*}}\" >> \"$SOPEN_LISTENERS\";; \
                 \\[::1\\]:*) p=${{fwd#*]:}}; printf 'tcp6 0 0 ::1.%s *.* LISTEN 0\\n' \"${{p%%:*}}\" >> \"$SOPEN_LISTENERS\";; esac; fi\n\
                 while [ \"$#\" -gt 0 ] && [ \"$1\" != -- ]; do shift; done\n\
                 shift\n\
                 line=$(printf '%s' \"$*\" | sed{unplug})\n\
                 mkdir -p '{dir}/target-home'\n\
                 if [ -n \"${{FAKE_SSH_LINGER:-}}\" ]; then \
                 HOME='{dir}/target-home' PATH='{dir}/target-path' DISPLAY=:99 /bin/sh -c \"$line\"; \
                 exec >/dev/null; trap '' INT HUP; exec sleep 120; fi\n\
                 HOME='{dir}/target-home' PATH='{dir}/target-path' DISPLAY=:99 exec /bin/sh -c \"$line\"\n",
                dir = dir.display()
            ),
        );
        // A sopen on this machine's PATH (first on it, beside the fake ssh) that the target must
        // never run: it leaves `decoy-ran` behind.
        executable(
            &dir.join("sopen"),
            &format!("#!/bin/sh\n: > '{}/decoy-ran'\n", dir.display()),
        );
        let sopen = dir.join("target-sopen");
        // The target's PATH has no `sleep`: the pace runs this machine's, by its path.
        let sleep = which("sleep").expect("sleep is not on PATH");
        executable(
            &sopen,
            &format!(
                "#!/bin/sh\n\
                 if [ -n \"${{FAKE_SOPEN_UNKNOWN:-}}\" ]; then for a in \"$@\"; do \
                 if [ \"$a\" = \"$FAKE_SOPEN_UNKNOWN\" ]; then printf \"error: unexpected argument \
                 '%s' found\\n\\n  tip: a similar argument exists: '--tui'\\n\\nUsage: sopen --here \
                 --tui [SOURCE]\\n\\nFor more information, try '--help'.\\n\" \"$a\" >&2; exit 2; fi; \
                 done; fi\n\
                 for a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{dir}/sopen-argv'\n\
                 if [ -n \"${{FAKE_SOPEN_SIGNAL:-}}\" ]; then kill -\"$FAKE_SOPEN_SIGNAL\" $$; fi\n\
                 if [ -n \"${{FAKE_SOPEN_PACE:-}}\" ]; then printf '%s\\n' \"$FAKE_SOPEN_SAY\" | \
                 while IFS= read -r l; do printf '%s\\n' \"$l\"; '{sleep}' \"$FAKE_SOPEN_PACE\"; done\n\
                 elif [ -n \"${{FAKE_SOPEN_SAY:-}}\" ]; then printf '%s\\n' \"$FAKE_SOPEN_SAY\"; fi\n\
                 if [ -n \"${{FAKE_SOPEN_HOLD:-}}\" ]; then while read -r _; do :; done; fi\n\
                 exit \"${{FAKE_SOPEN_EXIT:-0}}\"\n",
                dir = dir.display(),
                sleep = sleep.display()
            ),
        );
        let path = format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Self { dir, path, sopen }
    }

    fn lines(&self, name: &str) -> Option<Vec<String>> {
        let text = std::fs::read_to_string(self.dir.join(name)).ok()?;
        Some(text.lines().map(str::to_string).collect())
    }

    /// The arguments of the call that ran sopen on the target (`None`: ssh was not run).
    fn argv(&self) -> Option<Vec<String>> {
        self.lines("argv")
    }

    /// The arguments the target's sopen got (`None`: it did not run).
    fn sopen_argv(&self) -> Option<Vec<String>> {
        self.lines("sopen-argv")
    }

    fn forget(&self) {
        for name in ["argv", "sopen-argv"] {
            let _ = std::fs::remove_file(self.dir.join(name));
        }
    }

    fn connection(&self) -> Vec<String> {
        let mut args = vec!["-S".to_string(), CONTROL_PATH.to_string()];
        for opt in [
            "ControlMaster=auto",
            "ControlPersist=60s",
            "ServerAliveInterval=10",
            "ServerAliveCountMax=3",
            "ConnectTimeout=15",
        ] {
            args.extend(["-o".to_string(), opt.to_string()]);
        }
        args
    }
}

/// `'word'` — the target's words are single-quoted (spec §4a).
fn q(word: &str) -> String {
    format!("'{word}'")
}

fn usage_lines(home: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(home.join("state/usage.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn target_runs_sopen_there_over_ssh_and_keeps_its_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let bin = ssh.sopen.to_str().unwrap();
    let source = format!("{HOST}:/tmp/a b.png");
    let out = sopen(
        tmp.path(),
        &["--target", HOST, &source],
        &[
            ("PATH", &ssh.path),
            ("SOPEN_TARGET_BIN", bin),
            ("FAKE_SOPEN_EXIT", "3"),
        ],
    );
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let argv = ssh.argv().expect("ssh ran");
    let mut want = ssh.connection();
    // No terminal here (stdin is not one), so none there, and no input either; a RemoteCommand
    // in ~/.ssh/config is overridden.
    want.extend(
        [
            "-T",
            "-n",
            "-o",
            "RemoteCommand=none",
            "-o",
            "LogLevel=ERROR",
            HOST,
            "--",
        ]
        .map(String::from),
    );
    assert_eq!(argv[..want.len()], want[..], "{argv:#?}");
    let words = &argv[want.len()..];
    let hook = format!("SOPEN_TARGET_BIN={bin}");
    assert_eq!(
        words[..5],
        ["exec".to_string(), q("env"), q(&hook), q("sh"), q("-c")],
        "{words:#?}"
    );
    // The file is on the target itself: its path there, opened there with --here.
    assert_eq!(ssh.sopen_argv().unwrap(), ["--here", "--", "/tmp/a b.png"]);
    // HOST refused a write: a conflict in this machine's log too.
    let log = usage_lines(tmp.path());
    assert_eq!(log[0]["forwarded"], true);
    assert_eq!(log[0]["conflict"], true);
    assert_eq!(log[0]["ext"], "png");
    assert_eq!(log[0]["ok"], false, "exit 3 is not ok");

    // Quotes, backslashes, `$`, spaces and non-ASCII arrive as typed.
    let odd = r#"/tmp/it's a $HOME \x "y" 한글.png"#;
    let out = sopen(
        tmp.path(),
        &["--target", HOST, &format!("{HOST}:{odd}")],
        &[("PATH", &ssh.path), ("SOPEN_TARGET_BIN", bin)],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(ssh.sopen_argv().unwrap(), ["--here", "--", odd]);

    // A local file is fetched by the target from this machine; hooks travel, local paths stay.
    let blob = tmp.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1]).unwrap();
    let out = sopen(
        tmp.path(),
        &[
            "--target",
            HOST,
            "--app",
            "Hex Fiend",
            blob.to_str().unwrap(),
        ],
        &[
            ("PATH", &ssh.path),
            ("SOPEN_TARGET_BIN", bin),
            ("SOPEN_LOG", "debug"),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let short = sopen::forward::short_hostname().expect("this machine has a host name");
    let real = std::fs::canonicalize(&blob).unwrap();
    assert_eq!(
        ssh.sopen_argv().unwrap(),
        [
            "--here",
            "--app",
            "Hex Fiend",
            "--",
            &format!("{short}:{}", real.display())
        ]
    );
    let argv = ssh.argv().unwrap();
    let at = argv.iter().position(|a| a == "--").unwrap();
    assert_eq!(
        argv[at + 1..at + 5],
        ["exec".to_string(), q("env"), q("SOPEN_LOG=debug"), q(&hook)]
    );
    // `--app` waits for Enter on this side's input: no `-n`.
    let n = ssh.connection().len();
    assert_eq!(argv[n..n + 2], ["-T", "-o"], "{argv:#?}");

    // `--hex` travels to the target's sopen, with the dialog's hooks.
    let out = sopen(
        tmp.path(),
        &["--target", HOST, "--hex", &format!("{HOST}:/tmp/a.bin")],
        &[
            ("PATH", &ssh.path),
            ("SOPEN_TARGET_BIN", bin),
            ("SOPEN_CHOICE", "save"),
            ("SOPEN_SAVE_TO", "/tmp/sopen-receipts/a.bin"),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        ssh.sopen_argv().unwrap(),
        ["--here", "--hex", "--", "/tmp/a.bin"]
    );
    let argv = ssh.argv().unwrap();
    for hook in [
        "SOPEN_CHOICE=save",
        "SOPEN_SAVE_TO=/tmp/sopen-receipts/a.bin",
    ] {
        assert!(argv.contains(&q(hook)), "{hook}: {argv:#?}");
    }
    assert!(
        !argv.iter().any(|a| a.contains("SOPEN_CACHE_DIR")),
        "{argv:#?}"
    );

    // A missing local file is the same error as a local open, and nothing is run.
    ssh.forget();
    let missing = tmp.path().join("nope.txt");
    let out = sopen(
        tmp.path(),
        &["--target", HOST, missing.to_str().unwrap()],
        &[("PATH", &ssh.path), ("SOPEN_TARGET_BIN", bin)],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("no such file"),
        "{}",
        text(&out.stderr)
    );
    assert!(ssh.argv().is_none());
    let log = std::fs::read_to_string(tmp.path().join("state/usage.jsonl")).unwrap();
    assert!(!log.contains(HOST), "{log}");
}

/// `--gui` on a forward (10 §7.3, T35; X2-gui), until XF: passed only when the owner gave it
/// (decided 2026-11-26) — whatever the kind, a hook keeping the run without a window included —
/// never with `--app` (a usage error) nor for an address (§11 rows 1 and 4). Without it a run is
/// today's, HOST picking by kind; the kinds a sender sees are `seen_kind`'s, tested in the
/// binary, for XF's automatic `--gui`.
#[test]
fn a_target_run_passes_gui_only_when_given() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let bin = ssh.sopen.to_str().unwrap();
    let short = sopen::forward::short_hostname().expect("this machine has a host name");
    std::fs::create_dir(tmp.path().join("dir")).unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "text\n").unwrap();
    std::fs::write(tmp.path().join("blob"), [0u8, 1, 2, 0]).unwrap();
    std::fs::write(tmp.path().join("a.mp3"), b"").unwrap();
    let path = |name: &str| tmp.path().join(name).to_str().unwrap().to_string();
    let there = |name: &str| {
        let real = std::fs::canonicalize(tmp.path().join(name)).unwrap();
        format!("{short}:{}", real.display())
    };
    let ran = |args: &[&str], hook: (&str, &str)| -> Vec<String> {
        ssh.forget();
        let args: Vec<&str> = ["--target", HOST]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        let out = sopen(
            tmp.path(),
            &args,
            &[("PATH", &ssh.path), ("SOPEN_TARGET_BIN", bin), hook],
        );
        assert!(
            out.status.success(),
            "{args:?} {hook:?}: {}",
            text(&out.stderr)
        );
        ssh.sopen_argv().expect("sopen ran on the target")
    };
    let plain = ("SOPEN_EDITOR_WAIT", "3");
    let with = |gui: bool, source: &str| -> Vec<String> {
        let mut want = vec!["--here".to_string()];
        if gui {
            want.push("--gui".to_string());
        }
        want.extend(["--".to_string(), source.to_string()]);
        want
    };
    let image = format!("{HOST}:/srv/img.png");
    // No kind brings it: a folder, an image, an mp3, a binary, text, a name that decides nothing.
    for (source, as_there) in [
        (path("dir"), there("dir")),
        (path("a.mp3"), there("a.mp3")),
        (path("blob"), there("blob")),
        (path("notes.txt"), there("notes.txt")),
        (image.clone(), "/srv/img.png".to_string()),
        (format!("{HOST}:/srv/data"), "/srv/data".to_string()),
    ] {
        assert_eq!(ran(&[&source], plain), with(false, &as_there), "{source}");
        assert_eq!(
            ran(&["--gui", &source], plain),
            with(true, &as_there),
            "{source}"
        );
    }
    // Nor `--hex`, nor a hook — the owner's `--gui` goes with any of them.
    assert_eq!(
        ran(&["--hex", &image], plain),
        ["--here", "--hex", "--", "/srv/img.png"]
    );
    assert_eq!(
        ran(&["--gui", "--hex", &image], plain),
        ["--here", "--gui", "--hex", "--", "/srv/img.png"]
    );
    for hook in [
        ("SOPEN_LIST", "1"),
        ("SOPEN_AUTOSAVE", "1"),
        ("SOPEN_SCREENSHOT", "/tmp/shot.png"),
    ] {
        assert_eq!(
            ran(&[&image], hook),
            with(false, "/srv/img.png"),
            "{hook:?}"
        );
        assert_eq!(
            ran(&["--gui", &image], hook),
            with(true, "/srv/img.png"),
            "{hook:?}"
        );
    }
    // `--app` never; with `--gui` it is a usage error.
    assert_eq!(
        ran(&["--app", "X", &path("blob")], plain),
        ["--here", "--app", "X", "--", &there("blob")]
    );
    ssh.forget();
    let out = sopen(
        tmp.path(),
        &["--target", HOST, "--gui", "--app", "X", &path("blob")],
        &[("PATH", &ssh.path), ("SOPEN_TARGET_BIN", bin)],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(ssh.argv().is_none(), "ssh ran");
    // An address never: the owner's `--gui` stays here.
    let (browser, _) = stand_in_browser(tmp.path());
    let url = "https://example.com/a";
    ssh.forget();
    let out = sopen(
        tmp.path(),
        &["--gui", "--target", HOST, "--browser", url],
        &[
            ("PATH", &ssh.path),
            ("SOPEN_TARGET_BIN", bin),
            ("SOPEN_BROWSER", browser.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        ssh.sopen_argv().unwrap(),
        ["--here", "--browser", "--", url]
    );
}

/// The line sopen says when the target's sopen does not know `--gui` (X2-gui).
fn older_line() -> String {
    format!("sopen: sopen on {HOST} is older and does not know --gui; running it there again without it")
}

/// A target whose sopen predates `--gui` (X2-gui): clap there refuses it (exit 2, its line) before
/// anything runs, so the same run goes once more without it, and one line says why; HOST's exit
/// code is then the run's.
#[test]
fn an_older_sopen_on_the_target_gets_the_run_again_without_gui() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let dir = tmp.path().join("dir");
    std::fs::create_dir(&dir).unwrap();
    let out = sopen(
        tmp.path(),
        &[
            "--target",
            HOST,
            "--gui",
            "--hex",
            &format!("{HOST}:/srv/a.bin"),
        ],
        &[
            ("PATH", &ssh.path),
            ("SOPEN_TARGET_BIN", ssh.sopen.to_str().unwrap()),
            ("FAKE_SOPEN_UNKNOWN", "--gui"),
            ("FAKE_SOPEN_EXIT", "3"),
        ],
    );
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let calls = ssh_calls(&ssh);
    assert_eq!(calls.len(), 2, "{calls:#?}");
    // The first run asked for the window, the second is the same run without that one word.
    assert!(calls[0].contains(&q("--gui")), "{:#?}", calls[0]);
    let without: Vec<String> = calls[0]
        .iter()
        .filter(|w| *w != &q("--gui"))
        .cloned()
        .collect();
    assert_eq!(calls[1], without);
    assert_eq!(
        ssh.sopen_argv().unwrap(),
        ["--here", "--hex", "--", "/srv/a.bin"]
    );
    // HOST's own lines pass as they came; then sopen's one line.
    let err = text(&out.stderr);
    assert!(
        err.starts_with("error: unexpected argument '--gui' found\n"),
        "{err}"
    );
    assert_eq!(err.lines().last(), Some(older_line().as_str()), "{err}");
    assert_eq!(err.matches(&older_line()).count(), 1, "{err}");
    let log = usage_lines(tmp.path());
    assert_eq!(log.len(), 1, "one run in the log");
    assert_eq!(log[0]["conflict"], true);

    // A folder there: the same, with exit 0.
    ssh.forget();
    let _ = std::fs::remove_file(ssh.dir.join("calls"));
    let out = sopen(
        tmp.path(),
        &["--target", HOST, "--gui", dir.to_str().unwrap()],
        &[
            ("PATH", &ssh.path),
            ("SOPEN_TARGET_BIN", ssh.sopen.to_str().unwrap()),
            ("FAKE_SOPEN_UNKNOWN", "--gui"),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(ssh_calls(&ssh).len(), 2);
    let short = sopen::forward::short_hostname().expect("this machine has a host name");
    let real = std::fs::canonicalize(&dir).unwrap();
    assert_eq!(
        ssh.sopen_argv().unwrap(),
        ["--here", "--", &format!("{short}:{}", real.display())]
    );
    assert!(text(&out.stderr).ends_with(&format!("{}\n", older_line())));
}

/// Any other exit 2 from the target is its answer as it is, never retried: clap's line for
/// another flag (a sopen older still, without `--here`), or a usage error without that line.
#[test]
fn another_usage_error_on_the_target_is_not_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let source = format!("{HOST}:/srv/a.png");
    for (env, line) in [
        (
            ("FAKE_SOPEN_UNKNOWN", "--here"),
            Some("error: unexpected argument '--here' found"),
        ),
        (("FAKE_SOPEN_EXIT", "2"), None),
    ] {
        let _ = std::fs::remove_file(ssh.dir.join("calls"));
        let out = sopen(
            tmp.path(),
            &["--target", HOST, "--gui", &source],
            &[
                ("PATH", &ssh.path),
                ("SOPEN_TARGET_BIN", ssh.sopen.to_str().unwrap()),
                env,
            ],
        );
        assert_eq!(out.status.code(), Some(2), "{env:?}: {}", text(&out.stderr));
        let calls = ssh_calls(&ssh);
        assert_eq!(calls.len(), 1, "{env:?}: {calls:#?}");
        assert!(calls[0].contains(&q("--gui")), "{:#?}", calls[0]);
        let err = text(&out.stderr);
        assert!(!err.contains("is older"), "{env:?}: {err}");
        if let Some(line) = line {
            assert!(err.starts_with(&format!("{line}\n")), "{err}");
        }
    }
}

/// The run is made again only on evidence that HOST's sopen refused `--gui` before it did
/// anything (X2-gui, 2026-11-26): exit 2, clap's line the first HOST wrote on its stderr (blank
/// lines and colours aside), nothing on its stdout. A sopen that does something first and says so
/// — a line on its stdout, or another line on its stderr before clap's — then exits 2 with that
/// line is HOST's answer as it is: run once, its side effect once.
#[test]
fn a_target_that_did_something_before_the_line_is_not_run_again() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let ran = tmp.path().join("ran");
    let wrapper = tmp.path().join("wrapper-sopen");
    let clap = "error: unexpected argument '--gui' found";
    for (said, retried) in [
        (
            format!("printf 'wrote it\\n'; printf \"\\n{clap}\\n\" >&2"),
            false,
        ),
        (format!("printf \"fetching a.png\\n{clap}\\n\" >&2"), false),
        (
            format!("printf \"\\n\\n{clap}\\n\\nUsage: sopen\\n\" >&2"),
            true,
        ),
    ] {
        let _ = std::fs::remove_file(&ran);
        let _ = std::fs::remove_file(ssh.dir.join("calls"));
        // On the second run (no `--gui`) it opens as a newer sopen would: exit 0.
        executable(
            &wrapper,
            &format!(
                "#!/bin/sh\nprintf 'x\\n' >> '{ran}'\n\
                 for a in \"$@\"; do if [ \"$a\" = --gui ]; then {said}; exit 2; fi; done\nexit 0\n",
                ran = ran.display()
            ),
        );
        let out = sopen(
            tmp.path(),
            &["--target", HOST, "--gui", &format!("{HOST}:/srv/dir")],
            &[
                ("PATH", &ssh.path),
                ("SOPEN_TARGET_BIN", wrapper.to_str().unwrap()),
            ],
        );
        let err = text(&out.stderr);
        let runs = std::fs::read_to_string(&ran).unwrap().lines().count();
        let calls = ssh_calls(&ssh).len();
        if retried {
            assert_eq!(out.status.code(), Some(0), "{said}: {err}");
            assert_eq!((calls, runs), (2, 2), "{said}: {err}");
            assert!(err.ends_with(&format!("{}\n", older_line())), "{err}");
        } else {
            assert_eq!(out.status.code(), Some(2), "{said}: {err}");
            assert_eq!((calls, runs), (1, 1), "{said}: {err}");
            assert!(!err.contains("is older"), "{said}: {err}");
            assert!(err.contains(clap), "{said}: {err}");
        }
    }
}

/// HOST's sopen given `--gui` with no display there says §7.1's line; this side passes it on after
/// `on HOST: `, so it is not taken for this machine's, and leaves with HOST's exit 1.
#[test]
fn hosts_no_display_line_is_said_as_hosts() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let line =
        "sopen: no display here — sopen config target <host>, or --target, opens the window \
                on another machine";
    let wrapper = tmp.path().join("no-display-sopen");
    executable(
        &wrapper,
        &format!("#!/bin/sh\nprintf '%s\\n' '{line}' >&2\nexit 1\n"),
    );
    let out = sopen(
        tmp.path(),
        &["--target", HOST, "--gui", &format!("{HOST}:/srv/dir")],
        &[
            ("PATH", &ssh.path),
            ("SOPEN_TARGET_BIN", wrapper.to_str().unwrap()),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), format!("on {HOST}: {line}\n"));
}

/// `--gui` asks for the window where SOURCE opens; here, with no display, that is §7.1's line and
/// exit 1 (10 §11 row 2) before anything is read.
#[cfg(target_os = "linux")]
#[test]
fn gui_with_no_display_here_is_its_line_and_exit_1() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("dir");
    std::fs::create_dir(&dir).unwrap();
    let mut cmd = sopen_command(tmp.path());
    cmd.args(["--gui", "--here"])
        .arg(&dir)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stderr),
        "sopen: no display here — sopen config target <host>, or --target, opens the window on \
         another machine\n"
    );
}

#[test]
fn no_sopen_on_the_target_is_its_own_message_and_127() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let out = sopen(
        tmp.path(),
        &["--target", HOST, "other:/x.png"],
        &[("PATH", &ssh.path)],
    );
    assert_eq!(out.status.code(), Some(127), "{}", text(&out.stderr));
    assert!(
        !ssh.dir.join("decoy-ran").exists(),
        "the target ran a sopen from this machine's PATH"
    );
    assert_eq!(
        text(&out.stderr),
        format!("sopen is not installed on {HOST} (brew install 2lab-ai/tap/sopen-preview)\n")
    );
    assert!(ssh.sopen_argv().is_none());
    // What was sent is the real list: the fake only pointed these elsewhere.
    let argv = ssh.argv().unwrap();
    for path in INSTALLED {
        assert!(argv.iter().any(|w| w.contains(path)), "{path} not sent");
    }
}

#[test]
fn here_and_target_together_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let out = sopen(
        tmp.path(),
        &["--here", "--target", HOST, "x:/a.png"],
        &[("PATH", &ssh.path)],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("cannot be used with"),
        "{}",
        text(&out.stderr)
    );
    assert!(ssh.argv().is_none(), "ssh ran");
}

#[test]
fn ssh_failing_is_could_not_reach() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let out = sopen(
        tmp.path(),
        &["--target", HOST, "other:/x.png"],
        &[("PATH", &ssh.path), ("FAKE_SSH_EXIT", "255")],
    );
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert_eq!(err, format!("sopen: could not reach {HOST}\n"));
    // Another machine's file goes as it was typed.
    assert_eq!(ssh.argv().unwrap().last().unwrap(), &q("other:/x.png"));
    // The target's sopen stopped by a signal is said so — not "could not reach".
    let out = sopen(
        tmp.path(),
        &["--target", HOST, "other:/x.png"],
        &[
            ("PATH", &ssh.path),
            ("SOPEN_TARGET_BIN", ssh.sopen.to_str().unwrap()),
            ("FAKE_SOPEN_SIGNAL", "TERM"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    // (The target's shell may report the signal on its own line first.)
    let err = text(&out.stderr);
    assert_eq!(
        err.lines().last(),
        Some(format!("sopen: sopen on {HOST} ended with status 143 (signal 15?)").as_str()),
        "{err}"
    );
    // A HOST that would read as an ssh option is refused before ssh runs.
    ssh.forget();
    let out = sopen(
        tmp.path(),
        &["--target=-oProxyCommand=x", "other:/x.png"],
        &[("PATH", &ssh.path)],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("--target: "),
        "{}",
        text(&out.stderr)
    );
    assert!(ssh.argv().is_none());
}

/// The destination ssh was asked for (`None`: ssh was not run).
fn destination(ssh: &FakeSsh) -> Option<String> {
    let argv = ssh.argv()?;
    let at = argv.iter().position(|a| a == "--")?;
    Some(argv[at - 1].clone())
}

#[test]
fn default_target_from_config_or_env_and_the_flags_beat_both() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let path = [("PATH", ssh.path.as_str()), ("FAKE_SSH_EXIT", "0")];
    let set = sopen(tmp.path(), &["config", "target", HOST], &path);
    assert!(set.status.success(), "{}", text(&set.stderr));
    let shown = sopen(tmp.path(), &["config"], &path);
    let config = tmp.path().join("config.toml");
    assert_eq!(
        text(&shown.stdout),
        format!("{}\ntarget = {HOST}\n", config.display())
    );

    // The config's target, with no flag.
    let blob = tmp.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1]).unwrap();
    let out = sopen(tmp.path(), &["x:/a.mp3"], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(destination(&ssh).as_deref(), Some(HOST));

    // SOPEN_TARGET comes before the config.
    let env = [
        ("PATH", ssh.path.as_str()),
        ("FAKE_SSH_EXIT", "0"),
        ("SOPEN_TARGET", "env-target"),
    ];
    let out = sopen(tmp.path(), &["x:/a.mp3"], &env);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(destination(&ssh).as_deref(), Some("env-target"));

    // --target comes before both.
    let out = sopen(tmp.path(), &["--target", "flag-target", "x:/a.mp3"], &env);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(destination(&ssh).as_deref(), Some("flag-target"));

    // --here beats both: opened here (its binary-file dialog, answered "cancel"), no ssh.
    ssh.forget();
    let here = [env.as_slice(), &[("SOPEN_CHOICE", "cancel")]].concat();
    let out = sopen(tmp.path(), &["--here", blob.to_str().unwrap()], &here);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("sopen: cancelled"),
        "{}",
        text(&out.stderr)
    );
    assert!(ssh.argv().is_none(), "ssh ran for --here");
    assert_eq!(usage_lines(tmp.path()).last().unwrap()["forwarded"], false);

    // Unset: back to this machine.
    let out = sopen(tmp.path(), &["config", "target", "--unset"], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "");
    let shown = sopen(tmp.path(), &["config"], &path);
    assert!(text(&shown.stdout).ends_with("target is not set\n"));
    let here = [path.as_slice(), &[("SOPEN_CHOICE", "cancel")]].concat();
    let out = sopen(tmp.path(), &[blob.to_str().unwrap()], &here);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(ssh.argv().is_none(), "ssh ran with no target");
}

/// `sopen config fkeys` as the README and spec §4a show it (04 §6.2, K-F4's file half): it
/// writes `fkeys = "<word>"`, `sopen config` shows it once it is set, the target stays beside
/// it, and a word that is neither is refused with the file unchanged.
#[test]
fn config_fkeys_is_set_and_shown() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.toml");
    let shown = sopen(tmp.path(), &["config"], &[]);
    assert_eq!(
        text(&shown.stdout),
        format!("{}\ntarget is not set\n", config.display()),
        "no fkeys line while the file does not set it"
    );
    let set = sopen(tmp.path(), &["config", "fkeys", "system"], &[]);
    assert!(set.status.success(), "{}", text(&set.stderr));
    assert_eq!(
        text(&set.stdout),
        format!("fkeys = system ({})\n", config.display())
    );
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "fkeys = \"system\"\n"
    );
    let shown = sopen(tmp.path(), &["config"], &[]);
    assert_eq!(
        text(&shown.stdout),
        format!("{}\ntarget is not set\nfkeys = system\n", config.display())
    );
    let target = sopen(tmp.path(), &["config", "target", HOST], &[]);
    assert!(target.status.success(), "{}", text(&target.stderr));
    let set = sopen(tmp.path(), &["config", "fkeys", "function"], &[]);
    assert!(set.status.success(), "{}", text(&set.stderr));
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        format!("target = \"{HOST}\"\nfkeys = \"function\"\n")
    );
    let bad = sopen(tmp.path(), &["config", "fkeys", "media"], &[]);
    assert_eq!(bad.status.code(), Some(2), "{}", text(&bad.stderr));
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        format!("target = \"{HOST}\"\nfkeys = \"function\"\n")
    );
}

#[test]
fn an_unreadable_sopen_target_only_matters_without_a_flag() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let blob = tmp.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1]).unwrap();
    let bad = std::ffi::OsStr::from_bytes(b"box\xff");
    let run = |args: &[&str]| {
        sopen_command(tmp.path())
            .args(args)
            .env("PATH", &ssh.path)
            .env("FAKE_SSH_EXIT", "0")
            .env("SOPEN_TARGET", bad)
            // A run here reaches the binary-file dialog: answered without a window.
            .env("SOPEN_CHOICE", "cancel")
            .output()
            .unwrap()
    };
    let out = run(&["--here", blob.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = run(&["--target", HOST, "x:/a.mp3"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(destination(&ssh).as_deref(), Some(HOST));
    let out = run(&[blob.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "sopen: SOPEN_TARGET is not valid UTF-8\n"
    );
}

/// A 4 s sine WAV at `path`, or `false` when there is no ffmpeg (a failure under
/// `SOPEN_REQUIRE_FFMPEG`).
fn sine_wav(path: &Path) -> bool {
    let ffmpeg = std::env::var("SOPEN_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let generated = Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=4"])
        .arg(path)
        .status();
    match generated {
        Ok(s) if s.success() => true,
        _ if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but ffmpeg did not run")
        }
        _ => false,
    }
}

#[test]
fn a_broken_config_is_an_error_where_it_is_read() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.toml");
    std::fs::write(&config, "target = \"box\"\ncolour = \"red\"\n").unwrap();
    let blob = tmp.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1]).unwrap();
    let out = sopen(tmp.path(), &[blob.to_str().unwrap()], &[]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(
        err.starts_with(&format!("sopen: {}: line 2: ", config.display())),
        "{err}"
    );
    assert!(err.contains("colour"), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
    let shown = sopen(tmp.path(), &["config"], &[]);
    assert_eq!(shown.status.code(), Some(1));
    let set = sopen(tmp.path(), &["config", "target", "other"], &[]);
    assert_eq!(
        set.status.code(),
        Some(1),
        "a file sopen cannot read is not replaced"
    );
    assert!(std::fs::read_to_string(&config).unwrap().contains("colour"));
    let bad = sopen(tmp.path(), &["config", "target", "a b"], &[]);
    assert_eq!(bad.status.code(), Some(1));

    // Not consulted, so not in the way: --here, and cut.
    let out = sopen(
        tmp.path(),
        &["--here", blob.to_str().unwrap()],
        &[("SOPEN_CHOICE", "cancel")],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let wav = tmp.path().join("a.wav");
    if !sine_wav(&wav) {
        eprintln!("skipping the cut under a broken config: ffmpeg not available");
        return;
    }
    let out = sopen(
        tmp.path(),
        &["cut", wav.to_str().unwrap(), "1", "0:03"],
        &[],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).trim().ends_with("/a.cut-1-3.wav"));
}

/// The separation model's credit (its authors ask for it) travels with a binary that separates,
/// next to the font's, in `--version` and the long help (spec §7a).
#[test]
fn version_and_help_credit_the_separation_model() {
    let tmp = tempfile::tempdir().unwrap();
    for args in [&["--version"][..], &["--help"][..]] {
        let said = text(&sopen(tmp.path(), args, &[]).stdout);
        let credited = said.lines().any(|l| l.trim() == sopen::stems::CREDIT);
        assert_eq!(credited, cfg!(feature = "stems"), "{args:?}: {said}");
    }
}

/// `path` decoded to f32 samples (its own channels and rate), by ffmpeg.
fn samples(path: &Path) -> Vec<f32> {
    let ffmpeg = std::env::var("SOPEN_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let out = Command::new(&ffmpeg)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-c:a", "pcm_f32le", "-"])
        .output()
        .unwrap();
    assert!(out.status.success(), "decoding {}", path.display());
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

/// How voice + music compare with what they must add up to (the model's music is its window
/// minus the voice): the source, or for more than two channels its stereo downmix.
#[derive(Debug)]
struct StemsCheck {
    /// The level of voice + music against the reference's, dB.
    level_db: f64,
    /// Where voice + music lines up best with the reference, in frames (searched ±32).
    lag: isize,
    /// Reference over (reference − voice − music), dB, at lag 0.
    reconstruction_db: f64,
}

/// [`StemsCheck`] of `voice` and `music` against `reference` (all interleaved, `channels` per
/// frame). Each stem must be at least as long as the reference and at most `tail` frames longer:
/// 0 for PCM, one codec frame for a lossy codec, whose encoder pads the end.
fn check_stems(
    reference: &[f32],
    voice: &[f32],
    music: &[f32],
    channels: usize,
    tail: usize,
) -> StemsCheck {
    for (name, stem) in [("voice", voice), ("music", music)] {
        let (got, want) = (stem.len() / channels, reference.len() / channels);
        assert!(
            (want..=want + tail).contains(&got),
            "the {name} is {got} frames, the reference {want} (allowed: up to {tail} more)"
        );
    }
    let n = reference.len().min(voice.len()).min(music.len());
    let both: Vec<f64> = (0..n).map(|i| f64::from(voice[i] + music[i])).collect();
    let r: Vec<f64> = reference[..n].iter().map(|&x| f64::from(x)).collect();
    let energy = |x: &[f64]| x.iter().map(|v| v * v).sum::<f64>();
    let window = n.min(96_000 * channels);
    let lag = (-32isize..=32)
        .max_by(|&a, &b| {
            let corr = |l: isize| -> f64 {
                let shift = l * channels as isize;
                (0..window)
                    .filter_map(|i| {
                        let j = i as isize + shift;
                        (0..n as isize)
                            .contains(&j)
                            .then(|| r[i] * both[j as usize])
                    })
                    .sum()
            };
            corr(a).total_cmp(&corr(b))
        })
        .unwrap();
    let error: Vec<f64> = r.iter().zip(&both).map(|(a, b)| a - b).collect();
    StemsCheck {
        level_db: 10.0 * (energy(&both) / energy(&r)).log10(),
        lag,
        reconstruction_db: 10.0 * (energy(&r) / energy(&error).max(1e-30)).log10(),
    }
}

/// The length check: a stem may be longer than the reference by up to `tail` frames (a lossy
/// encoder pads its last frame), never shorter — a short one would be compared on its prefix
/// alone.
#[test]
fn check_stems_takes_a_padded_stem_but_not_a_short_one() {
    let reference = vec![0.5f32; 2 * 4096];
    let padded = vec![0.25f32; 2 * (4096 + 1024)];
    let check = check_stems(&reference, &padded, &padded, 2, 1024);
    assert_eq!(check.lag, 0, "{check:?}");
    for (stem, why) in [
        (vec![0.25f32; 2 * (4096 - 1)], "one frame short"),
        (
            vec![0.25f32; 2 * (4096 + 1025)],
            "a frame and one sample over",
        ),
    ] {
        let refused =
            std::panic::catch_unwind(|| check_stems(&reference, &stem, &padded, 2, 1024)).is_err();
        assert!(refused, "{why} was taken");
    }
}

/// [`check_stems`] against the source file itself.
fn check_stems_of(
    src: &Path,
    voice: &Path,
    music: &Path,
    channels: usize,
    tail: usize,
) -> StemsCheck {
    check_stems(
        &samples(src),
        &samples(voice),
        &samples(music),
        channels,
        tail,
    )
}

/// One separation at a time in this test binary: each holds 2–4 GB, and CI's macOS runner has 7.
static SEPARATING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The model `SOPEN_STEMS_MODEL` names (CI downloads it) and the turn to use it, or `None` with
/// the reason printed; `SOPEN_REQUIRE_STEMS=1` (CI) makes a missing one a failure.
fn stems_model() -> Option<(String, std::sync::MutexGuard<'static, ()>)> {
    match std::env::var("SOPEN_STEMS_MODEL") {
        Ok(path) if Path::new(&path).is_file() => {
            Some((path, SEPARATING.lock().unwrap_or_else(|e| e.into_inner())))
        }
        _ if std::env::var_os("SOPEN_REQUIRE_STEMS").is_some() => {
            panic!("SOPEN_REQUIRE_STEMS is set but SOPEN_STEMS_MODEL names no file")
        }
        _ => {
            eprintln!("skipping: SOPEN_STEMS_MODEL names no model");
            None
        }
    }
}

/// `sopen stems` is `sopen cut`'s shape: SOURCE is required, it never forwards, and a model
/// that is not there is one line naming where it was looked for — before anything is read.
#[test]
fn stems_argument_errors_are_one_line_each() {
    let tmp = tempfile::tempdir().unwrap();
    for args in [
        &["stems"][..],
        &["stems", "a.mp3", "b.mp3"],
        &["--target", "box", "stems", "a.mp3"],
        &["--here", "stems", "a.mp3"],
        &["stems", "a.mp3", "--no-such-flag"],
    ] {
        let out = sopen(tmp.path(), args, &[]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&out.stderr)
        );
    }
    let missing = tmp.path().join("no-model.onnx");
    let wav = tmp.path().join("a.wav");
    std::fs::write(&wav, b"RIFF").unwrap();
    let out = sopen(
        tmp.path(),
        &["stems", wav.to_str().unwrap()],
        &[("SOPEN_STEMS_MODEL", missing.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    if cfg!(feature = "stems") {
        if which("ffmpeg").is_none() && std::env::var_os("SOPEN_FFMPEG").is_none() {
            return eprintln!("skipping the model error: no ffmpeg");
        }
        assert!(err.contains(&missing.display().to_string()), "{err}");
        assert!(err.contains("SOPEN_STEMS_MODEL"), "{err}");
    } else {
        assert!(
            err.contains("built without voice/music separation"),
            "{err}"
        );
    }
    let logged = usage_lines(tmp.path());
    let last = logged.last().unwrap();
    assert_eq!(last["action"], "stems");
    assert_eq!(last["ok"], false);
}

/// The window's child removes its work folder if its parent goes, so the folder must be the
/// empty one the parent made: anything else is refused before stdin is even read.
#[test]
fn the_child_flags_need_an_empty_work_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let full = tmp.path().join("full");
    std::fs::create_dir(&full).unwrap();
    std::fs::write(full.join("keep.txt"), "mine").unwrap();
    let file = tmp.path().join("a-file");
    std::fs::write(&file, "mine").unwrap();
    for work in [&full, &file, &tmp.path().join("missing")] {
        let out = sopen(
            tmp.path(),
            &[
                "stems",
                "--work-dir",
                work.to_str().unwrap(),
                "--wait-to-publish",
                "--",
                "a.mp3",
            ],
            &[],
        );
        assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
        if cfg!(feature = "stems") {
            assert!(
                text(&out.stderr).contains("is not an empty folder"),
                "{}",
                text(&out.stderr)
            );
        }
    }
    assert_eq!(
        std::fs::read_to_string(full.join("keep.txt")).unwrap(),
        "mine"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
}

/// With the model: what it refuses (a folder, a text file), then a real separation of a short
/// local WAV — two new files next to it, their paths printed voice first, progress lines on
/// stderr — and a second run that never overwrites them.
#[test]
fn stems_on_a_local_file_writes_two_siblings_and_never_overwrites() {
    if !cfg!(feature = "stems") {
        return;
    }
    let Some((model, _turn)) = stems_model() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let env = [("SOPEN_STEMS_MODEL", model.as_str())];
    let notes = tmp.path().join("notes.txt");
    std::fs::write(&notes, "hello").unwrap();
    let err = text(&sopen(tmp.path(), &["stems", notes.to_str().unwrap()], &env).stderr);
    assert!(
        err.contains("stems works on audio and video files"),
        "{err}"
    );
    let folder = tmp.path().join("music.mp3");
    std::fs::create_dir(&folder).unwrap();
    let err = text(&sopen(tmp.path(), &["stems", folder.to_str().unwrap()], &env).stderr);
    assert!(err.contains("is a directory"), "{err}");

    let wav = tmp.path().join("a.wav");
    let made = stems_fixture(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=4",
            wav.to_str().unwrap(),
        ],
        "a 4 s mono tone",
    );
    if !made {
        return;
    }
    let out = sopen(tmp.path(), &["stems", wav.to_str().unwrap()], &env);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let printed = text(&out.stdout);
    let lines: Vec<&str> = printed.lines().collect();
    assert_eq!(lines.len(), 2, "{printed}");
    assert!(lines[0].ends_with("/a.voice.wav"), "{printed}");
    assert!(lines[1].ends_with("/a.music.wav"), "{printed}");
    assert!(lines.iter().all(|l| Path::new(l).is_file()), "{printed}");
    // A mono 16-bit source: the stems are mono 16-bit, as long as it, at its level.
    let check = check_stems_of(&wav, Path::new(lines[0]), Path::new(lines[1]), 1, 0);
    assert!(check.level_db.abs() < 0.1, "{check:?}");
    assert_eq!(check.lag, 0, "{check:?}");
    let progress: Vec<_> = text(&out.stderr)
        .lines()
        .filter_map(sopen::stems::PieceProgress::parse)
        .collect();
    assert_eq!(
        progress
            .iter()
            .map(|p| (p.done, p.total))
            .collect::<Vec<_>>(),
        [(0, 1), (1, 1)],
        "{}",
        text(&out.stderr)
    );
    // The first stems as they are: bytes, inode, modification time.
    let snapshot = |name: &str| {
        use std::os::unix::fs::MetadataExt;
        let path = tmp.path().join(name);
        let meta = std::fs::symlink_metadata(&path).unwrap();
        (
            std::fs::read(&path).unwrap(),
            meta.ino(),
            meta.mtime(),
            meta.mtime_nsec(),
        )
    };
    let first = [snapshot("a.voice.wav"), snapshot("a.music.wav")];
    // Names taken unevenly: the pair gets the first number free for both, never a mismatched
    // `a.voice-1` + `a.music-3`.
    for taken in ["a.music-1.wav", "a.music-2.wav"] {
        std::fs::write(tmp.path().join(taken), "someone else's").unwrap();
    }
    let again = sopen(tmp.path(), &["stems", wav.to_str().unwrap()], &env);
    assert!(again.status.success(), "{}", text(&again.stderr));
    let printed = text(&again.stdout);
    assert!(printed.contains("/a.voice-3.wav\n"), "{printed}");
    assert!(printed.contains("/a.music-3.wav\n"), "{printed}");
    assert!(tmp.path().join("a.voice-3.wav").is_file());
    assert!(tmp.path().join("a.music-3.wav").is_file());
    assert!(!tmp.path().join("a.voice-1.wav").exists());
    for taken in ["a.music-1.wav", "a.music-2.wav"] {
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(taken)).unwrap(),
            "someone else's"
        );
    }
    assert!(
        first == [snapshot("a.voice.wav"), snapshot("a.music.wav")],
        "the first stems were touched"
    );
    let logged = usage_lines(tmp.path());
    let last = logged.last().unwrap();
    assert_eq!(
        (last["action"].as_str(), last["ok"].as_bool()),
        (Some("stems"), Some(true))
    );
    assert_eq!(last["kind"], "audio");
}

/// Makes a separation fixture with `ffmpeg <args>`: `true` when made. When it cannot be made it
/// is a skip (`false`, the reason printed) — unless the tests are required
/// (`SOPEN_REQUIRE_FFMPEG` or `SOPEN_REQUIRE_STEMS`, as in CI), where it is a failure that shows
/// what ffmpeg said.
fn stems_fixture(args: &[&str], what: &str) -> bool {
    let ffmpeg = std::env::var("SOPEN_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let required = ["SOPEN_REQUIRE_FFMPEG", "SOPEN_REQUIRE_STEMS"]
        .iter()
        .any(|k| std::env::var_os(k).is_some());
    fixture_with(&ffmpeg, args, what, required)
}

fn fixture_with(ffmpeg: &str, args: &[&str], what: &str, required: bool) -> bool {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args);
    fixture_from(&mut || cmd.output(), ffmpeg, what, required)
}

/// [`fixture_with`]'s judgement on one way of running ffmpeg: a start refused as busy
/// (ETXTBSY, a script just written while another test thread forks) is retried; what ffmpeg
/// printed is kept for the failure.
fn fixture_from(
    run: &mut dyn FnMut() -> std::io::Result<Output>,
    ffmpeg: &str,
    what: &str,
    required: bool,
) -> bool {
    let out = retry_busy(run);
    let why = match out {
        Ok(out) if out.status.success() => return true,
        Ok(out) => format!(
            "{ffmpeg} exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => format!("cannot run {ffmpeg}: {e}"),
    };
    if required {
        panic!("cannot make {what} (the separation tests are required): {why}");
    }
    eprintln!("skipping: cannot make {what}: {why}");
    false
}

/// `path`: `seconds` of a 220 Hz tone, stereo 44.1 kHz WAV ([`stems_fixture`]).
fn tone_wav(path: &Path, seconds: u32) -> bool {
    let tone = format!("sine=frequency=220:duration={seconds}");
    let path = path.to_str().unwrap();
    stems_fixture(
        &["-f", "lavfi", "-i", &tone, "-ac", "2", path],
        &format!("a {seconds} s tone"),
    )
}

/// A fixture ffmpeg cannot make fails the required tests with what ffmpeg said, and is a skip
/// otherwise.
#[test]
fn a_fixture_ffmpeg_cannot_make_fails_required_runs_with_its_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("ffmpeg");
    executable(&fake, "#!/bin/sh\necho 'boom: no lavfi here' >&2\nexit 1\n");
    let fake = fake.to_str().unwrap();
    // Not required: a skip.
    assert!(!fixture_with(fake, &["x"], "a tone", false));
    let required = std::panic::catch_unwind(|| fixture_with(fake, &["x"], "a tone", true));
    let message = required
        .unwrap_err()
        .downcast::<String>()
        .map(|m| *m)
        .unwrap_or_default();
    assert!(message.contains("cannot make a tone"), "{message}");
    assert!(message.contains("boom: no lavfi here"), "{message}");
}

/// A start refused as busy (ETXTBSY — on Linux, a script just written while another test
/// thread forks) is retried inside the helper, required or not, and the failure then reported
/// is ffmpeg's own, with its stderr.
#[test]
fn a_busy_fixture_ffmpeg_is_retried_and_its_own_failure_reported() {
    use std::os::unix::process::ExitStatusExt;
    // Two starts refused as busy, then ffmpeg runs and fails with "boom".
    let calls = std::cell::Cell::new(0);
    let attempt = || {
        calls.set(calls.get() + 1);
        if calls.get() <= 2 {
            Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy))
        } else {
            Ok(Output {
                status: std::process::ExitStatus::from_raw(1 << 8),
                stdout: Vec::new(),
                stderr: b"boom: no lavfi here\n".to_vec(),
            })
        }
    };
    assert!(!fixture_from(&mut { attempt }, "ffmpeg", "a tone", false));
    assert_eq!(calls.get(), 3, "two busy starts, then ffmpeg's own failure");
    calls.set(0);
    let required = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fixture_from(&mut { attempt }, "ffmpeg", "a tone", true)
    }));
    assert_eq!(calls.get(), 3, "required: retried the same way");
    let message = required
        .unwrap_err()
        .downcast::<String>()
        .map(|m| *m)
        .unwrap_or_default();
    assert!(message.contains("boom: no lavfi here"), "{message}");
    assert!(!message.contains("busy"), "{message}");
}

/// The child's stderr, line by line, from a thread that reads it to its end (so the child never
/// blocks on a full pipe).
fn stderr_lines(stderr: impl std::io::Read + Send + 'static) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            let _ = tx.send(line);
        }
    });
    rx
}

/// Every line still to come from `lines`, up to the end of the stream (the reader thread may
/// still be passing on the last ones when the child has exited); at most 30 s.
fn rest_of(lines: &std::sync::mpsc::Receiver<String>) -> Vec<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut rest = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match lines.recv_timeout(left) {
            Ok(line) => rest.push(line),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return rest,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("the child's stderr stayed open; so far: {rest:?}")
            }
        }
    }
}

/// Kills `child`'s process group (it leads one) and reaps it.
fn kill_group(child: &mut std::process::Child) {
    // SAFETY: a plain syscall on our own child's group; it is not reaped yet.
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    let _ = child.wait();
}

/// The first line of `lines` that is `want` — within 120 s, else the child's group is killed
/// and the test fails with what it said.
fn wait_for_line(
    lines: &std::sync::mpsc::Receiver<String>,
    child: &mut std::process::Child,
    want: &dyn Fn(&str) -> bool,
    what: &str,
) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut said = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match lines.recv_timeout(left) {
            Ok(line) if want(&line) => return line,
            Ok(line) => said.push(line),
            Err(_) => {
                kill_group(child);
                panic!("no {what} line; the child said: {said:?}");
            }
        }
    }
}

fn first_progress(
    lines: &std::sync::mpsc::Receiver<String>,
    child: &mut std::process::Child,
) -> sopen::stems::PieceProgress {
    let line = wait_for_line(
        lines,
        child,
        &|l| sopen::stems::PieceProgress::parse(l).is_some(),
        "progress",
    );
    sopen::stems::PieceProgress::parse(&line).unwrap()
}

/// No process is left in the group `pgid` (within a few seconds: a killed grandchild is reaped by
/// init).
fn group_gone(pgid: u32) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        // SAFETY: signal 0 only asks whether the group has a member.
        let none = unsafe { libc::kill(-(pgid as i32), 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if none || std::time::Instant::now() > deadline {
            return none;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The window's way (spec §7a): the real `sopen stems` as a child — progress read back, the
/// ready line answered with the go-ahead, both stems published — and a second child cancelled
/// mid-separation: at once, its group (its ffmpeg children) gone, its work folder gone, nothing
/// published.
#[test]
fn the_real_child_publishes_on_the_go_ahead_and_cancels_cleanly() {
    if !cfg!(feature = "stems") {
        return;
    }
    let Some((model, _turn)) = stems_model() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let short = tmp.path().join("short.wav");
    if !tone_wav(&short, 4) {
        return;
    }
    let work_root = tmp.path().join("cache/.exports");
    let child = sopen::stems::spawn_child(
        sopen_command(tmp.path()),
        short.to_str().unwrap(),
        Some(Path::new(&model)),
        &work_root,
    )
    .unwrap();
    let work = child.work_dir().to_path_buf();
    let published = child.wait().unwrap();
    assert!(
        published.voice.ends_with("/short.voice.wav"),
        "{published:?}"
    );
    assert!(
        published.music.ends_with("/short.music.wav"),
        "{published:?}"
    );
    assert!(Path::new(&published.voice).is_file() && Path::new(&published.music).is_file());
    assert!(!work.exists());

    let long = tmp.path().join("long.wav");
    assert!(tone_wav(&long, 65));
    let mut child = sopen::stems::spawn_child(
        sopen_command(tmp.path()),
        long.to_str().unwrap(),
        Some(Path::new(&model)),
        &work_root,
    )
    .unwrap();
    let work = child.work_dir().to_path_buf();
    let pgid = child.id();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while child.progress().is_none() {
        assert!(child.poll().is_none(), "ended before separating");
        assert!(std::time::Instant::now() < deadline, "no progress line");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(child.progress().map(|p| (p.done, p.total)), Some((0, 3)));
    let asked = std::time::Instant::now();
    assert_eq!(child.cancel(), sopen::stems::Cancel::Cancelled);
    assert!(asked.elapsed() < std::time::Duration::from_secs(10));
    assert!(group_gone(pgid), "a process of the child's group survived");
    assert!(!work.exists());
    assert!(!tmp.path().join("long.voice.wav").exists());
    assert!(!tmp.path().join("long.music.wav").exists());
    assert!(child.wait().unwrap_err().to_string().contains("cancelled"));
}

/// The window's child whose parent goes away (its stdin ends) mid-separation stops at once: its
/// group killed, the parent's work folder removed, nothing published.
#[test]
fn the_real_child_stops_when_its_parent_goes() {
    if !cfg!(feature = "stems") {
        return;
    }
    let Some((model, _turn)) = stems_model() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let long = tmp.path().join("long.wav");
    if !tone_wav(&long, 65) {
        return;
    }
    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    use std::os::unix::process::CommandExt;
    let mut child = sopen_command(tmp.path())
        .args([
            "stems",
            "--work-dir",
            work.to_str().unwrap(),
            "--wait-to-publish",
        ])
        .args(["--model", &model, "--", long.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    // Separating has begun: the first progress line (ONNX Runtime may say things before it).
    let lines = stderr_lines(child.stderr.take().unwrap());
    let first = first_progress(&lines, &mut child);
    assert_eq!((first.done, first.total), (0, 3));
    drop(child.stdin.take());
    let asked = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            asked.elapsed() < std::time::Duration::from_secs(10),
            "the child outlived its parent"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert!(!status.success());
    assert!(
        group_gone(child.id()),
        "a process of the child's group survived"
    );
    assert!(!work.exists(), "the parent's work folder is removed");
    assert!(!tmp.path().join("long.voice.wav").exists());
}

/// A video's stems are audio only, AAC in `.m4a`, at the source's rate and channel count (here
/// mono 48 kHz; the engine runs at 44.1 kHz stereo).
#[test]
fn stems_of_a_mono_48k_video_are_m4a_aac_mono_48k() {
    if !cfg!(feature = "stems") {
        return;
    }
    let Some((model, _turn)) = stems_model() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let clip = tmp.path().join("clip.mp4");
    let made = stems_fixture(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=25:duration=4",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:duration=4:sample_rate=48000",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-ac",
            "1",
            "-shortest",
            clip.to_str().unwrap(),
        ],
        "an H.264 + AAC (mono 48 kHz) clip",
    );
    if !made {
        return;
    }
    let out = sopen(
        tmp.path(),
        &["stems", clip.to_str().unwrap()],
        &[("SOPEN_STEMS_MODEL", model.as_str())],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let printed = text(&out.stdout);
    assert!(printed.ends_with("/clip.voice.m4a\n") || printed.contains("/clip.voice.m4a\n"));
    let ffprobe = std::env::var("SOPEN_FFPROBE").unwrap_or_else(|_| "ffprobe".into());
    for name in ["clip.voice.m4a", "clip.music.m4a"] {
        let probe = Command::new(&ffprobe)
            .args(["-v", "error", "-show_entries"])
            .args([
                "stream=codec_type,codec_name,channels,sample_rate",
                "-of",
                "json",
            ])
            .arg(tmp.path().join(name))
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        let streams = v["streams"].as_array().unwrap();
        assert_eq!(streams.len(), 1, "{name}: {v}");
        let s = &streams[0];
        assert_eq!(s["codec_type"], "audio", "{name}");
        assert_eq!(s["codec_name"], "aac", "{name}");
        assert_eq!(s["channels"], 1, "{name}");
        assert_eq!(s["sample_rate"], "48000", "{name}");
    }
    // AAC both ways: as long as the source to within one AAC frame (1024 samples; the encoder
    // pads its end), lined up with it (lag 0), at its level.
    let check = check_stems_of(
        &clip,
        &tmp.path().join("clip.voice.m4a"),
        &tmp.path().join("clip.music.m4a"),
        1,
        1024,
    );
    assert_eq!(check.lag, 0, "{check:?}");
    assert!(check.level_db.abs() < 0.3, "{check:?}");
}

/// A 48 kHz source goes through the engine at 44.1 kHz and back: the stems still have exactly
/// the source's sample count at its own rate (a 192,512-frame source came back 192,513 frames
/// long — and in AAC, a whole frame longer), lined up with it, at its level.
#[test]
fn stems_of_a_48k_source_have_its_exact_length() {
    if !cfg!(feature = "stems") {
        return;
    }
    let Some((model, _turn)) = stems_model() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let wav = tmp.path().join("odd.wav");
    let made = stems_fixture(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:sample_rate=48000",
            "-f",
            "lavfi",
            "-i",
            "anoisesrc=c=pink:a=0.05:r=48000",
            "-filter_complex",
            "[0][1]amerge=inputs=2,atrim=end_sample=192512",
            "-c:a",
            "pcm_s16le",
            wav.to_str().unwrap(),
        ],
        "a 192,512-frame stereo 48 kHz WAV",
    );
    if !made {
        return;
    }
    let out = sopen(
        tmp.path(),
        &["stems", wav.to_str().unwrap()],
        &[("SOPEN_STEMS_MODEL", model.as_str())],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(samples(&wav).len(), 2 * 192_512, "the fixture");
    let check = check_stems_of(
        &wav,
        &tmp.path().join("odd.voice.wav"),
        &tmp.path().join("odd.music.wav"),
        2,
        0,
    );
    eprintln!("48 kHz stems: {check:?}");
    assert_eq!(check.lag, 0, "{check:?}");
    assert!(check.level_db.abs() < 0.1, "{check:?}");
    // The engine runs at 44.1 kHz: what the noise has above 22.05 kHz does not come back
    // (measured 38.5 dB), so this only guards against gross error.
    assert!(check.reconstruction_db > 30.0, "{check:?}");
}

/// A loud 5.1 source (every channel peaking at 0.89) is separated as stereo, downmixed the way
/// ffmpeg does it for integer output — normalised, so nothing clips: the 16-bit stems have no
/// full-scale sample and add up to that downmix.
#[test]
fn loud_surround_is_downmixed_without_clipping() {
    if !cfg!(feature = "stems") {
        return;
    }
    let Some((model, _turn)) = stems_model() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let six = tmp.path().join("six.wav");
    let made = stems_fixture(
        &[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.89*sin(2*PI*220*t)|0.89*sin(2*PI*330*t)|0.89*sin(2*PI*440*t)|\
             0.89*sin(2*PI*55*t)|0.89*sin(2*PI*550*t)|0.89*sin(2*PI*660*t):\
             channel_layout=5.1:sample_rate=44100:duration=4",
            "-c:a",
            "pcm_s16le",
            six.to_str().unwrap(),
        ],
        "a loud 5.1 WAV",
    );
    if !made {
        return;
    }
    let out = sopen(
        tmp.path(),
        &["stems", six.to_str().unwrap()],
        &[("SOPEN_STEMS_MODEL", model.as_str())],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("6-channel audio is separated as stereo"),
        "{}",
        text(&out.stderr)
    );
    let downmix = tmp.path().join("downmix.wav");
    assert!(stems_fixture(
        &[
            "-i",
            six.to_str().unwrap(),
            "-ac",
            "2",
            "-rematrix_maxval",
            "1.0",
            "-c:a",
            "pcm_f32le",
            downmix.to_str().unwrap(),
        ],
        "the reference downmix",
    ));
    let (voice, music) = (
        samples(&tmp.path().join("six.voice.wav")),
        samples(&tmp.path().join("six.music.wav")),
    );
    for (name, stem) in [("voice", &voice), ("music", &music)] {
        let full = stem.iter().filter(|x| x.abs() >= 32767.0 / 32768.0).count();
        assert_eq!(full, 0, "{name}: {full} full-scale samples");
    }
    let check = check_stems(&samples(&downmix), &voice, &music, 2, 0);
    eprintln!("5.1 stems: {check:?}");
    assert_eq!(check.lag, 0, "{check:?}");
    assert!(check.level_db.abs() < 0.1, "{check:?}");
    assert!(check.reconstruction_db > 40.0, "{check:?}");
}

/// The publish barrier on the real child (spec §7a): with its stdin held open it encodes, says
/// it is ready, and publishes nothing — however long it waits — until `publish` comes; any other
/// answer, or the end of its stdin, publishes nothing at all.
#[test]
fn the_real_child_publishes_nothing_before_the_go_ahead() {
    if !cfg!(feature = "stems") {
        return;
    }
    let Some((model, _turn)) = stems_model() else {
        return;
    };
    use std::io::Write;
    use std::os::unix::process::CommandExt;
    for answer in [Some("publish"), Some("no"), None] {
        let tmp = tempfile::tempdir().unwrap();
        let short = tmp.path().join("short.wav");
        if !tone_wav(&short, 4) {
            return;
        }
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let mut child = sopen_command(tmp.path())
            .args([
                "stems",
                "--work-dir",
                work.to_str().unwrap(),
                "--wait-to-publish",
            ])
            .args(["--model", &model, "--", short.to_str().unwrap()])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let lines = stderr_lines(child.stderr.take().unwrap());
        wait_for_line(
            &lines,
            &mut child,
            &|l| l == sopen::stems::READY_LINE,
            "ready",
        );
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let voice = tmp.path().join("short.voice.wav");
        let music = tmp.path().join("short.music.wav");
        assert!(
            !voice.exists() && !music.exists(),
            "published before the go-ahead"
        );
        let mut stdin = child.stdin.take().unwrap();
        match answer {
            Some(word) => writeln!(stdin, "{word}").unwrap(),
            None => drop(stdin),
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() > deadline {
                kill_group(&mut child);
                panic!("{answer:?}: the child did not end");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let said = rest_of(&lines);
        let mut printed = String::new();
        std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut printed).unwrap();
        if answer == Some("publish") {
            assert!(status.success(), "{said:?}");
            assert!(voice.is_file() && music.is_file());
            assert_eq!(printed.lines().count(), 2, "{printed}");
        } else {
            assert!(!status.success(), "{answer:?}");
            assert!(!voice.exists() && !music.exists(), "{answer:?}: published");
            assert!(printed.is_empty(), "{answer:?}: {printed}");
            if answer.is_some() {
                assert!(
                    said.iter().any(|l| l.contains("nothing was published")),
                    "{said:?}"
                );
            }
        }
        assert!(!work.exists(), "{answer:?}: the work folder is left");
    }
}

/// This machine's git for a test, or `None` with a note (a failure under `SOPEN_REQUIRE_GIT=1`).
fn test_git() -> Option<()> {
    let found = Command::new("git")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success());
    if found {
        return Some(());
    }
    assert!(
        std::env::var("SOPEN_REQUIRE_GIT").map_or(true, |v| v != "1"),
        "git is required (SOPEN_REQUIRE_GIT=1) and there is none"
    );
    eprintln!("skipping: no git on this machine");
    None
}

/// 03 G-G1's fixture: two branches and a merge, an octopus merge, an annotated and a
/// lightweight tag, a remote with `main` at the local `main` and `feature` behind it, a stash,
/// two uncommitted changes. Fixed identity and dates, no config of the machine's.
const G_G1: &str = r#"set -eu
root=$1
mkdir -p "$root/home"
export HOME="$root/home" GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null LC_ALL=C TZ=UTC
export GIT_AUTHOR_NAME=Ann GIT_AUTHOR_EMAIL=ann@example.com GIT_COMMITTER_NAME=Ann GIT_COMMITTER_EMAIL=ann@example.com
t=1767225600
c() { t=$((t+3600)); GIT_AUTHOR_DATE="$t +0000" GIT_COMMITTER_DATE="$t +0000" git "$@"; }
git init -q -b main "$root/repo"; cd "$root/repo"
echo one > a.txt; mkdir sub; echo s > sub/s.txt; git add -A; c commit -q -m "first commit"
git branch feature; git branch x; git branch y
echo two > b.txt; git add -A; c commit -q -m "main: second"; git tag light
git checkout -q feature; echo f > f.txt; git add -A; c commit -q -m "feature: work"
git checkout -q x; echo x > x.txt; git add -A; c commit -q -m "x: side work"
git checkout -q y; echo y > y.txt; git add -A; c commit -q -m "y: other side work"
git checkout -q main; c merge -q --no-ff -m "Merge branch 'feature'" feature
c merge -q --no-ff -m "Merge branches 'x' and 'y'" x y >/dev/null
c tag -a -m "version one" v1.0
echo three >> a.txt; git add -A; c commit -q -m "main: third"
git init -q --bare "$root/origin.git"
git remote add origin "$root/origin.git"
git push -q origin main feature
git checkout -q feature; echo more >> f.txt; git add -A; c commit -q -m "feature: ahead of origin"
git checkout -q main
git branch --set-upstream-to=origin/main main >/dev/null
echo stashed > u.txt; echo stash >> a.txt; c stash push -q -u -m "work in progress"
echo changed >> a.txt; echo new > n.txt
"#;

/// Builds G-G1's fixture under `root`: `root/repo` and its remote `root/origin.git`.
fn g_g1_fixture(root: &Path) {
    let out = Command::new("sh")
        .arg("-c")
        .arg(G_G1)
        .arg("sh")
        .arg(root)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
}

/// 03 G-G1 and G-D1 (`SOPEN_LIST=git`, 02 §9): in a work tree's subfolder the first page prints
/// — the frame title line is the top level, the uncommitted row first, `►main↔origin`,
/// `origin/feature`, both tags, `stash@{0}`, the octopus merge, the lanes in ASCII — then the
/// paging line; the golden text, dates in UTC.
#[test]
fn sopen_list_git_prints_the_first_page() {
    if test_git().is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    g_g1_fixture(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    let out = sopen(
        tmp.path(),
        &["--here", repo.join("sub").to_str().unwrap()],
        &[
            ("SOPEN_LIST", "git"),
            ("TZ", "UTC"),
            ("HOME", home.to_str().unwrap()),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let listed = text(&out.stdout);
    let lines: Vec<&str> = listed.lines().collect();
    assert_eq!(lines[0], repo.to_str().unwrap(), "{listed}");
    let golden = [
        "o         Uncommitted changes (2)  *    *",
        "| *       [feature] feature: ahead of origin  Ann  2026-01-01 10:00  a299fc4f",
        "| | =     [stash@{0}] On main: work in progress    2026-01-01 11:00  4a70fd50",
        "@/|       [►main↔origin] main: third  Ann  2026-01-01 09:00  b455002a",
        "*\\|       [v1.0] Merge branches 'x' and 'y'  Ann  2026-01-01 07:00  976b4028",
        "*\\| | |   Merge branch 'feature'  Ann  2026-01-01 06:00  cd8fa821",
        "| | | *   [y] y: other side work  Ann  2026-01-01 05:00  cf3dd192",
        "| | * |   [x] x: side work  Ann  2026-01-01 04:00  5d70bb93",
        "| * | |   [origin/feature] feature: work  Ann  2026-01-01 03:00  0b03602c",
        "* | | |   [light] main: second  Ann  2026-01-01 02:00  7ab4bb8e",
        "*/        first commit  Ann  2026-01-01 01:00  9ca3c71f",
        "9 commits · all loaded",
    ];
    assert_eq!(lines[1..], golden[..], "{listed}");
}

/// `SOPEN_STATE dialog=<id>` (02 §9, S3): an id of 03 §7 is taken — §7.8's history edits too
/// (S4: `drop`, `move-up`, `plan`) — and the run goes on, here to `SOPEN_LIST=git`'s first page
/// (whose load runs the in-progress probe too); any other word ends the run with the reason,
/// before anything is listed.
#[test]
fn sopen_state_dialog_takes_an_action_id() {
    if test_git().is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    g_g1_fixture(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    let run = |state: &str| {
        sopen(
            tmp.path(),
            &["--here", repo.to_str().unwrap()],
            &[
                ("SOPEN_LIST", "git"),
                ("SOPEN_STATE", state),
                ("TZ", "UTC"),
                ("HOME", home.to_str().unwrap()),
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ],
        )
    };
    for state in [
        "git=pin;select=v1.0;dialog=reset",
        "git=pin;select=v1.0;dialog=drop",
        "git=pin;dialog=move-up",
        "git=pin;dialog=plan",
    ] {
        let out = run(state);
        assert!(out.status.success(), "{state}: {}", text(&out.stderr));
        assert!(
            text(&out.stdout).contains("9 commits · all loaded"),
            "{state}"
        );
    }
    let out = run("git=pin;dialog=nonsense");
    let err = text(&out.stderr);
    assert!(
        !out.status.success() && err.contains("'nonsense' is not an action id"),
        "{err}"
    );
    assert!(text(&out.stdout).is_empty(), "nothing listed");
}

/// 03 G-D1's other half: a bare repository, the inside of `.git`, a plain folder, and
/// `SOPEN_GIT=/nonexistent` → the directory listing (as `SOPEN_LIST=1`) and one line saying why.
#[test]
fn sopen_list_git_outside_a_work_tree_lists_and_says_why() {
    if test_git().is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    g_g1_fixture(&root);
    let plain = root.join("plain");
    std::fs::create_dir(&plain).unwrap();
    std::fs::write(plain.join("a.txt"), "a").unwrap();
    let home = root.join("home");
    let list = |dir: &Path, git: Option<&str>| {
        let mut env = vec![
            ("SOPEN_LIST", "git"),
            ("HOME", home.to_str().unwrap()),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ];
        if let Some(git) = git {
            env.push(("SOPEN_GIT", git));
        }
        let out = sopen(tmp.path(), &["--here", dir.to_str().unwrap()], &env);
        assert!(out.status.success(), "{}", text(&out.stderr));
        text(&out.stdout)
    };
    for (dir, git, why) in [
        (root.join("origin.git"), None, "bare"),
        (root.join("repo/.git"), None, "not a work tree"),
        (plain.clone(), None, "not a repository"),
        (root.join("repo"), Some("/nonexistent"), "no git"),
    ] {
        let listed = list(&dir, git);
        let lines: Vec<&str> = listed.lines().collect();
        assert_eq!(lines[0], dir.to_str().unwrap(), "{listed}");
        assert!(
            lines[1].starts_with("..") && lines[1].ends_with("<UP>"),
            "{listed}"
        );
        let last = lines.last().unwrap();
        assert!(
            last.starts_with("git panel: ") && last.contains(why),
            "{why}: {listed}"
        );
        assert!(lines[lines.len() - 2].contains(" Listed = "), "{listed}");
    }
}

// ---------------------------------------------------------------------------------------------
// `sopen URL` (spec §4a). The browser is always a stand-in (`SOPEN_BROWSER`): a script that
// appends its arguments, one call a block ended by `---`, so no test opens a real browser. Every
// run that opens an address passes `--browser` (09 §7): the system browser's route, on every
// route, whatever the default becomes.
// `SOPEN_EDITOR_WAIT=60`: sopen waits for the stand-in's exit (a loaded machine can take longer
// than the usual 3 s to start it), so its call is recorded before sopen says "opened" — on the
// fake target too, which runs with this side's environment. `SOPEN_LISTENERS` points the
// target's port check at a table of the test's own (an empty one by default), never at what
// happens to listen on the test machine; the fake ssh adds the forward's loopback listener to
// it, as sshd opens one.

/// An address with a password, a query and a fragment: none of them may be printed.
const SECRET_URL: &str = "http://me:s3cret@127.0.0.1:1234/x?token=abc#frag9";
/// [`SECRET_URL`] as sopen prints it.
const SECRET_SHOWN: &str = "http://me:***@127.0.0.1:1234/x?…#…";

/// Nothing secret of [`SECRET_URL`] is in `text`.
fn no_secrets(text: &str) {
    for secret in ["s3cret", "token=abc", "frag9"] {
        assert!(!text.contains(secret), "{secret} printed: {text}");
    }
}

/// A stand-in browser in `dir` and the file its calls go to.
fn stand_in_browser(dir: &Path) -> (PathBuf, PathBuf) {
    let browser = dir.join("browser");
    let calls = dir.join("browser-calls");
    executable(
        &browser,
        &format!(
            "#!/bin/sh\n{{ for a in \"$@\"; do printf '%s\\n' \"$a\"; done; echo ---; }} >> '{}'\n",
            calls.display()
        ),
    );
    (browser, calls)
}

/// The browser's calls, each its arguments.
fn browser_calls(calls: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(calls)
        .unwrap_or_default()
        .split("---\n")
        .filter(|c| !c.is_empty())
        .map(|c| c.lines().map(str::to_string).collect())
        .collect()
}

/// An empty listener table in `home` (nothing listens anywhere), for the fake ssh to add the
/// tunnel's listener to.
fn no_listeners(home: &Path) -> PathBuf {
    let table = home.join("listeners-none");
    std::fs::write(&table, "").unwrap();
    table
}

/// A listener table in `home` with the tunnel's listener on `port`, on 127.0.0.1 only.
fn loopback_listener(home: &Path, port: u16) -> PathBuf {
    let table = home.join(format!("listeners-{port}"));
    std::fs::write(
        &table,
        format!("tcp4       0      0  127.0.0.1.{port}         *.*        LISTEN       0\n"),
    )
    .unwrap();
    table
}

#[test]
fn an_address_opens_in_this_machines_browser_once_and_sopen_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let (browser, calls) = stand_in_browser(tmp.path());
    let ssh = FakeSsh::new(tmp.path());
    let env = [
        ("SOPEN_BROWSER", browser.to_str().unwrap()),
        ("SOPEN_EDITOR_WAIT", "60"),
        ("SOPEN_LOG", "debug"),
        ("PATH", ssh.path.as_str()),
    ];
    let out = sopen(tmp.path(), &["--browser", SECRET_URL], &env);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!("opened {SECRET_SHOWN} in the browser\n")
    );
    // The browser, and only the browser, gets it whole; nothing printed (debug included) does.
    assert_eq!(browser_calls(&calls), [[SECRET_URL]]);
    no_secrets(&text(&out.stdout));
    no_secrets(&text(&out.stderr));
    assert!(
        text(&out.stderr).contains(SECRET_SHOWN),
        "{}",
        text(&out.stderr)
    );
    // `--here`, and a target that is this machine: here too, never over ssh.
    for (args, extra) in [
        (vec!["--here", "--browser", SECRET_URL], None),
        (vec!["--target", "localhost", "--browser", SECRET_URL], None),
        (
            vec!["--browser", SECRET_URL],
            Some(("SOPEN_TARGET", "127.0.0.1")),
        ),
    ] {
        let mut env = env.to_vec();
        env.extend(extra);
        let out = sopen(tmp.path(), &args, &env);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&out.stderr)
        );
    }
    assert_eq!(browser_calls(&calls).len(), 4);
    assert!(ssh.argv().is_none(), "ssh ran");
    // A browser that fails is said, as an error, without the secrets.
    executable(&browser, "#!/bin/sh\nexit 4\n");
    let out = sopen(tmp.path(), &["--browser", SECRET_URL], &env);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(
        err.contains(&format!(
            "sopen: cannot open {SECRET_SHOWN} in the browser: "
        )),
        "{err}"
    );
    no_secrets(&err);
}

#[test]
fn an_address_with_app_or_hex_is_a_usage_error_and_a_malformed_one_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let (browser, calls) = stand_in_browser(tmp.path());
    let env = [
        ("SOPEN_BROWSER", browser.to_str().unwrap()),
        ("SOPEN_EDITOR_WAIT", "60"),
    ];
    let url = "http://127.0.0.1:1234/";
    for (args, code, says) in [
        (
            vec!["--browser", "--app", "Safari", SECRET_URL],
            2,
            "sopen: --app opens a file; ",
        ),
        (
            vec!["--browser", "--hex", SECRET_URL],
            2,
            "sopen: --hex opens a file; ",
        ),
        (
            vec!["--browser", "http://"],
            1,
            "sopen: not an address sopen can open: no host in 'http://'",
        ),
        (
            vec!["--browser", "http://me:s3cret@box:99999/?token=abc#frag9"],
            1,
            "sopen: not an address sopen can open: '99999' is not a port",
        ),
        // A loopback address by another spelling is refused, not tunnelled or passed on.
        (
            vec![
                "--target",
                HOST,
                "--browser",
                "http://127.1:1234/?token=abc",
            ],
            1,
            "is read by browsers as an IPv4 address (write the host plainly (127.0.0.1))",
        ),
        // The hidden flag is HOST's, for an address, with --here.
        (vec!["--hold-until-eof", url], 2, "--here"),
        (
            vec!["--here", "--hold-until-eof", "box:/x"],
            2,
            "sopen: --hold-until-eof is for an http(s):// address",
        ),
    ] {
        let out = sopen(tmp.path(), &args, &env);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(code), "{args:?}: {err}");
        assert!(err.contains(says), "{args:?}: {err}");
        no_secrets(&err);
    }
    assert!(browser_calls(&calls).is_empty(), "the browser was called");
}

/// `--browser` is for an address (09 §7): with a file SOURCE — a remote one, a local one, one
/// that would go to a target — it is a usage error, as `--hold-until-eof` is, said on one line
/// before anything is read or run.
#[test]
fn browser_with_a_file_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let (browser, calls) = stand_in_browser(tmp.path());
    let ssh = FakeSsh::new(tmp.path());
    let local = tmp.path().join("a.png");
    small_png(&local);
    let env = [
        ("SOPEN_BROWSER", browser.to_str().unwrap()),
        ("SOPEN_EDITOR_WAIT", "60"),
        ("PATH", ssh.path.as_str()),
    ];
    for args in [
        vec!["--browser", "box:/x"],
        vec!["--browser", local.to_str().unwrap()],
        vec!["--here", "--browser", local.to_str().unwrap()],
        vec!["--target", HOST, "--browser", "box:/x.png"],
        vec!["--browser", "--app", "ImHex", local.to_str().unwrap()],
    ] {
        let out = sopen(tmp.path(), &args, &env);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {err}");
        assert_eq!(
            err, "sopen: --browser is for an http(s):// address, not a file\n",
            "{args:?}"
        );
        assert_eq!(text(&out.stdout), "", "{args:?}");
    }
    assert!(ssh.argv().is_none(), "ssh ran");
    assert!(browser_calls(&calls).is_empty(), "the browser was called");
}

/// `sopen --here --hold-until-eof URL` with `table` as the listener table: the running child,
/// its stdout and stderr (at their end).
fn held(
    home: &Path,
    browser: &Path,
    url: &str,
    table: &Path,
) -> (
    Reaped,
    std::sync::mpsc::Receiver<Vec<u8>>,
    std::sync::mpsc::Receiver<Vec<u8>>,
) {
    let mut cmd = held_command(home, browser, url, table);
    cmd.stdout(std::process::Stdio::piped());
    let mut guard = Reaped(Some(cmd.spawn().unwrap()));
    let child = guard.0.as_mut().unwrap();
    let stdout = collected(child.stdout.take());
    let stderr = collected(child.stderr.take());
    (guard, stdout, stderr)
}

/// HOST's half as the other side starts it (`--here --hold-until-eof --browser -- URL`), its
/// input and stderr piped; the caller sets its stdout.
fn held_command(home: &Path, browser: &Path, url: &str, table: &Path) -> Command {
    let mut cmd = sopen_command(home);
    cmd.args(["--here", "--hold-until-eof", "--browser", "--", url])
        .env("SOPEN_BROWSER", browser)
        .env("SOPEN_EDITOR_WAIT", "60")
        .env("SOPEN_LISTENERS", table)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd
}

/// The other side of a tunnel: `--here --hold-until-eof URL` opens the browser, says the fixed
/// cue (nothing of the address), and then stays until its input ends — the other machine's
/// sopen ending — and exits 0.
#[test]
fn hold_until_eof_opens_then_waits_for_the_inputs_end() {
    let tmp = tempfile::tempdir().unwrap();
    let (browser, calls) = stand_in_browser(tmp.path());
    let url = "http://127.0.0.1:50000/x?token=abc";
    let table = loopback_listener(tmp.path(), 50000);
    let (mut guard, stdout, stderr) = held(tmp.path(), &browser, url, &table);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while browser_calls(&calls).is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the browser was not called"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(browser_calls(&calls), [[url]]);
    // Opened, input still open: it holds.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let child = guard.0.as_mut().unwrap();
    assert!(child.try_wait().unwrap().is_none(), "it did not hold");
    drop(child.stdin.take());
    let status = loop {
        if let Some(status) = guard.0.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "it did not end at its input's end"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    guard.0 = None;
    let err = stderr.recv().unwrap();
    assert_eq!(status.code(), Some(0), "{}", text(&err));
    assert_eq!(
        text(&stdout.recv().unwrap()),
        format!("{}\n", sopen::url::TUNNEL_OPEN)
    );
}

/// HOST's half with its output gone before it says the cue (the other side's reader closed
/// early): the cue's write fails and is ignored — no panic — and it still holds until its input
/// ends, then exits 0 (09 §7: cues are never `println!`).
#[test]
fn hold_until_eof_with_its_output_closed_still_holds_and_exits_0() {
    let tmp = tempfile::tempdir().unwrap();
    let (browser, calls) = stand_in_browser(tmp.path());
    let url = "http://127.0.0.1:50000/";
    let table = loopback_listener(tmp.path(), 50000);
    // A pipe whose reader is gone before sopen starts: every write to it fails (EPIPE).
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut cmd = held_command(tmp.path(), &browser, url, &table);
    cmd.stdout(writer);
    let mut guard = Reaped(Some(cmd.spawn().unwrap()));
    drop(cmd);
    let stderr = collected(guard.0.as_mut().unwrap().stderr.take());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while browser_calls(&calls).is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the browser was not called"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Past the cue's write: it holds still.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let child = guard.0.as_mut().unwrap();
    let early = child.try_wait().unwrap();
    drop(child.stdin.take());
    let status = loop {
        if let Some(status) = guard.0.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "it did not end at its input's end"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    guard.0 = None;
    let err = text(&stderr.recv().unwrap());
    assert!(
        early.is_none(),
        "it ended before its input did ({early:?}): {err}"
    );
    assert_eq!(status.code(), Some(0), "{err}");
    assert!(!err.contains("panicked"), "{err}");
    assert_eq!(browser_calls(&calls), [[url]]);
}

/// The tunnel's port open to the network on HOST (sshd's `GatewayPorts yes` binds a forward to
/// every interface): HOST's half refuses before the browser opens. So it does without positive
/// evidence — no listener seen on the port, a table it cannot read: the check fails closed.
#[test]
fn hold_until_eof_refuses_a_port_open_to_the_network() {
    let tmp = tempfile::tempdir().unwrap();
    let (browser, calls) = stand_in_browser(tmp.path());
    let url = "http://127.0.0.1:50000/";
    let wildcard = tmp.path().join("listeners-wildcard");
    // netstat's `*.50000` (macOS) and /proc's 0.0.0.0:C350 (Linux) beside a loopback one.
    std::fs::write(
        &wildcard,
        "tcp4       0      0  127.0.0.1.50000        *.*                    LISTEN       0\n\
         tcp4       0      0  *.50000                *.*                    LISTEN       0\n\
            0: 00000000:C350 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 1 1\n",
    )
    .unwrap();
    let missing = tmp.path().join("no-such-table");
    let elsewhere = loopback_listener(tmp.path(), 50001);
    let unseen = "cannot confirm the tunnel is private on ";
    for (table, says) in [
        (&wildcard, "the tunnel's port 50000 listens on 0.0.0.0 on "),
        (&missing, "cannot read SOPEN_LISTENERS"),
        (&no_listeners(tmp.path()), unseen),
        (&elsewhere, unseen),
    ] {
        let (mut guard, stdout, stderr) = held(tmp.path(), &browser, url, table);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let status = loop {
            if let Some(status) = guard.0.as_mut().unwrap().try_wait().unwrap() {
                break status;
            }
            assert!(std::time::Instant::now() < deadline, "it did not refuse");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        guard.0 = None;
        let err = text(&stderr.recv().unwrap());
        assert_eq!(status.code(), Some(1), "{err}");
        assert!(err.contains(says), "{err}");
        if says == unseen {
            assert!(err.contains("(no listener seen on port 50000)"), "{err}");
        }
        assert_eq!(text(&stdout.recv().unwrap()), "", "the cue was said");
    }
    assert!(browser_calls(&calls).is_empty(), "the browser was called");
}

/// `sopen --target HOST <args>` through the fake ssh, HOST's half the real binary (unless `env`
/// names another `SOPEN_TARGET_BIN`), its input none and its stderr piped; the caller sets its
/// stdout.
fn tunnelled_command(
    home: &Path,
    ssh: &FakeSsh,
    browser: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> Command {
    let mut cmd = sopen_command(home);
    cmd.args(["--target", HOST])
        .args(args)
        .env("PATH", &ssh.path)
        .env("SOPEN_TARGET_BIN", env!("CARGO_BIN_EXE_sopen"))
        .env("SOPEN_BROWSER", browser)
        .env("SOPEN_EDITOR_WAIT", "60")
        .env("SOPEN_LISTENERS", no_listeners(home))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd
}

/// A run of `sopen --target HOST --browser URL` through the fake ssh, HOST's half the real
/// binary (unless `env` names another `SOPEN_TARGET_BIN`): its stdout lines as they come, its
/// stderr at its end.
struct Tunnelled {
    guard: Reaped,
    lines: std::sync::mpsc::Receiver<String>,
    stderr: std::sync::mpsc::Receiver<Vec<u8>>,
}

impl Tunnelled {
    fn start(home: &Path, ssh: &FakeSsh, browser: &Path, url: &str, env: &[(&str, &str)]) -> Self {
        let cmd = tunnelled_command(home, ssh, browser, &["--browser", url], env);
        Self::spawn(cmd)
    }

    /// [`Tunnelled::start`] with `args` after `--target HOST` (no `--browser` unless given).
    fn start_with(
        home: &Path,
        ssh: &FakeSsh,
        browser: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Self {
        Self::spawn(tunnelled_command(home, ssh, browser, args, env))
    }

    fn spawn(mut cmd: Command) -> Self {
        cmd.stdout(std::process::Stdio::piped());
        let mut guard = Reaped(Some(cmd.spawn().unwrap()));
        let child = guard.0.as_mut().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = collected(child.stderr.take());
        Self {
            guard,
            lines,
            stderr,
        }
    }

    /// The first stdout line, within 30 s.
    fn first_line(&self) -> String {
        self.lines
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap_or_else(|_| {
                let err = self
                    .stderr
                    .recv_timeout(std::time::Duration::from_millis(100))
                    .unwrap_or_default();
                panic!("no line from sopen within 30 s; stderr: {}", text(&err))
            })
    }

    /// The stdout lines after the first, once sopen has ended.
    fn rest(&self) -> Vec<String> {
        self.lines.try_iter().collect()
    }

    /// Once sopen has ended: the stdout lines not taken yet, to the output's end — or until none
    /// comes for 5 s (a child another test started at the wrong moment can hold the pipe open,
    /// so its end may never come; what sopen wrote is in it all the same).
    fn to_end(&self) -> Vec<String> {
        let mut lines = Vec::new();
        while let Ok(line) = self.lines.recv_timeout(std::time::Duration::from_secs(5)) {
            lines.push(line);
        }
        lines
    }

    /// How long from `since` until sopen exited (within 20 s); [`Tunnelled::ended`] then gives
    /// its status. Its outputs' ends are not waited for.
    fn exited_after(&mut self, since: std::time::Instant) -> std::time::Duration {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while self.guard.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline, "sopen did not end");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        since.elapsed()
    }

    fn signal(&self, signal: i32) {
        let pid = self.guard.0.as_ref().unwrap().id() as i32;
        // SAFETY: the child is ours and not reaped yet, so the id is still its own.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
    }

    /// Its exit status and stderr, within 20 s.
    fn ended(&mut self) -> (std::process::ExitStatus, String) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let status = loop {
            if let Some(status) = self.guard.0.as_mut().unwrap().try_wait().unwrap() {
                break status;
            }
            assert!(std::time::Instant::now() < deadline, "sopen did not end");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        self.guard.0 = None;
        let err = self
            .stderr
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap_or_default();
        (status, text(&err))
    }
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
    text(&out.stdout)
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

/// The fake ssh's pids and its parents' (the tunnel's guard), and the guards' children (ssh
/// and the watcher) as they are now.
fn tunnel_pids(ssh: &FakeSsh) -> Vec<i32> {
    let mut pids: Vec<i32> = ssh
        .lines("ssh-pids")
        .unwrap_or_default()
        .iter()
        .flat_map(|l| l.split_whitespace().map(|p| p.parse::<i32>().unwrap()))
        .collect();
    let guards: Vec<i32> = pids.iter().skip(1).step_by(2).copied().collect();
    for guard in guards {
        pids.extend(children(guard));
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Every one of `pids` is reaped within 10 s: no ssh, guard or watcher is left running or a
/// zombie.
fn all_gone(pids: &[i32]) {
    assert!(!pids.is_empty());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    for &pid in pids {
        while !reaped(pid) {
            if std::time::Instant::now() >= deadline {
                for &pid in pids {
                    // SAFETY: stand-ins this test started; they must not outlive the test.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
                panic!("{pid} of the tunnel outlived sopen (running, or a zombie)");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

/// The calls the fake ssh recorded, each its arguments.
fn ssh_calls(ssh: &FakeSsh) -> Vec<Vec<String>> {
    std::fs::read_to_string(ssh.dir.join("calls"))
        .unwrap_or_default()
        .split("---\n")
        .filter(|c| !c.is_empty())
        .map(|c| c.lines().map(str::to_string).collect())
        .collect()
}

#[test]
fn a_loopback_address_on_a_target_goes_through_a_tunnel_that_ends_with_sopen() {
    let tmp = tempfile::tempdir().unwrap();
    let (browser, calls) = stand_in_browser(tmp.path());
    let ssh = FakeSsh::new(tmp.path());
    let mut run = Tunnelled::start(
        tmp.path(),
        &ssh,
        &browser,
        SECRET_URL,
        &[("SOPEN_LOG", "debug")],
    );
    assert_eq!(
        run.first_line(),
        format!("{SECRET_SHOWN} is open on {HOST} through a tunnel — Ctrl-C closes it")
    );
    let pids = tunnel_pids(&ssh);
    // HOST's browser got the address as written.
    assert_eq!(browser_calls(&calls), [[SECRET_URL]]);
    let made = ssh_calls(&ssh);
    assert_eq!(made.len(), 1, "{made:#?}");
    let argv = &made[0];
    // Its own connection, never sopen's master; ExitOnForwardFailure; no terminal, and this
    // side's input (no -n); the address's own port on HOST, bound to its loopback.
    let at = argv.iter().position(|a| a == HOST).unwrap();
    let before: Vec<&str> = argv[..at].iter().map(String::as_str).collect();
    for (i, pair) in [
        ["-S", "none"],
        ["-o", "ControlMaster=no"],
        ["-o", "ExitOnForwardFailure=yes"],
        ["-o", "ServerAliveInterval=10"],
        ["-o", "ConnectTimeout=15"],
        ["-R", "127.0.0.1:1234:127.0.0.1:1234"],
    ]
    .iter()
    .enumerate()
    {
        assert!(before.windows(2).any(|w| w == pair), "{i}: {before:?}");
    }
    assert!(
        before.contains(&"-T") && !before.contains(&"-n"),
        "{before:?}"
    );
    assert!(
        !before.iter().any(|a| a.contains("ControlPersist")),
        "{before:?}"
    );
    assert_eq!(argv.last().unwrap(), &q(SECRET_URL));
    assert!(argv.contains(&q("--hold-until-eof")), "{argv:#?}");
    // `--browser` goes to HOST's half with it, before the address (09 §7).
    assert_eq!(
        argv[argv.len() - 4..],
        [
            q("--hold-until-eof"),
            q("--browser"),
            q("--"),
            q(SECRET_URL)
        ],
        "{argv:#?}"
    );

    // SIGTERM (as Ctrl-C) closes it: exit 0, and ssh, the guard and its watcher are gone.
    run.signal(libc::SIGTERM);
    let (status, err) = run.ended();
    assert_eq!(status.code(), Some(0), "{err}");
    // Nothing printed — HOST's debug lines and this side's included — holds the secrets.
    assert!(err.contains("sopen[debug]"), "{err}");
    no_secrets(&err);
    no_secrets(&run.rest().join("\n"));
    assert!(pids.len() >= 3, "{pids:?}");
    all_gone(&pids);
}

#[test]
fn a_taken_port_on_the_target_takes_another() {
    let tmp = tempfile::tempdir().unwrap();
    let (browser, calls) = stand_in_browser(tmp.path());
    let ssh = FakeSsh::new(tmp.path());
    let url = "http://localhost:1234/x";
    let mut run = Tunnelled::start(
        tmp.path(),
        &ssh,
        &browser,
        url,
        &[("FAKE_SSH_TAKEN", "1234")],
    );
    let line = run.first_line();
    let pids = tunnel_pids(&ssh);
    let made = ssh_calls(&ssh);
    assert_eq!(made.len(), 2, "{made:#?}");
    let forward = |call: &[String]| {
        let at = call.iter().position(|a| a == "-R").unwrap();
        call[at + 1].clone()
    };
    assert_eq!(forward(&made[0]), "127.0.0.1:1234:localhost:1234");
    let port: u16 = forward(&made[1])
        .strip_prefix("127.0.0.1:")
        .and_then(|f| f.strip_suffix(":localhost:1234"))
        .unwrap()
        .parse()
        .unwrap();
    assert!((49152..=65535).contains(&port), "{port}");
    let there = format!("http://localhost:{port}/x");
    assert_eq!(browser_calls(&calls), [[there.clone()]]);
    assert_eq!(
        line,
        format!(
            "{url} is open on {HOST} as {there} through a tunnel (port 1234 could not be \
             forwarded there) — Ctrl-C closes it"
        )
    );
    run.signal(libc::SIGINT);
    let (status, err) = run.ended();
    assert_eq!(status.code(), Some(0), "{err}");
    // ssh's own failure line is read, not passed on; sopen says what it does instead.
    assert!(!err.contains("remote port forwarding failed"), "{err}");
    assert!(
        err.contains(&format!(
            "sopen: port 1234 could not be forwarded on {HOST} (taken there, or forwarding \
             refused) — trying {port}"
        )),
        "{err}"
    );
    all_gone(&pids);
}

/// sopen killed outright (SIGKILL: nothing of it runs) still ends the tunnel — even when ssh
/// would stay for the forwarded connections a browser keeps open (`FAKE_SSH_LINGER`).
#[test]
fn a_killed_sopen_leaves_no_tunnel() {
    let tmp = tempfile::tempdir().unwrap();
    let (browser, _) = stand_in_browser(tmp.path());
    let ssh = FakeSsh::new(tmp.path());
    let url = "http://127.0.0.1:1234/";
    let mut run = Tunnelled::start(tmp.path(), &ssh, &browser, url, &[("FAKE_SSH_LINGER", "1")]);
    assert!(run.first_line().contains("through a tunnel"));
    let pids = tunnel_pids(&ssh);
    run.signal(libc::SIGKILL);
    let (status, _) = run.ended();
    assert_eq!(status.code(), None);
    all_gone(&pids);
}

/// HOST's sopen ends by itself (killed there, say) while ssh stays for the browser's
/// connections (`FAKE_SSH_LINGER`, its output closed as OpenSSH's is once the session ends):
/// this side sees the session's end, closes the tunnel and exits 1 — HOST gave no exit code.
#[test]
fn hosts_sopen_ending_closes_the_tunnel() {
    let tmp = tempfile::tempdir().unwrap();
    let (browser, _) = stand_in_browser(tmp.path());
    let ssh = FakeSsh::new(tmp.path());
    let url = "http://127.0.0.1:1234/";
    let mut run = Tunnelled::start(
        tmp.path(),
        &ssh,
        &browser,
        url,
        &[
            ("FAKE_SSH_LINGER", "1"),
            ("SOPEN_TARGET_BIN", ssh.sopen.to_str().unwrap()),
            ("FAKE_SOPEN_SAY", sopen::url::TUNNEL_OPEN),
        ],
    );
    assert!(run.first_line().contains("through a tunnel"));
    let pids = tunnel_pids(&ssh);
    let started = std::time::Instant::now();
    let (status, err) = run.ended();
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
    assert_eq!(status.code(), Some(1), "{err}");
    assert!(
        err.contains(&format!(
            "sopen: sopen on {HOST} ended while ssh stayed for the browser's connections; the \
             tunnel to {url} is closed"
        )),
        "{err}"
    );
    all_gone(&pids);
}

/// The tunnel's port open to the network on HOST: HOST's sopen refuses before its browser
/// opens, and this side ends with it, leaving nothing.
#[test]
fn a_tunnel_open_to_the_network_on_the_target_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (browser, calls) = stand_in_browser(tmp.path());
    let ssh = FakeSsh::new(tmp.path());
    let table = tmp.path().join("listeners-wildcard");
    std::fs::write(
        &table,
        "tcp46      0      0  *.1234                 *.*                    LISTEN       0\n",
    )
    .unwrap();
    let out = sopen(
        tmp.path(),
        &["--target", HOST, "--browser", "http://127.0.0.1:1234/"],
        &[
            ("PATH", ssh.path.as_str()),
            ("SOPEN_TARGET_BIN", env!("CARGO_BIN_EXE_sopen")),
            ("SOPEN_BROWSER", browser.to_str().unwrap()),
            ("SOPEN_EDITOR_WAIT", "60"),
            ("SOPEN_LISTENERS", table.to_str().unwrap()),
        ],
    );
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(
        err.contains("the tunnel's port 1234 listens on 0.0.0.0 on "),
        "{err}"
    );
    assert!(err.contains("refused, nothing was opened"), "{err}");
    assert_eq!(text(&out.stdout), "");
    assert!(browser_calls(&calls).is_empty(), "the browser was called");
    all_gone(&tunnel_pids(&ssh));

    // No listener seen on HOST for the tunnel's port: refused too — no positive evidence.
    let none = no_listeners(tmp.path());
    let out = sopen(
        tmp.path(),
        &["--target", HOST, "--browser", "http://127.0.0.1:1234/"],
        &[
            ("PATH", ssh.path.as_str()),
            ("SOPEN_TARGET_BIN", env!("CARGO_BIN_EXE_sopen")),
            ("SOPEN_BROWSER", browser.to_str().unwrap()),
            ("SOPEN_EDITOR_WAIT", "60"),
            ("SOPEN_LISTENERS", none.to_str().unwrap()),
            ("FAKE_SSH_NO_BIND", "1"),
        ],
    );
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(
        err.contains("cannot confirm the tunnel is private on "),
        "{err}"
    );
    assert!(err.contains("(no listener seen on port 1234)"), "{err}");
    assert_eq!(text(&out.stdout), "");
    assert!(browser_calls(&calls).is_empty(), "the browser was called");
    all_gone(&tunnel_pids(&ssh));
}

/// A non-loopback address goes to HOST's sopen as it is: no tunnel, no hold.
#[test]
fn another_address_on_a_target_is_opened_there_as_it_is() {
    let tmp = tempfile::tempdir().unwrap();
    let (browser, calls) = stand_in_browser(tmp.path());
    let ssh = FakeSsh::new(tmp.path());
    let url = "https://example.com/a?b";
    let out = sopen(
        tmp.path(),
        &["--target", HOST, "--browser", url],
        &[
            ("PATH", ssh.path.as_str()),
            ("SOPEN_TARGET_BIN", env!("CARGO_BIN_EXE_sopen")),
            ("SOPEN_BROWSER", browser.to_str().unwrap()),
            ("SOPEN_EDITOR_WAIT", "60"),
            ("SOPEN_LOG", "debug"),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(browser_calls(&calls), [[url]]);
    assert_eq!(
        text(&out.stdout),
        "opened https://example.com/a?… in the browser\n"
    );
    assert!(!text(&out.stderr).contains("?b"), "{}", text(&out.stderr));
    let argv = ssh.argv().unwrap();
    assert!(!argv
        .iter()
        .any(|a| a == "-R" || a.contains("hold-until-eof")));
    assert!(argv.contains(&"-n".to_string()), "{argv:#?}");
    assert_eq!(argv.last().unwrap(), &q(url));
    // `--browser` goes to HOST's sopen with it, before the address (09 §7).
    assert_eq!(
        argv[argv.len() - 4..],
        [q("sopen"), q("--browser"), q("--"), q(url)],
        "{argv:#?}"
    );
}

/// `SOPEN_WEB` (09 §4) travels with `--target` as the other hooks do, on the file route and on
/// an address's, so the fallback is HOST's own: it reaches HOST's command line before the
/// script that runs sopen there, and is absent when it is not set here.
#[test]
fn sopen_web_travels_to_host() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let bin = ssh.sopen.to_str().unwrap();
    let page = format!("{HOST}:/tmp/a.html");
    for (args, web) in [
        (vec!["--target", HOST, page.as_str()], true),
        (
            vec!["--target", HOST, "--browser", "https://example.com/"],
            true,
        ),
        (vec!["--target", HOST, page.as_str()], false),
    ] {
        let mut env = vec![("PATH", ssh.path.as_str()), ("SOPEN_TARGET_BIN", bin)];
        if web {
            env.push(("SOPEN_WEB", "off"));
        }
        let out = sopen(tmp.path(), &args, &env);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
        let argv = ssh.argv().expect("ssh ran");
        let at = argv.iter().position(|a| *a == q("env")).expect("env words");
        let sh = argv.iter().position(|a| *a == q("sh")).expect("the script");
        let words = &argv[at + 1..sh];
        assert_eq!(
            words.contains(&q("SOPEN_WEB=off")),
            web,
            "{args:?}: {argv:#?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The cues of 09 §7 on this side. HOST's half is the stand-in sopen playing a HOST of any
// version (`FAKE_SOPEN_SAY`, `FAKE_SOPEN_HOLD`): an older one, or one that opened the system
// browser, says only `sopen-tunnel-open`; one that opened the address's window says
// `sopen-tunnel-window` before it, and `sopen-tunnel-closed` once the window is closed.

/// A tunnelled run of `url` whose HOST half says `say`, one line each, then exits — or, with
/// `FAKE_SOPEN_HOLD` in `env`, reads its input to the end first.
fn host_saying(
    home: &Path,
    ssh: &FakeSsh,
    browser: &Path,
    url: &str,
    say: &[&str],
    env: &[(&str, &str)],
) -> Tunnelled {
    let say = say.join("\n");
    let mut env = env.to_vec();
    env.extend([
        ("SOPEN_TARGET_BIN", ssh.sopen.to_str().unwrap()),
        ("FAKE_SOPEN_SAY", say.as_str()),
    ]);
    Tunnelled::start(home, ssh, browser, url, &env)
}

/// The open line once HOST's window cue came first (09 §7).
fn window_line(shown: &str) -> String {
    format!(
        "{shown} is open on {HOST} through a tunnel — closing its window there, or Ctrl-C here, \
         closes it"
    )
}

/// The line once HOST's window was closed (09 §7).
fn closed_line(shown: &str) -> String {
    format!("{shown}: its window on {HOST} was closed; the tunnel is closed")
}

/// The open line says that closing HOST's window closes the tunnel only after HOST's window
/// cue — on the address's own port and on a fallback one; a HOST without it (older, or on the
/// system browser) gets today's line. The cues are never shown.
#[test]
fn the_open_line_names_hosts_window_only_after_its_cue() {
    use sopen::url::{TUNNEL_OPEN, TUNNEL_WINDOW};
    let hold = ("FAKE_SOPEN_HOLD", "1");
    for (say, want) in [
        (
            vec![TUNNEL_OPEN],
            format!("{SECRET_SHOWN} is open on {HOST} through a tunnel — Ctrl-C closes it"),
        ),
        (vec![TUNNEL_WINDOW, TUNNEL_OPEN], window_line(SECRET_SHOWN)),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (browser, _) = stand_in_browser(tmp.path());
        let ssh = FakeSsh::new(tmp.path());
        let mut run = host_saying(tmp.path(), &ssh, &browser, SECRET_URL, &say, &[hold]);
        assert_eq!(run.first_line(), want, "{say:?}");
        let pids = tunnel_pids(&ssh);
        // HOST's half got `--browser`, as the real one does.
        let argv = ssh.sopen_argv().unwrap();
        assert_eq!(
            argv,
            ["--here", "--hold-until-eof", "--browser", "--", SECRET_URL],
            "{say:?}"
        );
        run.signal(libc::SIGTERM);
        let (status, err) = run.ended();
        assert_eq!(status.code(), Some(0), "{say:?}: {err}");
        assert_eq!(run.to_end(), Vec::<String>::new(), "{say:?}");
        no_secrets(&err);
        all_gone(&pids);
    }

    // A fallback port: the same window form.
    let tmp = tempfile::tempdir().unwrap();
    let (browser, _) = stand_in_browser(tmp.path());
    let ssh = FakeSsh::new(tmp.path());
    let url = "http://localhost:1234/x";
    let mut run = host_saying(
        tmp.path(),
        &ssh,
        &browser,
        url,
        &[TUNNEL_WINDOW, TUNNEL_OPEN],
        &[hold, ("FAKE_SSH_TAKEN", "1234")],
    );
    let line = run.first_line();
    let pids = tunnel_pids(&ssh);
    let made = ssh_calls(&ssh);
    assert_eq!(made.len(), 2, "{made:#?}");
    let at = made[1].iter().position(|a| a == "-R").unwrap();
    let port = made[1][at + 1]
        .strip_prefix("127.0.0.1:")
        .and_then(|f| f.strip_suffix(":localhost:1234"))
        .unwrap()
        .to_string();
    assert_eq!(
        line,
        format!(
            "{url} is open on {HOST} as http://localhost:{port}/x through a tunnel (port 1234 \
             could not be forwarded there) — closing its window there, or Ctrl-C here, closes it"
        )
    );
    run.signal(libc::SIGINT);
    let (status, err) = run.ended();
    assert_eq!(status.code(), Some(0), "{err}");
    all_gone(&pids);
}

/// Cues are matched exactly (09 §7, version skew): a line that is not one of the three — a cue
/// of a later sopen, a cue with more on its line, before it or after it (a space, a tab) — is
/// HOST's plain output, passed on in its place and never acted on; the real cues beside them
/// are still read.
#[test]
fn a_line_that_is_not_a_cue_passes_as_a_plain_line() {
    use sopen::url::{TUNNEL_OPEN, TUNNEL_WINDOW};
    let tmp = tempfile::tempdir().unwrap();
    let (browser, _) = stand_in_browser(tmp.path());
    let ssh = FakeSsh::new(tmp.path());
    let plain = [
        "sopen-tunnel-later",
        "sopen-tunnel-closed now",
        " sopen-tunnel-window",
        "SOPEN-TUNNEL-CLOSED",
        "sopen-tunnel-window ",
        "sopen-tunnel-window\t",
        "sopen-tunnel-open ",
        "sopen-tunnel-open\t",
        "sopen-tunnel-closed ",
        "sopen-tunnel-closed\t",
    ];
    let mut say = plain.to_vec();
    say.extend([TUNNEL_WINDOW, TUNNEL_OPEN]);
    let url = "http://127.0.0.1:1234/";
    let mut run = host_saying(
        tmp.path(),
        &ssh,
        &browser,
        url,
        &say,
        &[("FAKE_SOPEN_HOLD", "1")],
    );
    let mut lines = vec![run.first_line()];
    while lines.len() < plain.len() + 1 {
        lines.push(run.first_line());
    }
    let mut want: Vec<String> = plain.iter().map(|l| l.to_string()).collect();
    want.push(window_line(url));
    assert_eq!(lines, want);
    let pids = tunnel_pids(&ssh);
    // Not closed by a line that only looks like the closed cue: the tunnel stays.
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        run.guard.0.as_mut().unwrap().try_wait().unwrap().is_none(),
        "the run ended on a plain line"
    );
    run.signal(libc::SIGTERM);
    let (status, err) = run.ended();
    assert_eq!(status.code(), Some(0), "{err}");
    assert_eq!(run.to_end(), Vec::<String>::new());
    all_gone(&pids);
}

/// HOST's window closed (`sopen-tunnel-closed`): this side closes the tunnel at once, says so
/// and exits 0 — while HOST's half and ssh still run (without the cue it would wait for them
/// for good); when HOST's half ends at once after the cue and ssh with it (the cue is read after
/// the relay has drained, whatever HOST's code); and when ssh stays after HOST's half for the
/// browser's connections (without the cue: the grace, then exit 1). The exit code and the lines
/// tell these apart; the time to the exit is only bounded loosely, the suite running in
/// parallel.
#[test]
fn hosts_closed_cue_closes_the_tunnel_at_once_whether_ssh_stays_or_goes() {
    use sopen::url::{TUNNEL_CLOSED, TUNNEL_OPEN, TUNNEL_WINDOW};
    for (case, env) in [
        ("HOST's half holds", vec![("FAKE_SOPEN_HOLD", "1")]),
        ("HOST's half ends, ssh goes", vec![]),
        (
            "HOST's half ends 3, ssh goes",
            vec![("FAKE_SOPEN_EXIT", "3")],
        ),
        (
            "HOST's half ends, ssh stays",
            vec![("FAKE_SSH_LINGER", "1")],
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (browser, _) = stand_in_browser(tmp.path());
        let ssh = FakeSsh::new(tmp.path());
        let mut run = host_saying(
            tmp.path(),
            &ssh,
            &browser,
            SECRET_URL,
            &[TUNNEL_WINDOW, TUNNEL_OPEN, TUNNEL_CLOSED],
            &env,
        );
        let first = run.first_line();
        let opened = std::time::Instant::now();
        let pids = tunnel_pids(&ssh);
        let to_exit = run.exited_after(opened);
        let (status, err) = run.ended();
        let mut lines = vec![first];
        lines.extend(run.to_end());
        assert_eq!(
            lines,
            [window_line(SECRET_SHOWN), closed_line(SECRET_SHOWN)],
            "{case}: {err}"
        );
        assert_eq!(status.code(), Some(0), "{case}: {err}");
        assert!(!err.contains("sopen: "), "{case}: {err}");
        no_secrets(&err);
        assert!(
            to_exit < std::time::Duration::from_secs(10),
            "{case}: {to_exit:?}"
        );
        all_gone(&pids);
    }
}

/// Any other end of HOST's half keeps today's rule (09 §7), the window cue or not: HOST's own
/// code when ssh goes with it, and the tunnel closed with an error when ssh stays.
#[test]
fn hosts_end_without_the_closed_cue_keeps_todays_rule() {
    use sopen::url::{TUNNEL_OPEN, TUNNEL_WINDOW};
    let url = "http://127.0.0.1:1234/";
    for say in [vec![TUNNEL_OPEN], vec![TUNNEL_WINDOW, TUNNEL_OPEN]] {
        let opened = match say.len() {
            1 => format!("{url} is open on {HOST} through a tunnel — Ctrl-C closes it"),
            _ => window_line(url),
        };
        let tmp = tempfile::tempdir().unwrap();
        let (browser, _) = stand_in_browser(tmp.path());
        let ssh = FakeSsh::new(tmp.path());
        let mut run = host_saying(
            tmp.path(),
            &ssh,
            &browser,
            url,
            &say,
            &[("FAKE_SOPEN_EXIT", "4")],
        );
        assert_eq!(run.first_line(), opened, "{say:?}");
        let pids = tunnel_pids(&ssh);
        let (status, err) = run.ended();
        assert_eq!(status.code(), Some(4), "{say:?}: {err}");
        assert_eq!(run.to_end(), Vec::<String>::new(), "{say:?}");
        all_gone(&pids);

        let tmp = tempfile::tempdir().unwrap();
        let ssh = FakeSsh::new(tmp.path());
        let mut run = host_saying(
            tmp.path(),
            &ssh,
            &browser,
            url,
            &say,
            &[("FAKE_SSH_LINGER", "1")],
        );
        assert_eq!(run.first_line(), opened, "{say:?}");
        let pids = tunnel_pids(&ssh);
        let (status, err) = run.ended();
        assert_eq!(status.code(), Some(1), "{say:?}: {err}");
        assert!(
            err.contains(&format!(
                "sopen: sopen on {HOST} ended while ssh stayed for the browser's connections; \
                 the tunnel to {url} is closed"
            )),
            "{say:?}: {err}"
        );
        all_gone(&pids);
    }
}

/// This side's own lines are written as HOST's plain ones are, a write error ignored: with its
/// output gone (`sopen … | head -1`), the open and closed lines are no panic — HOST's window
/// closed still ends the run with 0 (09 §7, as HOST's cues).
#[test]
fn the_tunnels_lines_on_a_closed_output_are_no_panic() {
    use sopen::url::{TUNNEL_CLOSED, TUNNEL_OPEN, TUNNEL_WINDOW};
    let tmp = tempfile::tempdir().unwrap();
    let (browser, _) = stand_in_browser(tmp.path());
    let ssh = FakeSsh::new(tmp.path());
    let say = [TUNNEL_WINDOW, TUNNEL_OPEN, TUNNEL_CLOSED].join("\n");
    let mut cmd = tunnelled_command(
        tmp.path(),
        &ssh,
        &browser,
        &["--browser", SECRET_URL],
        &[
            ("SOPEN_TARGET_BIN", ssh.sopen.to_str().unwrap()),
            ("FAKE_SOPEN_SAY", &say),
            ("FAKE_SOPEN_HOLD", "1"),
        ],
    );
    // A pipe whose reader is gone before sopen starts: every write to it fails (EPIPE).
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    cmd.stdout(writer);
    let mut guard = Reaped(Some(cmd.spawn().unwrap()));
    drop(cmd);
    let stderr = collected(guard.0.as_mut().unwrap().stderr.take());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = guard.0.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(std::time::Instant::now() < deadline, "sopen did not end");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    guard.0 = None;
    let err = text(
        &stderr
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap_or_default(),
    );
    assert_eq!(status.code(), Some(0), "{err}");
    assert!(!err.contains("panicked"), "{err}");
    all_gone(&tunnel_pids(&ssh));
}

/// What this side prints keeps HOST's order (09 §7): HOST's lines coming apart in time, the open
/// line stands where the open cue was, HOST's plain line after it, the closed line last. In one
/// burst a plain line after the open cue may come out before the open line — the relay writes
/// it at once, the open line waits for the next 20 ms poll, as before 09 — so there only what is
/// ordered is held: a plain line before the open cue comes before the open line, and the closed
/// line comes last, after every plain line.
#[test]
fn the_tunnels_output_keeps_hosts_order() {
    use sopen::url::{TUNNEL_CLOSED, TUNNEL_OPEN, TUNNEL_WINDOW};
    let url = "http://127.0.0.1:1234/";
    let said = "a plain line of HOST's own";
    let before = "a plain line before the cues";
    for (say, pace) in [
        (
            vec![TUNNEL_WINDOW, TUNNEL_OPEN, said, TUNNEL_CLOSED],
            Some("1"),
        ),
        (
            vec![before, TUNNEL_WINDOW, TUNNEL_OPEN, said, TUNNEL_CLOSED],
            None,
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (browser, _) = stand_in_browser(tmp.path());
        let ssh = FakeSsh::new(tmp.path());
        let mut env = vec![("FAKE_SOPEN_HOLD", "1")];
        env.extend(pace.map(|p| ("FAKE_SOPEN_PACE", p)));
        let mut run = host_saying(tmp.path(), &ssh, &browser, url, &say, &env);
        let first = run.first_line();
        let pids = tunnel_pids(&ssh);
        run.exited_after(std::time::Instant::now());
        let (status, err) = run.ended();
        let mut lines = vec![first];
        lines.extend(run.to_end());
        assert_eq!(status.code(), Some(0), "{say:?}: {err}");
        match pace {
            Some(_) => assert_eq!(
                lines,
                [window_line(url), said.to_string(), closed_line(url)],
                "{say:?}"
            ),
            None => {
                assert_eq!(lines.len(), 4, "{lines:?}");
                assert_eq!(lines[0], before, "{lines:?}");
                assert_eq!(lines[3], closed_line(url), "{lines:?}");
                let mut middle = lines[1..3].to_vec();
                middle.sort();
                let mut want = vec![window_line(url), said.to_string()];
                want.sort();
                assert_eq!(middle, want, "{lines:?}");
            }
        }
        all_gone(&pids);
    }
}

/// Without `--browser`: HOST's half is given none (`--here --hold-until-eof -- URL`) and a HOST
/// that says only the open cue gets today's line, Ctrl-C ending it with 0; an address passed as
/// it is goes as `--here -- URL`.
#[test]
fn without_browser_host_is_given_no_browser_flag() {
    use sopen::url::TUNNEL_OPEN;
    let tmp = tempfile::tempdir().unwrap();
    let (browser, calls) = stand_in_browser(tmp.path());
    let ssh = FakeSsh::new(tmp.path());
    let bin = ssh.sopen.to_str().unwrap();
    let mut run = Tunnelled::start_with(
        tmp.path(),
        &ssh,
        &browser,
        &[SECRET_URL],
        &[
            ("SOPEN_TARGET_BIN", bin),
            ("FAKE_SOPEN_SAY", TUNNEL_OPEN),
            ("FAKE_SOPEN_HOLD", "1"),
        ],
    );
    assert_eq!(
        run.first_line(),
        format!("{SECRET_SHOWN} is open on {HOST} through a tunnel — Ctrl-C closes it")
    );
    let pids = tunnel_pids(&ssh);
    assert_eq!(
        ssh.sopen_argv().unwrap(),
        ["--here", "--hold-until-eof", "--", SECRET_URL]
    );
    run.signal(libc::SIGTERM);
    let (status, err) = run.ended();
    assert_eq!(status.code(), Some(0), "{err}");
    all_gone(&pids);

    ssh.forget();
    let url = "https://example.com/a?b";
    let out = sopen(
        tmp.path(),
        &["--target", HOST, url],
        &[
            ("PATH", ssh.path.as_str()),
            ("SOPEN_TARGET_BIN", bin),
            ("SOPEN_BROWSER", browser.to_str().unwrap()),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(ssh.sopen_argv().unwrap(), ["--here", "--", url]);
    let argv = ssh.argv().unwrap();
    assert_eq!(
        argv[argv.len() - 3..],
        [q("sopen"), q("--"), q(url)],
        "{argv:#?}"
    );
    // The stand-in HOST opened nothing.
    assert!(browser_calls(&calls).is_empty());
}
