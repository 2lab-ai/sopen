//! What the file browser's preview of a video or a sound is made of (02 §4.2–§4.4), on the
//! preview's worker: the probe, then one frame — the frame at 0 s, the media window's first — or
//! the waveform of the sound's first minutes. Every ffprobe and ffmpeg it runs is one a stop
//! reaches: started under [`Children`]'s lock, watched while it runs ([`Watch::check`]: the
//! preview was left, or the step is over its time), killed and reaped on every way out. The
//! previewed window itself runs none ([`super::embed_preview`]); handed over, it starts from
//! what was made here — the frame is not decoded again, the waveform goes on from where it
//! stopped, and how Save and Export render the file is already worked out (the one ffmpeg run
//! that takes, for a video above 8 bits, is made here too).

use super::child::{Ending, Polled, StopOnUnwind, POLL};
use super::{frame_command, wave, Media};
use crate::cut;
use crate::ffmpeg::{self, ErrorTail, Input, Probe, Tools};
use crate::kind::{self, Kind};
use anyhow::{anyhow, bail, Context as _, Result};
use std::io::Read;
use std::process::{Command, ExitStatus};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// The child a preview's preparation runs right now (one at a time): see [`Slot`]. Whoever
/// stops the preparation stops it here, at once ([`Slot::stop`]: the window closing does not
/// wait for the worker to notice); the worker reaps it.
pub use super::child::Slot as Children;

/// Runs `cmd` (which reads `input`) to its end, `read` taking its output; stopped — killed and
/// reaped — as soon as `watch` says so (which is then the error) or `children` is stopped from
/// outside (a cancel). Returns what `read` made, how the child ended and the last line it said.
/// However it returns — unwinding too — no child is left ([`Ending`]).
fn run<T>(
    cmd: &mut Command,
    input: &Input,
    children: &Children,
    watch: &dyn Watch,
    read: impl FnOnce(&mut dyn Read) -> T,
) -> Result<(T, ExitStatus, String)> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    watch.check()?;
    let started = children
        .start(cmd, input.local())
        .with_context(|| format!("cannot run {program}"))?;
    let Some((mut out, err)) = started else {
        return Err(anyhow!(crate::remote::Cancelled));
    };
    // From here on, every way out kills and reaps what is still in the slot.
    let _ending = Ending(children);
    let said = ErrorTail::spawn(err);
    let stopped: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let (got, status) = std::thread::scope(|scope| -> Result<(T, Option<ExitStatus>)> {
        // Looks at the child until it has ended: by itself, by a stop from here (the watch),
        // or by one from whoever holds `children` (the window closing). A child told to stop
        // is looked at until it is reaped; the watch is not asked again.
        let watching = || loop {
            if !children.stopped() {
                if let Err(e) = watch.check() {
                    *stopped.lock().unwrap() = Some(e);
                    children.stop();
                }
            }
            match children.poll() {
                Polled::Running => std::thread::sleep(POLL),
                Polled::Ended(status) => return Some(status),
                Polled::Gone => return None,
            }
        };
        #[cfg(test)]
        if children
            .fail_watcher
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(anyhow!("cannot start the thread that watches {program}"));
        }
        let watcher = std::thread::Builder::new()
            .spawn_scoped(scope, watching)
            .with_context(|| format!("cannot start the thread that watches {program}"))?;
        // A reader that unwinds stops the child: the watcher then ends, instead of being waited
        // for while it watches a child nobody reads.
        let _unwinding = StopOnUnwind(children);
        #[cfg(test)]
        if children
            .panic_in_read
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            panic!("reading {program}'s output");
        }
        let got = read(&mut out);
        // Whatever was not read: the child ends at its next write.
        drop(out);
        Ok((got, watcher.join().expect("the watcher")))
    })?;
    let last = said.join().unwrap_or_default().last_line();
    if let Some(e) = stopped.into_inner().unwrap() {
        return Err(e);
    }
    match status {
        Some(status) if !children.stopped() => Ok((got, status, last)),
        _ => Err(anyhow!(crate::remote::Cancelled)),
    }
}

/// A [`Watch`] with one time for one step, on a clock of its holder's: the media window's own
/// encoder question (a window opened by itself asks it; 02 §4.4).
pub struct Timed {
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    pub clock: Clock,
    pub time: Duration,
    since: Mutex<Option<std::time::Instant>>,
}

/// The time, as whoever times a step reads it (tests move it).
pub type Clock = Arc<dyn Fn() -> std::time::Instant + Send + Sync>;

/// A step was stopped because it ran over its time ([`Timed`]).
#[derive(Debug)]
pub struct OverTime;

impl std::fmt::Display for OverTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("over its time")
    }
}

impl std::error::Error for OverTime {}

impl Timed {
    pub fn new(cancel: Arc<std::sync::atomic::AtomicBool>, clock: Clock, time: Duration) -> Self {
        Self {
            cancel,
            clock,
            time,
            since: Mutex::new(None),
        }
    }
}

impl Watch for Timed {
    fn begin(&self, _: Step) {
        *self.since.lock().unwrap() = Some((self.clock)());
    }

    fn check(&self) -> Result<()> {
        let since = *self.since.lock().unwrap();
        let now = (self.clock)();
        if self.cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(anyhow!(crate::remote::Cancelled));
        }
        match since {
            Some(since) if now.saturating_duration_since(since) >= self.time => {
                Err(anyhow!(OverTime))
            }
            _ => Ok(()),
        }
    }

    fn sound(&self) -> Option<f64> {
        None
    }
}

/// What ffmpeg's H.264 encoder takes ([`cut::PLAN_ENCODER`]'s pixel formats), asked by a run
/// that can be stopped: the question a video above 8 bits needs answered before Save and Export
/// are planned.
pub fn ask_encoder(
    tools: &Tools,
    input: &Input,
    children: &Children,
    watch: &dyn Watch,
) -> Result<std::collections::HashSet<String>> {
    watch.begin(Step::Encoder);
    let mut cmd = ffmpeg::encoder_help_command(tools, cut::PLAN_ENCODER);
    let (help, _, _) = run(&mut cmd, input, children, watch, |out| {
        let mut help = Vec::new();
        let _ = out.read_to_end(&mut help);
        help
    })?;
    Ok(ffmpeg::parse_pix_fmts(&String::from_utf8_lossy(&help)))
}

/// `said` (a line ffprobe or ffmpeg printed) without what changes from run to run or names the
/// file a second time.
pub(crate) fn plain(said: &str, input: &Input) -> String {
    let mut said = said.trim();
    // `[component @ 0x…] `, as often as it leads the line: the address is another every run.
    while let Some(rest) = said.strip_prefix('[') {
        let Some((tag, rest)) = rest.split_once("] ") else {
            break;
        };
        if !tag.contains(" @ 0x") {
            break;
        }
        said = rest.trim_start();
    }
    let named = format!("{}: ", input.show());
    said.strip_prefix(&named).unwrap_or(said).to_string()
}

/// ffprobe cannot read the file as media: its own reason, without the file's path (whoever
/// shows it names the file as its owner knows it — a remote one is read here from a private
/// copy).
#[derive(Debug)]
pub struct Unreadable(pub String);

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ffprobe cannot read it: {}", self.0)
    }
}

impl std::error::Error for Unreadable {}

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
        return Ok(Err(anyhow!(Unreadable(plain(&last, input)))));
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
    let media = Media::of_probe(tools.clone(), input, name, &input.show(), probed?)?;
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
                Err(_) => bail!(
                    "no frame at {}: {}",
                    super::clock_text(0.0),
                    plain(&last, input)
                ),
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
                bail!(
                    "cannot read the waveform: ffmpeg {status}: {}",
                    plain(&last, input)
                );
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
        match ask_encoder(tools, input, children, watch) {
            Ok(takes) => media.plan_with(Some(&takes)),
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
