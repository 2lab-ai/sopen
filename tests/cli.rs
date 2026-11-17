//! The `sopen` binary on local targets: no ssh, no network. Cache, state and config go to a temp
//! dir; `--target` runs a fake `ssh`.

use std::path::Path;
use std::process::{Command, Output};

/// The binary with cache, state and config under `home`, and nothing from the caller's
/// environment that would forward the run (a default target) or change it (a receipt hook).
fn sopen_command(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sopen"));
    cmd.env("SOPEN_CACHE_DIR", home.join("cache"))
        .env("SOPEN_STATE_DIR", home.join("state"))
        .env("SOPEN_CONFIG", home.join("config.toml"))
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

#[test]
fn other_files_print_their_path_and_log_usage() {
    let tmp = tempfile::tempdir().unwrap();
    let blob = tmp.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1, 2, 3]).unwrap();
    let out = sopen(tmp.path(), &[blob.to_str().unwrap()], &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let real = std::fs::canonicalize(&blob).unwrap();
    assert_eq!(text(&out.stdout).trim(), real.to_str().unwrap());
    let log = std::fs::read_to_string(tmp.path().join("state/usage.jsonl")).unwrap();
    let v: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(v["kind"], "other");
    assert_eq!(v["ext"], "bin");
    assert_eq!(v["action"], "open");
    assert_eq!(v["host_hash"], "local");
    assert_eq!(v["ok"], true);
}

#[test]
fn folders_and_text_go_to_the_editor_command() {
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
    for target in [dir.clone(), dir.join("Makefile")] {
        let out = sopen(tmp.path(), &[target.to_str().unwrap()], &zed);
        assert!(out.status.success(), "{}", text(&out.stderr));
        assert!(
            text(&out.stdout).starts_with("opened "),
            "{}",
            text(&out.stdout)
        );
    }
    // The CLI is detached; give it a moment to write.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = String::new();
    while std::time::Instant::now() < deadline {
        seen = std::fs::read_to_string(&record).unwrap_or_default();
        if seen.lines().count() >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let real = std::fs::canonicalize(&dir).unwrap();
    assert_eq!(
        seen.lines().collect::<Vec<_>>(),
        [
            real.to_str().unwrap().to_string(),
            real.join("Makefile").to_str().unwrap().to_string()
        ]
    );

    // No Zed: a folder is a clear error naming the editor.
    let out = sopen(tmp.path(), &[dir.to_str().unwrap()], &[("SOPEN_ZED", "")]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("Zed was not found"),
        "{}",
        text(&out.stderr)
    );
    // No Zed: local text runs $VISUAL in place.
    let out = sopen(
        tmp.path(),
        &[dir.join("Makefile").to_str().unwrap()],
        &[("SOPEN_ZED", ""), ("VISUAL", "true")],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
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

/// Files under `dir`, recursively.
fn files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
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
    let backups = files(&tmp.path().join("cache/backup"));
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
    let kept: Vec<_> = files(&tmp.path().join("cache/edits"))
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
// Where it opens (spec §4a). `ssh` is a stand-in that answers `ssh -G` and records the call that
// runs sopen on the target.

/// A target name no test machine has.
const HOST: &str = "sopen-test-target";

/// What the fake `ssh -G` reports; nothing binds it. Short, so no temp dir is too long for it.
const CONTROL_PATH: &str = "/s/cm-test";

struct FakeSsh {
    dir: std::path::PathBuf,
    path: String,
}

impl FakeSsh {
    fn new(home: &Path) -> Self {
        let dir = home.join("fake-ssh");
        std::fs::create_dir_all(&dir).unwrap();
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = -G ]; then printf 'controlpath {CONTROL_PATH}\\nconnecttimeout none\\n'; exit 0; fi\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{dir}/argv'\n\
             exit \"${{FAKE_SSH_EXIT:-0}}\"\n",
            dir = dir.display()
        );
        let ssh = dir.join("ssh");
        std::fs::write(&ssh, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Self { dir, path }
    }

    /// The arguments of the call that ran sopen on the target (`None`: ssh was not run).
    fn argv(&self) -> Option<Vec<String>> {
        let text = std::fs::read_to_string(self.dir.join("argv")).ok()?;
        Some(text.lines().map(str::to_string).collect())
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

#[test]
fn target_runs_sopen_there_over_ssh_and_keeps_its_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let source = format!("{HOST}:/tmp/a b.png");
    let out = sopen(
        tmp.path(),
        &["--target", HOST, &source],
        &[("PATH", &ssh.path), ("FAKE_SSH_EXIT", "3")],
    );
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let argv = ssh.argv().expect("ssh ran");
    let mut want = ssh.connection();
    // Not a terminal here (stdin is not one), so no terminal there.
    want.extend(["-T", HOST, "--"].map(String::from));
    assert_eq!(argv[..want.len()], want[..], "{argv:#?}");
    let words = &argv[want.len()..];
    assert_eq!(words[..2], [q("sh"), q("-c")], "{words:#?}");
    // The file is on the target itself: its path there.
    assert_eq!(
        words[3..],
        [q("sopen"), q("--"), q("/tmp/a b.png")],
        "{words:#?}"
    );
    assert!(words[2].contains("--here"), "{}", words[2]);

    // A local file is fetched by the target from this machine.
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
        &[("PATH", &ssh.path), ("SOPEN_LOG", "debug")],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let argv = ssh.argv().unwrap();
    let short = sopen::forward::short_hostname().expect("this machine has a host name");
    let real = std::fs::canonicalize(&blob).unwrap();
    assert_eq!(
        argv[argv.len() - 4..],
        [
            q("--app"),
            q("Hex Fiend"),
            q("--"),
            q(&format!("{short}:{}", real.display()))
        ],
        "{argv:#?}"
    );
    // Receipt hooks travel; settings naming local paths stay.
    let at = argv.iter().position(|a| a == "--").unwrap();
    assert_eq!(
        argv[at + 1..at + 4],
        [q("env"), q("SOPEN_LOG=debug"), q("sh")]
    );
    assert!(
        !argv.iter().any(|a| a.contains("SOPEN_CACHE_DIR")),
        "{argv:#?}"
    );

    // A missing local file is the same error as a local open, and nothing is run.
    std::fs::remove_file(ssh.dir.join("argv")).unwrap();
    let missing = tmp.path().join("nope.txt");
    let out = sopen(
        tmp.path(),
        &["--target", HOST, missing.to_str().unwrap()],
        &[("PATH", &ssh.path)],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("no such file"),
        "{}",
        text(&out.stderr)
    );
    assert!(ssh.argv().is_none());

    let log = std::fs::read_to_string(tmp.path().join("state/usage.jsonl")).unwrap();
    let first: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(first["forwarded"], true);
    assert_eq!(first["ext"], "png");
    assert_eq!(first["ok"], false, "exit 3 is not ok");
    assert!(!log.contains(HOST), "{log}");
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
    // Not installed there: the target's own 127 and message come through.
    let out = sopen(
        tmp.path(),
        &["--target", HOST, "other:/x.png"],
        &[("PATH", &ssh.path), ("FAKE_SSH_EXIT", "127")],
    );
    assert_eq!(out.status.code(), Some(127));
    // A HOST that would read as an ssh option is refused before ssh runs.
    std::fs::remove_file(ssh.dir.join("argv")).unwrap();
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

#[test]
fn default_target_from_config_or_env_and_here_beats_both() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = FakeSsh::new(tmp.path());
    let path = [("PATH", ssh.path.as_str())];
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
    assert!(ssh.argv().unwrap().contains(&HOST.to_string()));

    // SOPEN_TARGET comes before the config.
    let env = [("PATH", ssh.path.as_str()), ("SOPEN_TARGET", "env-target")];
    let out = sopen(tmp.path(), &["x:/a.mp3"], &env);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let argv = ssh.argv().unwrap();
    assert!(argv.contains(&"env-target".to_string()), "{argv:#?}");
    assert!(!argv.contains(&HOST.to_string()), "{argv:#?}");

    // --here beats both: opened here, no ssh.
    std::fs::remove_file(ssh.dir.join("argv")).unwrap();
    let out = sopen(tmp.path(), &["--here", blob.to_str().unwrap()], &env);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let real = std::fs::canonicalize(&blob).unwrap();
    assert_eq!(text(&out.stdout).trim(), real.to_str().unwrap());
    assert!(ssh.argv().is_none(), "ssh ran for --here");
    let log = std::fs::read_to_string(tmp.path().join("state/usage.jsonl")).unwrap();
    let last: serde_json::Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!(last["forwarded"], false);

    // Unset: back to this machine.
    let out = sopen(tmp.path(), &["config", "target", "--unset"], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "");
    let shown = sopen(tmp.path(), &["config"], &path);
    assert!(text(&shown.stdout).ends_with("target is not set\n"));
    let out = sopen(tmp.path(), &[blob.to_str().unwrap()], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(ssh.argv().is_none(), "ssh ran with no target");
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

    // Not consulted, so not in the way: --here, and cut.
    let out = sopen(tmp.path(), &["--here", blob.to_str().unwrap()], &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = sopen(tmp.path(), &["cut", blob.to_str().unwrap(), "3", "1"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        !text(&out.stderr).contains("config.toml"),
        "{}",
        text(&out.stderr)
    );

    let bad = sopen(tmp.path(), &["config", "target", "a b"], &[]);
    assert_eq!(bad.status.code(), Some(1));
}
