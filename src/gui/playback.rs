//! In-window playback (spec §7, §11). Two ffmpeg subprocesses decode from the play position —
//! video to RGBA frames (≤ 1280 wide, forced to a constant frame rate so a frame's time is its
//! index over the rate), audio to f32 PCM at the output device's rate and channel count — and
//! the audio output is the master clock. PCM reaches the device callback through a lock-free
//! ring ([`Mixer`]); the clock is what the device has started playing: the frames handed out
//! before the latest callback, plus the time since that callback, minus the device's output
//! latency ([`heard_seconds`]).
//!
//! When there is no audio stream, the device cannot be opened, or it stops asking for audio (no
//! callback for [`STALL`]), the clock continues on the wall clock from where it was: video keeps
//! playing silently and [`Player::audio_note`] says why.
//!
//! The clock, the mixer and the frame picker are plain values driven by explicit timestamps, so
//! the tests run them without a window, a device or ffmpeg; [`Sink::Null`] is the device-free
//! output for the tests that decode real media.

use crate::ffmpeg::Tools;
use anyhow::{anyhow, bail, Context as _, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Widest decode for display; the crop rectangle still addresses source pixels.
pub const MAX_DECODE_WIDTH: u32 = 1280;
/// No device callback for this long while playing = the device is gone; switch to wall clock.
pub const STALL: Duration = Duration::from_millis(500);
/// Audio decoded before the clock starts.
const PREROLL_S: f64 = 0.1;
/// Ring capacity, in seconds of audio.
const RING_S: f64 = 1.0;
/// Interpolation since the last callback is capped: a late callback must not run the clock on.
const MAX_INTERPOLATE_S: f64 = 0.05;

/// Display decode size: at most [`MAX_DECODE_WIDTH`] wide, aspect kept, both sides even.
pub fn decode_size(w: u32, h: u32) -> (u32, u32) {
    let dw = (w.min(MAX_DECODE_WIDTH) & !1).max(2);
    let dh = ((f64::from(h) * f64::from(dw) / f64::from(w.max(1))).round() as u32 & !1).max(2);
    (dw, dh)
}

fn ffmpeg_cmd(tools: &Tools) -> Command {
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    cmd
}

fn kill(child: &Mutex<Option<Child>>) {
    if let Some(mut c) = child.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Seconds of audio the listener has heard. `before` = frames handed to the device before the
/// latest callback's buffer, `played` = including it; the latest buffer starts sounding
/// `latency_s` after its callback. Never more than was handed out.
pub fn heard_seconds(before: u64, played: u64, rate: u32, since_cb_s: f64, latency_s: f64) -> f64 {
    if played == 0 || rate == 0 {
        return 0.0;
    }
    let rate = f64::from(rate);
    let at = before as f64 / rate + since_cb_s.clamp(0.0, MAX_INTERPOLATE_S) - latency_s;
    at.clamp(0.0, played as f64 / rate)
}

/// Counters shared by the device callback (writer) and the UI (reader). Times are nanoseconds
/// since `epoch`.
#[derive(Debug)]
pub struct Shared {
    epoch: Instant,
    pub frames_played: AtomicU64,
    frames_before_cb: AtomicU64,
    pub callbacks: AtomicU64,
    /// Callbacks that ran out of decoded audio while playing (before the decoder finished).
    pub underruns: AtomicU64,
    pub xruns: AtomicU64,
    last_cb_ns: AtomicU64,
    latency_ns: AtomicU64,
    /// Samples (not frames) the decoder has pushed.
    pub pushed: AtomicU64,
    /// Output is silence until the clock starts (pre-roll).
    started: AtomicBool,
    /// The decoder reached the end of its input.
    pub eof: AtomicBool,
}

impl Shared {
    pub fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            frames_played: AtomicU64::new(0),
            frames_before_cb: AtomicU64::new(0),
            callbacks: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            xruns: AtomicU64::new(0),
            last_cb_ns: AtomicU64::new(0),
            latency_ns: AtomicU64::new(0),
            pushed: AtomicU64::new(0),
            started: AtomicBool::new(false),
            eof: AtomicBool::new(false),
        }
    }

    pub fn ns(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.epoch).as_nanos() as u64
    }

    pub fn start(&self) {
        self.started.store(true, SeqCst);
    }

    /// When the device last asked for audio.
    pub fn last_callback(&self) -> Option<Instant> {
        (self.callbacks.load(SeqCst) > 0)
            .then(|| self.epoch + Duration::from_nanos(self.last_cb_ns.load(SeqCst)))
    }

    pub fn heard(&self, rate: u32, now: Instant) -> f64 {
        let since = self.ns(now).saturating_sub(self.last_cb_ns.load(SeqCst)) as f64 / 1e9;
        let latency = self.latency_ns.load(SeqCst) as f64 / 1e9;
        heard_seconds(
            self.frames_before_cb.load(SeqCst),
            self.frames_played.load(SeqCst),
            rate,
            since,
            latency,
        )
    }
}

/// The device side of the ring: fills each output buffer and counts what it played.
pub struct Mixer {
    cons: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
    gain: f32,
    channels: usize,
}

impl Mixer {
    pub fn new(cons: rtrb::Consumer<f32>, shared: Arc<Shared>, gain: f32, channels: u16) -> Self {
        Self {
            cons,
            shared,
            gain,
            channels: usize::from(channels.max(1)),
        }
    }

    /// One device callback at `now_ns` (since the shared epoch) asking for `out.len()` samples
    /// that start sounding `latency_ns` later.
    pub fn render(&mut self, out: &mut [f32], latency_ns: u64, now_ns: u64) {
        let s = &self.shared;
        s.callbacks.fetch_add(1, SeqCst);
        if !s.started.load(SeqCst) {
            out.fill(0.0);
            s.last_cb_ns.store(now_ns, SeqCst);
            return;
        }
        let want = out.len() - out.len() % self.channels;
        // Whole frames only: the decoder can be mid-frame, and a split frame would swap the
        // channels for the rest of the session.
        let mut n = self.cons.slots().min(want);
        n -= n % self.channels;
        let (filled, _) = self.cons.pop_partial_slice(&mut out[..n]);
        debug_assert_eq!(filled.len(), n);
        if self.gain != 1.0 {
            out[..n].iter_mut().for_each(|x| *x *= self.gain);
        }
        out[n..].fill(0.0);
        if n < want && !s.eof.load(SeqCst) {
            s.underruns.fetch_add(1, SeqCst);
        }
        let before = s
            .frames_played
            .fetch_add((n / self.channels) as u64, SeqCst);
        s.frames_before_cb.store(before, SeqCst);
        s.latency_ns.store(latency_ns, SeqCst);
        s.last_cb_ns.store(now_ns, SeqCst);
    }
}

/// Where decoded audio goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    /// The default output device.
    Device,
    /// No device: a thread asks for 512-frame buffers at real-time pace (48 kHz stereo).
    Null,
}

enum Output {
    /// Kept alive for as long as playback runs; dropping it stops the device.
    Device(#[allow(dead_code)] cpal::Stream),
    Null,
}

/// Where the PCM comes from: ffmpeg from a position (playback), or a WAV file read in-process
/// (`--selftest-audio` on a machine without ffmpeg).
pub enum Decode<'a> {
    Ffmpeg {
        tools: &'a Tools,
        path: &'a Path,
        t0: f64,
    },
    Wav(&'a Path),
}

/// A PCM WAV (16/24/32-bit integer or 32-bit float) as interleaved f32 at `rate` × `channels`:
/// channels are mapped round-robin, the rate is converted by nearest sample — enough to drive
/// a device check, not for listening.
pub fn wav_to_f32(bytes: &[u8], rate: u32, channels: u16) -> Result<Vec<f32>> {
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at =
        |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("not a WAV file");
    }
    let (mut fmt, mut data) = (None, None);
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32_at(i + 4) as usize;
        let body = i + 8..(i + 8 + len).min(bytes.len());
        match id {
            b"fmt " if body.len() >= 16 => fmt = Some(body.start),
            b"data" => data = Some(body.clone()),
            _ => {}
        }
        i = body.end + (len & 1);
    }
    let (Some(f), Some(data)) = (fmt, data) else {
        bail!("WAV without fmt or data chunk");
    };
    let mut format = u16_at(f);
    if format == 0xFFFE && f + 26 <= bytes.len() {
        format = u16_at(f + 24); // WAVE_FORMAT_EXTENSIBLE: the sub-format GUID starts with it
    }
    let (in_ch, in_rate, bits) = (usize::from(u16_at(f + 2)), u32_at(f + 4), u16_at(f + 14));
    let width = usize::from(bits / 8);
    if in_ch == 0 || in_rate == 0 || width == 0 {
        bail!("WAV with an empty format");
    }
    let sample = |k: usize| -> Option<f32> {
        let b = bytes.get(data.start + k * width..data.start + (k + 1) * width)?;
        Some(match (format, bits) {
            (1, 16) => f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0,
            (1, 24) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
            (1, 32) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
            (3, 32) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            _ => return None,
        })
    };
    if !matches!((format, bits), (1, 16) | (1, 24) | (1, 32) | (3, 32)) {
        bail!("WAV format {format} with {bits}-bit samples is not supported");
    }
    let in_frames = data.len() / width / in_ch;
    let out_frames = (in_frames as u64 * u64::from(rate) / u64::from(in_rate)) as usize;
    let ch = usize::from(channels.max(1));
    let mut out = Vec::with_capacity(out_frames * ch);
    for j in 0..out_frames {
        let src = (j as u64 * u64::from(in_rate) / u64::from(rate)) as usize;
        for c in 0..ch {
            out.push(sample(src * in_ch + c % in_ch).unwrap_or(0.0));
        }
    }
    Ok(out)
}

/// Audio playback from one position: the decoder, the ring and the output.
pub struct Audio {
    pub shared: Arc<Shared>,
    pub rate: u32,
    pub channels: u16,
    pub device: String,
    _output: Output,
    child: Arc<Mutex<Option<Child>>>,
    stop: Arc<AtomicBool>,
}

fn open_device() -> Result<(cpal::Device, cpal::StreamConfig, String)> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("no audio output device")?;
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "unnamed device".to_string());
    let config = device
        .default_output_config()
        .map_err(|e| anyhow!("audio device {name}: {e}"))?
        .config();
    Ok((device, config, name))
}

impl Audio {
    pub fn start(decode: Decode<'_>, gain: f32, sink: Sink) -> Result<Self> {
        let shared = Arc::new(Shared::new(Instant::now()));
        let stop = Arc::new(AtomicBool::new(false));
        let device = match sink {
            Sink::Device => Some(open_device()?),
            Sink::Null => None,
        };
        let (rate, channels) = device
            .as_ref()
            .map(|(_, c, _)| (c.sample_rate, c.channels))
            .unwrap_or((48_000, 2));
        let capacity = (f64::from(rate) * f64::from(channels) * RING_S) as usize;
        let (prod, cons) = rtrb::RingBuffer::<f32>::new(capacity);
        let mut mixer = Mixer::new(cons, shared.clone(), gain, channels);
        let (output, name) = match device {
            Some((device, config, name)) => {
                let cb_shared = shared.clone();
                let err_shared = shared.clone();
                let stream = device
                    .build_output_stream::<f32, _, _>(
                        config,
                        move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                            let ts = info.timestamp();
                            let latency = ts.playback.duration_since(ts.callback).as_nanos();
                            let now = cb_shared.ns(Instant::now());
                            mixer.render(data, latency as u64, now);
                        },
                        move |e: cpal::Error| {
                            if matches!(e.kind(), cpal::ErrorKind::Xrun) {
                                err_shared.xruns.fetch_add(1, SeqCst);
                            } else {
                                crate::debug!("audio stream error: {e}");
                            }
                        },
                        None,
                    )
                    .map_err(|e| anyhow!("audio device {name}: {e}"))?;
                stream
                    .play()
                    .map_err(|e| anyhow!("audio device {name}: {e}"))?;
                (Output::Device(stream), name)
            }
            None => {
                let (quit, pace) = (stop.clone(), shared.clone());
                std::thread::spawn(move || {
                    let mut buf = vec![0f32; 512 * usize::from(channels)];
                    let period = Duration::from_secs_f64(512.0 / f64::from(rate));
                    let begin = Instant::now();
                    let mut k = 0u32;
                    while !quit.load(SeqCst) {
                        mixer.render(&mut buf, 0, pace.ns(Instant::now()));
                        k += 1;
                        if let Some(d) = (begin + period * k).checked_duration_since(Instant::now())
                        {
                            std::thread::sleep(d);
                        }
                    }
                });
                (Output::Null, "null".to_string())
            }
        };
        let (mut out, child): (Box<dyn Read + Send>, Option<Child>) = match decode {
            Decode::Ffmpeg { tools, path, t0 } => {
                let mut cmd = ffmpeg_cmd(tools);
                cmd.args(["-ss", &format!("{t0:.3}"), "-i"])
                    .arg(path)
                    .args(["-vn", "-sn", "-f", "f32le", "-ac"])
                    .arg(channels.to_string())
                    .arg("-ar")
                    .arg(rate.to_string())
                    .arg("-");
                let mut child = cmd
                    .spawn()
                    .with_context(|| format!("cannot run {}", tools.ffmpeg.display()))?;
                (Box::new(child.stdout.take().expect("piped")), Some(child))
            }
            Decode::Wav(path) => {
                let bytes = std::fs::read(path)
                    .with_context(|| format!("cannot read {}", path.display()))?;
                let pcm: Vec<u8> = wav_to_f32(&bytes, rate, channels)?
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect();
                (Box::new(std::io::Cursor::new(pcm)), None)
            }
        };
        let child = Arc::new(Mutex::new(child));
        let (dec_shared, dec_stop) = (shared.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut prod = prod;
            let mut bytes = vec![0u8; 16 * 1024];
            let mut carry: Vec<u8> = Vec::new();
            let mut samples: Vec<f32> = Vec::new();
            'read: loop {
                let n = match out.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                carry.extend_from_slice(&bytes[..n]);
                let whole = carry.len() - carry.len() % 4;
                samples.clear();
                samples.extend(
                    carry[..whole]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b)),
                );
                carry.drain(..whole);
                let mut rest = samples.as_slice();
                while !rest.is_empty() {
                    let (pushed, left) = prod.push_partial_slice(rest);
                    dec_shared.pushed.fetch_add(pushed.len() as u64, SeqCst);
                    rest = left;
                    if !rest.is_empty() {
                        if dec_stop.load(SeqCst) {
                            break 'read;
                        }
                        std::thread::sleep(Duration::from_millis(3));
                    }
                }
            }
            dec_shared.eof.store(true, SeqCst);
        });
        Ok(Self {
            shared,
            rate,
            channels,
            device: name,
            _output: output,
            child,
            stop,
        })
    }

    pub fn heard(&self, now: Instant) -> f64 {
        self.shared.heard(self.rate, now)
    }

    /// Enough is decoded to start the clock.
    pub fn primed(&self) -> bool {
        let need = f64::from(self.rate) * f64::from(self.channels) * PREROLL_S;
        self.shared.pushed.load(SeqCst) as f64 >= need || self.shared.eof.load(SeqCst)
    }

    /// Everything decoded has been heard.
    pub fn drained(&self, now: Instant) -> bool {
        let total = self.shared.pushed.load(SeqCst) as f64
            / f64::from(self.channels.max(1))
            / f64::from(self.rate.max(1));
        self.shared.eof.load(SeqCst) && self.heard(now) >= total - 0.02
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        kill(&self.child);
    }
}

/// One decoded video frame at `pts` seconds.
pub struct Frame {
    pub pts: f64,
    pub rgba: Vec<u8>,
}

/// What the decoder has right now.
pub enum Next {
    Frame(Frame),
    Empty,
    Done,
}

/// Video decode from one position: `size` RGBA frames at a constant `fps`.
pub struct Video {
    rx: Receiver<Frame>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Video {
    pub fn start(
        tools: &Tools,
        path: &Path,
        t0: f64,
        fps: f64,
        (w, h): (u32, u32),
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self> {
        let mut cmd = ffmpeg_cmd(tools);
        cmd.args(["-ss", &format!("{t0:.3}"), "-i"])
            .arg(path)
            .args(["-an", "-sn", "-vf"])
            .arg(format!("fps={fps:.6},scale={w}:{h},format=rgba"))
            .args(["-f", "rawvideo", "-"]);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("cannot run {}", tools.ffmpeg.display()))?;
        let mut out = child.stdout.take().expect("piped");
        let child = Arc::new(Mutex::new(Some(child)));
        let (tx, rx) = std::sync::mpsc::sync_channel::<Frame>(6);
        let reader_child = child.clone();
        std::thread::spawn(move || {
            let mut i = 0u64;
            loop {
                let mut rgba = vec![0u8; (w * h * 4) as usize];
                if out.read_exact(&mut rgba).is_err() {
                    break;
                }
                let pts = t0 + i as f64 / fps;
                i += 1;
                if tx.send(Frame { pts, rgba }).is_err() {
                    break;
                }
                wake();
            }
            drop(tx);
            kill(&reader_child);
            wake();
        });
        Ok(Self { rx, child })
    }

    pub fn next(&self) -> Next {
        match self.rx.try_recv() {
            Ok(f) => Next::Frame(f),
            Err(TryRecvError::Empty) => Next::Empty,
            Err(TryRecvError::Disconnected) => Next::Done,
        }
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        kill(&self.child);
    }
}

/// Chooses the frame to show at a clock time: the newest frame due (`pts ≤ clock + half a
/// frame`). Older due frames are dropped (counted); a later one waits as `pending`.
#[derive(Default)]
pub struct Picker {
    pending: Option<Frame>,
    pub shown: u64,
    pub dropped: u64,
    pub done: bool,
}

impl Picker {
    pub fn pick(&mut self, next: &mut dyn FnMut() -> Next, clock: f64, half: f64) -> Option<Frame> {
        let mut show: Option<Frame> = None;
        loop {
            if self.pending.is_none() {
                match next() {
                    Next::Frame(f) => self.pending = Some(f),
                    Next::Empty => break,
                    Next::Done => {
                        self.done = true;
                        break;
                    }
                }
            }
            match &self.pending {
                Some(f) if f.pts <= clock + half => {
                    if show.is_some() {
                        self.dropped += 1;
                    }
                    show = self.pending.take();
                }
                _ => break,
            }
        }
        if show.is_some() {
            self.shown += 1;
        }
        show
    }

    /// Time of the frame waiting to be shown.
    pub fn next_pts(&self) -> Option<f64> {
        self.pending.as_ref().map(|f| f.pts)
    }

    /// Before the clock starts: true once the first frame is decoded (or there is none).
    pub fn prime(&mut self, next: &mut dyn FnMut() -> Next) -> bool {
        if self.pending.is_none() {
            match next() {
                Next::Frame(f) => self.pending = Some(f),
                Next::Done => self.done = true,
                Next::Empty => {}
            }
        }
        self.pending.is_some() || self.done
    }
}

/// What to play.
pub struct Source {
    pub path: PathBuf,
    pub duration: f64,
    /// `(decode size, fps)` when there is a video stream.
    pub video: Option<((u32, u32), f64)>,
    pub audio: bool,
}

/// One pass of [`Player::pump`].
pub struct Pump {
    pub frame: Option<Frame>,
    pub position: f64,
    pub ended: bool,
    /// When to call again (the next frame is due, or a playhead refresh).
    pub wake_in: Duration,
}

/// A playback session from `t0`. Dropping it stops both decoders and the output.
pub struct Player {
    t0: f64,
    duration: f64,
    fps: f64,
    video: Option<Video>,
    audio: Option<Audio>,
    picker: Picker,
    started_at: Option<Instant>,
    /// Wall-clock base `(at, position)` once audio cannot be the clock.
    wall: Option<(Instant, f64)>,
    /// Why there is no sound although the file has audio.
    pub audio_note: Option<String>,
}

impl Player {
    pub fn start(
        tools: &Tools,
        src: &Source,
        t0: f64,
        gain: f32,
        sink: Sink,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self> {
        let t0 = t0.clamp(0.0, src.duration.max(0.0));
        let video = match src.video {
            Some((size, fps)) => Some(Video::start(tools, &src.path, t0, fps, size, wake)?),
            None => None,
        };
        let (audio, audio_note) = if src.audio {
            let decode = Decode::Ffmpeg {
                tools,
                path: &src.path,
                t0,
            };
            match Audio::start(decode, gain, sink) {
                Ok(a) => (Some(a), None),
                Err(e) => {
                    crate::debug!("audio off: {e:#}");
                    (None, Some(format!("no audio device ({e:#})")))
                }
            }
        } else {
            (None, None)
        };
        Ok(Self {
            t0,
            duration: src.duration,
            fps: src.video.map(|(_, fps)| fps).unwrap_or(30.0).max(1.0),
            video,
            audio,
            picker: Picker::default(),
            started_at: None,
            wall: None,
            audio_note,
        })
    }

    /// Playback position at `now`.
    pub fn clock(&self, now: Instant) -> f64 {
        let Some(started) = self.started_at else {
            return self.t0;
        };
        if let Some((at, pos)) = self.wall {
            return pos + now.saturating_duration_since(at).as_secs_f64();
        }
        match &self.audio {
            Some(a) => self.t0 + a.heard(now),
            None => self.t0 + now.saturating_duration_since(started).as_secs_f64(),
        }
    }

    /// Frames shown and dropped so far.
    pub fn stats(&self) -> (u64, u64) {
        (self.picker.shown, self.picker.dropped)
    }

    pub fn pump(&mut self, now: Instant) -> Pump {
        if self.started_at.is_none() {
            let video_ready = match &self.video {
                Some(v) => self.picker.prime(&mut || v.next()),
                None => true,
            };
            let audio_ready = self.audio.as_ref().is_none_or(Audio::primed);
            if !(video_ready && audio_ready) {
                return Pump {
                    frame: None,
                    position: self.t0,
                    ended: false,
                    wake_in: Duration::from_millis(5),
                };
            }
            self.started_at = Some(now);
            if let Some(a) = &self.audio {
                a.shared.start();
            }
        }
        if let (Some(a), None, Some(started)) = (&self.audio, self.wall, self.started_at) {
            let last = a.shared.last_callback().unwrap_or(started).max(started);
            if now.saturating_duration_since(last) > STALL && !a.drained(now) {
                let never = a.shared.callbacks.load(SeqCst) == 0;
                self.wall = Some((now, self.clock(now)));
                self.audio_note = Some(if never {
                    format!("no audio device ({} never started)", a.device)
                } else {
                    format!("audio device {} stopped", a.device)
                });
            }
        }
        let clock = self.clock(now);
        let half = 0.5 / self.fps;
        let frame = match &self.video {
            Some(v) => self.picker.pick(&mut || v.next(), clock, half),
            None => None,
        };
        let audio_done = match (&self.audio, self.wall) {
            (Some(a), None) => a.drained(now),
            _ => true,
        };
        let video_done =
            self.video.is_none() || (self.picker.done && self.picker.next_pts().is_none());
        let ended = clock >= self.duration
            || (video_done && audio_done && (self.video.is_some() || self.audio.is_some()));
        let wake_in = match (self.picker.next_pts(), &self.video) {
            (Some(pts), _) => (pts - half - clock).clamp(0.001, 1.0 / self.fps),
            (None, Some(_)) => 0.005,
            (None, None) => 1.0 / 30.0,
        };
        Pump {
            frame,
            position: clock.min(self.duration),
            ended,
            wake_in: Duration::from_secs_f64(wake_in),
        }
    }
}

/// `sopen --selftest-audio <file>`: plays the file on the default output device at gain 0 for
/// about two seconds and reports what the device did.
pub fn selftest(file: &Path) -> Result<String> {
    if !file.is_file() {
        bail!("{}: no such file", file.display());
    }
    // ffmpeg when there is one (the playback path); otherwise a WAV is read in-process, so the
    // device check also runs on a machine without ffmpeg.
    let tools = crate::ffmpeg::resolve();
    let (decode, decoder) = match &tools {
        Ok(tools) => (
            Decode::Ffmpeg {
                tools,
                path: file,
                t0: 0.0,
            },
            "ffmpeg",
        ),
        Err(_) => (Decode::Wav(file), "wav"),
    };
    let audio = Audio::start(decode, 0.0, Sink::Device)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while !audio.primed() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    audio.shared.start();
    let started = Instant::now();
    std::thread::sleep(Duration::from_secs(2));
    let s = &audio.shared;
    let line = format!(
        "callbacks={} underruns={} device={} rate={}Hz channels={} heard={:.2}s/{:.2}s xruns={} decoder={decoder}",
        s.callbacks.load(SeqCst),
        s.underruns.load(SeqCst),
        audio.device,
        audio.rate,
        audio.channels,
        audio.heard(Instant::now()),
        started.elapsed().as_secs_f64(),
        s.xruns.load(SeqCst),
    );
    if s.callbacks.load(SeqCst) == 0 {
        bail!("{line}: the device opened but never asked for audio");
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixer(
        capacity: usize,
        channels: u16,
        gain: f32,
    ) -> (rtrb::Producer<f32>, Mixer, Arc<Shared>) {
        let shared = Arc::new(Shared::new(Instant::now()));
        let (prod, cons) = rtrb::RingBuffer::new(capacity);
        (
            prod,
            Mixer::new(cons, shared.clone(), gain, channels),
            shared,
        )
    }

    fn wav(format: u16, bits: u16, channels: u16, rate: u32, data: &[u8]) -> Vec<u8> {
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&format.to_le_bytes());
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        let align = channels * bits / 8;
        w.extend_from_slice(&(rate * u32::from(align)).to_le_bytes());
        w.extend_from_slice(&align.to_le_bytes());
        w.extend_from_slice(&bits.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&(data.len() as u32).to_le_bytes());
        w.extend_from_slice(data);
        w
    }

    #[test]
    fn wav_reader_maps_channels_and_rate() {
        // Mono 16-bit at 2 Hz: [0.5, -0.5] → stereo at 4 Hz (each frame twice, both channels).
        let data: Vec<u8> = [16384i16, -16384]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let out = wav_to_f32(&wav(1, 16, 1, 2, &data), 4, 2).unwrap();
        assert_eq!(out, [0.5, 0.5, 0.5, 0.5, -0.5, -0.5, -0.5, -0.5]);
        // Stereo float, same rate: unchanged.
        let data: Vec<u8> = [0.25f32, -1.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(
            wav_to_f32(&wav(3, 32, 2, 8, &data), 8, 2).unwrap(),
            [0.25, -1.0]
        );
        assert!(wav_to_f32(b"RIFF\0\0\0\0AVI ", 8, 2).is_err());
        assert!(
            wav_to_f32(&wav(2, 4, 1, 8, &[0]), 8, 2).is_err(),
            "ADPCM refused"
        );
    }

    #[test]
    fn decode_sizes() {
        assert_eq!(decode_size(1920, 1080), (1280, 720));
        assert_eq!(decode_size(640, 360), (640, 360));
        assert_eq!(decode_size(641, 361), (640, 360));
        assert_eq!(decode_size(1080, 1920), (1080, 1920));
        assert_eq!(decode_size(3840, 1606), (1280, 534));
    }

    #[test]
    fn heard_interpolates_subtracts_latency_and_is_bounded() {
        assert_eq!(heard_seconds(0, 0, 48_000, 0.01, 0.0), 0.0);
        // 1 s handed out before the latest 10 ms buffer.
        let (before, played) = (48_000, 48_480);
        assert!((heard_seconds(before, played, 48_000, 0.004, 0.0) - 1.004).abs() < 1e-9);
        assert!((heard_seconds(before, played, 48_000, 0.004, 0.003) - 1.001).abs() < 1e-9);
        // Interpolation stops at what was handed out, and at the cap.
        assert!((heard_seconds(before, played, 48_000, 0.04, 0.0) - 1.01).abs() < 1e-9);
        assert!((heard_seconds(before, 96_000, 48_000, 3.0, 0.0) - 1.05).abs() < 1e-9);
        assert_eq!(heard_seconds(0, 480, 48_000, 0.0, 0.5), 0.0);
    }

    #[test]
    fn mixer_is_silent_before_start_then_plays_with_gain() {
        let (mut prod, mut mix, shared) = mixer(64, 2, 0.5);
        for v in [1.0f32, -1.0, 0.5, -0.5] {
            prod.push(v).unwrap();
        }
        let mut out = [9.0f32; 4];
        mix.render(&mut out, 0, 1);
        assert_eq!(out, [0.0; 4], "pre-roll is silence");
        assert_eq!(shared.frames_played.load(SeqCst), 0);
        assert_eq!(shared.underruns.load(SeqCst), 0);
        shared.start();
        let mut out = [9.0f32; 6];
        mix.render(&mut out, 0, 2);
        assert_eq!(out, [0.5, -0.5, 0.25, -0.25, 0.0, 0.0]);
        assert_eq!(shared.frames_played.load(SeqCst), 2);
        assert_eq!(shared.underruns.load(SeqCst), 1, "ran dry before eof");
        shared.eof.store(true, SeqCst);
        mix.render(&mut out, 0, 3);
        assert_eq!(shared.underruns.load(SeqCst), 1, "dry after eof is the end");
        assert_eq!(shared.callbacks.load(SeqCst), 3);
    }

    #[test]
    fn mixer_never_splits_a_frame() {
        // Three samples in stereo: one whole frame plays, the odd sample waits for its pair.
        let (mut prod, mut mix, shared) = mixer(64, 2, 1.0);
        for v in [0.1f32, 0.2, 0.3] {
            prod.push(v).unwrap();
        }
        shared.start();
        let mut out = [9.0f32; 4];
        mix.render(&mut out, 0, 1);
        assert_eq!(shared.frames_played.load(SeqCst), 1);
        assert_eq!(out, [0.1, 0.2, 0.0, 0.0]);
        prod.push(0.4).unwrap();
        mix.render(&mut out, 0, 2);
        assert_eq!(out, [0.3, 0.4, 0.0, 0.0]);
    }

    fn frames(fps: f64, n: usize) -> impl FnMut() -> Next {
        let mut i = 0usize;
        move || {
            if i >= n {
                return Next::Done;
            }
            let f = Frame {
                pts: i as f64 / fps,
                rgba: Vec::new(),
            };
            i += 1;
            Next::Frame(f)
        }
    }

    #[test]
    fn picker_shows_the_newest_due_frame_and_counts_drops() {
        let mut picker = Picker::default();
        let mut src = frames(10.0, 5);
        let half = 0.05;
        assert_eq!(picker.pick(&mut src, 0.0, half).map(|f| f.pts), Some(0.0));
        // Nothing new is due 40 ms later.
        assert!(picker.pick(&mut src, 0.04, half).is_none());
        // The UI stalled until 0.31: frames 0.1 and 0.2 are dropped, 0.3 is shown.
        assert_eq!(picker.pick(&mut src, 0.31, half).map(|f| f.pts), Some(0.3));
        assert_eq!((picker.shown, picker.dropped), (2, 2));
        assert_eq!(picker.next_pts(), Some(0.4));
        assert_eq!(picker.pick(&mut src, 1.0, half).map(|f| f.pts), Some(0.4));
        assert!(picker.done && picker.next_pts().is_none());
    }

    /// Audio is the clock: a simulated 48 kHz device with 10 ms callbacks and 5 ms output
    /// latency, a 30 fps decoder and a 60 Hz UI, all on explicit timestamps. The clock equals
    /// simulated time minus the latency, every frame is shown (none dropped) and no frame is
    /// shown more than half a frame from the clock.
    #[test]
    fn audio_clock_drives_frames_without_drift() {
        let rate = 48_000u32;
        let (mut prod, mut mix, shared) = mixer(rate as usize * 2, 2, 1.0);
        let silence = vec![0f32; rate as usize * 2]; // 1 s stereo
        let (pushed, _) = prod.push_partial_slice(&silence);
        assert_eq!(pushed.len(), silence.len());
        shared.start();
        let epoch = shared.epoch;
        let mut picker = Picker::default();
        let mut src = frames(30.0, 27);
        let mut out = vec![0f32; 480 * 2];
        let latency_ns = 5_000_000;
        let (mut next_cb, mut next_ui) = (0u64, 0u64);
        let mut worst = 0f64;
        while next_ui <= 900_000_000 {
            if next_cb <= next_ui {
                mix.render(&mut out, latency_ns, next_cb);
                next_cb += 10_000_000;
                continue;
            }
            let now = epoch + Duration::from_nanos(next_ui);
            let clock = shared.heard(rate, now);
            let sim = next_ui as f64 / 1e9;
            assert!(
                (clock - (sim - 0.005).max(0.0)).abs() < 1e-6,
                "clock {clock} at {sim}"
            );
            if let Some(f) = picker.pick(&mut src, clock, 0.5 / 30.0) {
                worst = worst.max((clock - f.pts).abs());
            }
            next_ui += 16_666_667;
        }
        assert_eq!(picker.dropped, 0);
        assert_eq!(picker.shown, 27);
        assert!(worst <= 0.5 / 30.0 + 1e-9, "drift {worst}");
        assert_eq!(shared.underruns.load(SeqCst), 0);
    }

    fn tools_or_skip() -> Option<Tools> {
        match crate::ffmpeg::resolve() {
            Ok(t) => Some(t),
            Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
                panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
            }
            Err(e) => {
                eprintln!("skipping playback test: {e}");
                None
            }
        }
    }

    fn generate(tools: &Tools, dst: &Path, inputs: &[&str], codec: &[&str]) {
        let mut cmd = Command::new(&tools.ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);
        for input in inputs {
            cmd.args(["-f", "lavfi", "-i", input]);
        }
        assert!(cmd.args(codec).arg(dst).status().unwrap().success());
    }

    /// Real decoders into the null sink: playback starts at t0, frames arrive in order at the
    /// probed rate and size, the audio clock advances without underruns. Timing bounds are
    /// loose (shared CI runners).
    #[test]
    fn player_plays_a_real_file_into_the_null_sink() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.mp4");
        generate(
            &tools,
            &path,
            &[
                "testsrc2=size=320x180:rate=25:duration=3",
                "sine=frequency=440:duration=3",
            ],
            &[
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ],
        );
        let probe = crate::ffmpeg::probe(&tools, &path).unwrap();
        assert_eq!(probe.fps, Some(25.0));
        let src = Source {
            path: path.clone(),
            duration: probe.duration.unwrap(),
            video: Some((decode_size(320, 180), 25.0)),
            audio: true,
        };
        let mut player =
            Player::start(&tools, &src, 1.0, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
        assert!(player.audio_note.is_none());
        let begin = Instant::now();
        let mut pts = Vec::new();
        let mut ended = false;
        while begin.elapsed() < Duration::from_millis(1200) {
            let mut p = player.pump(Instant::now());
            ended |= p.ended;
            if let Some(f) = p.frame.take() {
                assert_eq!(f.rgba.len(), 320 * 180 * 4);
                pts.push(f.pts);
            }
            std::thread::sleep(p.wake_in.min(Duration::from_millis(10)));
        }
        assert!(!ended);
        assert!(pts.len() >= 10, "frames shown: {}", pts.len());
        assert!((pts[0] - 1.0).abs() < 1e-9, "starts at t0: {}", pts[0]);
        assert!(pts.windows(2).all(|w| w[1] > w[0]));
        let audio = player.audio.as_ref().unwrap();
        assert_eq!(audio.shared.underruns.load(SeqCst), 0);
        let pos = player.clock(Instant::now());
        assert!(pos > 1.4 && pos < 2.3, "position {pos}");
    }

    /// The real output device at gain 0 — or, where there is none (CI runners, sandboxes), the
    /// silent fallback: either way the session starts, video frames keep coming and nothing
    /// panics; without a device `audio_note` says why.
    #[test]
    fn device_or_silent_fallback_keeps_video_playing() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.mp4");
        generate(
            &tools,
            &path,
            &[
                "testsrc2=size=160x90:rate=20:duration=2",
                "sine=frequency=440:duration=2",
            ],
            &[
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ],
        );
        let src = Source {
            path,
            duration: 2.0,
            video: Some(((160, 90), 20.0)),
            audio: true,
        };
        let mut player =
            Player::start(&tools, &src, 0.0, 0.0, Sink::Device, Arc::new(|| {})).unwrap();
        let begin = Instant::now();
        let mut shown = 0;
        while begin.elapsed() < Duration::from_millis(1200) {
            let p = player.pump(Instant::now());
            shown += usize::from(p.frame.is_some());
            std::thread::sleep(p.wake_in.min(Duration::from_millis(10)));
        }
        eprintln!(
            "device test: shown={shown} audio_note={:?} position={:.2}",
            player.audio_note,
            player.clock(Instant::now())
        );
        assert!(shown >= 8, "frames shown: {shown}");
        assert!(player.clock(Instant::now()) > 0.5);
    }

    #[test]
    fn audio_only_plays_to_the_end() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.wav");
        generate(&tools, &path, &["sine=frequency=440:duration=0.6"], &[]);
        let src = Source {
            path,
            duration: 0.6,
            video: None,
            audio: true,
        };
        let mut player =
            Player::start(&tools, &src, 0.2, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
        let begin = Instant::now();
        let ended = loop {
            let p = player.pump(Instant::now());
            if p.ended || begin.elapsed() > Duration::from_secs(5) {
                break p.ended;
            }
            std::thread::sleep(p.wake_in);
        };
        assert!(ended, "never reached the end");
        assert!(
            begin.elapsed() >= Duration::from_millis(300),
            "{:?}",
            begin.elapsed()
        );
    }
}
