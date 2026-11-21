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

/// How long a run that should end by itself may take before the test gives up on it.
const RUN_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Runs sopen to its end and returns what it printed. A run that has not ended in
/// [`RUN_LIMIT`] is killed and reaped, and the test fails — it never hangs the suite.
fn sopen(home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = sopen_command(home);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    // Read both pipes while it runs, so a run that prints a lot is never stuck on a full pipe.
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = std::time::Instant::now() + RUN_LIMIT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("sopen {args:?} did not end within {RUN_LIMIT:?}; killed");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
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
// sopen on the target, and then runs it as OpenSSH does — the words after the destination
// joined with spaces, `sh -c "<line>"` on the "target" (here, in a home of its own, with a
// PATH of only the tools the script uses and a display set; the script's Linux branch has its
// own tests). The target has nothing installed: no sopen on that PATH, and the absolute install
// paths in the command are pointed at a directory that does not exist, so a sopen installed on
// the test machine is never run. `FAKE_SSH_EXIT` stops it before the run, with that code.

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
                 if [ -n \"${{FAKE_SSH_EXIT:-}}\" ]; then exit \"$FAKE_SSH_EXIT\"; fi\n\
                 while [ \"$#\" -gt 0 ] && [ \"$1\" != -- ]; do shift; done\n\
                 shift\n\
                 line=$(printf '%s' \"$*\" | sed{unplug})\n\
                 mkdir -p '{dir}/target-home'\n\
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
        executable(
            &sopen,
            &format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{dir}/sopen-argv'\n\
                 if [ -n \"${{FAKE_SOPEN_SIGNAL:-}}\" ]; then kill -\"$FAKE_SOPEN_SIGNAL\" $$; fi\n\
                 exit \"${{FAKE_SOPEN_EXIT:-0}}\"\n",
                dir = dir.display()
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
