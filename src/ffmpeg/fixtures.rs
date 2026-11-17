//! Test fixtures whose availability is decided by ffmpeg/ffprobe's own output — never by the
//! probe code under test, so a probe regression fails a test instead of turning into a skip.

use super::Tools;
use std::path::Path;
use std::process::{Command, Stdio};

/// What ffprobe itself reports as the first video stream's rotation (display matrix
/// `rotation=…` or the legacy `TAG:rotate=…`), in degrees.
pub(crate) fn ffprobe_rotation(tools: &Tools, file: &Path) -> Option<i64> {
    let out = Command::new(&tools.ffprobe)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg("stream_side_data=rotation:stream_tags=rotate")
        .args(["-of", "default=noprint_wrappers=1"])
        .arg(file)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("rotation=") || l.starts_with("TAG:rotate="))
        .find_map(|l| l.split_once('=')?.1.trim().parse::<f64>().ok())
        .map(|d| d.round() as i64)
}

/// `ffmpeg -version` major: `5.1.9-0+deb12u1` → 5, `n6.1.1` → 6; `None` for git builds.
pub(crate) fn ffmpeg_major(tools: &Tools) -> Option<u32> {
    let out = Command::new(&tools.ffmpeg).arg("-version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let version = text.lines().next()?.strip_prefix("ffmpeg version ")?;
    let version = version.strip_prefix('n').unwrap_or(version);
    version.split(['.', '-', ' ']).next()?.parse().ok()
}

/// `plain` re-muxed to display rotated by 90°: the display matrix (`-display_rotation`,
/// ffmpeg ≥ 6), else the legacy `rotate` tag. True when ffprobe itself then reports a quarter
/// turn — the caller must then require the app's probe to agree. False (the caller skips) only
/// where this ffmpeg cannot write one: a CI that requires ffmpeg (`SOPEN_REQUIRE_FFMPEG`) must
/// not skip on ffmpeg 6+, which has `-display_rotation`.
pub(crate) fn rotated_copy(tools: &Tools, plain: &Path, dst: &Path) -> bool {
    let attempts: [(&[&str], &[&str]); 2] = [
        (&["-display_rotation", "90"], &[]),
        (&[], &["-metadata:s:v:0", "rotate=90"]),
    ];
    let made = attempts.into_iter().any(|(input_opts, output_opts)| {
        Command::new(&tools.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
            .args(input_opts)
            .arg("-i")
            .arg(plain)
            .args(["-c", "copy"])
            .args(output_opts)
            .arg(dst)
            .status()
            .is_ok_and(|s| s.success())
            && ffprobe_rotation(tools, dst).is_some_and(|d| d.rem_euclid(180) == 90)
    });
    if !made {
        let major = ffmpeg_major(tools);
        if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() && major.is_some_and(|m| m >= 6) {
            panic!("ffmpeg {major:?} wrote no rotation ffprobe reads into the fixture");
        }
        eprintln!("skipping: this ffmpeg ({major:?}) cannot tag a fixture as rotated");
    }
    made
}
