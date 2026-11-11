//! ffmpeg/ffprobe as external programs (AGENTS.md: never link libav*). Resolution:
//! `SOPEN_FFMPEG` / `SOPEN_FFPROBE`, then PATH. Exports re-encode so cuts are exact (spec §7):
//! H.264 + AAC for video, the source codec family for audio.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

fn resolve_one(var: &str, name: &str) -> Result<PathBuf> {
    match std::env::var_os(var) {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => find_on_path(name)
            .ok_or_else(|| anyhow!("{name} not found on PATH (install ffmpeg, or set {var})")),
    }
}

pub fn resolve() -> Result<Tools> {
    Ok(Tools {
        ffmpeg: resolve_one("SOPEN_FFMPEG", "ffmpeg")?,
        ffprobe: resolve_one("SOPEN_FFPROBE", "ffprobe")?,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub duration: Option<f64>,
    /// `(width, height)` of the first video stream that is not an attached picture (cover art).
    pub video: Option<(u32, u32)>,
    pub has_audio: bool,
}

/// `ffprobe -v error -print_format json -show_format -show_streams <path>`.
pub fn probe(tools: &Tools, path: &Path) -> Result<Probe> {
    let out = Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run {}", tools.ffprobe.display()))?;
    if !out.status.success() {
        bail!(
            "ffprobe cannot read {}: {}",
            path.display(),
            last_line(&String::from_utf8_lossy(&out.stderr))
        );
    }
    parse_probe(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_probe(json: &str) -> Result<Probe> {
    let v: serde_json::Value = serde_json::from_str(json).context("ffprobe printed bad JSON")?;
    let num = |x: &serde_json::Value| -> Option<f64> {
        x.as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| x.as_f64())
    };
    let duration = num(&v["format"]["duration"]);
    let mut video = None;
    let mut has_audio = false;
    for s in v["streams"].as_array().into_iter().flatten() {
        match s["codec_type"].as_str() {
            Some("video") if s["disposition"]["attached_pic"].as_i64() != Some(1) => {
                if video.is_none() {
                    let w = s["width"].as_u64().unwrap_or(0) as u32;
                    let h = s["height"].as_u64().unwrap_or(0) as u32;
                    video = Some((w, h));
                }
            }
            Some("audio") => has_audio = true,
            _ => {}
        }
    }
    Ok(Probe {
        duration,
        video,
        has_audio,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl FromStr for Rect {
    type Err = anyhow::Error;
    /// `x,y,w,h` in source pixels.
    fn from_str(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split(',').map(str::trim).collect();
        let [x, y, w, h] = parts.as_slice() else {
            bail!("crop must be x,y,w,h (got '{s}')");
        };
        let n = |v: &str| -> Result<u32> {
            v.parse()
                .map_err(|_| anyhow!("crop must be x,y,w,h in whole pixels (got '{s}')"))
        };
        Ok(Rect {
            x: n(x)?,
            y: n(y)?,
            w: n(w)?,
            h: n(h)?,
        })
    }
}

/// Width and height rounded down to even (yuv420p needs it); errors when the rectangle leaves
/// the frame or rounds to nothing.
pub fn fit_crop(r: Rect, width: u32, height: u32) -> Result<Rect> {
    let even = Rect {
        w: r.w & !1,
        h: r.h & !1,
        ..r
    };
    if even.w == 0 || even.h == 0 {
        bail!("crop {}x{} is too small", r.w, r.h);
    }
    if u64::from(r.x) + u64::from(even.w) > u64::from(width)
        || u64::from(r.y) + u64::from(even.h) > u64::from(height)
    {
        bail!(
            "crop {},{} {}x{} is outside the {width}x{height} frame",
            r.x,
            r.y,
            r.w,
            r.h
        );
    }
    Ok(even)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Audio,
}

/// Container of the export. H.264/AAC cannot go into WebM, so a WebM source exports as MP4.
pub fn output_ext(src_ext: &str, kind: MediaKind) -> String {
    let ext = src_ext.to_ascii_lowercase();
    match (kind, ext.as_str()) {
        (MediaKind::Video, "webm") => "mp4".to_string(),
        _ => ext,
    }
}

fn secs_arg(s: f64) -> OsString {
    OsString::from(format!("{s:.3}"))
}

/// The ffmpeg arguments for one export (everything after the global flags). Pure.
pub fn export_args(
    src: &Path,
    dst: &Path,
    in_s: f64,
    out_s: f64,
    crop: Option<Rect>,
    kind: MediaKind,
) -> Result<Vec<OsString>> {
    if !(in_s >= 0.0 && out_s > in_s && out_s.is_finite()) {
        bail!("cut range {in_s}..{out_s} is empty or negative");
    }
    let ext = dst
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut args: Vec<OsString> = vec![
        "-ss".into(),
        secs_arg(in_s),
        "-to".into(),
        secs_arg(out_s),
        "-i".into(),
        src.as_os_str().to_owned(),
    ];
    let push = |args: &mut Vec<OsString>, list: &[&str]| {
        args.extend(list.iter().map(OsString::from));
    };
    match kind {
        MediaKind::Video => {
            if let Some(r) = crop {
                let (w, h) = (r.w & !1, r.h & !1);
                push(
                    &mut args,
                    &["-vf", &format!("crop={w}:{h}:{}:{}", r.x, r.y)],
                );
            }
            push(
                &mut args,
                &[
                    "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p",
                    "-c:a", "aac", "-b:a", "192k",
                ],
            );
            // A muxer rejects options it does not know; +faststart is mov/mp4 only.
            if matches!(ext.as_str(), "mp4" | "mov" | "m4v") {
                push(&mut args, &["-movflags", "+faststart"]);
            }
        }
        MediaKind::Audio => {
            if crop.is_some() {
                bail!("--crop applies to video only");
            }
            // -vn: cover art is not part of a cut, and not every audio container can hold it.
            push(&mut args, &["-vn"]);
            let codec: &[&str] = match ext.as_str() {
                "mp3" => &["-c:a", "libmp3lame", "-q:a", "2"],
                "wav" => &["-c:a", "pcm_s16le"],
                "flac" => &["-c:a", "flac"],
                "m4a" | "aac" => &["-c:a", "aac", "-b:a", "192k"],
                "ogg" => &["-c:a", "libvorbis"],
                "opus" => &["-c:a", "libopus"],
                other => bail!("no audio export for .{other} files"),
            };
            push(&mut args, codec);
        }
    }
    args.push(dst.as_os_str().to_owned());
    Ok(args)
}

/// Encoder names (`libx264`, `aac`, …) from `ffmpeg -hide_banner -encoders`.
pub fn parse_encoders(listing: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut in_table = false;
    for line in listing.lines() {
        if line.trim_start().starts_with("------") {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        let mut cols = line.split_whitespace();
        if let (Some(_flags), Some(name)) = (cols.next(), cols.next()) {
            names.insert(name.to_string());
        }
    }
    names
}

pub fn available_encoders(tools: &Tools) -> Result<HashSet<String>> {
    let out = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run {}", tools.ffmpeg.display()))?;
    if !out.status.success() {
        bail!("{} -encoders failed", tools.ffmpeg.display());
    }
    Ok(parse_encoders(&String::from_utf8_lossy(&out.stdout)))
}

/// Checks every `-c:v` / `-c:a` encoder in `args` against this ffmpeg. One substitution: builds
/// without libvorbis (Homebrew's) use ffmpeg's own `vorbis` encoder, which needs `-strict -2`.
pub fn adapt_to_encoders(
    mut args: Vec<OsString>,
    available: &HashSet<String>,
) -> Result<Vec<OsString>> {
    let mut i = 0;
    while i + 1 < args.len() {
        if args[i] == "-c:v" || args[i] == "-c:a" {
            let name = args[i + 1].to_string_lossy().into_owned();
            if !available.contains(&name) {
                if name == "libvorbis" && available.contains("vorbis") {
                    args[i + 1] = "vorbis".into();
                    args.splice(i + 2..i + 2, ["-strict".into(), "-2".into()]);
                } else {
                    bail!("this ffmpeg has no '{name}' encoder; install an ffmpeg built with it");
                }
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    Ok(args)
}

fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no error output")
        .trim()
        .to_string()
}

/// Runs `ffmpeg <globals> <args>` with `-progress pipe:1`; `progress` gets 0.0..=1.0 of
/// `length_s`. On failure the last line ffmpeg printed is the error.
pub fn run_export(
    tools: &Tools,
    args: &[OsString],
    length_s: f64,
    progress: &mut dyn FnMut(f64),
) -> Result<()> {
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args([
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-loglevel",
        "error",
        "-y",
        "-progress",
        "pipe:1",
    ])
    .args(args)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    crate::debug!(
        "{}",
        crate::render_command(
            cmd.get_program(),
            &cmd.get_args().map(|a| a.to_os_string()).collect::<Vec<_>>()
        )
    );
    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot run {}", tools.ffmpeg.display()))?;
    let mut stderr = child.stderr.take().expect("piped");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let stdout = child.stdout.take().expect("piped");
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if let Some(us) = line.strip_prefix("out_time_us=") {
            if let Ok(us) = us.trim().parse::<f64>() {
                if length_s > 0.0 {
                    progress((us / 1e6 / length_s).clamp(0.0, 1.0));
                }
            }
        } else if line == "progress=end" {
            progress(1.0);
        }
    }
    let status = child.wait()?;
    let errors = errors.join().unwrap_or_default();
    if !status.success() {
        bail!("ffmpeg failed ({status}): {}", last_line(&errors));
    }
    Ok(())
}

/// `SS`, `MM:SS` or `HH:MM:SS`, each with optional `.fraction`. Higher units bound the lower ones
/// (`1:75` is refused); a bare number may be any size (`90` = 1:30).
pub fn parse_time(s: &str) -> Result<f64> {
    let bad = || anyhow!("time '{s}' must be SS, MM:SS or HH:MM:SS(.ms)");
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|p| p.is_empty()) {
        return Err(bad());
    }
    let (last, whole) = parts.split_last().ok_or_else(bad)?;
    if !last.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err(bad());
    }
    let secs: f64 = last.parse().map_err(|_| bad())?;
    let mut total = 0.0;
    for p in whole {
        if !p.chars().all(|c| c.is_ascii_digit()) {
            return Err(bad());
        }
        total = total * 60.0 + p.parse::<f64>().map_err(|_| bad())?;
    }
    if !whole.is_empty() {
        if secs >= 60.0 || (whole.len() == 2 && whole[1].parse::<u32>().unwrap_or(60) >= 60) {
            return Err(bad());
        }
        total *= 60.0;
    }
    Ok(total + secs)
}

/// Seconds as they appear in an export name: `1`, `1.5`, `62.25` (≤ 3 decimals, no `:`).
pub fn name_time(s: f64) -> String {
    let fixed = format!("{s:.3}");
    fixed
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

/// `<stem>.cut-<in>-<out>.<ext>` (spec A4).
pub fn cut_name(stem: &str, in_s: f64, out_s: f64, ext: &str) -> String {
    format!("{stem}.cut-{}-{}.{ext}", name_time(in_s), name_time(out_s))
}

#[cfg(test)]
mod tests;
