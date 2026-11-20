//! The separation itself (feature `stems`): ffmpeg decodes the first audio track to interleaved
//! stereo f32 at the model's rate on a pipe; the engine separates it window by window
//! ([`Layout`]), reading only as far ahead as the next window needs; the kept part of each
//! window goes straight to two ffmpeg encoders on pipes. Memory is one window and the model,
//! however long the track.

use super::engine::{Engine, Separated};
use super::{media_kind_of, stem_names, Layout, PieceProgress, Published, Span};
use crate::ffmpeg::{self, MediaKind, Tools};
use crate::kind::{self, Kind};
use crate::remote::{self, RemoteFs};
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
    let out_args = |dst: &Path| {
        let mut args: Vec<OsString> = if mono {
            vec!["-af".into(), "pan=mono|c0=0.5*c0+0.5*c1".into()]
        } else {
            vec!["-ac".into(), "2".into()]
        };
        if let Some(r) = format.rate.filter(|r| *r != rate) {
            args.extend(["-ar".into(), r.to_string().into()]);
        }
        args.extend(encoder.iter().cloned());
        args.push(dst.as_os_str().to_owned());
        args
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
            "-ar".into(),
            rate.to_string().into(),
            "-f".into(),
            "f32le".into(),
            "-c:a".into(),
            "pcm_f32le".into(),
            "pipe:1".into(),
        ],
        Pipe::Out,
    )?;
    let mut voice = Ffmpeg::start(tools, &encoder_input(rate, out_args(&files.0[0])), Pipe::In)?;
    let mut music = Ffmpeg::start(tools, &encoder_input(rate, out_args(&files.0[1])), Pipe::In)?;

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
            voice.write(v)?;
            music.write(m)
        },
        progress,
    )?;
    decoder.finish("decoding")?;
    if done == 0 {
        bail!("{src_name}: ffmpeg decoded no audio");
    }
    voice.finish("encoding the voice")?;
    music.finish("encoding the music")?;

    before_publish()?;
    let voice_published = fs.write_new_sibling(&files.0[0], dir, &voice_name)?;
    let music_published = fs
        .write_new_sibling(&files.0[1], dir, &music_name)
        .map_err(|e| {
            anyhow::anyhow!(
                "{e:#} — the voice was published as {}, the music was not",
                remote::join(dir, &voice_published)
            )
        })?;
    Ok(Published {
        voice: voice_published,
        music: music_published,
        notes,
    })
}

/// What the piece loop needs of an engine: one stereo window separated (planar, each stem as
/// long as the window). [`Engine`] is the real one; the tests have a stand-in.
trait Separate {
    fn separate(&mut self, left: &[f32], right: &[f32]) -> Result<Separated>;
}

impl Separate for Engine {
    fn separate(&mut self, left: &[f32], right: &[f32]) -> Result<Separated> {
        Engine::separate(self, left, right)
    }
}

/// Where the kept part of each window goes: interleaved f32le bytes, voice then music.
type Sink<'a> = &'a mut dyn FnMut(&[u8], &[u8]) -> Result<()>;

/// The piece loop: reads interleaved stereo f32le frames from `input` only as far as the next
/// window needs, separates each window ([`Layout::span`]) and hands the kept part of each stem
/// (interleaved bytes, voice then music) to `out`; reports progress from `estimate` pieces
/// (exact once the input ends). Returns how many pieces there were.
fn run_pieces(
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
#[derive(Debug, Default)]
struct Tail {
    lines: std::collections::VecDeque<String>,
    bytes: usize,
    partial: Vec<u8>,
    cut: bool,
}

impl Tail {
    /// Reads `r` to its end (a read error ends it too), keeping the tail.
    fn read(mut r: impl Read) -> Self {
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

    fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let (part, ended) = match bytes.iter().position(|&b| b == b'\n') {
                Some(i) => (&bytes[..i], Some(i + 1)),
                None => (bytes, None),
            };
            let room = TAIL_BYTES.saturating_sub(self.partial.len());
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

    fn end_line(&mut self) {
        let mut line = String::from_utf8_lossy(&self.partial).into_owned();
        if self.cut {
            line.push('…');
        }
        self.partial.clear();
        self.cut = false;
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.lines.len() > TAIL_LINES || (self.bytes > TAIL_BYTES && self.lines.len() > 1) {
            if let Some(gone) = self.lines.pop_front() {
                self.bytes -= gone.len();
            }
        }
    }

    /// The bytes kept (lines and the unfinished one).
    #[cfg(test)]
    fn kept(&self) -> usize {
        self.bytes + self.partial.len()
    }

    /// The last line with something on it.
    fn last_line(&self) -> String {
        self.lines
            .iter()
            .rev()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("no error output")
            .to_string()
    }
}

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
        let errors = std::thread::spawn(move || Tail::read(stderr));
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

    /// A stand-in engine whose voice at each sample is the mean of the window's samples on both
    /// sides of it (silence past the window's edges) — so it depends on context — and whose
    /// music is the window minus that. It keeps every window it was given.
    #[derive(Default)]
    struct Neighbours {
        windows: Vec<[Vec<f32>; 2]>,
    }

    impl Separate for Neighbours {
        fn separate(&mut self, left: &[f32], right: &[f32]) -> Result<Separated> {
            let voice = |w: &[f32]| -> Vec<f32> {
                (0..w.len())
                    .map(|i| {
                        let before = if i > 0 { w[i - 1] } else { 0.0 };
                        let after = w.get(i + 1).copied().unwrap_or(0.0);
                        0.5 * (before + after)
                    })
                    .collect()
            };
            let (vl, vr) = (voice(left), voice(right));
            let music = |w: &[f32], v: &[f32]| -> Vec<f32> {
                w.iter().zip(v).map(|(a, b)| a - b).collect()
            };
            let separated = Separated {
                music: [music(left, &vl), music(right, &vr)],
                voice: [vl, vr],
            };
            self.windows.push([left.to_vec(), right.to_vec()]);
            Ok(separated)
        }
    }

    /// Reads at most 13 bytes at a time: frames arrive cut anywhere.
    struct Ragged(std::io::Cursor<Vec<u8>>);
    impl Read for Ragged {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = out.len().min(13);
            self.0.read(&mut out[..n])
        }
    }

    /// Through the real piece loop with a stand-in that depends on context: every window is the
    /// file's own frames around its piece (silence only past the file's ends), and every kept
    /// sample is the stand-in's answer for that very sample with its real neighbours — at the
    /// joins between pieces too; the last piece is the shorter rest.
    #[test]
    fn the_piece_loop_gives_each_window_its_real_context_and_keeps_the_right_samples() {
        let n = 30usize;
        let x = |c: usize, t: usize| -> f32 {
            if c == 0 {
                (t as f32 * 0.37).sin() + 0.01 * t as f32
            } else {
                (t as f32 * 0.91).cos() - 0.02 * t as f32
            }
        };
        let bytes: Vec<u8> = (0..n)
            .flat_map(|t| [x(0, t), x(1, t)])
            .flat_map(f32::to_le_bytes)
            .collect();
        let layout = Layout {
            piece: 7,
            context: 3,
        };
        // ffprobe's estimate (5), and one too low (1): the total is never below what is done,
        // and exact once the input ends (it is read one window ahead, so at piece 4).
        let mut engine = Neighbours::default();
        let (mut voice, mut music) = (Vec::new(), Vec::new());
        for (estimate, want) in [
            (5, [(0, 5), (1, 5), (2, 5), (3, 5), (4, 5), (5, 5)]),
            (1, [(0, 1), (1, 1), (2, 2), (3, 3), (4, 5), (5, 5)]),
        ] {
            engine = Neighbours::default();
            voice.clear();
            music.clear();
            let mut seen = Vec::new();
            let done = run_pieces(
                &mut engine,
                layout,
                Ragged(std::io::Cursor::new(bytes.clone())),
                estimate,
                Instant::now(),
                &mut |v, m| {
                    voice.extend_from_slice(v);
                    music.extend_from_slice(m);
                    Ok(())
                },
                &mut |p| seen.push((p.done, p.total)),
            )
            .unwrap();
            assert_eq!(done, 5, "7 + 7 + 7 + 7 + 2");
            assert_eq!(seen, want, "estimate {estimate}");
        }
        // The windows: the piece and 3 frames on each side, the file's own where it has them.
        let at = |c: usize, t: isize| -> f32 {
            if t < 0 || t >= n as isize {
                0.0
            } else {
                x(c, t as usize)
            }
        };
        assert_eq!(engine.windows.len(), 5);
        for (k, w) in engine.windows.iter().enumerate() {
            let start = (k * 7) as isize - 3;
            let core = if k == 4 { 2 } else { 7 };
            for (c, channel) in w.iter().enumerate() {
                let want: Vec<f32> = (0..core as isize + 6).map(|i| at(c, start + i)).collect();
                assert_eq!(channel, &want, "window {k} channel {c}");
            }
        }
        // The kept samples: each one the stand-in's answer with its real neighbours.
        let decode = |b: &[u8]| -> Vec<f32> {
            b.as_chunks::<4>()
                .0
                .iter()
                .map(|q| f32::from_le_bytes(*q))
                .collect()
        };
        let (voice, music) = (decode(&voice), decode(&music));
        assert_eq!(voice.len(), 2 * n);
        assert_eq!(music.len(), 2 * n);
        for t in 0..n as isize {
            for c in 0..2 {
                let v = 0.5 * (at(c, t - 1) + at(c, t + 1));
                let i = 2 * t as usize + c;
                assert_eq!(voice[i], v, "voice at {t}/{c}");
                assert_eq!(music[i], at(c, t) - v, "music at {t}/{c}");
            }
        }
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
