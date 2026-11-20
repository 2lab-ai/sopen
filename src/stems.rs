//! `sopen stems` (spec §7a): split a file's audio into voice and music, published next to it as
//! two new files, `<stem>.voice.<ext>` and `<stem>.music.<ext>` — never over an existing file.
//!
//! The separation runs in this process through sherpa-onnx (feature `stems`, [`engine`]) with
//! the UVR-MDX-NET-Voc_FT model, which is not in sopen's release assets: Homebrew installs it
//! next to the binary and sopen checks its SHA-256 before loading it ([`find_model`]). ffmpeg
//! decodes the first audio track to stereo f32 at the model's rate on a pipe; the track is
//! separated in pieces of 30 s, each given 1 s of real audio on both sides (silence at the
//! file's ends) that is cut off again ([`Layout`]); each stem is encoded by ffmpeg in the
//! source's audio codec (a video source gives AAC in `.m4a`).
//!
//! The window runs `sopen stems` as a child process ([`child`]): one `Process` call cannot be
//! interrupted and the engine holds several GB, so cancelling is killing the child, which also
//! gives the memory back.

pub mod child;
#[cfg(feature = "stems")]
mod engine;

pub use child::{spawn_child, Cancel, StemsChild};

use crate::kind::{self, Kind};
use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The model file's name, as Homebrew stages it into the formula's `pkgshare`.
pub const MODEL_FILE: &str = "UVR-MDX-NET-Voc_FT.onnx";
/// The model sopen was measured with (spec §7a); any other file is refused.
pub const MODEL_SHA256: &str = "e411182ce2c53541fefdcc99a8f46f2fe03978eb22038b9497c1d9d95a00fad4";
pub const MODEL_BYTES: u64 = 66_762_795;
/// Where the model is, instead of the one installed with sopen.
pub const MODEL_ENV: &str = "SOPEN_STEMS_MODEL";

/// Whether this build can separate (cargo feature `stems`; on in every release build).
pub const AVAILABLE: bool = cfg!(feature = "stems");

/// The credit the model's authors ask for, in `--version` and the long help next to the font's.
pub const CREDIT: &str = "Voice/music separation: UVR-MDX-NET-Voc_FT from Ultimate Vocal Remover \
                          (Anjok07 & aufr33, MIT), ONNX conversion and engine k2-fsa/sherpa-onnx \
                          (Apache-2.0)";

/// The kind `sopen stems` accepts, by extension, before anything is fetched (as `sopen cut`).
pub fn media_kind_of(name: &str) -> Result<Kind> {
    match kind::by_extension(name) {
        Some(k @ (Kind::Video | Kind::Audio)) => Ok(k),
        None if kind::maybe_video(name) => Ok(Kind::Video),
        _ => bail!("stems works on audio and video files; {name} is neither"),
    }
}

/// The Homebrew formula this build is installed by: its `pkgshare` holds the model.
pub fn formula() -> &'static str {
    match option_env!("SOPEN_BUILD_CHANNEL") {
        Some("release") => "sopen",
        _ => "sopen-preview",
    }
}

/// Where the model was looked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelFrom {
    /// `--model PATH`.
    Flag,
    /// [`MODEL_ENV`].
    Env,
    /// `<keg>/share/<formula>/` next to the binary's `<keg>/bin/`, where Homebrew puts it.
    Installed,
}

/// The path the model must be at: `--model`, else [`MODEL_ENV`] (when non-empty), else
/// `<canonical exe>/../../share/<formula>/<MODEL_FILE>`. `exe` = the canonical path of the
/// running binary (`None` when it cannot be known).
pub fn locate_model(
    flag: Option<&Path>,
    env: Option<OsString>,
    exe: Option<&Path>,
    formula: &str,
) -> Result<(PathBuf, ModelFrom)> {
    if let Some(path) = flag {
        return Ok((path.to_path_buf(), ModelFrom::Flag));
    }
    if let Some(path) = env.filter(|p| !p.is_empty()) {
        return Ok((PathBuf::from(path), ModelFrom::Env));
    }
    let keg = exe
        .and_then(Path::parent)
        .and_then(Path::parent)
        .with_context(|| {
            format!("cannot tell where sopen is installed; set {MODEL_ENV} to {MODEL_FILE}")
        })?;
    let path = keg.join("share").join(formula).join(MODEL_FILE);
    Ok((path, ModelFrom::Installed))
}

/// `path` holds exactly `bytes` bytes with SHA-256 `sha` (lower-case hex); the error says how
/// it differs.
pub fn verify_file(path: &Path, bytes: u64, sha: &str) -> Result<()> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .len();
    if size != bytes {
        bail!("{size} bytes, want {bytes}");
    }
    let got = crate::cache::hex(&crate::app::file_hash(path)?);
    if got != sha {
        bail!("SHA-256 {got}, want {sha}");
    }
    Ok(())
}

/// The model, located ([`locate_model`]) and checked: present, and exactly the measured file.
/// Every failure is one line naming the path it expected.
pub fn find_model(
    flag: Option<&Path>,
    env: Option<OsString>,
    exe: Option<&Path>,
) -> Result<PathBuf> {
    let (path, from) = locate_model(flag, env, exe, formula())?;
    let source = match from {
        ModelFrom::Flag => "--model".to_string(),
        ModelFrom::Env => MODEL_ENV.to_string(),
        ModelFrom::Installed => format!("installed with sopen ({} formula)", formula()),
    };
    if !path.is_file() {
        let how = match from {
            ModelFrom::Installed => format!(
                "`brew reinstall {}` puts it there, or set {MODEL_ENV} to a copy",
                formula()
            ),
            _ => format!("point it at {MODEL_FILE}"),
        };
        bail!(
            "the voice/music model is missing: no {} ({source}); {how}",
            path.display()
        );
    }
    verify_file(&path, MODEL_BYTES, MODEL_SHA256).map_err(|e| {
        anyhow::anyhow!(
            "{} ({source}) is not the voice/music model {MODEL_FILE}: {e:#}",
            path.display()
        )
    })?;
    Ok(path)
}

/// How a track is cut into windows for the engine (in frames: one sample per channel): each
/// window keeps `piece` frames and carries `context` frames before and after them — the file's
/// own audio, or silence past its ends — which are cut off its output again. The engine
/// distorts a window's edges; the context keeps that out of what is kept (measured lossless in
/// the research spike).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub piece: u64,
    pub context: u64,
}

/// One window: what of the file it keeps, what of the file it reads, and the silence around
/// that. `lead + read.len() + tail = core.len() + 2 × context`, and the kept output is
/// `[context, context + core.len())` of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub core: std::ops::Range<u64>,
    pub read: std::ops::Range<u64>,
    pub lead: u64,
    pub tail: u64,
}

impl Span {
    pub fn window_len(&self) -> u64 {
        self.lead + (self.read.end - self.read.start) + self.tail
    }
}

impl Layout {
    /// 30 s pieces with 1 s of context at `rate` (spec §7a).
    pub fn standard(rate: u32) -> Self {
        Self::seconds(rate, 30.0, 1.0)
    }

    pub fn seconds(rate: u32, piece_s: f64, context_s: f64) -> Self {
        Self {
            piece: ((piece_s * f64::from(rate)).round() as u64).max(1),
            context: (context_s * f64::from(rate)).round() as u64,
        }
    }

    /// Piece `k`, given that the audio is known to reach at least `end` frames, and no further
    /// when `end` is short of `(k + 1) × piece + context` (then `end` is the file's length).
    /// `None` when piece `k` would start at or past the end.
    pub fn span(&self, k: u64, end: u64) -> Option<Span> {
        let start = k.checked_mul(self.piece)?;
        if start >= end {
            return None;
        }
        let core = start..(start + self.piece).min(end);
        let read = start.saturating_sub(self.context)..(core.end + self.context).min(end);
        let lead = self.context - (start - read.start);
        let tail = self.context - (read.end - core.end);
        Some(Span {
            core,
            read,
            lead,
            tail,
        })
    }

    /// Every window of a track `total` frames long.
    pub fn spans(&self, total: u64) -> Vec<Span> {
        (0..)
            .map_while(|k| self.span(k, total.min((k + 1) * self.piece + self.context)))
            .collect()
    }

    /// How many windows a track `total` frames long takes.
    pub fn count(&self, total: u64) -> u64 {
        total.div_ceil(self.piece)
    }
}

/// Progress of a separation, as `sopen stems` prints it on stderr — one line per finished
/// piece, `stems: piece 3/8, 61s elapsed, 102s remaining` (`remaining` only once a piece is
/// done) — and as the window reads it back from the child ([`PieceProgress::parse`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PieceProgress {
    pub done: u64,
    pub total: u64,
    pub elapsed_s: u64,
    pub remaining_s: Option<u64>,
}

/// What a supervised child prints when every piece is encoded, before it waits for the go-ahead
/// to publish ([`child`]).
pub const READY_LINE: &str = "stems: ready to publish";
/// What the parent answers on the child's stdin to let it publish.
pub const GO_AHEAD: &str = "publish";

impl PieceProgress {
    pub fn line(&self) -> String {
        let mut line = format!(
            "stems: piece {}/{}, {}s elapsed",
            self.done, self.total, self.elapsed_s
        );
        if let Some(r) = self.remaining_s {
            line.push_str(&format!(", {r}s remaining"));
        }
        line
    }

    /// The line [`PieceProgress::line`] prints, back; `None` for any other line.
    pub fn parse(line: &str) -> Option<Self> {
        let rest = line.trim().strip_prefix("stems: piece ")?;
        let mut parts = rest.split(", ");
        let (done, total) = parts.next()?.split_once('/')?;
        let (done, total) = (done.parse().ok()?, total.parse().ok()?);
        let elapsed_s = parts.next()?.strip_suffix("s elapsed")?.parse().ok()?;
        let remaining_s = match parts.next() {
            Some(r) => Some(r.strip_suffix("s remaining")?.parse().ok()?),
            None => None,
        };
        if parts.next().is_some() || done > total || total == 0 {
            return None;
        }
        Some(Self {
            done,
            total,
            elapsed_s,
            remaining_s,
        })
    }

    pub fn fraction(&self) -> f64 {
        self.done as f64 / self.total.max(1) as f64
    }
}

/// Two published stems and what was not carried over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub voice: String,
    pub music: String,
    pub notes: Vec<String>,
}

/// The names the pair is published under: the first `n` (none, then `-1`, `-2`, …) for which
/// neither `voice` nor `music` with it names anything in `dir` (not even a dangling link), so
/// the two carry the same number — not `a.voice-1.wav` with `a.music-3.wav` — unless another
/// program takes one of them before [`publish_pair`] publishes it, which then never overwrites:
/// that one gets the next free name.
pub fn free_pair(
    fs: &mut dyn crate::remote::RemoteFs,
    dir: &str,
    voice: &str,
    music: &str,
) -> Result<(String, String)> {
    use crate::remote::{join, suffixed};
    for n in 0..10_000 {
        let (v, m) = match n {
            0 => (voice.to_string(), music.to_string()),
            n => (suffixed(voice, n), suffixed(music, n)),
        };
        if fs.lstat_opt(&join(dir, &v))?.is_none() && fs.lstat_opt(&join(dir, &m))?.is_none() {
            return Ok((v, m));
        }
    }
    bail!("no free pair of names for {voice} and {music} in {dir}")
}

/// Publishes the two local encodes as new siblings in `dir` under [`free_pair`]'s names, voice
/// first, and returns the names they got. Each publish is on its own and never overwrites: a
/// name another program takes between the look and the publish gets the next free one, so then
/// the numbers may differ — what was published is what is returned.
pub fn publish_pair(
    fs: &mut dyn crate::remote::RemoteFs,
    (voice_file, voice): (&Path, &str),
    (music_file, music): (&Path, &str),
    dir: &str,
) -> Result<(String, String)> {
    let (voice, music) = free_pair(fs, dir, voice, music)?;
    let voice = fs.write_new_sibling(voice_file, dir, &voice)?;
    let music = fs.write_new_sibling(music_file, dir, &music).map_err(|e| {
        anyhow::anyhow!(
            "{e:#} — the voice was published as {}, the music was not",
            crate::remote::join(dir, &voice)
        )
    })?;
    Ok((voice, music))
}

/// `<stem>.voice.<ext>`, `<stem>.music.<ext>`.
pub fn stem_names(src_name: &str, out_ext: &str) -> (String, String) {
    let (stem, _) = crate::cut::stem_ext(src_name);
    (
        format!("{stem}.voice.{out_ext}"),
        format!("{stem}.music.{out_ext}"),
    )
}

#[cfg(feature = "stems")]
mod pipeline;
#[cfg(feature = "stems")]
pub use pipeline::{separate_and_publish, Options};

#[cfg(test)]
mod tests;
