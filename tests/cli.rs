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

fn sopen(home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = sopen_command(home);
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn version_line() {
    let tmp = tempfile::tempdir().unwrap();
    let out = sopen(tmp.path(), &["--version"], &[]);
    assert!(out.status.success());
    let line = text(&out.stdout);
    let want = format!("sopen {} (", env!("CARGO_PKG_VERSION"));
    assert!(line.starts_with(&want), "{line}");
    assert!(line.trim_end().ends_with(')'), "{line}");
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
    assert!(lines[4].starts_with("2 listed = 4 bytes"), "{listed}");
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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !record.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let seen = std::fs::read_to_string(&record).unwrap();
    assert_eq!(seen.trim(), real.join("a.txt").to_str().unwrap());
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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
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
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("42"), "{}", text(&out.stderr));
    assert!(!marker.exists());
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
    // The CLI is detached; give it a moment to write.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
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
    let mut retries = 0;
    loop {
        match cmd.output() {
            Ok(out) => return out,
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && retries < 50 => {
                retries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => panic!("cannot run {cmd:?}: {e}"),
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
