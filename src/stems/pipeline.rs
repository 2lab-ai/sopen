//! The separation itself (feature `stems`): ffmpeg decodes the first audio track to interleaved
//! stereo f32 at the model's rate on a pipe; the engine separates it window by window
//! ([`Layout`]), reading only as far ahead as the next window needs; the kept part of each
//! window goes straight to two ffmpeg encoders on pipes. Memory is one window and the model,
//! however long the track.

use super::engine::{Engine, Separated};
use super::{media_kind_of, stem_names, Layout, PieceProgress, Published, Span};
use crate::ffmpeg::{self, MediaKind, Tools};
use crate::kind::{self, Kind};
use crate::remote::RemoteFs;
use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread::JoinHandle;
use std::time::Instant;

/// How `sopen stems` separates (the CLI's are [`Options::default`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Seconds kept per window, and of context on each side (spec §7a: 30 and 1).
    pub piece_s: f64,
    pub context_s: f64,
    /// The engine's threads.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            piece_s: 30.0,
            context_s: 1.0,
            threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
        }
    }
}

/// Separates the local file `src` (originally `src_name` in `dir` on `fs`) with the checked
/// `model`, encodes both stems into `work` (an existing private folder; what is written there
/// is removed again), calls `before_publish` (a supervised child waits there for the go-ahead;
/// an error publishes nothing) and publishes the stems as new siblings, voice first.
#[allow(clippy::too_many_arguments)]
pub fn separate_and_publish(
    fs: &mut dyn RemoteFs,
    tools: &Tools,
    model: &Path,
    src: &Path,
    dir: &str,
    src_name: &str,
    opts: &Options,
    work: &Path,
    before_publish: &mut dyn FnMut() -> Result<()>,
    progress: &mut dyn FnMut(&PieceProgress),
) -> Result<Published> {
    let by_ext = media_kind_of(src_name)?;
    let probe = ffmpeg::probe(tools, src)?;
    if !probe.has_audio {
        bail!("{src_name} has no audio stream to separate");
    }
    let format = audio_format(tools, src)?;
    let video = kind::refine_with_probe(by_ext, probe.video.is_some()) == Kind::Video;
    // A video source gives audio-only AAC in .m4a; audio keeps its codec (and its container
    // when that holds it), as `sopen cut` does.
    let (out_ext, codec) = if video {
        ("m4a".to_string(), Some("aac"))
    } else {
        let (_, ext) = crate::cut::stem_ext(src_name);
        let codec = probe.audio_codec.as_deref();
        (ffmpeg::output_ext(ext, MediaKind::Audio, codec), codec)
    };
    let encoder: Vec<OsString> = ffmpeg::audio_encoder(codec, &out_ext)?
        .into_iter()
        .map(OsString::from)
        .collect();
    let encoder = ffmpeg::adapt_to_encoders(encoder, &ffmpeg::available_encoders(tools)?)?;
    let mut notes = Vec::new();
    if probe.audio_streams > 1 {
        notes.push(format!(
            "only the first audio track is separated ({} more not)",
            probe.audio_streams - 1
        ));
    }
    if format.channels > 2 {
        notes.push(format!(
            "{}-channel audio is separated as stereo",
            format.channels
        ));
    }

    let started = Instant::now();
    let mut engine = Engine::new(model, opts.threads)?;
    let rate = engine.rate();
    let layout = Layout::seconds(rate, opts.piece_s, opts.context_s);
    let (voice_name, music_name) = stem_names(src_name, &out_ext);
    let files = WorkFiles(vec![work.join(&voice_name), work.join(&music_name)]);
    // Mono goes through the engine as two equal channels and comes back as their mean, by
    // explicit `pan`s: ffmpeg's own mono ↔ stereo matrix is not gain-neutral (a 16-bit mono
    // source came back 3 dB down).
    let mono = format.channels == 1;
    // Back at the source's own rate, the stems have exactly its sample count: the resampling
    // there and back can add a sample at the end (192,512 → 192,513; in AAC a whole frame
    // more), so that count is taken from the source itself — on a decode of its own, while the
    // first piece is separated — and each stem is padded or cut to it.
    let resample_to = format.rate.filter(|r| *r != rate);
    let mut count = match resample_to {
        Some(_) => Some(SampleCount::start(tools, src)?),
        None => None,
    };
    let out_args = |dst: &Path, frames: Option<u64>| {
        let mut chain: Vec<String> = Vec::new();
        if mono {
            chain.push("pan=mono|c0=0.5*c0+0.5*c1".into());
        }
        if let Some(r) = resample_to {
            chain.push(format!("aresample={r}"));
        }
        if let Some(n) = frames {
            chain.push(format!("apad=whole_len={n}"));
            chain.push(format!("atrim=end_sample={n}"));
        }
        let mut args: Vec<OsString> = Vec::new();
        if !chain.is_empty() {
            args.extend(["-af".into(), chain.join(",").into()]);
        }
        if !mono {
            args.extend(["-ac".into(), "2".into()]);
        }
        args.extend(encoder.iter().cloned());
        args.push(dst.as_os_str().to_owned());
        args
    };
    // More than two channels: downmixed the way ffmpeg downmixes for integer output, normalised
    // so nothing goes over full scale (unnormalised, a loud 5.1 file came in at +6.6 dBFS and
    // its stems clipped).
    let rematrix: &[&str] = if format.channels > 2 {
        &["-rematrix_maxval", "1.0"]
    } else {
        &[]
    };
    let mut decoder = Ffmpeg::start(
        tools,
        &[
            "-i".into(),
            src.as_os_str().to_owned(),
            "-map".into(),
            "0:a:0".into(),
            "-vn".into(),
            "-sn".into(),
            "-dn".into(),
            "-af".into(),
            if mono {
                "pan=stereo|c0=c0|c1=c0".into()
            } else {
                "anull".into()
            },
            "-ac".into(),
            "2".into(),
        ]
        .into_iter()
        .chain(rematrix.iter().map(OsString::from))
        .chain([
            "-ar".into(),
            rate.to_string().into(),
            "-f".into(),
            "f32le".into(),
            "-c:a".into(),
            "pcm_f32le".into(),
            "pipe:1".into(),
        ])
        .collect::<Vec<OsString>>(),
        Pipe::Out,
    )?;
    // The encoders start with the first piece's output, once the source's count is known.
    let mut encoders: Option<(Ffmpeg, Ffmpeg)> = None;

    let estimate = probe
        .duration
        .map(|d| layout.count((d * f64::from(rate)).round() as u64))
        .unwrap_or(1)
        .max(1);
    let decoded = decoder.child.stdout.take().expect("piped");
    let done = run_pieces(
        &mut engine,
        layout,
        decoded,
        estimate,
        started,
        &mut |v, m| {
            if encoders.is_none() {
                let frames = match count.take() {
                    Some(count) => Some(count.frames()?),
                    None => None,
                };
                let start = |dst: &Path| {
                    Ffmpeg::start(tools, &encoder_input(rate, out_args(dst, frames)), Pipe::In)
                };
                encoders = Some((start(&files.0[0])?, start(&files.0[1])?));
            }
            let (voice, music) = encoders.as_mut().expect("started");
            voice.write(v)?;
            music.write(m)
        },
        progress,
    )?;
    decoder.finish("decoding")?;
    let Some((mut voice, mut music)) = encoders.take().filter(|_| done > 0) else {
        bail!("{src_name}: ffmpeg decoded no audio");
    };
    voice.finish("encoding the voice")?;
    music.finish("encoding the music")?;

    before_publish()?;
    let (voice, music) = super::publish_pair(
        fs,
        (&files.0[0], &voice_name),
        (&files.0[1], &music_name),
        dir,
    )?;
    Ok(Published {
        voice,
        music,
        notes,
    })
}

/// What the piece loop needs of an engine: one stereo window separated (planar, each stem as
/// long as the window). [`Engine`] is the real one; the tests have a stand-in.
pub(super) trait Separate {
    fn separate(&mut self, left: &[f32], right: &[f32]) -> Result<Separated>;
}

impl Separate for Engine {
    fn separate(&mut self, left: &[f32], right: &[f32]) -> Result<Separated> {
        Engine::separate(self, left, right)
    }
}

/// Where the kept part of each window goes: interleaved f32le bytes, voice then music.
pub(super) type Sink<'a> = &'a mut dyn FnMut(&[u8], &[u8]) -> Result<()>;

/// The piece loop: reads interleaved stereo f32le frames from `input` only as far as the next
/// window needs, separates each window ([`Layout::span`]) and hands the kept part of each stem
/// (interleaved bytes, voice then music) to `out`; reports progress from `estimate` pieces
/// (exact once the input ends). Returns how many pieces there were.
pub(super) fn run_pieces(
    engine: &mut dyn Separate,
    layout: Layout,
    input: impl Read,
    estimate: u64,
    started: Instant,
    out: Sink<'_>,
    progress: &mut dyn FnMut(&PieceProgress),
) -> Result<u64> {
    let mut frames = Frames::new(input);
    // Interleaved stereo; `base` = the file frame `buf` starts at.
    let mut buf: Vec<f32> = Vec::new();
    let mut base = 0u64;
    let mut total = estimate.max(1);
    progress(&PieceProgress {
        done: 0,
        total,
        elapsed_s: 0,
        remaining_s: None,
    });
    let mut done = 0u64;
    loop {
        let need = (done + 1) * layout.piece + layout.context;
        frames
            .fill(&mut buf, (need - base) as usize)
            .context("reading the decoded audio")?;
        let have = base + (buf.len() / 2) as u64;
        let end = if frames.eof { have } else { need };
        if frames.eof {
            total = layout.count(have).max(1);
        }
        let Some(span) = layout.span(done, end) else {
            break;
        };
        let (left, right) = window(&buf, base, &span);
        let separated = engine.separate(&left, &right)?;
        let keep =
            layout.context as usize..(layout.context + span.core.end - span.core.start) as usize;
        let mut kept = Vec::with_capacity(2);
        for stem in [&separated.voice, &separated.music] {
            if stem[0].len() < keep.end || stem[1].len() < keep.end {
                bail!(
                    "the separation engine returned {} samples for a window of {}",
                    stem[0].len(),
                    span.window_len()
                );
            }
            kept.push(interleave(&stem[0][keep.clone()], &stem[1][keep.clone()]));
        }
        out(&kept[0], &kept[1])?;
        done += 1;
        total = total.max(done);
        let elapsed = started.elapsed().as_secs_f64();
        progress(&PieceProgress {
            done,
            total,
            elapsed_s: elapsed.round() as u64,
            remaining_s: Some((elapsed / done as f64 * (total - done) as f64).round() as u64),
        });
        // Drop what no later window reads.
        let next = (done * layout.piece)
            .saturating_sub(layout.context)
            .max(base);
        buf.drain(..((next - base) as usize * 2).min(buf.len()));
        base = next;
    }
    Ok(done)
}

/// The window for `span`: its silence and the file's frames from `buf` (frame `base` first),
/// split into left and right.
fn window(buf: &[f32], base: u64, span: &Span) -> (Vec<f32>, Vec<f32>) {
    let len = span.window_len() as usize;
    let (mut left, mut right) = (Vec::with_capacity(len), Vec::with_capacity(len));
    left.resize(span.lead as usize, 0.0);
    right.resize(span.lead as usize, 0.0);
    let from = (span.read.start - base) as usize * 2;
    let to = (span.read.end - base) as usize * 2;
    for [l, r] in buf[from..to].as_chunks::<2>().0 {
        left.push(*l);
        right.push(*r);
    }
    left.resize(len, 0.0);
    right.resize(len, 0.0);
    (left, right)
}

fn interleave(left: &[f32], right: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(left.len() * 8);
    for (l, r) in left.iter().zip(right) {
        bytes.extend_from_slice(&l.to_le_bytes());
        bytes.extend_from_slice(&r.to_le_bytes());
    }
    bytes
}

/// The first audio stream's sample rate and channel count.
struct AudioFormat {
    rate: Option<u32>,
    channels: u32,
}

fn audio_format(tools: &Tools, src: &Path) -> Result<AudioFormat> {
    let out = Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_rate,channels",
            "-of",
            "json",
        ])
        .arg(src)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run {}", tools.ffprobe.display()))?;
    if !out.status.success() {
        bail!("ffprobe cannot read {}", src.display());
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("ffprobe printed bad JSON")?;
    let stream = &v["streams"][0];
    let rate = stream["sample_rate"]
        .as_str()
        .and_then(|r| r.parse().ok())
        .filter(|r| *r > 0);
    let channels = stream["channels"].as_u64().unwrap_or(2) as u32;
    Ok(AudioFormat { rate, channels })
}

/// How many samples per channel the source's first audio track decodes to at its own rate: a
/// decode of its own (to one 8-bit channel, the least to count) whose output is counted.
struct SampleCount {
    ffmpeg: Ffmpeg,
    bytes: JoinHandle<std::io::Result<u64>>,
}

impl SampleCount {
    fn start(tools: &Tools, src: &Path) -> Result<Self> {
        let args: Vec<OsString> = vec![
            "-i".into(),
            src.as_os_str().to_owned(),
            "-map".into(),
            "0:a:0".into(),
            "-vn".into(),
            "-sn".into(),
            "-dn".into(),
            "-ac".into(),
            "1".into(),
            "-c:a".into(),
            "pcm_u8".into(),
            "-f".into(),
            "u8".into(),
            "pipe:1".into(),
        ];
        let mut ffmpeg = Ffmpeg::start(tools, &args, Pipe::Out)?;
        let mut out = ffmpeg.child.stdout.take().expect("piped");
        let bytes = std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));
        Ok(Self { ffmpeg, bytes })
    }

    fn frames(mut self) -> Result<u64> {
        let counted = self
            .bytes
            .join()
            .map_err(|_| anyhow::anyhow!("the count panicked"))?;
        self.ffmpeg.finish("counting the source's samples")?;
        counted.context("counting the source's samples")
    }
}

/// The encoder's input: raw stereo f32 at `rate` on stdin, then `out`.
fn encoder_input(rate: u32, out: Vec<OsString>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "-f".into(),
        "f32le".into(),
        "-ar".into(),
        rate.to_string().into(),
        "-ac".into(),
        "2".into(),
        "-i".into(),
        "pipe:0".into(),
    ];
    args.extend(out);
    args
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pipe {
    In,
    Out,
}

/// One ffmpeg child with the tail of its stderr kept; killed if it is dropped unfinished.
struct Ffmpeg {
    child: Child,
    stdin: Option<ChildStdin>,
    errors: Option<JoinHandle<Tail>>,
}

/// How much of a stderr stream is kept: the last lines, at most this many of them and this many
/// bytes, and a line that never ends cut to its first bytes. ffmpeg prints an error per damaged
/// frame for as long as it runs; only the last line is ever reported.
const TAIL_LINES: usize = 64;
const TAIL_BYTES: usize = 16 * 1024;

/// The bounded tail of a stream ([`TAIL_LINES`], [`TAIL_BYTES`]); the whole stream is read.
type Tail = crate::ffmpeg::Tail<TAIL_LINES, TAIL_BYTES>;

impl Ffmpeg {
    fn start(tools: &Tools, args: &[OsString], pipe: Pipe) -> Result<Self> {
        let mut cmd = Command::new(&tools.ffmpeg);
        cmd.args([
            "-hide_banner",
            "-nostdin",
            "-nostats",
            "-loglevel",
            "error",
            "-y",
        ])
        .args(args)
        .stdin(if pipe == Pipe::In {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(if pipe == Pipe::Out {
            Stdio::piped()
        } else {
            Stdio::null()
        })
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
        let stderr = child.stderr.take().expect("piped");
        let errors = Tail::spawn(stderr);
        let stdin = child.stdin.take();
        Ok(Self {
            child,
            stdin,
            errors: Some(errors),
        })
    }

    /// Writes to the encoder; a broken pipe is reported with what ffmpeg said.
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let stdin = self.stdin.as_mut().expect("an encoder");
        if let Err(e) = stdin.write_all(bytes) {
            self.stdin = None;
            let said = self.wait_said();
            bail!("ffmpeg stopped encoding ({e}): {said}");
        }
        Ok(())
    }

    fn wait_said(&mut self) -> String {
        let _ = self.child.wait();
        self.errors
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default()
            .last_line()
    }

    /// Closes stdin and waits; a failure is the last line ffmpeg printed.
    fn finish(&mut self, what: &str) -> Result<()> {
        self.stdin = None;
        let status = self.child.wait()?;
        let said = self.wait_said();
        if !status.success() {
            bail!("ffmpeg failed {what} ({status}): {said}");
        }
        Ok(())
    }
}

impl Drop for Ffmpeg {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The local encodes, removed however the separation ends (published copies are elsewhere).
struct WorkFiles(Vec<PathBuf>);

impl Drop for WorkFiles {
    fn drop(&mut self) {
        for f in &self.0 {
            let _ = std::fs::remove_file(f);
        }
    }
}

/// Interleaved stereo f32le frames from a pipe.
struct Frames<R> {
    r: R,
    carry: Vec<u8>,
    chunk: Vec<u8>,
    eof: bool,
}

impl<R: Read> Frames<R> {
    fn new(r: R) -> Self {
        Self {
            r,
            carry: Vec::new(),
            chunk: vec![0; 1 << 20],
            eof: false,
        }
    }

    /// Appends whole frames to `buf` until it holds `want` frames or the input ends.
    fn fill(&mut self, buf: &mut Vec<f32>, want: usize) -> std::io::Result<()> {
        while !self.eof && buf.len() / 2 < want {
            let n = self.r.read(&mut self.chunk)?;
            if n == 0 {
                self.eof = true;
                break;
            }
            self.carry.extend_from_slice(&self.chunk[..n]);
            let whole = self.carry.len() / 8 * 8;
            buf.extend(
                self.carry[..whole]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b)),
            );
            self.carry.drain(..whole);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in ffmpeg: `/bin/sh` running `body`.
    fn fake_ffmpeg(dir: &Path, body: &str) -> Tools {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-ffmpeg");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Tools {
            ffmpeg: path.clone(),
            ffprobe: path,
        }
    }

    /// `Ffmpeg::start`, retried while Linux calls the just-written script busy (ETXTBSY).
    fn start(tools: &Tools) -> Ffmpeg {
        for _ in 0..50 {
            match Ffmpeg::start(tools, &[], Pipe::Out) {
                Ok(f) => return f,
                Err(e)
                    if e.chain().any(|c| {
                        c.downcast_ref::<std::io::Error>()
                            .is_some_and(|io| io.kind() == std::io::ErrorKind::ExecutableFileBusy)
                    }) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                Err(e) => panic!("{e:#}"),
            }
        }
        panic!("the stand-in stayed busy");
    }

    /// However much is fed — 50 MB of error lines, then a 1 MiB line that never ends — the tail
    /// keeps at most its bound, ends with the last line, and cuts the endless one.
    #[test]
    fn a_tail_stays_bounded() {
        struct Endless(u64);
        impl Read for Endless {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.0 == 0 {
                    return Ok(0);
                }
                let line = b"frame 123456: invalid data found when processing input\n";
                let n = out.len().min(line.len()).min(self.0 as usize);
                out[..n].copy_from_slice(&line[..n]);
                self.0 -= n as u64;
                Ok(n)
            }
        }
        // Whole lines (each read gives one: it is shorter than a read's buffer).
        let line_len = "frame 123456: invalid data found when processing input\n".len() as u64;
        let tail = Tail::read(Endless(line_len * 900_000));
        assert!(tail.kept() <= TAIL_BYTES, "{} bytes kept", tail.kept());
        assert!(tail.lines.len() <= TAIL_LINES);
        assert_eq!(
            tail.last_line(),
            "frame 123456: invalid data found when processing input"
        );
        let mut endless = Tail::default();
        endless.feed(b"first\n");
        for _ in 0..1024 {
            endless.feed(&[b'x'; 1024]);
            assert!(
                endless.kept() <= 2 * TAIL_BYTES,
                "{} bytes kept",
                endless.kept()
            );
        }
        endless.end_line();
        let last = endless.last_line();
        assert_eq!(last.len(), TAIL_BYTES + '…'.len_utf8());
        assert!(last.ends_with("x…"));
        assert_eq!(Tail::default().last_line(), "no error output");
    }

    /// A damaged input makes ffmpeg print an error per frame for as long as it runs: only a
    /// bounded tail of it is kept, and the error is its last line — cut short when that line
    /// is itself endless.
    #[test]
    fn a_long_error_stream_is_kept_as_a_bounded_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let repeated = fake_ffmpeg(
            tmp.path(),
            "i=0; while [ $i -lt 20000 ]; do echo \"frame $i: invalid data\" >&2; i=$((i+1)); done
exit 1",
        );
        let err = start(&repeated).finish("decoding").unwrap_err().to_string();
        assert!(err.ends_with("frame 19999: invalid data"), "{err}");

        let endless = fake_ffmpeg(
            tmp.path(),
            "echo 'frame 0: invalid data' >&2
head -c 1048576 /dev/zero | tr '\\0' x >&2
exit 1",
        );
        let err = start(&endless).finish("decoding").unwrap_err().to_string();
        assert!(err.len() < 20_000, "a {} byte error", err.len());
        assert!(err.contains("xxxx"), "{err}");
    }

    #[test]
    fn frames_come_whole_across_odd_reads() {
        // 3 frames, delivered 5 bytes at a time.
        let data: Vec<f32> = vec![0.5, -0.5, 0.25, -0.25, 1.0, -1.0];
        let bytes: Vec<u8> = data.iter().flat_map(|f| f.to_le_bytes()).collect();
        struct Dribble(Vec<u8>, usize);
        impl Read for Dribble {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let n = (self.0.len() - self.1).min(5).min(out.len());
                out[..n].copy_from_slice(&self.0[self.1..self.1 + n]);
                self.1 += n;
                Ok(n)
            }
        }
        let mut frames = Frames::new(Dribble(bytes, 0));
        let mut buf = Vec::new();
        frames.fill(&mut buf, 2).unwrap();
        assert_eq!(buf.len() / 2, 2);
        assert!(!frames.eof);
        frames.fill(&mut buf, 10).unwrap();
        assert!(frames.eof);
        assert_eq!(buf, data);
    }

    #[test]
    fn a_window_is_its_silence_and_its_frames() {
        let layout = Layout {
            piece: 3,
            context: 2,
        };
        // Frames 0..7 as (f, -f).
        let buf: Vec<f32> = (0..7).flat_map(|f| [f as f32, -(f as f32)]).collect();
        let first = layout.span(0, 5).unwrap();
        let (l, r) = window(&buf, 0, &first);
        assert_eq!(l, [0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!(r, [0.0, 0.0, 0.0, -1.0, -2.0, -3.0, -4.0]);
        // The last piece of a 7-frame file, read from a buffer that starts at frame 4.
        let last = layout.span(2, 7).unwrap();
        let (l, _) = window(&buf[8..], 4, &last);
        assert_eq!(l, [4.0, 5.0, 6.0, 0.0, 0.0]);
    }
}
