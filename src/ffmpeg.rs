//! ffmpeg/ffprobe as external programs (AGENTS.md: never link libav*). Resolution:
//! `SOPEN_FFMPEG` / `SOPEN_FFPROBE`, then PATH. Exports re-encode so cuts are exact (spec §7):
//! H.264 + AAC for video; for audio the source's codec (from ffprobe), in the source's container
//! when it can hold that codec.

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
    /// `(width, height)` of the first video stream that is not an attached picture (cover art),
    /// as coded.
    pub video: Option<(u32, u32)>,
    /// That stream's display rotation (0, 90, 180 or 270 degrees, from the display matrix or
    /// the legacy `rotate` tag).
    pub rotation: u32,
    /// `video` as shown: width and height swapped for 90/270. ffmpeg rotates frames before
    /// filters run, so crop rectangles are in these pixels.
    pub display: Option<(u32, u32)>,
    /// Frame rate of that stream (`avg_frame_rate`, else `r_frame_rate`), when ffprobe knows it.
    pub fps: Option<f64>,
    /// That stream's index among the video streams (`-map 0:v:N`; cover art counts).
    pub video_index: usize,
    /// That stream's pixel format and bits per sample, when ffprobe reports them.
    pub pix_fmt: Option<String>,
    pub bits: Option<u32>,
    /// That stream's colour tags as `(ffmpeg output option, value)`: primaries, transfer,
    /// matrix, range — only the ones the file sets.
    pub color: Vec<(String, String)>,
    pub has_audio: bool,
    /// ffprobe `codec_name` of the first audio stream (`aac`, `opus`, `pcm_s24le`, …).
    pub audio_codec: Option<String>,
    pub audio_streams: usize,
    /// Streams no export keeps, one entry per stream: `subtitle`, `data`, `attachment`,
    /// `cover art`, `video` (a second picture stream).
    pub dropped: Vec<String>,
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
    // `30000/1001`; `0/0` when unknown.
    let rate = |x: &serde_json::Value| -> Option<f64> {
        let (a, b) = x.as_str()?.split_once('/')?;
        let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
        (a > 0.0 && b > 0.0).then(|| a / b)
    };
    let mut video = None;
    let mut rotation = 0;
    let mut fps = None;
    let mut audio_codec = None;
    let mut has_audio = false;
    let (mut video_index, mut videos_seen) = (0, 0);
    let (mut pix_fmt, mut bits, mut color) = (None, None, Vec::new());
    let mut audio_streams = 0;
    let mut dropped = Vec::new();
    let text = |x: &serde_json::Value| x.as_str().map(str::to_string);
    for s in v["streams"].as_array().into_iter().flatten() {
        let kind = s["codec_type"].as_str();
        if kind == Some("video") {
            videos_seen += 1;
        }
        match kind {
            Some("video") if s["disposition"]["attached_pic"].as_i64() == Some(1) => {
                dropped.push("cover art".to_string());
            }
            Some("video") if video.is_some() => dropped.push("video".to_string()),
            // The first video stream that is not cover art.
            Some("video") => {
                let w = s["width"].as_u64().unwrap_or(0) as u32;
                let h = s["height"].as_u64().unwrap_or(0) as u32;
                video = Some((w, h));
                video_index = videos_seen - 1;
                pix_fmt = text(&s["pix_fmt"]);
                bits = num(&s["bits_per_raw_sample"]).map(|b| b as u32);
                for (key, option) in [
                    ("color_primaries", "-color_primaries"),
                    ("color_transfer", "-color_trc"),
                    ("color_space", "-colorspace"),
                    ("color_range", "-color_range"),
                ] {
                    if let Some(value) = text(&s[key]).filter(|v| {
                        !matches!(v.as_str(), "unknown" | "unspecified" | "reserved" | "")
                    }) {
                        color.push((option.to_string(), value));
                    }
                }
                fps = rate(&s["avg_frame_rate"]).or_else(|| rate(&s["r_frame_rate"]));
                let matrix = s["side_data_list"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find_map(|d| d["rotation"].as_f64());
                rotation = quarter_turns(matrix.or_else(|| num(&s["tags"]["rotate"])));
            }
            Some("audio") => {
                if !has_audio {
                    audio_codec = s["codec_name"].as_str().map(str::to_string);
                }
                has_audio = true;
                audio_streams += 1;
            }
            Some(other @ ("subtitle" | "data" | "attachment")) => dropped.push(other.to_string()),
            _ => {}
        }
    }
    let display = video.map(|(w, h)| if rotation % 180 == 90 { (h, w) } else { (w, h) });
    Ok(Probe {
        duration,
        video,
        rotation,
        display,
        fps,
        video_index,
        pix_fmt,
        bits,
        color,
        has_audio,
        audio_codec,
        audio_streams,
        dropped,
    })
}

/// More than 8 bits per sample: from `bits_per_raw_sample`, else the pixel format's name.
pub fn high_depth(probe: &Probe) -> bool {
    if let Some(bits) = probe.bits {
        return bits > 8;
    }
    let Some(fmt) = probe.pix_fmt.as_deref() else {
        return false;
    };
    const MARKERS: [&str; 22] = [
        "p9", "p10", "p12", "p14", "p16", "p010", "p012", "p016", "p210", "p216", "p410", "p416",
        "gray9", "gray10", "gray12", "gray14", "gray16", "rgb48", "bgr48", "rgba64", "bgra64",
        "f32",
    ];
    MARKERS.iter().any(|m| fmt.contains(m))
        || [
            "y210", "y212", "xv30", "xv36", "x2rgb10", "x2bgr10", "f16", "ya16",
        ]
        .iter()
        .any(|m| fmt.contains(m))
}

/// The pixel format carries an alpha channel (`yuva*`, `rgba`, `argb`, `bgra`, `abgr`,
/// `gbrap*`, `ya*`).
pub fn has_alpha(pix_fmt: &str) -> bool {
    pix_fmt.starts_with("yuva")
        || pix_fmt.starts_with("ya")
        || pix_fmt.starts_with("gbrap")
        || ["rgba", "argb", "bgra", "abgr"]
            .iter()
            .any(|a| pix_fmt.contains(a))
}

/// Pixel formats from `ffmpeg -h encoder=<name>` ("Supported pixel formats: …").
pub fn parse_pix_fmts(help: &str) -> HashSet<String> {
    help.lines()
        .find_map(|l| l.trim().strip_prefix("Supported pixel formats:"))
        .map(|list| list.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

/// The pixel formats this ffmpeg's `encoder` accepts; asked once per ffmpeg and encoder.
pub fn encoder_pix_fmts(tools: &Tools, encoder: &str) -> Result<HashSet<String>> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Cache = Mutex<HashMap<(PathBuf, String), HashSet<String>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let key = (tools.ffmpeg.clone(), encoder.to_string());
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(hit) = cache.lock().unwrap().get(&key) {
        return Ok(hit.clone());
    }
    let out = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-h"])
        .arg(format!("encoder={encoder}"))
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run {}", tools.ffmpeg.display()))?;
    let formats = parse_pix_fmts(&String::from_utf8_lossy(&out.stdout));
    cache.lock().unwrap().insert(key, formats.clone());
    Ok(formats)
}

/// How one export maps and encodes the source, and what it does not keep (spec §7: never lose
/// data silently — the window shows `notes` before Export, the result line repeats them).
#[derive(Debug, Clone, PartialEq)]
pub struct ExportPlan {
    /// The source's first audio codec (audio exports re-create it).
    pub audio_codec: Option<String>,
    /// `-map 0:v:N` for video exports.
    pub video_index: usize,
    /// Output pixel format for video exports.
    pub pix_fmt: &'static str,
    /// Colour tags carried over, as `(option, value)`.
    pub color: Vec<(String, String)>,
    pub notes: Vec<String>,
}

/// Containers that hold one audio stream.
fn single_audio(ext: &str) -> bool {
    matches!(ext, "mp3" | "wav" | "flac" | "aac")
}

fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

impl ExportPlan {
    /// First video stream, 8-bit 4:2:0, no colour tags, nothing to report.
    pub fn basic(audio_codec: Option<&str>) -> Self {
        Self {
            audio_codec: audio_codec.map(str::to_string),
            video_index: 0,
            pix_fmt: "yuv420p",
            color: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// The plan for exporting `probe`'s file as `kind` into a `.dst_ext` file. `x264_formats`
    /// = what this ffmpeg's libx264 accepts (only needed for a source above 8 bits).
    pub fn new(
        probe: &Probe,
        kind: MediaKind,
        dst_ext: &str,
        x264_formats: Option<&HashSet<String>>,
    ) -> Self {
        let mut plan = Self::basic(probe.audio_codec.as_deref());
        plan.video_index = probe.video_index;
        let mut dropped: Vec<(String, usize)> = Vec::new();
        for kind_name in &probe.dropped {
            match dropped.iter_mut().find(|(k, _)| k == kind_name) {
                Some((_, n)) => *n += 1,
                None => dropped.push((kind_name.clone(), 1)),
            }
        }
        if kind == MediaKind::Audio && probe.video.is_some() {
            dropped.push(("video".to_string(), 1));
        }
        let mut lost: Vec<String> = dropped
            .iter()
            .map(|(k, n)| match k.as_str() {
                "cover art" => "cover art".to_string(),
                other => count(*n, &format!("{other} stream")),
            })
            .collect();
        if single_audio(dst_ext) && probe.audio_streams > 1 {
            lost.push(format!(
                "{} (.{dst_ext} holds one)",
                count(probe.audio_streams - 1, "more audio stream")
            ));
        }
        if !lost.is_empty() {
            plan.notes.push(format!("not kept: {}", lost.join(", ")));
        }
        if kind == MediaKind::Video {
            plan.color = probe.color.clone();
            if high_depth(probe) {
                if x264_formats.is_some_and(|f| f.contains("yuv420p10le")) {
                    plan.pix_fmt = "yuv420p10le";
                } else {
                    plan.notes.push(
                        "exported as 8-bit (this ffmpeg's libx264 has no 10-bit)".to_string(),
                    );
                }
            }
            if probe.pix_fmt.as_deref().is_some_and(has_alpha) {
                plan.notes
                    .push("alpha is dropped (H.264 has no alpha)".to_string());
            }
        }
        plan
    }
}

/// Degrees (either sign, any multiple) → 0, 90, 180 or 270.
fn quarter_turns(degrees: Option<f64>) -> u32 {
    let d = degrees.unwrap_or(0.0).round() as i64;
    let nearest = (d.rem_euclid(360) + 45) / 90 * 90;
    (nearest % 360) as u32
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

/// Encoder arguments that re-create an audio codec (spec §7), by ffprobe `codec_name`.
/// PCM keeps its exact sample format, so 24-bit and float WAV keep their depth.
fn encoder_for_codec(codec: &str) -> Option<Vec<String>> {
    let args: &[&str] = match codec {
        "aac" => &["-c:a", "aac", "-b:a", "192k"],
        "opus" => &["-c:a", "libopus"],
        "vorbis" => &["-c:a", "libvorbis"],
        "mp3" => &["-c:a", "libmp3lame", "-q:a", "2"],
        "flac" => &["-c:a", "flac"],
        "alac" => &["-c:a", "alac"],
        pcm if pcm.starts_with("pcm_") => return Some(vec!["-c:a".into(), pcm.into()]),
        _ => return None,
    };
    Some(args.iter().map(|a| a.to_string()).collect())
}

/// Audio encoder by container, for a source codec that is unknown or has no entry above.
fn encoder_for_container(ext: &str) -> Option<&'static [&'static str]> {
    Some(match ext {
        "mp3" | "avi" => &["-c:a", "libmp3lame", "-q:a", "2"],
        "wav" => &["-c:a", "pcm_s16le"],
        "flac" | "mkv" => &["-c:a", "flac"],
        "m4a" | "aac" | "mp4" | "m4v" | "mov" | "ts" => &["-c:a", "aac", "-b:a", "192k"],
        "ogg" => &["-c:a", "libvorbis"],
        "opus" | "webm" => &["-c:a", "libopus"],
        _ => return None,
    })
}

fn is_little_endian_pcm(codec: &str) -> bool {
    codec.starts_with("pcm_") && (codec.ends_with("le") || codec == "pcm_u8")
}

/// Whether ffmpeg's muxer for container `ext` carries audio codec `codec` (conservatively).
fn container_holds(ext: &str, codec: &str) -> bool {
    let pcm = codec.starts_with("pcm_");
    match ext {
        "mp4" | "m4a" | "m4v" => matches!(codec, "aac" | "mp3" | "alac"),
        "mov" => pcm || matches!(codec, "aac" | "mp3" | "alac"),
        "mkv" => true,
        "webm" => matches!(codec, "opus" | "vorbis"),
        "ogg" => matches!(codec, "vorbis" | "opus" | "flac"),
        "opus" => codec == "opus",
        "mp3" => codec == "mp3",
        "flac" => codec == "flac",
        "aac" => codec == "aac",
        "wav" => is_little_endian_pcm(codec),
        "avi" => pcm || codec == "mp3",
        "ts" => matches!(codec, "aac" | "mp3" | "opus"),
        _ => false,
    }
}

/// The audio-only container a codec goes to when the source's cannot carry it.
fn home_container(codec: &str) -> &'static str {
    match codec {
        "aac" | "alac" => "m4a",
        "opus" => "opus",
        "vorbis" => "ogg",
        "mp3" => "mp3",
        "flac" => "flac",
        c if is_little_endian_pcm(c) => "wav",
        _ => "mkv",
    }
}

/// Container of the export. Video: H.264/AAC cannot go into WebM, so a WebM source exports as
/// MP4. Audio (`audio_codec` = the source's, from ffprobe): the source container when it can
/// carry that codec, else the codec's own (an AAC-only `.mp4` stays `.mp4`, Opus in `.mkv`
/// stays, Opus elsewhere → `.opus`).
pub fn output_ext(src_ext: &str, kind: MediaKind, audio_codec: Option<&str>) -> String {
    let ext = src_ext.to_ascii_lowercase();
    match kind {
        MediaKind::Video if ext == "webm" => "mp4".to_string(),
        MediaKind::Video => ext,
        MediaKind::Audio => match audio_codec.filter(|c| encoder_for_codec(c).is_some()) {
            Some(c) if container_holds(&ext, c) => ext,
            Some(c) => home_container(c).to_string(),
            None if encoder_for_container(&ext).is_some() => ext,
            None => "m4a".to_string(),
        },
    }
}

/// How far before IN a transport stream is seeked on input; the rest is decoded and trimmed.
const TS_PRESEEK_S: f64 = 10.0;

fn secs_arg(s: f64) -> OsString {
    OsString::from(format!("{s:.3}"))
}

/// The ffmpeg arguments for one export (everything after the global flags). Pure. Streams are
/// mapped explicitly: video = the plan's video stream, audio = every audio stream (the first
/// only, for containers that hold one), each re-encoded. `plan.audio_codec` is the source's
/// audio codec (ffprobe `codec_name`); audio exports re-create it, falling back to the
/// destination container's usual codec when it is unknown.
pub fn export_args(
    src: &Path,
    dst: &Path,
    in_s: f64,
    out_s: f64,
    crop: Option<Rect>,
    kind: MediaKind,
    plan: &ExportPlan,
) -> Result<Vec<OsString>> {
    let audio_codec = plan.audio_codec.as_deref();
    if !(in_s >= 0.0 && out_s > in_s && out_s.is_finite()) {
        bail!("cut range {in_s}..{out_s} is empty or negative");
    }
    let ext = dst
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let src_ext = src
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut args: Vec<OsString> = if src_ext == "ts" {
        // MPEG-TS has no index: a seek before `-i` lands mid-GOP and loses every frame up to the
        // next keyframe (all of them, when there is none). Seek early, then trim after decoding.
        let coarse = (in_s - TS_PRESEEK_S).max(0.0);
        let mut args: Vec<OsString> = Vec::new();
        if coarse > 0.0 {
            args.extend(["-ss".into(), secs_arg(coarse)]);
        }
        args.extend([
            "-i".into(),
            src.as_os_str().to_owned(),
            "-ss".into(),
            secs_arg(in_s - coarse),
            "-to".into(),
            secs_arg(out_s - coarse),
        ]);
        args
    } else {
        vec![
            "-ss".into(),
            secs_arg(in_s),
            "-to".into(),
            secs_arg(out_s),
            "-i".into(),
            src.as_os_str().to_owned(),
        ]
    };
    let push = |args: &mut Vec<OsString>, list: &[&str]| {
        args.extend(list.iter().map(OsString::from));
    };
    match kind {
        MediaKind::Video => {
            let video_map = format!("0:v:{}", plan.video_index);
            let audio_map = if single_audio(&ext) { "0:a:0?" } else { "0:a?" };
            push(&mut args, &["-map", &video_map, "-map", audio_map]);
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
                    "-c:v",
                    "libx264",
                    "-preset",
                    "veryfast",
                    "-crf",
                    "18",
                    "-pix_fmt",
                    plan.pix_fmt,
                ],
            );
            for (option, value) in &plan.color {
                push(&mut args, &[option, value]);
            }
            push(&mut args, &["-c:a", "aac", "-b:a", "192k"]);
            // A muxer rejects options it does not know; +faststart is mov/mp4 only.
            if matches!(ext.as_str(), "mp4" | "mov" | "m4v") {
                push(&mut args, &["-movflags", "+faststart"]);
            }
        }
        MediaKind::Audio => {
            if crop.is_some() {
                bail!("--crop applies to video only");
            }
            // Cover art is not part of a cut, and not every audio container can hold it.
            let audio_map = if single_audio(&ext) { "0:a:0" } else { "0:a" };
            push(&mut args, &["-map", audio_map, "-vn"]);
            match audio_codec.and_then(encoder_for_codec) {
                Some(codec) => args.extend(codec.into_iter().map(OsString::from)),
                None => match encoder_for_container(&ext) {
                    Some(codec) => push(&mut args, codec),
                    None => bail!("no audio export for .{ext} files"),
                },
            }
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
