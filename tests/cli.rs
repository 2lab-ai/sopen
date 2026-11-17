//! The `sopen` binary on local targets: no ssh, no network. Cache and state go to a temp dir.

use std::path::Path;
use std::process::{Command, Output};

fn sopen(home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sopen"));
    cmd.args(args)
        .env("SOPEN_CACHE_DIR", home.join("cache"))
        .env("SOPEN_STATE_DIR", home.join("state"))
        .env_remove("SOPEN_LOG");
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
    let mut child = Command::new(env!("CARGO_BIN_EXE_sopen"))
        .arg(&png)
        .env("SOPEN_CACHE_DIR", tmp.path().join("cache"))
        .env("SOPEN_STATE_DIR", tmp.path().join("state"))
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
