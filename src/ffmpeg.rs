//! ffmpeg/ffprobe as external programs (AGENTS.md: never link libav*). Resolution:
//! `SOPEN_FFMPEG` / `SOPEN_FFPROBE`, then PATH. Exports re-encode so cuts are exact (spec §7):
//! H.264 + AAC for video; for audio the source's codec (from ffprobe), in the source's container
//! when it can hold that codec.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
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

/// What ffmpeg and ffprobe read for a media window: one value — a local path today, and a URL
/// once remote media is streamed — handed unchanged to every run on the source: the probe,
/// playback, the paused frame, the waveform, Save and Export. Nothing reads the source any
/// other way (Save's backup asks for [`Input::local`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input(OsString);

impl Input {
    pub fn path(path: &Path) -> Self {
        Self(path.as_os_str().to_owned())
    }

    /// As ffmpeg's `-i` (and ffprobe's) argument.
    pub fn arg(&self) -> &OsStr {
        &self.0
    }

    /// A local file's path; `None` for a URL.
    pub fn local(&self) -> Option<&Path> {
        let text = self.0.to_string_lossy();
        (!text.contains("://")).then(|| Path::new(&self.0))
    }

    /// The extension of its last path segment (a URL's query and fragment left out; a local
    /// name may hold `?` and `#`), lowercase.
    pub fn ext(&self) -> String {
        let text = self.0.to_string_lossy();
        let path = if self.local().is_some() {
            &text[..]
        } else {
            text.split(['?', '#']).next().unwrap_or_default()
        };
        let last = path.rsplit('/').next().unwrap_or_default();
        match last.rfind('.') {
            Some(i) if i > 0 => last[i + 1..].to_ascii_lowercase(),
            _ => String::new(),
        }
    }

    pub fn show(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl AsRef<OsStr> for Input {
    fn as_ref(&self) -> &OsStr {
        &self.0
    }
}

/// Starts an ffmpeg or ffprobe run. Every run on a media file goes through here or
/// [`output`], so the tests can count them per file ([`spawns`]): editing must start none.
pub fn spawn(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    #[cfg(test)]
    spawns::note(cmd);
    cmd.spawn()
}

/// [`spawn`], waited for, its output collected.
pub fn output(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    #[cfg(test)]
    spawns::note(cmd);
    cmd.output()
}

/// Test-only: how many ffmpeg / ffprobe runs read each file, by what they were for.
#[cfg(test)]
pub mod spawns {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Counts {
        /// Decodes for the screen: playback, a paused frame, the waveform.
        pub preview: u64,
        /// Runs that write a file (Save, Export, `sopen cut`).
        pub render: u64,
        /// ffprobe.
        pub probe: u64,
    }

    impl Counts {
        pub fn total(&self) -> u64 {
            self.preview + self.render + self.probe
        }
    }

    fn map() -> &'static Mutex<HashMap<PathBuf, Counts>> {
        static MAP: OnceLock<Mutex<HashMap<PathBuf, Counts>>> = OnceLock::new();
        MAP.get_or_init(Mutex::default)
    }

    /// Records `cmd` against every file it reads (`-i FILE`; ffprobe's last argument). Tests
    /// run in parallel, each on files of its own, so the count of one file is one test's.
    pub fn note(cmd: &Command) {
        let args: Vec<_> = cmd.get_args().collect();
        let probe = Path::new(cmd.get_program())
            .file_name()
            .is_some_and(|n| n.to_string_lossy().contains("ffprobe"));
        let render = args.iter().any(|a| *a == "-progress");
        let mut files: Vec<PathBuf> = args
            .windows(2)
            .filter(|w| w[0] == "-i")
            .map(|w| PathBuf::from(w[1]))
            .collect();
        if probe {
            files.extend(args.last().map(PathBuf::from));
        }
        files.sort();
        files.dedup();
        let mut map = map().lock().unwrap();
        for file in files {
            let c = map.entry(file).or_default();
            if probe {
                c.probe += 1;
            } else if render {
                c.render += 1;
            } else {
                c.preview += 1;
            }
        }
    }

    /// The runs that read `file` so far.
    pub fn of(file: &Path) -> Counts {
        map().lock().unwrap().get(file).copied().unwrap_or_default()
    }
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
    /// That stream's own length (its `duration`, or Matroska's `DURATION` tag), when known: the
    /// container's can be longer (a longer sound track).
    pub video_duration: Option<f64>,
    /// Where that stream's first frame is, in seconds from the file's start (what every
    /// timestamp is counted from): 0 for most files; a transport stream or a file whose sound
    /// starts first puts it later.
    pub video_start: Option<f64>,
    /// That stream's pixels' shape, width to height (`sample_aspect_ratio`), when they are not
    /// square: the picture is shown that much wider (or narrower) than its pixels count.
    pub pixel_aspect: Option<(u32, u32)>,
    /// The first audio stream's channel layout as ffprobe names it (`stereo`, `5.1`), if it
    /// does.
    pub channel_layout: Option<String>,
    /// That stream's pixel format and bits per sample, when ffprobe reports them.
    pub pix_fmt: Option<String>,
    pub bits: Option<u32>,
    /// That stream's colour tags as `(ffmpeg output option, value)`: primaries, transfer,
    /// matrix, range — only the ones the file sets.
    pub color: Vec<(String, String)>,
    pub has_audio: bool,
    /// ffprobe `codec_name` of the first audio stream (`aac`, `opus`, `pcm_s24le`, …).
    pub audio_codec: Option<String>,
    /// The first audio stream's channel count and sample rate, when ffprobe reports them.
    pub channels: Option<u32>,
    pub sample_rate: Option<u32>,
    /// Every audio stream's channels and rate, in order.
    pub tracks: Vec<Track>,
    /// Chapters in the file (an edited output does not carry them).
    pub chapters: usize,
    pub audio_streams: usize,
    /// Streams no export keeps, one entry per stream: `subtitle`, `data`, `attachment`,
    /// `cover art`, `video` (a second picture stream).
    pub dropped: Vec<String>,
}

/// `ffprobe -v error -print_format json -show_format -show_streams -show_chapters <input>` (a
/// path or a URL).
pub fn probe(tools: &Tools, input: impl AsRef<OsStr>) -> Result<Probe> {
    let path = Path::new(input.as_ref());
    let mut cmd = probe_command(tools, input.as_ref());
    let out =
        output(&mut cmd).with_context(|| format!("cannot run {}", tools.ffprobe.display()))?;
    if !out.status.success() {
        bail!(
            "{}",
            probe_refusal(path, &last_line(&String::from_utf8_lossy(&out.stderr)))
        );
    }
    parse_probe(&String::from_utf8_lossy(&out.stdout))
}

/// The ffprobe run [`probe`] makes on `input`, not started: a preview runs the same one where it
/// can be stopped ([`crate::gui::media::preview`]). Its stdout is what [`parse_probe`] reads.
pub fn probe_command(tools: &Tools, input: &OsStr) -> Command {
    let mut cmd = Command::new(&tools.ffprobe);
    cmd.args([
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
        "-show_chapters",
    ])
    .arg(input)
    .stdin(Stdio::null());
    cmd
}

/// What is said when ffprobe ended badly on `input` (`said`: its last line).
pub fn probe_refusal(input: &Path, said: &str) -> String {
    // ffprobe names the file in front of its reason: said once here.
    let named = format!("{}: ", input.display());
    let said = said.strip_prefix(&named).unwrap_or(said);
    format!("ffprobe cannot read {}: {said}", input.display())
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
    let (mut channels, mut sample_rate) = (None, None);
    let mut tracks = Vec::new();
    let mut has_audio = false;
    let (mut video_index, mut videos_seen) = (0, 0);
    let mut video_duration = None;
    let mut video_start_abs = None;
    let (mut pix_fmt, mut bits, mut color) = (None, None, Vec::new());
    let (mut pixel_aspect, mut channel_layout) = (None, None);
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
                video_start_abs = num(&s["start_time"]);
                video_duration = num(&s["duration"])
                    .or_else(|| {
                        let tag = s["tags"]["DURATION"].as_str()?;
                        // `00:00:04.000000000`: nanoseconds, more digits than parse_time takes.
                        let (whole, frac) = tag.split_once('.').unwrap_or((tag, "0"));
                        let frac: f64 = format!("0.{frac}").parse().ok()?;
                        Some(parse_time(whole).ok()? + frac)
                    })
                    .filter(|d| *d > 0.0);
                pix_fmt = text(&s["pix_fmt"]);
                pixel_aspect = s["sample_aspect_ratio"].as_str().and_then(|sar| {
                    let (n, d) = sar.split_once(':')?;
                    let (n, d): (u32, u32) = (n.parse().ok()?, d.parse().ok()?);
                    (n > 0 && d > 0 && n != d).then_some((n, d))
                });
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
                let track = Track {
                    channels: s["channels"].as_u64().map(|c| c as u32).filter(|c| *c > 0),
                    rate: num(&s["sample_rate"]).map(|r| r as u32).filter(|r| *r > 0),
                    format: text(&s["sample_fmt"]),
                };
                if !has_audio {
                    channel_layout = text(&s["channel_layout"]);
                    audio_codec = s["codec_name"].as_str().map(str::to_string);
                    (channels, sample_rate) = (track.channels, track.rate);
                }
                tracks.push(track);
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
        video_duration,
        video_start: video_start_abs
            .map(|t| (t - num(&v["format"]["start_time"]).unwrap_or(0.0)).max(0.0)),
        pixel_aspect,
        channel_layout,
        pix_fmt,
        bits,
        color,
        has_audio,
        audio_codec,
        channels,
        sample_rate,
        tracks,
        chapters: v["chapters"].as_array().map_or(0, Vec::len),
        audio_streams,
        dropped,
    })
}

impl Probe {
    /// Where the picture's frames sit within one frame period, from the file's start: frame
    /// slot `k` of the timeline is the frame at `frame_origin + k / fps`. Cutting both picture
    /// and sound there keeps them together when the frames are not on whole periods from the
    /// start (a transport stream whose sound starts first, by AAC's priming).
    pub fn frame_origin(&self) -> f64 {
        let (Some(start), Some(fps)) = (self.video_start, self.fps.filter(|f| *f > 0.0)) else {
            return 0.0;
        };
        let period = 1.0 / fps;
        let origin = start.rem_euclid(period);
        if period - origin < 1e-6 {
            0.0
        } else {
            origin
        }
    }
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

/// The source's bits per sample, when known: `bits_per_raw_sample`, else the number in the
/// pixel format's name (`yuv422p12le` → 12, `rgb48le` → 16 per channel). Float formats → 32.
pub fn source_bits(probe: &Probe) -> Option<u32> {
    if let Some(bits) = probe.bits.filter(|b| *b > 0) {
        return Some(bits);
    }
    let fmt = probe.pix_fmt.as_deref()?;
    const TABLE: [(&str, u32); 28] = [
        ("f32", 32),
        ("f16", 16),
        ("rgb48", 16),
        ("bgr48", 16),
        ("rgba64", 16),
        ("bgra64", 16),
        ("ya16", 16),
        ("p16", 16),
        ("p016", 16),
        ("p216", 16),
        ("p416", 16),
        ("gray16", 16),
        ("p14", 14),
        ("gray14", 14),
        ("p12", 12),
        ("p012", 12),
        ("gray12", 12),
        ("y212", 12),
        ("xv36", 12),
        ("p10", 10),
        ("p010", 10),
        ("p210", 10),
        ("p410", 10),
        ("gray10", 10),
        ("y210", 10),
        ("xv30", 10),
        ("p9", 9),
        ("gray9", 9),
    ];
    TABLE
        .iter()
        .find(|(marker, _)| fmt.contains(marker))
        .map(|(_, bits)| *bits)
        .or_else(|| (fmt.contains("x2rgb10") || fmt.contains("x2bgr10")).then_some(10))
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
    let out = output(&mut encoder_help_command(tools, encoder))
        .with_context(|| format!("cannot run {}", tools.ffmpeg.display()))?;
    let formats = parse_pix_fmts(&String::from_utf8_lossy(&out.stdout));
    cache.lock().unwrap().insert(key, formats.clone());
    Ok(formats)
}

/// The ffmpeg run [`encoder_pix_fmts`] makes (`ffmpeg -h encoder=<encoder>`), not started: a
/// preview runs the same one where it can be stopped. Its stdout is what [`parse_pix_fmts`]
/// reads.
pub fn encoder_help_command(tools: &Tools, encoder: &str) -> Command {
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args(["-hide_banner", "-h"])
        .arg(format!("encoder={encoder}"))
        .stdin(Stdio::null());
    cmd
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
pub fn single_audio(ext: &str) -> bool {
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
                let ten = x264_formats.is_some_and(|f| f.contains("yuv420p10le"));
                if ten {
                    plan.pix_fmt = "yuv420p10le";
                }
                let out_bits = if ten { 10 } else { 8 };
                // Fewer bits out than in: say so (12/16-bit → 10-bit, anything deeper → 8-bit).
                let src = source_bits(probe);
                if src.is_none_or(|b| b > out_bits) {
                    let source = match src {
                        Some(32) => "source is floating point".to_string(),
                        Some(b) => format!("source is {b}-bit"),
                        None => "source has more than 8 bits".to_string(),
                    };
                    let why = if ten {
                        String::new()
                    } else {
                        "; this ffmpeg's libx264 has no 10-bit".to_string()
                    };
                    plan.notes
                        .push(format!("exported as {out_bits}-bit ({source}{why})"));
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

/// One piece of an edited output: `len` seconds of input file `file` from `start`, as exactly
/// `frames` video frames and `samples[t]` samples of audio track `t` where those are known
/// (see [`EditSpec::budget`]; without them the piece is cut by duration).
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub file: usize,
    pub start: f64,
    pub len: f64,
    pub frames: Option<u64>,
    pub samples: Vec<u64>,
}

impl Span {
    /// `len` seconds of the source from `start`, its counts not set yet.
    pub fn of(start: f64, len: f64) -> Self {
        Self {
            file: 0,
            start,
            len,
            frames: None,
            samples: Vec::new(),
        }
    }
}

/// An audio track of the source as ffprobe reports it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Track {
    pub channels: Option<u32>,
    pub rate: Option<u32>,
    /// ffprobe's `sample_fmt` (`s16`, `s32p`, `fltp`, `dbl`, …).
    pub format: Option<String>,
}

impl Track {
    /// The PCM codec that holds this track's decoded samples exactly (a multi-pass render's
    /// intermediate): its own sample format, as integers or floats of its width.
    pub fn lossless_pcm(&self) -> &'static str {
        match self.format.as_deref().map(|f| f.trim_end_matches('p')) {
            Some("u8" | "s16") => "pcm_s16le",
            Some("s32") => "pcm_s32le",
            Some("s64") => "pcm_s64le",
            Some("dbl") => "pcm_f64le",
            // Floats, and anything compressed (decoded as floats).
            _ => "pcm_f32le",
        }
    }
}

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

/// A frame rate as ffmpeg should be given it: the exact fraction where a small one fits
/// (`30000/1001`, `25/1`, `125/11`), else the decimal. A rounded decimal drifts from the
/// source's grid over a long file.
pub fn rate_arg(fps: f64) -> String {
    // Continued fraction: the first convergent within 1e-12 (relative), denominators ≤ 100000.
    let (mut h0, mut h1, mut k0, mut k1) = (0i64, 1i64, 1i64, 0i64);
    let mut x = fps;
    for _ in 0..32 {
        let a = x.floor();
        let (h2, k2) = (a as i64 * h1 + h0, a as i64 * k1 + k0);
        if k2 > 100_000 {
            break;
        }
        (h0, h1, k0, k1) = (h1, h2, k1, k2);
        if ((h1 as f64 / k1 as f64) - fps).abs() <= fps * 1e-12 {
            return format!("{h1}/{k1}");
        }
        let frac = x - a;
        if frac < 1e-12 {
            break;
        }
        x = 1.0 / frac;
    }
    format!("{fps:.9}")
}

/// Counts per piece on one grid laid over the whole output: piece `k` of the pieces with
/// lengths `lens` (seconds) gets `round(end_k · per_s) − round(start_k · per_s)` steps (frames
/// or samples), so no rounding builds up across pieces, the total is `round(total · per_s)`,
/// and a piece shorter than a step may get none (it is never stretched to one).
pub fn grid_counts(lens: &[f64], per_s: f64) -> Vec<u64> {
    let mut at = 0.0f64;
    let mut done = 0i64;
    lens.iter()
        .map(|len| {
            at += len;
            let end = (at * per_s).round() as i64;
            let n = (end - done).max(0);
            done = done.max(end);
            n as u64
        })
        .collect()
}

/// An edited output (spec §7, message 13): its pieces in order, Mono → Stereo, the crop, and
/// the audio tracks it carries.
#[derive(Debug, Clone, PartialEq)]
pub struct EditSpec {
    pub spans: Vec<Span>,
    /// The video's frame rate (cut points are its frame times).
    pub fps: Option<f64>,
    /// Mono → Stereo: every mono track becomes stereo, left = right = its channel.
    pub stereo: bool,
    pub crop: Option<Rect>,
    /// The audio tracks rendered, in order (all of the source's, or the first for a container
    /// that holds one).
    pub tracks: Vec<Track>,
}

impl EditSpec {
    /// The output's length in seconds.
    pub fn length(&self) -> f64 {
        self.spans.iter().map(|s| s.len).sum()
    }

    /// Every piece's frames and samples (per track) from one grid over the whole output
    /// ([`grid_counts`]): a render in passes counts out exactly what one pass would.
    pub fn budget(mut self) -> Self {
        let lens: Vec<f64> = self.spans.iter().map(|s| s.len).collect();
        let frames = self
            .fps
            .filter(|f| *f > 0.0)
            .map(|fps| grid_counts(&lens, fps));
        let samples: Vec<Option<Vec<u64>>> = self
            .tracks
            .iter()
            .map(|t| t.rate.map(|r| grid_counts(&lens, f64::from(r))))
            .collect();
        let known = samples.iter().all(Option::is_some);
        for (k, span) in self.spans.iter_mut().enumerate() {
            span.frames = frames.as_ref().map(|f| f[k]);
            span.samples = if known {
                samples.iter().flatten().map(|s| s[k]).collect()
            } else {
                Vec::new()
            };
        }
        self
    }

    /// Runs of pieces in file and source order, near enough to share one input.
    pub fn groups(&self) -> Vec<Vec<usize>> {
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for (i, s) in self.spans.iter().enumerate() {
            let joins = groups.last().is_some_and(|g| {
                let prev = &self.spans[*g.last().expect("never empty")];
                let end = prev.start + prev.len;
                prev.file == s.file && s.start >= end - 1e-9 && s.start - end <= SHARE_GAP_S
            });
            match groups.last_mut() {
                Some(g) if joins => g.push(i),
                _ => groups.push(vec![i]),
            }
        }
        groups
    }
}

/// One render opens at most this many inputs, each a demuxer and decoder of its own (300 of
/// them took 1.6 GB on a 64×64 source and ran out of file descriptors at `ulimit -n 256`). An
/// edit that needs more is rendered in passes ([`edit_passes`]).
pub const MAX_INPUTS: usize = 24;
/// Pieces that follow each other in the source closer than this share one input: decoded
/// through, not sought again.
const SHARE_GAP_S: f64 = 10.0;
/// A filter graph longer than this goes to a file (one argument may hold 128 KiB on Linux).
const INLINE_GRAPH: usize = 32 * 1024;
/// Every input starts this far before its first piece, which is then trimmed from the decoded
/// stream: right after a seek AAC and MP3 come out a few milliseconds off (154 samples for AAC
/// at 44.1 kHz, measured); 0.1 s of run-up made both sample-exact.
const EDIT_PREROLL_S: f64 = 0.2;

/// How an edit render encodes: for the file it makes, or losslessly for a later pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encode {
    Final,
    /// H.264 at `-qp 0` (lossless) in the output pixel format, each audio track as PCM of its
    /// own sample format ([`Track::lossless_pcm`]), in Matroska.
    Lossless,
}

/// An edit render's arguments: the inputs, the filter graph, the maps / encoders / output.
#[derive(Debug, Clone, PartialEq)]
pub struct EditArgs {
    pub inputs: Vec<OsString>,
    pub graph: String,
    pub outputs: Vec<OsString>,
}

/// The ffmpeg arguments that render `spec` from `files` into `dst` (pure). Exact cuts for any
/// number of pieces without decoding the source once per piece: pieces that follow each other
/// in the source share one input (`-ss` to just before the first, `-t` past the last, then
/// `split`), a piece out of order gets an input of its own. Every stream of a piece is cut at
/// the piece's start and moved by the same amount — the piece's start becomes 0 for all of
/// them, so the offsets between picture and sound and between tracks are kept, and a stream
/// that starts later in the source keeps its gap (filled: the first picture held, silence) —
/// then counted out to the piece's frames and samples ([`EditSpec::budget`]; padded if the
/// stream is short there); video is put on the frame-rate grid from the piece's start (`fps`:
/// a no-op on a constant-rate source, the preview's resampling on a variable one). Picture and
/// each track are joined by a `concat` of their own (a piece shorter than a frame adds no
/// picture). Input-side `-t` alone is not exact (AAC came out a frame short), nor is a trim
/// right at the seek point ([`EDIT_PREROLL_S`]). Mono → Stereo pans the mono tracks
/// (`pan=stereo|c0=c0|c1=c0`, full level). Chapters are not carried: an edit moves them.
pub fn edit_args(
    files: &[&Input],
    dst: &Path,
    spec: &EditSpec,
    kind: MediaKind,
    plan: &ExportPlan,
    encode: Encode,
) -> Result<EditArgs> {
    if spec.spans.is_empty()
        || spec
            .spans
            .iter()
            .any(|s| s.len.is_nan() || s.len <= 0.0 || s.start < 0.0 || s.file >= files.len())
    {
        bail!("the edit has nothing to render");
    }
    let ext = dst
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let video = kind == MediaKind::Video;
    if !video && spec.crop.is_some() {
        bail!("--crop applies to video only");
    }
    let tracks = &spec.tracks;
    if !video && tracks.is_empty() {
        bail!("no audio stream to render");
    }
    // Half a frame early, so the first frame is the one at the cut whatever the rounding.
    let half = match (video, spec.fps) {
        (true, Some(fps)) if fps > 0.0 => 0.5 / fps,
        _ => 0.0,
    };
    let groups = spec.groups();
    if groups.len() > MAX_INPUTS {
        bail!(
            "one render reads at most {MAX_INPUTS} inputs; this edit needs {} (render it in \
             passes)",
            groups.len()
        );
    }
    let mut inputs: Vec<OsString> = Vec::new();
    let mut graph: Vec<String> = Vec::new();
    // Each piece's labels, in output order; `None` where a piece adds nothing to a stream.
    let mut piece_v: Vec<Option<String>> = vec![None; spec.spans.len()];
    let mut piece_a: Vec<Vec<Option<String>>> = vec![vec![None; tracks.len()]; spec.spans.len()];
    for (g, members) in groups.iter().enumerate() {
        let first = &spec.spans[members[0]];
        let file = files[first.file];
        let ts = file.ext() == "ts";
        let end = members
            .iter()
            .map(|&i| spec.spans[i].start + spec.spans[i].len)
            .fold(0.0, f64::max);
        let pre = if ts { TS_PRESEEK_S } else { EDIT_PREROLL_S };
        let seek = (first.start - half - pre).max(0.0);
        if seek > 0.0 {
            inputs.extend(["-ss".into(), OsString::from(format!("{seek:.6}"))]);
        }
        inputs.extend([
            "-t".into(),
            OsString::from(format!("{:.6}", end - seek + 1.0)),
            "-i".into(),
            file.arg().to_owned(),
        ]);
        let wanted_v: Vec<usize> = members
            .iter()
            .copied()
            .filter(|&i| spec.spans[i].frames != Some(0))
            .collect();
        let fan = |graph: &mut Vec<String>, from: String, tag: &str, filter: &str, m: usize| {
            if m == 1 {
                return vec![from];
            }
            let outs: Vec<String> = (0..m).map(|k| format!("[g{g}{tag}{k}]")).collect();
            graph.push(format!("{from}{filter}={m}{}", outs.concat()));
            outs
        };
        // Intermediate files hold one picture stream and the rendered tracks, in order.
        let video_index = if first.file == 0 { plan.video_index } else { 0 };
        if video && !wanted_v.is_empty() {
            let from = format!("[{g}:v:{video_index}]");
            let ins = fan(&mut graph, from, "v", "split", wanted_v.len());
            for (k, &i) in wanted_v.iter().enumerate() {
                let s = &spec.spans[i];
                let origin = (s.start - seek).max(0.0);
                let count = match (s.frames, spec.fps) {
                    (Some(n), Some(fps)) => format!(
                        ",fps=fps={}:start_time=0,trim=end_frame={n},\
                         tpad=stop={n}:stop_mode=clone,trim=end_frame={n},setpts=PTS-STARTPTS",
                        rate_arg(fps)
                    ),
                    _ => format!(",trim=duration={:.6},setpts=PTS-STARTPTS", s.len),
                };
                let label = format!("[v{i}]");
                graph.push(format!(
                    "{}trim=start={:.6},setpts=PTS-{origin:.6}/TB{count}{label}",
                    ins[k],
                    (origin - half).max(0.0)
                ));
                piece_v[i] = Some(label);
            }
        }
        for (t, _) in tracks.iter().enumerate() {
            let wanted: Vec<usize> = members
                .iter()
                .copied()
                .filter(|&i| spec.spans[i].samples.get(t) != Some(&0))
                .collect();
            if wanted.is_empty() {
                continue;
            }
            let from = format!("[{g}:a:{t}]");
            let ins = fan(&mut graph, from, &format!("a{t}_"), "asplit", wanted.len());
            for (k, &i) in wanted.iter().enumerate() {
                let s = &spec.spans[i];
                let origin = (s.start - seek).max(0.0);
                let count = match s.samples.get(t) {
                    Some(n) => format!(",atrim=end_sample={n},apad=whole_len={n}"),
                    None => format!(",atrim=duration={:.6}", s.len),
                };
                let label = format!("[a{i}_{t}]");
                graph.push(format!(
                    "{}atrim=start={origin:.6},asetpts=PTS-{origin:.6}/TB,aresample=first_pts=0\
                     {count},asetpts=PTS-STARTPTS{label}",
                    ins[k]
                ));
                piece_a[i][t] = Some(label);
            }
        }
    }
    let join = |graph: &mut Vec<String>, labels: Vec<String>, kind: &str, out: &str| {
        let n = labels.len();
        graph.push(format!("{}concat=n={n}:{kind}{out}", labels.concat()));
    };
    if video {
        let labels: Vec<String> = piece_v.iter().flatten().cloned().collect();
        // A piece's last frame carries no duration through `concat` (a one-frame piece ended
        // where it began, and the next one's first frame took its time and was dropped): the
        // joined frames are timed by their index on the grid instead.
        let out = match spec.fps {
            Some(fps) => format!(",setpts=N/({})/TB[cv]", rate_arg(fps)),
            None => "[cv]".to_string(),
        };
        join(&mut graph, labels, "v=1:a=0", &out);
    }
    let n_tracks = tracks.len();
    for t in 0..n_tracks {
        let labels: Vec<String> = piece_a.iter().filter_map(|p| p[t].clone()).collect();
        join(&mut graph, labels, "v=0:a=1", &format!("[ca{t}]"));
    }
    let outs_a: Vec<String> = (0..n_tracks).map(|t| format!("[ca{t}]")).collect();
    let last = encode == Encode::Final;
    let mut v_out = "[cv]".to_string();
    if let (true, true, Some(r)) = (last, video, spec.crop) {
        let (w, h) = (r.w & !1, r.h & !1);
        graph.push(format!("[cv]crop={w}:{h}:{}:{}[vo]", r.x, r.y));
        v_out = "[vo]".to_string();
    }
    let mut a_out = outs_a.clone();
    if last && spec.stereo {
        for (label, (t, track)) in a_out.iter_mut().zip(tracks.iter().enumerate()) {
            if track.channels == Some(1) {
                graph.push(format!("{label}pan=stereo|c0=c0|c1=c0[so{t}]"));
                *label = format!("[so{t}]");
            }
        }
    }
    let mut outputs: Vec<OsString> = Vec::new();
    let push = |args: &mut Vec<OsString>, list: &[&str]| {
        args.extend(list.iter().map(OsString::from));
    };
    if video {
        push(&mut outputs, &["-map", &v_out]);
    }
    for label in &a_out {
        push(&mut outputs, &["-map", label]);
    }
    push(&mut outputs, &["-map_chapters", "-1"]);
    // Each audio track's own tags (language, title) travel with it; a filter's output has none.
    for t in 0..n_tracks {
        push(
            &mut outputs,
            &[&format!("-map_metadata:s:a:{t}"), &format!("0:s:a:{t}")],
        );
    }
    match (encode, video) {
        (Encode::Lossless, _) => {
            if video {
                push(
                    &mut outputs,
                    &[
                        "-c:v",
                        "libx264",
                        "-preset",
                        "ultrafast",
                        "-qp",
                        "0",
                        "-pix_fmt",
                        plan.pix_fmt,
                    ],
                );
            }
            for (t, track) in tracks.iter().enumerate() {
                push(&mut outputs, &[&format!("-c:a:{t}"), track.lossless_pcm()]);
            }
            push(&mut outputs, &["-f", "matroska"]);
        }
        (Encode::Final, true) => {
            push(
                &mut outputs,
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
                push(&mut outputs, &[option, value]);
            }
            if n_tracks > 0 {
                push(&mut outputs, &["-c:a", "aac", "-b:a", "192k"]);
            }
            if matches!(ext.as_str(), "mp4" | "mov" | "m4v") {
                push(&mut outputs, &["-movflags", "+faststart"]);
            }
        }
        (Encode::Final, false) => match plan.audio_codec.as_deref().and_then(encoder_for_codec) {
            Some(codec) => outputs.extend(codec.into_iter().map(OsString::from)),
            None => match encoder_for_container(&ext) {
                Some(codec) => push(&mut outputs, codec),
                None => bail!("no audio export for .{ext} files"),
            },
        },
    }
    outputs.push(dst.as_os_str().to_owned());
    Ok(EditArgs {
        inputs,
        graph: graph.join(";"),
        outputs,
    })
}

/// How an edit is rendered: passes whose pieces need at most `max_inputs` inputs each, made
/// losslessly ([`Encode::Lossless`]) into intermediate files 1, 2, … (file 0 is the source),
/// then the final pass over those files in order. One pass when it fits; more than
/// `max_inputs`² separate stretches of the source is refused. `spec` is budgeted first
/// ([`EditSpec::budget`]) and an intermediate's counts are its pieces' sums, so the final file
/// holds exactly the frames and samples one pass would.
pub fn edit_passes(spec: &EditSpec, max_inputs: usize) -> Result<Vec<EditSpec>> {
    let spec = spec.clone().budget();
    let groups = spec.groups();
    if groups.len() <= max_inputs {
        return Ok(vec![spec]);
    }
    let chunks: Vec<&[Vec<usize>]> = groups.chunks(max_inputs).collect();
    if chunks.len() > max_inputs {
        bail!(
            "this edit takes {} separate stretches of the source; at most {} can be rendered \
             at once — save part of the edit first",
            groups.len(),
            max_inputs * max_inputs
        );
    }
    let mut passes: Vec<EditSpec> = chunks
        .iter()
        .map(|chunk| EditSpec {
            spans: chunk
                .iter()
                .flatten()
                .map(|&i| spec.spans[i].clone())
                .collect(),
            ..spec.clone()
        })
        .collect();
    // Each intermediate is one piece of the final pass: the whole file, its counts summed.
    let last = EditSpec {
        spans: passes
            .iter()
            .enumerate()
            .map(|(k, p)| Span {
                file: k + 1,
                start: 0.0,
                len: p.length(),
                frames: p.spans.iter().map(|s| s.frames).sum::<Option<u64>>(),
                samples: (0..spec.tracks.len())
                    .map(|t| p.spans.iter().filter_map(|s| s.samples.get(t)).sum())
                    .filter(|_| p.spans.iter().all(|s| !s.samples.is_empty()))
                    .collect(),
            })
            .collect(),
        ..spec.clone()
    };
    passes.push(last);
    Ok(passes)
}

/// This ffmpeg's major version (`ffmpeg version 9.0.2`, `n7.1`, `6.1.1-3ubuntu5`); a git build
/// (`N-…`) counts as new. Asked once per ffmpeg.
pub fn major_version(tools: &Tools) -> Option<u32> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<u32>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(hit) = cache.lock().unwrap().get(&tools.ffmpeg) {
        return *hit;
    }
    let out = output(
        Command::new(&tools.ffmpeg)
            .arg("-version")
            .stdin(Stdio::null()),
    )
    .ok();
    let major = out.and_then(|o| parse_major(&String::from_utf8_lossy(&o.stdout)));
    cache.lock().unwrap().insert(tools.ffmpeg.clone(), major);
    major
}

/// The major version in `ffmpeg -version`'s first line.
pub fn parse_major(text: &str) -> Option<u32> {
    let rest = text.lines().next()?.split("version ").nth(1)?;
    if rest.starts_with("N-") {
        return Some(u32::MAX);
    }
    let digits: String = rest
        .trim_start_matches('n')
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The whole argument list for an edit render: the graph inline, or — when long — in a file in
/// `work` (`-/filter_complex FILE` from ffmpeg 7, which dropped `-filter_complex_script`; that
/// one before).
pub fn assemble(tools: &Tools, edit: EditArgs, work: &Path, name: &str) -> Result<Vec<OsString>> {
    assemble_inline_up_to(tools, edit, work, name, INLINE_GRAPH)
}

/// [`assemble`] with the longest graph kept inline given (the tests force the file).
pub fn assemble_inline_up_to(
    tools: &Tools,
    edit: EditArgs,
    work: &Path,
    name: &str,
    inline: usize,
) -> Result<Vec<OsString>> {
    let mut args = edit.inputs;
    if edit.graph.len() <= inline {
        args.extend(["-filter_complex".into(), edit.graph.into()]);
    } else {
        let file = work.join(name);
        std::fs::write(&file, &edit.graph)
            .with_context(|| format!("cannot write {}", file.display()))?;
        let option = if major_version(tools).is_some_and(|m| m >= 7) {
            "-/filter_complex"
        } else {
            "-filter_complex_script"
        };
        args.extend([option.into(), file.into_os_string()]);
    }
    args.extend(edit.outputs);
    Ok(args)
}

/// The encoder arguments an audio export into a `.ext` file uses (as [`export_args`] picks
/// them): the source's codec (`codec` = its ffprobe `codec_name`), else the container's usual
/// one. `sopen stems` encodes each stem with them.
pub fn audio_encoder(codec: Option<&str>, ext: &str) -> Result<Vec<String>> {
    let ext = ext.to_ascii_lowercase();
    match codec.and_then(encoder_for_codec) {
        Some(args) => Ok(args),
        None => match encoder_for_container(&ext) {
            Some(args) => Ok(args.iter().map(|a| a.to_string()).collect()),
            None => bail!("no audio export for .{ext} files"),
        },
    }
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
    let out = output(
        Command::new(&tools.ffmpeg)
            .args(["-hide_banner", "-encoders"])
            .stdin(Stdio::null()),
    )
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

/// The last line of `text` that says something (an error output's last words).
pub(crate) fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no error output")
        .trim()
        .to_string()
}

/// The bounded tail of a child's error output: the last `LINES` lines, at most `BYTES` bytes of
/// them, a line that never ends cut to its first `BYTES` bytes (at a character's edge) and
/// marked `…`. However much is printed — ffmpeg prints an error per damaged frame for as long as
/// it runs — the whole stream is read (the child never blocks on a full pipe) and only this
/// much is kept; only its last line is ever reported.
#[derive(Debug, Default)]
pub(crate) struct Tail<const LINES: usize, const BYTES: usize> {
    pub(crate) lines: std::collections::VecDeque<String>,
    bytes: usize,
    partial: Vec<u8>,
    cut: bool,
}

/// What playback, the waveform and a render keep of ffmpeg's error output.
pub(crate) type ErrorTail = Tail<64, 2048>;

impl<const LINES: usize, const BYTES: usize> Tail<LINES, BYTES> {
    /// Reads `r` to its end (a read error ends it too), keeping the tail.
    pub(crate) fn read(mut r: impl Read) -> Self {
        let mut tail = Self::default();
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => tail.feed(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        if !tail.partial.is_empty() || tail.cut {
            tail.end_line();
        }
        tail
    }

    /// [`Tail::read`] on a thread of its own (a child's stderr, read while it runs).
    pub(crate) fn spawn(r: impl Read + Send + 'static) -> std::thread::JoinHandle<Self> {
        std::thread::spawn(move || Self::read(r))
    }

    pub(crate) fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let (part, ended) = match bytes.iter().position(|&b| b == b'\n') {
                Some(i) => (&bytes[..i], Some(i + 1)),
                None => (bytes, None),
            };
            let room = BYTES.saturating_sub(self.partial.len());
            self.partial
                .extend_from_slice(&part[..part.len().min(room)]);
            self.cut |= part.len() > room;
            match ended {
                Some(next) => {
                    self.end_line();
                    bytes = &bytes[next..];
                }
                None => break,
            }
        }
    }

    pub(crate) fn end_line(&mut self) {
        if self.cut {
            // Cut at a character's edge, not inside one.
            if let Err(e) = std::str::from_utf8(&self.partial) {
                if e.error_len().is_none() {
                    self.partial.truncate(e.valid_up_to());
                }
            }
        }
        let mut line = String::from_utf8_lossy(&self.partial).into_owned();
        if self.cut {
            line.push('…');
        }
        self.partial.clear();
        self.cut = false;
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.lines.len() > LINES || (self.bytes > BYTES && self.lines.len() > 1) {
            if let Some(gone) = self.lines.pop_front() {
                self.bytes -= gone.len();
            }
        }
    }

    /// The bytes kept (lines and the unfinished one).
    #[cfg(test)]
    pub(crate) fn kept(&self) -> usize {
        self.bytes + self.partial.len()
    }

    /// The last line with something on it.
    pub(crate) fn last_line(&self) -> String {
        self.lines
            .iter()
            .rev()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("no error output")
            .to_string()
    }
}

/// How long a save or export may go without its output advancing before ffmpeg is stopped.
/// ffmpeg 7+ keeps printing progress blocks while it is stuck (the same `out_time` every half
/// second), so the watchdog watches what they say, not whether they come; a libx264 deadlock
/// once ran for 44 hours with nothing to stop it.
pub const STALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// The stall rule, on explicit instants (the tests drive it without a clock): the time without
/// progress counts only while ffmpeg is waited for — never while the caller's progress callback
/// runs (the export-hold receipt hook sleeps there on purpose).
#[derive(Debug, Clone)]
pub struct Watchdog {
    limit: std::time::Duration,
    /// Time waited since the output last advanced, and since when the current wait runs.
    idle: std::time::Duration,
    mark: std::time::Instant,
    /// The last `out_time_us`, `frame` and `total_size` seen.
    seen: [Option<String>; 3],
}

impl Watchdog {
    pub fn new(limit: std::time::Duration, now: std::time::Instant) -> Self {
        Self {
            limit,
            idle: std::time::Duration::ZERO,
            mark: now,
            seen: [None, None, None],
        }
    }

    /// One `key=value` line of `-progress` output at `now`; true when it says the output moved
    /// on.
    pub fn line(&mut self, line: &str, now: std::time::Instant) -> bool {
        self.idle += now.saturating_duration_since(self.mark);
        self.mark = now;
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        let slot = match key {
            "out_time_us" => 0,
            "frame" => 1,
            "total_size" => 2,
            _ => return false,
        };
        let value = value.trim();
        if value == "N/A" || self.seen[slot].as_deref() == Some(value) {
            return false;
        }
        let first = self.seen[slot].is_none();
        self.seen[slot] = Some(value.to_string());
        // A key's first value is a step forward only above zero (`frame=0` is where it starts).
        let moved = !first || value.parse::<i64>().is_ok_and(|v| v > 0);
        if moved {
            self.idle = std::time::Duration::ZERO;
        }
        moved
    }

    /// The caller's own work (the progress callback) ended at `now`: that time does not count.
    pub fn resume(&mut self, now: std::time::Instant) {
        self.mark = now;
    }

    /// Whether ffmpeg has made no progress for longer than the limit, as of `now`.
    pub fn stalled(&mut self, now: std::time::Instant) -> bool {
        self.idle += now.saturating_duration_since(self.mark);
        self.mark = now;
        self.idle > self.limit
    }
}

/// What a run stopped by the watchdog says: how long it stood still, and where its output was.
pub fn stall_message(limit: std::time::Duration, out_time_us: Option<&str>) -> String {
    let secs = limit.as_secs_f64();
    let limit = if secs >= 10.0 {
        format!("{secs:.0} s")
    } else {
        format!("{secs:.1} s")
    };
    let at = out_time_us
        .and_then(|us| us.parse::<f64>().ok())
        .map(|us| format!(" (stuck at {:.2} s of output)", us / 1e6))
        .unwrap_or_default();
    format!("ffmpeg made no progress for {limit}{at} and was stopped")
}

/// A watcher that kills a render's ffmpeg if sopen dies before it ends: a small `sh` in a
/// process group of its own (a terminal's Ctrl+C does not reach it) reading a pipe whose
/// writing end only sopen holds. Any end of sopen — exit, a signal, SIGKILL — closes the pipe;
/// the watcher's read returns, and it kills the ffmpeg if that pid still runs that program
/// (its command line names it). A run that ends normally kills the watcher first.
pub struct Orphan {
    watcher: Option<std::process::Child>,
}

impl Orphan {
    pub fn guard(child: &std::process::Child, program: &Path) -> Self {
        use std::os::unix::process::CommandExt;
        let name = program
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let watcher = Command::new("/bin/sh")
            .args([
                "-c",
                "read _ ; case \"$(ps -o args= -p \"$1\" 2>/dev/null)\" in \
                 *\"$2\"*) kill -KILL \"$1\" 2>/dev/null ;; esac",
                "sh",
                &child.id().to_string(),
                &name,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .ok();
        Self { watcher }
    }
}

impl Drop for Orphan {
    fn drop(&mut self) {
        if let Some(mut w) = self.watcher.take() {
            let _ = w.kill();
            let _ = w.wait();
        }
    }
}

/// Runs `ffmpeg <globals> <args>` with `-progress pipe:1`; `progress` gets 0.0..=1.0 of
/// `length_s`. On failure the last line ffmpeg printed is the error. A run whose output does not
/// advance for [`STALL_LIMIT`] is stopped ([`Watchdog`]).
pub fn run_export(
    tools: &Tools,
    args: &[OsString],
    length_s: f64,
    progress: &mut dyn FnMut(f64),
) -> Result<()> {
    run_watched(tools, args, length_s, progress, STALL_LIMIT)
}

/// [`run_export`] with the stall limit given.
pub fn run_watched(
    tools: &Tools,
    args: &[OsString],
    length_s: f64,
    progress: &mut dyn FnMut(f64),
    limit: std::time::Duration,
) -> Result<()> {
    use std::sync::mpsc::{sync_channel, RecvTimeoutError};
    use std::time::{Duration, Instant};
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
    let mut child =
        spawn(&mut cmd).with_context(|| format!("cannot run {}", tools.ffmpeg.display()))?;
    // Dies with this run: a sopen killed mid-render (even by SIGKILL) takes its ffmpeg along,
    // instead of leaving it rendering for an hour to a file nobody keeps.
    let _guard = Orphan::guard(&child, &tools.ffmpeg);
    let errors = ErrorTail::spawn(child.stderr.take().expect("piped"));
    // A small queue: while the callback runs (the export-hold hook sleeps in it), the reader
    // stops and ffmpeg blocks on its progress pipe, as it did when this thread read it.
    let (tx, rx) = sync_channel::<std::io::Result<String>>(16);
    let stdout = child.stdout.take().expect("piped");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let failed = line.is_err();
            if tx.send(line).is_err() || failed {
                return;
            }
        }
    });
    let poll = Duration::from_millis(250).min(limit);
    let mut dog = Watchdog::new(limit, Instant::now());
    let stall = |child: &mut std::process::Child, dog: &Watchdog| {
        let _ = child.kill();
        let _ = child.wait();
        anyhow!(stall_message(limit, dog.seen[0].as_deref()))
    };
    loop {
        match rx.recv_timeout(poll) {
            Ok(Ok(line)) => {
                dog.line(&line, Instant::now());
                if let Some(us) = line.strip_prefix("out_time_us=") {
                    if let Ok(us) = us.trim().parse::<f64>() {
                        if length_s > 0.0 {
                            progress((us / 1e6 / length_s).clamp(0.0, 1.0));
                        }
                    }
                } else if line == "progress=end" {
                    progress(1.0);
                }
                dog.resume(Instant::now());
                // Stuck, ffmpeg 7+ still prints a block every half second: checked here too,
                // not only when nothing comes.
                if dog.stalled(Instant::now()) {
                    return Err(stall(&mut child, &dog));
                }
            }
            Ok(Err(e)) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e).context("reading ffmpeg's progress");
            }
            Err(RecvTimeoutError::Timeout) => {
                if dog.stalled(Instant::now()) {
                    return Err(stall(&mut child, &dog));
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // Its output is closed; it should be exiting. Still waited for under the same rule.
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if dog.stalled(Instant::now()) {
            return Err(stall(&mut child, &dog));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let errors = errors.join().unwrap_or_default();
    if !status.success() {
        bail!("ffmpeg failed ({status}): {}", errors.last_line());
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
pub(crate) mod fixtures;

#[cfg(test)]
mod tests;
