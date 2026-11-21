//! What the file browser's preview of a video or a sound is made of (02 §4.2–§4.4), on the
//! preview's worker: the probe, then one frame — the frame at 0 s, the media window's first — or
//! the waveform of the sound's first minutes. Every ffprobe and ffmpeg it runs is one a stop
//! reaches: started under [`Children`]'s lock, watched while it runs ([`Watch::check`]: the
//! preview was left, or the step is over its time), killed and reaped on every way out. The
//! previewed window itself runs none ([`super::embed_preview`]); handed over, it starts from
//! what was made here — the frame is not decoded again, the waveform goes on from where it
//! stopped, and how Save and Export render the file is already worked out (the one ffmpeg run
//! that takes, for a video above 8 bits, is made here too).

use super::{frame_command, wave, Media};
use crate::cut;
use crate::ffmpeg::{self, ErrorTail, Input, Probe, Tools};
use crate::kind::{self, Kind};
use anyhow::{anyhow, bail, Context as _, Result};
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often a running child is looked at: its end, a cancel, its step's time.
const POLL: Duration = Duration::from_millis(5);

/// A step of the preparation with a time of its own (02 §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `ffprobe`.
    Probe,
    /// The first frame.
    Frame,
    /// The waveform.
    Wave,
    /// What ffmpeg's H.264 encoder takes, asked for a video above 8 bits (how Save and Export
    /// will render it).
    Encoder,
}

/// What a preview's preparation asks of whoever runs it (the preview host's budget and stop
/// flag; tests: their own).
pub trait Watch: Sync {
    /// `step` starts: its time counts from now.
    fn begin(&self, step: Step);
    /// `Err`: stop here — the preview was cancelled, or the step is over its time (what the
    /// info card says). Looked at before every child starts and every few milliseconds while
    /// one runs.
    fn check(&self) -> Result<()>;
    /// How many seconds of sound the waveform reads; `None`: all of it.
    fn sound(&self) -> Option<f64>;
}

/// The child a preview's preparation runs right now (one at a time). Whoever stops the
/// preparation kills it here, at once ([`Children::kill`]: the window closing does not wait for
/// the worker to notice); the worker reaps it otherwise. A child starts only under this lock
/// and after a look at [`Watch::check`], so a stop either comes before it (and it never starts)
/// or finds it here.
#[derive(Default)]
pub struct Children {
    live: Mutex<Option<Child>>,
}

impl Children {
    /// Kills and reaps the running child, if there is one.
    pub fn kill(&self) {
        let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(mut child) = live.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Tests: every child a preview's preparation started, by the file it read.
#[cfg(test)]
static STARTED: Mutex<Vec<(std::path::PathBuf, u32)>> = Mutex::new(Vec::new());

/// Tests: the process ids of every child started to read a file under `dir`.
#[cfg(test)]
pub(crate) fn started_under(dir: &std::path::Path) -> Vec<u32> {
    let started = STARTED.lock().unwrap_or_else(|p| p.into_inner());
    started
        .iter()
        .filter(|(file, _)| file.starts_with(dir))
        .map(|&(_, pid)| pid)
        .collect()
}

/// Tests: how many of `pids` are still processes — running, or ended and not reaped (`ps`
/// lists both).
#[cfg(test)]
pub(crate) fn still_processes(pids: &[u32]) -> usize {
    if pids.is_empty() {
        return 0;
    }
    let list: Vec<String> = pids.iter().map(u32::to_string).collect();
    let out = Command::new("ps")
        .args(["-o", "pid=", "-p", &list.join(",")])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

/// Runs `cmd` (which reads `input`) to its end, `read` taking its output; stopped — killed and
/// reaped — as soon as `watch` says so, which is then the error. Returns what `read` made, how
/// the child ended and the last line it said.
fn run<T>(
    cmd: &mut Command,
    input: &Input,
    children: &Children,
    watch: &dyn Watch,
    read: impl FnOnce(&mut dyn Read) -> T,
) -> Result<(T, ExitStatus, String)> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let program = cmd.get_program().to_string_lossy().into_owned();
    let (mut out, err) = {
        let mut live = children.live.lock().unwrap_or_else(|p| p.into_inner());
        // Under the lock a stop kills under: stopped before, nothing starts.
        watch.check()?;
        let mut child = ffmpeg::spawn(cmd).with_context(|| format!("cannot run {program}"))?;
        #[cfg(test)]
        if let Some(file) = input.local() {
            let mut started = STARTED.lock().unwrap_or_else(|p| p.into_inner());
            started.push((file.to_path_buf(), child.id()));
        }
        let _ = input;
        let pipes = (
            child.stdout.take().expect("piped"),
            child.stderr.take().expect("piped"),
        );
        *live = Some(child);
        pipes
    };
    let said = ErrorTail::spawn(err);
    let stopped: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let (got, status) = std::thread::scope(|scope| {
        // Looks at the child until it has ended: by itself, by a stop from here, or by one from
        // whoever holds `children` (the window closing).
        let watcher = scope.spawn(|| loop {
            if let Err(e) = watch.check() {
                children.kill();
                *stopped.lock().unwrap() = Some(e);
                return None;
            }
            {
                let mut live = children.live.lock().unwrap_or_else(|p| p.into_inner());
                match live.as_mut().map(Child::try_wait) {
                    // Killed and reaped from outside.
                    None => return None,
                    Some(Ok(Some(status))) => {
                        live.take();
                        return Some(status);
                    }
                    Some(Ok(None)) => {}
                    Some(Err(_)) => {
                        drop(live);
                        children.kill();
                        return None;
                    }
                }
            }
            std::thread::sleep(POLL);
        });
        let got = read(&mut out);
        // Whatever was not read: the child ends at its next write.
        drop(out);
        (got, watcher.join().expect("the watcher"))
    });
    let last = said.join().unwrap_or_default().last_line();
    if let Some(e) = stopped.into_inner().unwrap() {
        return Err(e);
    }
    match status {
        Some(status) => Ok((got, status, last)),
        None => Err(anyhow!(crate::remote::Cancelled)),
    }
}

/// ffprobe's view of `input`, by a run that can be stopped. The outer error is a stop (or
/// ffprobe not starting); the inner one is ffprobe's own: it cannot read the file as media.
fn probe(
    tools: &Tools,
    input: &Input,
    children: &Children,
    watch: &dyn Watch,
) -> Result<Result<Probe>> {
    watch.begin(Step::Probe);
    let mut cmd = ffmpeg::probe_command(tools, input.arg());
    let (json, status, last) = run(&mut cmd, input, children, watch, |out| {
        let mut json = Vec::new();
        let _ = out.read_to_end(&mut json);
        json
    })?;
    if !status.success() {
        let path = std::path::Path::new(input.arg());
        return Ok(Err(anyhow!("{}", ffmpeg::probe_refusal(path, &last))));
    }
    Ok(ffmpeg::parse_probe(&String::from_utf8_lossy(&json)))
}

/// A video's or a sound's preview, made: the probed file and what the previewed window shows of
/// it without running anything.
pub struct Prepared {
    pub media: Media,
    /// The frame at 0 s (video), RGBA at the size the window decodes at.
    pub(super) frame: Option<Arc<Vec<u8>>>,
    /// The waveform's peaks (a sound without picture).
    pub(super) wave: Option<wave::Source>,
    /// The peaks cover only this many seconds of the sound (02 §4.3); `None`: all of it.
    pub(super) cut: Option<f64>,
    /// How Save and Export render the file is worked out (`media.plan`); `false`: the encoder
    /// did not answer in its time, and the window asks it again, off its frames, at the
    /// hand-over.
    pub(super) planned: bool,
}

/// What [`prepare`] found the file to be.
pub enum Found {
    Media(Box<Prepared>),
    /// A `.ts` ffprobe does not read as media: not a medium (its hex view, as Enter shows it).
    NotMedia,
}

/// Makes the preview of the media file `input` (named `name`; `kind` by its name and first
/// bytes): the probe, then the first frame (video) or the waveform (sound) — each a step with
/// its own time, and all of it stopped as soon as `watch` says so.
pub fn prepare(
    tools: &Tools,
    input: &Input,
    name: &str,
    kind: Kind,
    children: &Children,
    watch: &dyn Watch,
) -> Result<Found> {
    let probed = probe(tools, input, children, watch)?;
    // As Enter refines it once the file is local: a `.ts` ffprobe does not read as media is
    // not media.
    let streams = probed
        .as_ref()
        .ok()
        .map(|p| (p.video.is_some(), p.has_audio));
    if kind::refine_after_fetch(name, kind, streams) == Kind::Other {
        return Ok(Found::NotMedia);
    }
    let media = Media::of_probe(tools.clone(), input, name, probed?)?;
    let (mut frame, mut peaks, mut cut) = (None, None, None);
    match media.decode_size() {
        Some((w, h)) => {
            // The media window's first frame: source frame 0, decoded as its paused frame is.
            watch.begin(Step::Frame);
            let (from, slot_end) = super::frame_slot(media.origin(), media.fps.max(1.0), 0);
            let span = slot_end - from + super::LOOK_PAST_S;
            let mut cmd = frame_command(tools, input, (from, span), (w, h), None);
            let (rgba, _, last) = run(&mut cmd, input, children, watch, |out| {
                let mut rgba = vec![0u8; (w * h * 4) as usize];
                out.read_exact(&mut rgba).map(|()| rgba)
            })?;
            match rgba {
                Ok(rgba) => frame = Some(Arc::new(rgba)),
                Err(_) if last.is_empty() => bail!("no frame at {}", super::clock_text(0.0)),
                Err(_) => bail!("no frame at {}: {last}", super::clock_text(0.0)),
            }
        }
        None => {
            watch.begin(Step::Wave);
            // No further than the preview reads; a sound that ends before that is read whole.
            let upto = watch.sound().filter(|upto| *upto < media.duration);
            let lanes = media.lanes().max(1);
            let mut cmd = wave::command(tools, input, lanes, 0.0, upto);
            #[cfg(test)]
            wave::note_start(input, 0.0, upto);
            let (read, status, last) = run(&mut cmd, input, children, watch, |out| {
                wave::read_onto(out, lanes, media.duration, None, &mut |_| {})
            })?;
            let read = read.context("reading the decoded sound")?;
            if !status.success() {
                bail!("cannot read the waveform: ffmpeg {status}: {last}");
            }
            peaks = Some(wave::Source {
                complete: upto.is_none(),
                ..read
            });
            cut = upto;
        }
    }
    // How Save and Export render it, the hand-over's to have: for a video above 8 bits ffmpeg
    // is asked what its H.264 encoder takes — here, where the run can be stopped, never on a
    // frame of the window. Left without an answer in its time, the preview shows all the same
    // and the window asks again when it is handed over.
    let mut media = media;
    let mut planned = true;
    if cut::plan_asks_encoder(&media.probe, name) {
        watch.begin(Step::Encoder);
        let mut cmd = ffmpeg::encoder_help_command(tools, cut::PLAN_ENCODER);
        let asked = run(&mut cmd, input, children, watch, |out| {
            let mut help = Vec::new();
            let _ = out.read_to_end(&mut help);
            help
        });
        match asked {
            Ok((help, _, _)) => {
                let takes = ffmpeg::parse_pix_fmts(&String::from_utf8_lossy(&help));
                media.plan_with(Some(&takes));
            }
            Err(e) if crate::remote::is_cancelled(&e) => return Err(e),
            Err(_) => planned = false,
        }
    } else {
        media.plan_with(None);
    }
    Ok(Found::Media(Box::new(Prepared {
        media,
        frame,
        wave: peaks,
        cut,
        planned,
    })))
}
