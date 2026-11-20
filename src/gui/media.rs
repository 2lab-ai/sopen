//! The media window (spec §7): video plays in the window with sound, audio shows its waveform
//! (one lane per channel of a stereo file); both have a scrubbable timeline, Space play/pause
//! and the edits of message 13 — a drag (or `I`/`O` at the playhead) selects a stretch of the
//! timeline, Cmd/Ctrl+C copies it, Cmd/Ctrl+X cuts it (the gap closes), Delete removes it,
//! Cmd/Ctrl+V pastes at the playhead or over the selection, `M` makes a mono file stereo,
//! Cmd/Ctrl+Z / Shift+Cmd/Ctrl+Z undo and redo. Editing changes only the edit list
//! ([`timeline`]) and plays it at once; only Save (Cmd/Ctrl+S, F5) and Export run ffmpeg to
//! render it. Save writes back over the original ([`file`]: backup first, conflict-checked);
//! Export (`E`) writes `<stem>.cut-<in>-<out>.<ext>` next to it — never over anything. On video
//! a drag on the picture draws the crop rectangle in source pixels (rounded to even on export),
//! applied to the whole output.
//!
//! Save and Export are one render ([`crate::cut::render_edit`]) of the edit list from the one
//! [`Input`] every run on the file reads; they run on a worker thread in the window and
//! directly in the `SOPEN_AUTOSAVE` / `SOPEN_AUTOEXPORT` hooks. `sopen cut` keeps its own
//! export ([`crate::cut::cut_and_publish`]).

use super::image::CANNOT_REPLACE;
use super::playback::{self, Piece, Player, Sink, Source};
use super::{Hooks, Map, Origin, Outcome, Selection, Status};
use crate::cut;
use crate::ffmpeg::{
    self, EditSpec, ExportPlan, Input, MediaKind, Probe, Rect as CropRect, Span, Tools,
};
use anyhow::{bail, Context as _, Result};
use eframe::egui::{self, Color32, Key, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};
use file::{MediaFile, Saved};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::SeqCst};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;
use timeline::{Clip, Tick, Timeline, View};

/// Closing was asked for during an export: what happens, and what a second close does (its own
/// row: read whole on a narrow window).
const CLOSE_AGAIN: &str = "the window closes when the export is done — close it again to close \
     it now; sopen still waits for the export and prints the result in the terminal";
/// Closing was asked for while a save runs.
const CLOSES_AFTER_SAVE: &str = "the window closes when the save is done";

/// The window's title in its frame, in the manner of PC Shell's `File Edit`: a video, a sound.
const VIDEO_TITLE: &str = "Video Edit";
const AUDIO_TITLE: &str = "Audio Edit";
/// Decoded frames a paused video keeps for its playhead (bytes), so the frame an edit brings
/// under the playhead is usually on screen at once.
const FRAME_CACHE_BYTES: usize = 64 << 20;

/// The probed file.
#[derive(Debug, Clone)]
pub struct Media {
    pub tools: Tools,
    /// What every ffmpeg run on the file reads (see [`Input`]): probe, playback, the paused
    /// frame, the waveform, Save and Export.
    pub input: Input,
    pub duration: f64,
    /// Frame size as displayed (rotation applied), when there is a video stream. ffmpeg
    /// rotates frames before filters run, so the preview, the crop rectangle and the export's
    /// crop are all in these pixels.
    pub video: Option<(u32, u32)>,
    /// Display rotation in degrees (0, 90, 180, 270).
    pub rotation: u32,
    pub fps: f64,
    pub has_audio: bool,
    /// The first audio track's channels (0 without audio) and sample rate.
    pub channels: u32,
    pub sample_rate: Option<u32>,
    /// What Save and Export will not keep (or keep differently), shown before either.
    pub notes: Vec<String>,
    /// ffprobe's view of the file (the streams a render maps).
    pub probe: Probe,
    /// How Save and Export render it: video or audio, the output extension, the plan; `None`
    /// when its name is not one `sopen cut` takes.
    pub plan: Option<(MediaKind, String, ExportPlan)>,
    /// The original's name (its extension is what Save must keep).
    pub name: String,
}

impl Media {
    pub fn probe(local: &Path) -> Result<Self> {
        let name = local
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::probe_as(&Input::path(local), &name)
    }

    /// `input` of the original named `name` (a saved remote file plays from a staged copy
    /// whose name is not the original's; a streamed one from a URL).
    pub fn probe_as(input: &Input, name: &str) -> Result<Self> {
        let tools = ffmpeg::resolve()?;
        let probe = ffmpeg::probe(&tools, input)?;
        let duration = probe
            .duration
            .filter(|d| *d > 0.0)
            .with_context(|| format!("ffprobe reports no duration for {}", input.show()))?;
        if probe.video.is_none() && !probe.has_audio {
            bail!("{} has no audio or video stream", input.show());
        }
        let plan = cut::plan(&tools, &probe, name).ok();
        let mut notes = plan
            .as_ref()
            .map(|(_, _, plan)| plan.notes.clone())
            .unwrap_or_default();
        if probe.chapters > 0 {
            notes.push(format!(
                "{} chapter{} not kept by an edited Save or Export (the edit moves them)",
                probe.chapters,
                if probe.chapters == 1 { "" } else { "s" }
            ));
        }
        Ok(Self {
            tools,
            input: input.clone(),
            duration,
            video: probe.display,
            rotation: probe.rotation,
            fps: probe.fps.unwrap_or(30.0),
            has_audio: probe.has_audio,
            channels: if probe.has_audio {
                probe.channels.unwrap_or(2)
            } else {
                0
            },
            sample_rate: probe.sample_rate,
            notes,
            plan,
            probe,
            name: name.to_string(),
        })
    }

    fn source_size(&self) -> Option<Vec2> {
        self.video.map(|(w, h)| Vec2::new(w as f32, h as f32))
    }

    fn decode_size(&self) -> Option<(u32, u32)> {
        self.video.map(|(w, h)| playback::decode_size(w, h))
    }

    /// Where timeline tick 0 is in the source (seconds): on video, the picture's offset within
    /// a frame period ([`Probe::frame_origin`]), so a cut is at a frame's own time for picture
    /// and sound alike; 0 on audio.
    pub fn origin(&self) -> f64 {
        if self.video.is_some() {
            self.probe.frame_origin()
        } else {
            0.0
        }
    }

    /// The picture's last frame (its own length, else the file's).
    pub fn last_frame(&self) -> Tick {
        let length = self.probe.video_duration.unwrap_or(self.duration);
        ((length * self.fps.max(1.0)).round() as Tick - 1).max(0)
    }

    /// Ticks per second of the edit list: frames on video, samples on audio.
    pub fn rate(&self) -> f64 {
        if self.video.is_some() {
            self.fps.max(1.0)
        } else {
            f64::from(self.sample_rate.unwrap_or(48_000))
        }
    }

    /// The source's length in ticks.
    pub fn ticks(&self) -> Tick {
        ((self.duration * self.rate()).round() as Tick).max(1)
    }

    /// A mono audio file: Mono → Stereo is offered (video gets cut and paste only, message 13).
    pub fn mono_audio(&self) -> bool {
        self.video.is_none() && self.channels == 1
    }

    /// Waveform lanes: one for mono, two for anything with more channels.
    fn lanes(&self) -> usize {
        match self.channels {
            0 => 0,
            1 => 1,
            _ => 2,
        }
    }

    /// Why Save cannot write this file back in its own format, if it cannot.
    pub fn save_block(&self) -> Option<String> {
        let ext = self
            .name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match &self.plan {
            None => Some(format!("Save cannot render {}", self.name)),
            Some((kind, out, _)) if *out != ext => Some(format!(
                "Save cannot write {} back as .{ext} ({} would not fit in it); Export writes .{out}",
                self.name,
                if *kind == MediaKind::Video {
                    "H.264 + AAC".to_string()
                } else {
                    format!("its {} sound", self.probe.audio_codec.as_deref().unwrap_or("?"))
                }
            )),
            Some(_) => None,
        }
    }

    /// The audio tracks a render carries: all, or the first for a container that holds one.
    fn tracks(&self) -> Vec<ffmpeg::Track> {
        let out = self.plan.as_ref().map_or("", |(_, ext, _)| ext.as_str());
        cut::tracks_for(&self.probe, out)
    }

    /// What Save re-encodes, for its hover text.
    fn save_encodes(&self) -> String {
        match &self.plan {
            Some((MediaKind::Video, ..)) => "re-encoded as H.264 (CRF 18) + AAC".to_string(),
            _ => format!(
                "re-encoded as {}",
                self.probe
                    .audio_codec
                    .as_deref()
                    .unwrap_or("the same codec")
            ),
        }
    }
}

/// `m:ss.cc` (or `h:mm:ss.cc`).
pub fn clock_text(secs: f64) -> String {
    let cs = (secs.max(0.0) * 100.0).round() as u64;
    let (h, m, s, c) = (cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{c:02}")
    } else {
        format!("{m}:{s:02}.{c:02}")
    }
}

/// The transport's clock line, as wide for any playhead and markers as for the clip's end: a
/// line that grew while playing (9:59.99 → 10:00.00) would jump, and on a narrow window be cut
/// at a different place.
fn transport_text(marks: &Marks, duration: f64) -> String {
    let (in_s, out_s) = marks.range(duration);
    let w = clock_text(duration).len();
    let dw = format!("{duration:.2}").len();
    format!(
        "{:>w$} / {}  ·  In {:>w$}  Out {:>w$}  ({:>dw$.2} s)",
        clock_text(marks.t),
        clock_text(duration),
        clock_text(in_s),
        clock_text(out_s),
        (out_s - in_s).max(0.0)
    )
}

/// The frame asked for next: timeline time and source frame (the latest request wins).
type Request = Arc<(Mutex<Option<(f64, Tick)>>, Condvar)>;
/// Decoded frames kept, by source frame, oldest first.
type Kept = Arc<Mutex<VecDeque<(Tick, Arc<Vec<u8>>)>>>;
/// The waveform's columns, per lane, for (timeline revision, peaks version, columns, lanes).
type Columns = Option<((u64, u64, usize, usize), Vec<Vec<[f32; 2]>>)>;

/// One-frame decodes for a paused playhead (the latest request wins), with the last frames
/// kept by source frame: an edit that brings a frame already seen under the playhead shows it
/// at once.
struct Scrubber {
    req: Request,
    result: Arc<Mutex<Option<Result<Scrubbed, String>>>>,
    quit: Arc<AtomicBool>,
    cache: Kept,
}

/// A paused frame: timeline time `t`, source frame `key`.
struct Scrubbed {
    t: f64,
    key: Tick,
    rgba: Arc<Vec<u8>>,
}

impl Scrubber {
    fn start(media: &Media, ctx: egui::Context) -> Option<Self> {
        let (w, h) = media.decode_size()?;
        let req: Request = Arc::default();
        let result: Arc<Mutex<Option<Result<Scrubbed, String>>>> = Arc::default();
        let quit = Arc::new(AtomicBool::new(false));
        let cache: Kept = Arc::default();
        let (rq, res, q, store) = (req.clone(), result.clone(), quit.clone(), cache.clone());
        let (tools, path, fps) = (media.tools.clone(), media.input.clone(), media.fps.max(1.0));
        let origin = media.origin();
        let keep = (FRAME_CACHE_BYTES / (w * h * 4) as usize).max(2);
        std::thread::spawn(move || loop {
            let (t, key) = {
                let mut g = rq.0.lock().unwrap();
                while g.is_none() && !q.load(SeqCst) {
                    g = rq.1.wait(g).unwrap();
                }
                if q.load(SeqCst) {
                    return;
                }
                g.take().expect("checked")
            };
            // Half a frame early, as Save renders a cut: the frame shown is frame `key` (keys
            // are clamped to the picture's last frame where they are made, [`MediaWindow::
            // frame_at`]). If nothing comes from there (a last frame stamped a little early),
            // the last picture of the two seconds before it is the one.
            let at = (origin + key as f64 / fps - 0.5 / fps).max(0.0);
            let grab = |from: f64, one: bool| -> Result<Vec<u8>, String> {
                let mut cmd = std::process::Command::new(&tools.ffmpeg);
                cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-ss"])
                    .arg(format!("{from:.6}"))
                    .arg("-i")
                    .arg(path.arg())
                    .args(["-an", "-sn", "-vf"])
                    .arg(format!("scale={w}:{h},format=rgba"));
                if one {
                    cmd.args(["-frames:v", "1"]);
                }
                cmd.args(["-f", "rawvideo", "-"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                let mut child = ffmpeg::spawn(&mut cmd)
                    .map_err(|e| format!("cannot run {}: {e}", tools.ffmpeg.display()))?;
                use std::io::Read;
                let mut out = child.stdout.take().expect("piped");
                let mut last: Option<Vec<u8>> = None;
                loop {
                    let mut rgba = vec![0u8; (w * h * 4) as usize];
                    if out.read_exact(&mut rgba).is_err() {
                        break;
                    }
                    last = Some(rgba);
                    if one {
                        break;
                    }
                }
                let _ = child.kill();
                let _ = child.wait();
                last.ok_or_else(|| format!("no frame at {}", clock_text(key as f64 / fps)))
            };
            let frame = {
                let read = grab(at, true).or_else(|_| grab((at - 2.0).max(0.0), false));
                {
                    read.map(|rgba| {
                        let rgba = Arc::new(rgba);
                        let mut c = store.lock().unwrap();
                        c.retain(|(k, _)| *k != key);
                        c.push_back((key, rgba.clone()));
                        while c.len() > keep {
                            c.pop_front();
                        }
                        Scrubbed { t, key, rgba }
                    })
                }
            };
            *res.lock().unwrap() = Some(frame);
            ctx.request_repaint();
        });
        Some(Self {
            req,
            result,
            quit,
            cache,
        })
    }

    /// The frame of source frame `key` for timeline time `t`: from the kept frames at once, or
    /// decoded.
    fn request(&self, t: f64, key: Tick) {
        let hit = self
            .cache
            .lock()
            .unwrap()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, f)| f.clone());
        match hit {
            Some(rgba) => {
                *self.req.0.lock().unwrap() = None;
                *self.result.lock().unwrap() = Some(Ok(Scrubbed { t, key, rgba }));
            }
            None => {
                *self.req.0.lock().unwrap() = Some((t, key));
                self.req.1.notify_one();
            }
        }
    }
}

impl Drop for Scrubber {
    fn drop(&mut self) {
        self.quit.store(true, SeqCst);
        self.req.1.notify_one();
    }
}

/// The playhead and the selection (In / Out), in timeline seconds.
#[derive(Debug, Clone, Default)]
struct Marks {
    t: f64,
    in_s: Option<f64>,
    out_s: Option<f64>,
}

impl Marks {
    fn range(&self, duration: f64) -> (f64, f64) {
        (self.in_s.unwrap_or(0.0), self.out_s.unwrap_or(duration))
    }

    fn selected(&self) -> bool {
        self.in_s.is_some() || self.out_s.is_some()
    }

    fn clear(&mut self) {
        self.in_s = None;
        self.out_s = None;
    }
}

/// The export of a timeline's `[in, out]` (`spec`, already that stretch) as a new sibling of
/// the original. Returns the new file's printed path (`host:/dir/name`) and what it did not
/// keep.
pub fn export_edit(
    origin: &mut Origin,
    media: &Media,
    spec: &EditSpec,
    range: (f64, f64),
    work_root: &Path,
    progress: &mut dyn FnMut(f64),
) -> Result<(String, Vec<String>)> {
    let (dir, name) = origin.parent();
    let fs = origin.fs()?;
    let published = cut::render_and_publish(
        fs,
        &media.tools,
        &media.input,
        &dir,
        &name,
        spec,
        range,
        work_root,
        progress,
    )?;
    let path = origin.show(&crate::remote::join(&dir, &published.name));
    Ok((path, published.notes))
}

/// A failed export, in one line. An upload that got no reply is "outcome unknown — check
/// <path>" (the sibling may exist), never "export failed".
fn export_error(e: &anyhow::Error) -> anyhow::Error {
    match crate::remote::outcome_unknown(e) {
        Some(unknown) => anyhow::anyhow!("export: {unknown} (cause: {})", e.root_cause()),
        None => anyhow::anyhow!("export failed: {e:#}"),
    }
}

/// The render of timeline `[a, b)` of `media` edited as `timeline`, cropped by `crop`.
fn edit_spec(
    media: &Media,
    timeline: &Timeline,
    (a, b): (Tick, Tick),
    crop: Option<[u32; 4]>,
) -> EditSpec {
    let video = media.video.is_some();
    let clips = timeline::extract(timeline.clips(), a, b);
    EditSpec {
        spans: clips
            .iter()
            .map(|c| {
                Span::of(
                    media.origin() + timeline.secs(c.start),
                    timeline.secs(c.len()),
                )
            })
            .collect(),
        fps: video.then_some(media.fps),
        stereo: timeline.stereo(),
        crop: crop
            .filter(|_| video)
            .map(|[x, y, w, h]| CropRect { x, y, w, h }),
        tracks: media.tracks(),
    }
}

/// What an Export does: the render of the timeline's In–Out (the whole timeline without a
/// selection) and the crop, on any thread — edited or not, the same render as Save
/// ([`crate::cut::render_and_publish`]), reading the one [`Input`].
struct ExportWork {
    spec: EditSpec,
    range: (f64, f64),
}

impl ExportWork {
    fn new(media: &Media, timeline: &Timeline, marks: &Marks, crop: Option<[u32; 4]>) -> Self {
        let (in_s, out_s) = marks.range(timeline.secs(timeline.len()));
        let (a, b) = (timeline.tick(in_s), timeline.tick(out_s));
        let (a, b) = (a.clamp(0, timeline.len()), b.clamp(0, timeline.len()));
        Self {
            spec: edit_spec(media, timeline, (a, b), crop),
            range: (timeline.secs(a), timeline.secs(b)),
        }
    }

    fn range(&self) -> (f64, f64) {
        self.range
    }

    fn crop(&self) -> Option<CropRect> {
        self.spec.crop
    }

    fn run(
        &self,
        origin: &mut Origin,
        media: &Media,
        work: &Path,
        progress: &mut dyn FnMut(f64),
    ) -> Result<(String, Vec<String>)> {
        let (a, b) = self.range;
        if b <= a || self.spec.spans.is_empty() {
            bail!(
                "Out ({}) must be after In ({})",
                clock_text(b),
                clock_text(a)
            );
        }
        export_edit(origin, media, &self.spec, (a, b), work, progress)
    }
}

/// The receipt hook's edits (`clips=`, `stereo=1`) on a fresh timeline.
fn apply_state(timeline: &mut Timeline, media: &Media, hooks: &Hooks) -> Result<()> {
    let state = &hooks.state;
    if !state.clips.is_empty() {
        let clips = state
            .clips
            .iter()
            .map(|&(a, b)| Clip {
                start: timeline.tick(a),
                end: timeline.tick(b),
            })
            .collect();
        if !timeline.set_clips(clips, View::default()) && !timeline.is_identity() {
            bail!("SOPEN_STATE clips: nothing inside the file");
        }
    }
    if state.stereo {
        if !media.mono_audio() {
            bail!("SOPEN_STATE stereo=1: the file is not mono audio");
        }
        timeline.set_stereo(true, View::default());
    }
    Ok(())
}

/// Opens the media file: the window, or the windowless `SOPEN_AUTOEXPORT` / `SOPEN_AUTOSAVE`
/// run. `notices` (where earlier copies' unsaved edits went) are shown whole in the window's
/// rows.
pub fn open(
    origin: Origin,
    local: &Path,
    cache: &Path,
    hooks: &Hooks,
    notices: Vec<String>,
) -> Result<Outcome> {
    let (_, name) = origin.parent();
    sweep(cache);
    let media = Media::probe_as(&Input::path(local), &name)?;
    let state = &hooks.state;
    if !state.ops.is_empty() {
        bail!("SOPEN_STATE op applies to images; media takes rect, t, in, out, clips and stereo");
    }
    let mut timeline = Timeline::new(media.rate(), media.ticks());
    apply_state(&mut timeline, &media, hooks)?;
    let length = timeline.secs(timeline.len());
    let marks = Marks {
        t: state.t.unwrap_or(0.0).clamp(0.0, length),
        in_s: state.in_s,
        out_s: state.out_s,
    };
    let mut sel = Selection::default();
    if let (Some(r), Some(_)) = (state.rect, media.video) {
        sel.set(r);
    }
    let work_root = crate::cache::exports_root(cache);
    let mut file = MediaFile::new(origin, cache);
    if hooks.autosave {
        file.hash_in_background();
        return autosave(&mut file, &media, &timeline, &work_root, hooks);
    }
    if hooks.autoexport {
        let crop = media.source_size().and_then(|b| sel.pixels(b));
        let job = ExportWork::new(&media, &timeline, &marks, crop);
        let started = Instant::now();
        for note in &media.notes {
            eprintln!("sopen: export: {note}");
        }
        let (path, notes) = job
            .run(&mut file.origin, &media, &work_root, &mut |_| {})
            .map_err(|e| export_error(&e))?;
        let (in_s, out_s) = job.range();
        let line = format!(
            "exported {path} ({} → {}{}) in {:.1} s",
            clock_text(in_s),
            clock_text(out_s),
            job.crop()
                .map(|r| format!(", crop {},{} {}×{}", r.x, r.y, r.w & !1, r.h & !1))
                .unwrap_or_default(),
            started.elapsed().as_secs_f64()
        );
        println!("{}", super::with_notes(&line, &notes));
        return Ok(Outcome {
            exported: true,
            ..Outcome::default()
        });
    }
    let title = file.origin.display();
    let size = match media.decode_size() {
        Some((w, h)) => [
            (w as f32 + 2.0 * super::chrome::CELL.x).clamp(640.0, 1280.0),
            (h as f32 + 150.0).clamp(420.0, 900.0),
        ],
        None => [1000.0, 460.0],
    };
    let mut window = MediaWindow::new(media, file, work_root, marks, sel, notices);
    window.timeline = timeline;
    window.export_hold = hooks.export_hold;
    window.ask_close = state.ask_close;
    let outcome = window.outcome.clone();
    let leftover = window.leftover.clone();
    let run = super::run_window(&title, size, window, hooks);
    // Never exit with a write in flight without its outcome on the terminal.
    if let Some(job) = leftover.lock().unwrap().take() {
        job.settle(true, &mut outcome.lock().unwrap());
    }
    run?;
    let outcome = outcome.lock().unwrap().clone();
    Ok(outcome)
}

/// What a Save or an Export left behind when its sopen was killed: removed before this window
/// starts its own ([`crate::cache::sweep_stale_jobs`]).
fn sweep(cache: &Path) {
    for gone in crate::cache::sweep_stale_jobs(cache) {
        crate::debug!(
            "removed {} (left by a sopen that was killed)",
            gone.display()
        );
    }
}

/// `SOPEN_AUTOSAVE`: the state's edits saved without a window (exit 3 when refused).
fn autosave(
    file: &mut MediaFile,
    media: &Media,
    timeline: &Timeline,
    work_root: &Path,
    hooks: &Hooks,
) -> Result<Outcome> {
    if timeline.is_identity() {
        bail!("SOPEN_AUTOSAVE on media needs an edit (SOPEN_STATE clips= or stereo=1)");
    }
    if let Some(delay) = hooks.save_delay {
        eprintln!("waiting {} ms before saving", delay.as_millis());
        std::thread::sleep(delay);
    }
    let spec = edit_spec(media, timeline, (0, timeline.len()), None);
    let mut outcome = Outcome::default();
    match file.save(media, &spec, work_root, &mut |_| {})? {
        Saved::Written {
            remote,
            notes,
            not_kept,
            ..
        } => {
            outcome.saved = true;
            let line = format!(
                "saved {remote} ({}, {})",
                clock_text(spec.length()),
                media.save_encodes()
            );
            println!("{}", super::with_notes(&line, &not_kept));
            for note in notes {
                eprintln!("sopen: {note}");
            }
        }
        Saved::Refused { message, .. } => {
            outcome.conflict = true;
            outcome.refused = Some(message);
        }
    }
    Ok(outcome)
}

/// The media window inside another window (the file browser): the window [`open`] shows for
/// the probed `media`, without the receipt hooks. `cache` is where exports are staged and Save
/// keeps its backup.
pub fn embed(origin: Origin, media: Media, cache: &Path, notices: Vec<String>) -> super::Embedded {
    sweep(cache);
    let work_root = crate::cache::exports_root(cache);
    let window = MediaWindow::new(
        media,
        MediaFile::new(origin, cache),
        work_root,
        Marks::default(),
        Selection::default(),
        notices,
    );
    let (outcome, leftover) = (window.outcome.clone(), window.leftover.clone());
    super::Embedded {
        window: Box::new(window),
        settle: Box::new(move |wait| {
            let mut slot = leftover.lock().unwrap();
            if let Some(job) = slot.as_ref() {
                if !job.settle(wait, &mut outcome.lock().unwrap()) {
                    return None;
                }
                *slot = None;
            }
            Some(outcome.lock().unwrap().clone())
        }),
    }
}

/// Prints a finished export to the terminal, records it and returns the one-line status and
/// what must be shown whole (the disclosure rows): what the export did not keep, or the error.
fn report_export(
    result: Result<(String, Vec<String>)>,
    outcome: &mut Outcome,
) -> (Status, Vec<String>) {
    match result {
        Ok((path, notes)) => {
            outcome.exported = true;
            println!("{}", super::with_notes(&format!("exported {path}"), &notes));
            (Status::info(format!("exported {path}")), notes)
        }
        Err(e) => {
            let e = export_error(&e);
            eprintln!("sopen: {e:#}");
            (Status::error("export failed"), vec![format!("{e:#}")])
        }
    }
}

/// Prints a finished save, records it and returns the status and the rows to show whole.
fn report_save(result: &Result<Saved>, outcome: &mut Outcome) -> (Status, Vec<String>) {
    match result {
        Ok(Saved::Written {
            remote,
            backup,
            notes,
            not_kept,
            media,
        }) => {
            outcome.saved = true;
            outcome.refused = None;
            println!(
                "{}",
                super::with_notes(&format!("saved {remote}"), not_kept)
            );
            for note in notes {
                eprintln!("sopen: {note}");
            }
            let mut shown: Vec<String> = notes.iter().chain(not_kept).cloned().collect();
            if let Err(e) = media.as_ref() {
                shown.push(format!(
                    "saved, but sopen cannot read the result back ({e}); editing is off — close \
                     and open the file again"
                ));
            }
            let status = Status::info(format!(
                "saved {remote} · original backed up at {}",
                backup.display()
            ));
            (status, shown)
        }
        Ok(Saved::Refused { message, .. }) => {
            outcome.conflict = true;
            outcome.refused = Some(message.clone());
            eprintln!("sopen: {message}");
            (
                Status::error("not saved: the file changed"),
                vec![message.clone()],
            )
        }
        Err(e) => {
            eprintln!("sopen: {e:#}");
            (Status::error("save failed"), vec![format!("{e:#}")])
        }
    }
}

struct ExportJob {
    /// 0..=1000 (per mille), written by the worker.
    progress: Arc<AtomicU32>,
    rx: Receiver<Result<(String, Vec<String>)>>,
}

struct SaveJob {
    progress: Arc<AtomicU32>,
    rx: Receiver<Result<Saved>>,
    close_after: bool,
}

/// A write still running when the window went: its opener waits for it and prints it.
enum Leftover {
    Export(ExportJob),
    Save(SaveJob),
}

impl Leftover {
    /// Takes what it reported — waiting for it (`wait`), or only once it has finished — and
    /// prints and counts it; false while it still runs (without `wait`).
    fn settle(&self, wait: bool, outcome: &mut Outcome) -> bool {
        let what = match self {
            Leftover::Export(_) => "an export",
            Leftover::Save(_) => "a save",
        };
        if wait {
            eprintln!("sopen: the window closed during {what}; waiting for it to finish");
        }
        fn take<T>(rx: &Receiver<Result<T>>, wait: bool, what: &str) -> Option<Result<T>> {
            let stopped = || Err(anyhow::anyhow!("{what} stopped without a result"));
            if wait {
                return Some(rx.recv().unwrap_or_else(|_| stopped()));
            }
            match rx.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(stopped()),
            }
        }
        match self {
            Leftover::Export(job) => match take(&job.rx, wait, "the export") {
                Some(result) => {
                    let _ = report_export(result, outcome);
                    true
                }
                None => false,
            },
            Leftover::Save(job) => match take(&job.rx, wait, "the save") {
                Some(result) => {
                    let _ = report_save(&result, outcome);
                    true
                }
                None => false,
            },
        }
    }
}

/// A clipboard shortcut whose key has not come up yet (see the image window's `Held`):
/// egui-winit repeats the event while the key is held; only the first one acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    Cut,
    Paste,
}

struct MediaWindow {
    media: Arc<Media>,
    file: Arc<Mutex<MediaFile>>,
    title: String,
    can_replace: bool,
    work_root: PathBuf,
    timeline: Timeline,
    /// Source ranges of this file (the system clipboard is not used for media).
    clipboard: Vec<Clip>,
    marks: Marks,
    sel: Selection,
    tex: Option<TextureHandle>,
    /// The frame on screen (timeline time, source frame when known) and the one asked for.
    tex_at: Option<(f64, Option<Tick>)>,
    want: Option<(f64, Tick)>,
    scrub: Option<Scrubber>,
    wave: Option<wave::Shared>,
    /// The waveform's columns for (timeline revision, columns, lanes).
    columns: Columns,
    player: Option<Player>,
    job: Option<ExportJob>,
    save: Option<SaveJob>,
    status: Status,
    outcome: Arc<Mutex<Outcome>>,
    started: bool,
    painted: bool,
    /// Close was asked for during an export: close when it is done.
    close_when_done: bool,
    /// Close was asked for during a save: close when it is done (and nothing is unsaved).
    close_when_saved: bool,
    close_ok: bool,
    /// The unsaved-changes prompt is up.
    prompt: bool,
    /// `SOPEN_STATE` `close=ask`: closing is asked for on the first pass.
    ask_close: bool,
    /// A Save landed but its file could not be read back: no more edits.
    stale: bool,
    /// Where a write still in flight goes if the window is dropped; `open` waits for it.
    leftover: Arc<Mutex<Option<Leftover>>>,
    /// Receipt hook: start an Export on open and hold it at 30 % (AGENTS.md "Receipts").
    export_hold: Option<std::time::Duration>,
    /// Playback paused by a timeline drag, to resume on release.
    resume: bool,
    /// Where a drag on the timeline started (timeline seconds): it selects from there.
    drag: Option<f64>,
    held: Option<Held>,
    /// A V key-down arrived (a plain V), and a paste was handled, since the last V key-up: a
    /// key-up with neither is Cmd/Ctrl+V whose press egui-winit swallowed (an empty system
    /// clipboard sends no event).
    v_down: bool,
    pasted: bool,
    /// Where playback sound goes: the device (tests: the null sink).
    sink: Sink,
    /// Where the last pass drew the frame (screen ↔ source pixels) and the timeline.
    map: Option<Map>,
    timeline_rect: Option<Rect>,
    /// Lines from open that must be read whole and stay (see [`super::disclosures`]).
    opened: Vec<String>,
    /// What the last write reported that must be read whole, and the next such report, held
    /// while the button is down.
    notices: Vec<String>,
    pending_notices: Option<Vec<String>>,
}

impl MediaWindow {
    /// The window as `open` makes it. `notices` (where earlier copies' unsaved edits went) are
    /// read whole, in the always-visible rows, for the whole session.
    fn new(
        media: Media,
        file: MediaFile,
        work_root: PathBuf,
        marks: Marks,
        sel: Selection,
        notices: Vec<String>,
    ) -> Self {
        let timeline = Timeline::new(media.rate(), media.ticks());
        let mut file = file;
        file.hash_in_background();
        let can_replace = file.origin.can_replace();
        Self {
            title: file.origin.display(),
            can_replace,
            media: Arc::new(media),
            file: Arc::new(Mutex::new(file)),
            work_root,
            timeline,
            clipboard: Vec::new(),
            marks,
            sel,
            tex: None,
            tex_at: None,
            want: None,
            scrub: None,
            wave: None,
            columns: None,
            player: None,
            job: None,
            save: None,
            status: Status::default(),
            outcome: Arc::default(),
            started: false,
            painted: false,
            close_when_done: false,
            close_when_saved: false,
            close_ok: false,
            prompt: false,
            ask_close: false,
            stale: false,
            leftover: Arc::default(),
            export_hold: None,
            resume: false,
            drag: None,
            held: None,
            v_down: false,
            pasted: false,
            sink: Sink::Device,
            map: None,
            timeline_rect: None,
            opened: notices,
            notices: Vec::new(),
            pending_notices: None,
        }
    }

    fn audio_only(&self) -> bool {
        self.media.video.is_none()
    }

    fn crop(&self) -> Option<[u32; 4]> {
        self.media.source_size().and_then(|b| self.sel.pixels(b))
    }

    /// The timeline's length in seconds.
    fn length(&self) -> f64 {
        self.timeline.secs(self.timeline.len())
    }

    /// The time of the frame on screen (timeline seconds).
    #[cfg(test)]
    fn tex_pts(&self) -> Option<f64> {
        self.tex_at.map(|(t, _)| t)
    }

    /// The frame asked for (at the playhead, of the timeline as it is now) is on screen.
    fn frame_ready(&self) -> bool {
        self.want
            .is_some_and(|(t, key)| self.tex_at == Some((t, Some(key))))
    }

    /// Unsaved edits: the output is not the file as it is.
    fn unsaved(&self) -> bool {
        !self.timeline.is_identity()
    }

    fn busy(&self) -> bool {
        self.job.is_some() || self.save.is_some()
    }

    /// Editing is possible: no save running (it renders the list as it was), nothing stale.
    fn editable(&self) -> bool {
        self.save.is_none() && !self.stale && !self.prompt
    }

    fn show_frame(&mut self, ctx: &egui::Context, rgba: &[u8], t: f64, key: Option<Tick>) {
        let Some((w, h)) = self.media.decode_size() else {
            return;
        };
        let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba);
        match &mut self.tex {
            Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("frame", image, egui::TextureOptions::LINEAR)),
        }
        self.tex_at = Some((t, key));
    }

    /// The source frame shown at timeline time `t`: past the picture's last frame (a longer
    /// sound track), the last frame holds.
    fn frame_at(&self, t: f64) -> Tick {
        let tl = &self.timeline;
        let tick = tl.tick(t).clamp(0, (tl.len() - 1).max(0));
        timeline::source_at(tl.clips(), tick).min(self.media.last_frame())
    }

    fn seek(&mut self, ctx: &egui::Context, t: f64) {
        let t = t.clamp(0.0, self.length());
        self.marks.t = t;
        if self.player.is_some() {
            self.player = None;
            self.play(ctx);
        } else if self.scrub.is_some() {
            let key = self.frame_at(t);
            self.want = Some((t, key));
            if let Some(scrub) = &self.scrub {
                scrub.request(t, key);
            }
        }
    }

    /// A seek from the timeline. While a drag is in progress playback pauses and frames are
    /// scrubbed (restarting both decoders and the audio device per mouse move would thrash);
    /// it resumes from the release point.
    fn scrub_to(&mut self, ctx: &egui::Context, t: f64, done: bool) {
        if self.player.take().is_some() {
            self.resume = true;
        }
        self.seek(ctx, t);
        if done && std::mem::take(&mut self.resume) {
            self.play(ctx);
        }
    }

    fn play(&mut self, ctx: &egui::Context) {
        let tl = &self.timeline;
        let length = self.length();
        let from = if self.marks.t >= length - 0.05 {
            0
        } else {
            tl.tick(self.marks.t)
        };
        let t0 = tl.secs(from);
        let media = &self.media;
        let pieces: Vec<Piece> = tl
            .from(from)
            .iter()
            .map(|c| Piece {
                start: media.origin() + tl.secs(c.start),
                len: tl.secs(c.len()),
            })
            .collect();
        let src = Source {
            input: media.input.clone(),
            duration: length,
            video: media.decode_size().map(|size| (size, media.fps)),
            audio: media.has_audio,
            pieces,
            stereo: tl.stereo(),
        };
        let waker = ctx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || waker.request_repaint());
        match Player::start(&media.tools, &src, t0, 1.0, self.sink, wake) {
            Ok(player) => {
                self.player = Some(player);
                self.status = Status::default();
            }
            Err(e) => self.status = Status::error(format!("cannot play: {e:#}")),
        }
    }

    fn pause(&mut self, ctx: &egui::Context) {
        if let Some(player) = self.player.take() {
            self.marks.t = player.clock(Instant::now()).min(self.length());
            let (shown, dropped) = player.stats();
            crate::debug!(
                "paused at {:.3}: {shown} frames shown, {dropped} dropped",
                self.marks.t
            );
            // The frame on screen is the one at the pause point; a scrub keeps them in step.
            self.seek(ctx, self.marks.t);
        }
    }

    fn pump(&mut self, ctx: &egui::Context) {
        let Some(player) = self.player.as_mut() else {
            return;
        };
        let pump = player.pump(Instant::now());
        let note = player.audio_note.clone();
        self.marks.t = pump.position;
        if let Some(frame) = pump.frame {
            self.show_frame(ctx, &frame.rgba, frame.pts, None);
        }
        if let Some(note) = note {
            self.status = Status::error(format!("{note} — playing without sound"));
        }
        if let Some(error) = &pump.error {
            self.status = Status::error(format!("playback stopped: {error}"));
        }
        if pump.ended {
            self.player = None;
            self.marks.t = pump.position;
        } else {
            ctx.request_repaint_after(pump.wake_in);
        }
    }

    /// `t` on the timeline's grid (a frame time on video): what the cut will be.
    fn snap(&self, t: f64) -> f64 {
        self.timeline.secs(self.timeline.tick(t))
    }

    /// `I`: the selection starts at the playhead (to the end, or to its Out).
    fn set_in(&mut self) {
        let t = self.snap(self.marks.t);
        if self.marks.out_s.is_some_and(|o| o <= t) {
            self.marks.out_s = None;
        }
        self.marks.in_s = Some(t);
    }

    /// `O`: the selection ends at the playhead (from the start, or from its In).
    fn set_out(&mut self) {
        let t = self.snap(self.marks.t);
        if self.marks.in_s.is_some_and(|i| i >= t) {
            self.marks.in_s = None;
        }
        self.marks.out_s = Some(t);
    }

    /// The selection in timeline ticks, when there is one that is not empty.
    fn selection(&self) -> Option<(Tick, Tick)> {
        if !self.marks.selected() {
            return None;
        }
        let (a, b) = self.marks.range(self.length());
        let (a, b) = (self.timeline.tick(a), self.timeline.tick(b));
        (b > a).then_some((a, b))
    }

    fn view(&self) -> View {
        View {
            at: self.timeline.tick(self.marks.t),
            sel: self.selection(),
        }
    }

    fn restore(&mut self, view: View) {
        self.marks.t = self.timeline.secs(view.at);
        match view.sel {
            Some((a, b)) => {
                self.marks.in_s = Some(self.timeline.secs(a));
                self.marks.out_s = Some(self.timeline.secs(b));
            }
            None => self.marks.clear(),
        }
    }

    /// After the edit list changed: the playhead and selection inside it, playback restarted
    /// from the playhead, or the paused frame there (usually kept: no decode).
    fn edited(&mut self, ctx: &egui::Context) {
        let length = self.length();
        self.marks.t = self.marks.t.clamp(0.0, length);
        if self.marks.in_s.is_some_and(|i| i >= length)
            || self.marks.out_s.is_some_and(|o| o > length)
        {
            self.marks.clear();
        }
        self.seek(ctx, self.marks.t);
    }

    fn secs_text(&self, ticks: Tick) -> String {
        format!("{:.2} s", self.timeline.secs(ticks))
    }

    /// Cmd/Ctrl+C: the selection to this window's clipboard.
    fn copy(&mut self) {
        let Some((a, b)) = self.selection() else {
            return self.status = Status::error("select a stretch of the timeline first");
        };
        if let Some(piece) = self.timeline.copy(a, b) {
            self.clipboard = piece;
            self.status = Status::info(format!("copied {}", self.secs_text(b - a)));
        }
    }

    /// Cmd/Ctrl+X (`keep` the cut on the clipboard) or Delete: the selection goes, the gap
    /// closes.
    fn remove(&mut self, ctx: &egui::Context, keep: bool) {
        if !self.editable() {
            return;
        }
        let Some((a, b)) = self.selection() else {
            return self.status = Status::error("select a stretch of the timeline first");
        };
        let view = self.view();
        match self.timeline.cut(a, b, view) {
            Some(piece) => {
                if keep {
                    self.clipboard = piece;
                }
                self.marks.clear();
                self.marks.t = self.timeline.secs(a);
                let what = if keep { "cut" } else { "deleted" };
                self.status = Status::info(format!("{what} {}", self.secs_text(b - a)));
                self.edited(ctx);
            }
            None => {
                self.status = Status::error("the whole timeline cannot be removed");
            }
        }
    }

    /// Cmd/Ctrl+V: the clipboard at the playhead, or over the selection.
    fn paste(&mut self, ctx: &egui::Context) {
        if !self.editable() {
            return;
        }
        if self.clipboard.is_empty() {
            return self.status = Status::error("nothing to paste — copy or cut a stretch first");
        }
        let view = self.view();
        let len = timeline::total(&self.clipboard);
        let piece = self.clipboard.clone();
        let (at, done) = match self.selection() {
            Some((a, b)) => (a, self.timeline.replace(a, b, &piece, view)),
            None => {
                let at = self.timeline.tick(self.marks.t);
                (at, self.timeline.insert(at, &piece, view))
            }
        };
        if done {
            self.marks.clear();
            self.marks.t = self.timeline.secs(at + len);
            self.status = Status::info(format!("pasted {}", self.secs_text(len)));
            self.edited(ctx);
        }
    }

    fn undo(&mut self, ctx: &egui::Context) {
        if !self.editable() {
            return;
        }
        if let Some(view) = self.timeline.undo(self.view()) {
            self.restore(view);
            self.status = Status::info("undone");
            self.edited(ctx);
        }
    }

    fn redo(&mut self, ctx: &egui::Context) {
        if !self.editable() {
            return;
        }
        if let Some(view) = self.timeline.redo(self.view()) {
            self.restore(view);
            self.status = Status::info("redone");
            self.edited(ctx);
        }
    }

    /// `M`: Mono → Stereo on (or off again), as one undo step.
    fn toggle_stereo(&mut self, ctx: &egui::Context) {
        if !self.editable() || !self.media.mono_audio() {
            return;
        }
        let on = !self.timeline.stereo();
        let view = self.view();
        if self.timeline.set_stereo(on, view) {
            self.status = Status::info(if on {
                "mono → stereo: left and right are both the mono channel"
            } else {
                "mono again"
            });
            self.edited(ctx);
        }
    }

    fn start_export(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let work = ExportWork::new(&self.media, &self.timeline, &self.marks, self.crop());
        let (in_s, out_s) = work.range();
        if out_s <= in_s {
            self.status = Status::error("Out must be after In");
            return;
        }
        let progress = Arc::new(AtomicU32::new(0));
        let (tx, rx) = mpsc::channel();
        let (media, file, root) = (
            self.media.clone(),
            self.file.clone(),
            self.work_root.clone(),
        );
        let (shared, waker) = (progress.clone(), ctx.clone());
        let mut hold = self.export_hold;
        std::thread::spawn(move || {
            // A worker that panicked mid-write must not lock the file out of every later
            // Save and Export: its state is the file's own (origin, backup), still valid.
            let mut file = file.lock().unwrap_or_else(|e| e.into_inner());
            let result = work.run(&mut file.origin, &media, &root, &mut |f| {
                shared.store((f * 1000.0) as u32, SeqCst);
                waker.request_repaint();
                if f >= 0.3 {
                    if let Some(d) = hold.take() {
                        // Receipt hook: ffmpeg blocks on its progress pipe meanwhile.
                        std::thread::sleep(d);
                    }
                }
            });
            let _ = tx.send(result);
            waker.request_repaint();
        });
        self.status = Status::info("exporting…");
        self.job = Some(ExportJob { progress, rx });
    }

    fn start_save(&mut self, ctx: &egui::Context, close_after: bool) {
        if self.busy() {
            return;
        }
        if self.stale {
            return self.status = Status::error("editing is off: close and open the file again");
        }
        if !self.can_replace {
            return self.status = Status::error(CANNOT_REPLACE);
        }
        if let Some(why) = self.media.save_block() {
            return self.status = Status::error(why);
        }
        if !self.unsaved() {
            return self.status = Status::info("no changes to save");
        }
        let spec = edit_spec(&self.media, &self.timeline, (0, self.timeline.len()), None);
        let progress = Arc::new(AtomicU32::new(0));
        let (tx, rx) = mpsc::channel();
        let (media, file, root) = (
            self.media.clone(),
            self.file.clone(),
            self.work_root.clone(),
        );
        let (shared, waker) = (progress.clone(), ctx.clone());
        // Save writes the edit list; the rectangle is Export's (it stays drawn).
        let saving = if self.crop().is_some() {
            "saving the edit (not the crop: that is Export's)…"
        } else {
            "saving…"
        };
        if let Some(player) = self.player.take() {
            self.marks.t = player.clock(Instant::now()).min(self.length());
        }
        std::thread::spawn(move || {
            // A worker that panicked mid-write must not lock the file out of every later
            // Save and Export: its state is the file's own (origin, backup), still valid.
            let mut file = file.lock().unwrap_or_else(|e| e.into_inner());
            let result = file.save(&media, &spec, &root, &mut |f| {
                shared.store((f * 1000.0) as u32, SeqCst);
                waker.request_repaint();
            });
            drop(file);
            let _ = tx.send(result);
            waker.request_repaint();
        });
        self.status = Status::info(saving);
        self.save = Some(SaveJob {
            progress,
            rx,
            close_after,
        });
    }

    fn poll_export(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else {
            return;
        };
        // A worker that ended without a word (it panicked) is a failure, said as one.
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(anyhow::anyhow!("the export stopped without a result"))
            }
        };
        self.job = None;
        let (status, shown) = report_export(result, &mut self.outcome.lock().unwrap());
        self.status = status;
        // The plan's notes are on screen from the start; show only what is new.
        let new: Vec<String> = shown
            .into_iter()
            .filter(|n| !self.media.notes.contains(n))
            .collect();
        self.pending_notices = Some(new);
        if std::mem::take(&mut self.close_when_done) {
            self.close_ok = true;
            super::request_close(ctx);
        }
    }

    /// The new file a save landed: the window plays it, unedited (the edit list, its history
    /// and the clipboard were ranges of the old one).
    fn reload(&mut self, ctx: &egui::Context, media: Media) {
        let t = self.marks.t;
        self.player = None;
        self.scrub = None;
        self.wave = None;
        self.columns = None;
        self.tex_at = None;
        self.want = None;
        self.timeline = Timeline::new(media.rate(), media.ticks());
        self.clipboard.clear();
        self.marks = Marks::default();
        self.media = Arc::new(media);
        self.start_decoders(ctx);
        self.seek(ctx, t);
    }

    fn poll_save(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.save else {
            return;
        };
        // A worker that ended without a word (it panicked) is a failure, said as one: the edits
        // stay and the window is usable again.
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(anyhow::anyhow!("the save stopped without a result"))
            }
        };
        let job = self.save.take().expect("checked above");
        let (status, shown) = report_save(&result, &mut self.outcome.lock().unwrap());
        self.status = status;
        self.pending_notices = Some(shown);
        let mut landed_clean = false;
        if let Ok(Saved::Written { media, notes, .. }) = result {
            match *media {
                Ok(media) => self.reload(ctx, media),
                Err(_) => self.stale = true,
            }
            landed_clean = notes.is_empty();
        }
        let waited = std::mem::take(&mut self.close_when_saved);
        if (job.close_after && landed_clean) || (waited && !self.unsaved()) {
            self.close_ok = true;
            super::request_close(ctx);
        } else if job.close_after || waited {
            // Closing was asked for, but the save did not land: ask what to do.
            self.prompt = true;
        }
    }

    /// Cmd/Ctrl+C, X and V (egui-winit hands them over as events, never as key presses) and
    /// the key-ups that end a held one — not while a text field has the keyboard or the prompt
    /// is up. A Cmd/Ctrl+V whose press sent nothing (the system clipboard was empty) is
    /// recognised at its V key-up: this window's clipboard is its own.
    fn clipboard(&mut self, ctx: &egui::Context) {
        let off = self.prompt || ctx.text_edit_focused();
        let events: Vec<egui::Event> = ctx.input(|i| {
            i.events
                .iter()
                .filter(|e| {
                    matches!(
                        e,
                        egui::Event::Copy
                            | egui::Event::Cut
                            | egui::Event::Paste(_)
                            | egui::Event::PasteImage(_)
                            | egui::Event::Key { .. }
                            | egui::Event::WindowFocused(false)
                            | egui::Event::ModifiersChanged(_)
                    )
                })
                .cloned()
                .collect()
        });
        for event in events {
            match event {
                egui::Event::Key {
                    key: Key::V,
                    pressed: true,
                    ..
                } => self.v_down = true,
                egui::Event::Key {
                    key: Key::V,
                    pressed: false,
                    ..
                } => {
                    let swallowed = !self.v_down && !self.pasted;
                    if self.held == Some(Held::Paste) {
                        self.held = None;
                    }
                    self.v_down = false;
                    self.pasted = false;
                    if swallowed && !off {
                        self.paste(ctx);
                    }
                }
                egui::Event::Key {
                    key: Key::X | Key::Cut,
                    pressed: false,
                    ..
                } => {
                    if self.held == Some(Held::Cut) {
                        self.held = None;
                    }
                }
                egui::Event::Key { .. } => {}
                egui::Event::WindowFocused(_) => self.held = None,
                egui::Event::ModifiersChanged(mods) => {
                    if !mods.command {
                        self.held = None;
                    }
                }
                egui::Event::Copy => {
                    if !off {
                        self.copy();
                    }
                }
                egui::Event::Cut => {
                    if !off && self.held != Some(Held::Cut) {
                        self.remove(ctx, true);
                        self.held = Some(Held::Cut);
                    }
                }
                egui::Event::Paste(_) | egui::Event::PasteImage(_) => {
                    self.pasted = true;
                    if !off && self.held != Some(Held::Paste) {
                        self.paste(ctx);
                        self.held = Some(Held::Paste);
                    }
                }
                _ => {}
            }
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
        use super::{command_key, plain_key};
        if self.prompt {
            return;
        }
        if plain_key(ctx, Key::Space) {
            if self.player.is_some() {
                self.pause(ctx);
            } else {
                self.play(ctx);
            }
        }
        if plain_key(ctx, Key::I) {
            self.set_in();
        }
        if plain_key(ctx, Key::O) {
            self.set_out();
        }
        if plain_key(ctx, Key::E) {
            self.start_export(ctx);
        }
        if command_key(ctx, Key::Z, true) {
            self.redo(ctx);
        } else if command_key(ctx, Key::Z, false) {
            self.undo(ctx);
        }
        // F5 is PC Shell's Save, as Cmd/Ctrl+S (the image and hex windows' rule).
        if command_key(ctx, Key::S, false) || plain_key(ctx, Key::F5) {
            self.start_save(ctx, false);
        }
        if plain_key(ctx, Key::Delete) || plain_key(ctx, Key::Backspace) {
            self.remove(ctx, false);
        }
        if plain_key(ctx, Key::M) {
            self.toggle_stereo(ctx);
        }
        // Esc: the crop rectangle first, then the selection (with neither, the browser
        // around a hosted window takes it to leave).
        if self.sel.is_set() && plain_key(ctx, Key::Escape) {
            self.sel.clear();
        } else if self.marks.selected() && plain_key(ctx, Key::Escape) {
            self.marks.clear();
        }
        let tick = 1.0 / self.timeline.rate;
        let fine = if self.audio_only() { 0.01 } else { tick };
        let step = |ctx: &egui::Context, key| {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, key)) {
                Some(5.0)
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, key)) {
                Some(fine)
            } else if plain_key(ctx, key) {
                Some(1.0)
            } else {
                None
            }
        };
        if let Some(s) = step(ctx, Key::ArrowLeft) {
            self.seek(ctx, self.marks.t - s);
        }
        if let Some(s) = step(ctx, Key::ArrowRight) {
            self.seek(ctx, self.marks.t + s);
        }
    }

    /// The waveform's columns for the timeline as it is now (from the peaks so far; no decode).
    fn wave_columns(&mut self, width: usize, lanes: usize) -> Option<Vec<Vec<[f32; 2]>>> {
        let shared = self.wave.as_ref()?.clone();
        let guard = shared.lock().unwrap();
        let peaks = guard.as_ref()?;
        let key = (self.timeline.rev(), peaks.version, width, lanes);
        if let Some((k, cols)) = &self.columns {
            if *k == key {
                return Some(cols.clone());
            }
        }
        let cols = wave::columns(peaks, &self.timeline, width, lanes);
        drop(guard);
        self.columns = Some((key, cols.clone()));
        Some(cols)
    }

    /// The timeline: selection, waveform lanes (audio), clip boundaries, markers, playhead.
    /// A click moves the playhead and clears the selection; a drag selects, the playhead
    /// following the pointer (so the frame there shows). Returns a seek and whether the gesture
    /// ended.
    fn timeline(&mut self, ui: &mut egui::Ui, height: f32, waveform: bool) -> Option<(f64, bool)> {
        let (rect, resp) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), height),
            Sense::click_and_drag(),
        );
        self.timeline_rect = Some(rect);
        let p = super::chrome::PALETTE;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, p.timeline);
        let duration = self.length().max(1e-6);
        let x_of = |t: f64| rect.left() + (t / duration) as f32 * rect.width();
        let (in_s, out_s) = self.marks.range(duration);
        if self.marks.selected() {
            let span = Rect::from_x_y_ranges(x_of(in_s)..=x_of(out_s), rect.y_range());
            painter.rect_filled(span, 0.0, p.span);
        }
        let font = super::chrome::font();
        if waveform {
            let lanes = if self.timeline.stereo() {
                2
            } else {
                self.media.lanes().max(1)
            };
            let width = rect.width().max(1.0) as usize;
            if let Some(cols) = self.wave_columns(width, lanes) {
                let lane_h = rect.height() / lanes as f32;
                for (lane, column) in cols.iter().enumerate() {
                    let top = rect.top() + lane as f32 * lane_h;
                    let mid = top + lane_h / 2.0;
                    let amp = lane_h * 0.45;
                    let n = column.len() as f32;
                    for (k, [lo, hi]) in column.iter().enumerate() {
                        let x = rect.left() + (k as f32 + 0.5) / n * rect.width();
                        painter.line_segment(
                            [Pos2::new(x, mid - hi * amp), Pos2::new(x, mid - lo * amp)],
                            Stroke::new(1.0, p.wave),
                        );
                    }
                    if lanes == 2 {
                        painter.text(
                            Pos2::new(rect.left() + 3.0, top + 2.0),
                            egui::Align2::LEFT_TOP,
                            if lane == 0 { "L" } else { "R" },
                            font.clone(),
                            p.dim,
                        );
                    }
                }
            } else {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "reading the waveform…",
                    font.clone(),
                    p.dim,
                );
            }
            // A decode that failed is said where the waveform is, not drawn as silence.
            let failed = self
                .wave
                .as_ref()
                .and_then(|w| w.lock().unwrap().as_ref().and_then(|p| p.error.clone()));
            if let Some(why) = failed {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("cannot read the waveform: {why}"),
                    font.clone(),
                    p.error,
                );
            }
        }
        // Where the edits joined pieces of the source.
        for start in timeline::starts(self.timeline.clips()).into_iter().skip(1) {
            let x = x_of(self.timeline.secs(start));
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(1.0, p.dim),
            );
        }
        // Each marker's line, and its label laid out (measured, not counted): right of the
        // line, left of it where the right would leave the timeline. When In's and Out's
        // labels would still meet (a narrow selection, either end), In's goes left of its
        // line if there is room, else Out's drops to a second row — the timeline's height
        // never changes for it.
        let row = ui
            .text_style_height(&egui::TextStyle::Body)
            .max(super::chrome::CELL.y);
        let place = |x: f64, label: &str| -> Rect {
            let galley = painter.layout_no_wrap(label.to_string(), font.clone(), Color32::WHITE);
            let w = galley.size().x;
            let x = x_of(x);
            let left = x + 3.0 + w > rect.right() && x - 3.0 - w >= rect.left();
            let x0 = if left { x - 3.0 - w } else { x + 3.0 };
            Rect::from_min_size(Pos2::new(x0, rect.top() + 2.0), Vec2::new(w, row))
        };
        let flip_left = |r: Rect, x: f64| -> Rect {
            let x = x_of(x);
            Rect::from_min_size(Pos2::new(x - 3.0 - r.width(), r.top()), r.size())
        };
        let in_label = self
            .marks
            .in_s
            .map(|i| (i, format!("In {}", clock_text(i))));
        let out_label = self
            .marks
            .out_s
            .map(|o| (o, format!("Out {}", clock_text(o))));
        let mut in_rect = in_label.as_ref().map(|(i, l)| place(*i, l));
        let mut out_rect = out_label.as_ref().map(|(o, l)| place(*o, l));
        if let (Some((i, _)), Some(ir), Some(or)) = (&in_label, in_rect, out_rect) {
            if ir.intersects(or) {
                let left = flip_left(ir, *i);
                if left.left() >= rect.left() && !left.intersects(or) {
                    in_rect = Some(left);
                } else {
                    out_rect = Some(or.translate(Vec2::new(0.0, row)));
                }
            }
        }
        for (mark, color, at) in [
            (in_label.zip(in_rect), p.mark_in, self.marks.in_s),
            (out_label.zip(out_rect), p.mark_out, self.marks.out_s),
        ] {
            let (Some(((_, label), r)), Some(t)) = (mark, at) else {
                continue;
            };
            let x = x_of(t);
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(2.0, color),
            );
            painter.text(r.min, egui::Align2::LEFT_TOP, label, font.clone(), color);
        }
        let playhead = x_of(self.marks.t);
        painter.line_segment(
            [
                Pos2::new(playhead, rect.top()),
                Pos2::new(playhead, rect.bottom()),
            ],
            Stroke::new(2.0, p.playhead),
        );
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let at =
            |p: Pos2| f64::from(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0)) * duration;
        if resp.drag_started() {
            let origin = ui
                .input(|i| i.pointer.press_origin())
                .or(resp.interact_pointer_pos());
            self.drag = origin.map(at);
        }
        let pointer = resp.interact_pointer_pos();
        if let (Some(anchor), Some(p)) = (self.drag, pointer) {
            if resp.dragged() || resp.drag_stopped() {
                let t = at(p);
                let tl = &self.timeline;
                let snap = |t: f64| tl.secs(tl.tick(t));
                self.marks.in_s = Some(snap(anchor.min(t)));
                self.marks.out_s = Some(snap(anchor.max(t)));
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
            let t = pointer.map_or(self.marks.t, at);
            return Some((t, true));
        }
        if resp.clicked() {
            self.marks.clear();
            if let Some(p) = pointer {
                return Some((at(p), true));
            }
        }
        if resp.dragged() {
            if let Some(p) = pointer {
                return Some((at(p), false));
            }
        }
        None
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let editable = self.editable();
        ui.horizontal_wrapped(|ui| {
            let playing = self.player.is_some();
            // Fixed width: "Play" ↔ "Pause" must not reflow the row.
            let play = egui::Button::new(if playing { "Pause" } else { "Play" })
                .min_size(Vec2::new(64.0, 0.0));
            if ui.add(play).on_hover_text("Space").clicked() {
                if playing {
                    self.pause(&ctx);
                } else {
                    self.play(&ctx);
                }
            }
            if ui
                .button("In  I")
                .on_hover_text("the selection starts at the playhead")
                .clicked()
            {
                self.set_in();
            }
            if ui
                .button("Out  O")
                .on_hover_text("the selection ends at the playhead")
                .clicked()
            {
                self.set_out();
            }
            if !self.audio_only()
                && ui
                    .add_enabled(self.crop().is_some(), egui::Button::new("Clear rect"))
                    .on_hover_text("Esc")
                    .clicked()
            {
                self.sel.clear();
            }
            ui.separator();
            if ui
                .add_enabled(
                    editable && self.timeline.can_undo(),
                    egui::Button::new("Undo"),
                )
                .on_hover_text("⌘/Ctrl+Z")
                .clicked()
            {
                self.undo(&ctx);
            }
            if ui
                .add_enabled(
                    editable && self.timeline.can_redo(),
                    egui::Button::new("Redo"),
                )
                .on_hover_text("⇧⌘/Shift+Ctrl+Z")
                .clicked()
            {
                self.redo(&ctx);
            }
            if self.media.mono_audio() {
                let on = self.timeline.stereo();
                let button = ui.add_enabled_ui(editable, |ui| {
                    super::chrome::choice(ui, on, "Mono → Stereo  M").on_hover_text(
                        "the output's left and right are both the mono channel (Save writes a \
                         stereo file)",
                    )
                });
                if button.inner.clicked() {
                    self.toggle_stereo(&ctx);
                }
            }
            ui.separator();
            let busy = self.busy();
            let save_ok = editable
                && !busy
                && self.unsaved()
                && self.can_replace
                && self.media.save_block().is_none();
            let why = if !self.can_replace {
                CANNOT_REPLACE.to_string()
            } else if let Some(why) = self.media.save_block() {
                why
            } else if busy {
                "a save or export is running".to_string()
            } else {
                "no changes to save".to_string()
            };
            if ui
                .add_enabled(save_ok, egui::Button::new("Save"))
                .on_hover_text(format!(
                    "⌘/Ctrl+S or F5 — renders the edit ({}) and overwrites the original after \
                     a local backup; the crop rectangle is Export's, not Save's",
                    self.media.save_encodes()
                ))
                .on_disabled_hover_text(why)
                .clicked()
            {
                self.start_save(&ctx, false);
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Export  E"))
                .on_hover_text(
                    "re-encode In–Out of the timeline (and the crop) into a new file next to \
                     the original",
                )
                .clicked()
            {
                self.start_export(&ctx);
            }
        });
        // The clock line has a row of its own, always there and always one line — cut with "…"
        // (whole on hover) where the window is too narrow (a portrait clip makes a narrow
        // window); in the buttons' wrapping row it broke mid-text. A running export or save
        // shows its spinner and progress at the row's end, no taller than the row: a row that
        // came and went with it would move the frame under the pointer.
        let progress = match (&self.job, &self.save) {
            (Some(job), _) => Some(("exporting", job.progress.load(SeqCst))),
            (_, Some(save)) => Some(("saving", save.progress.load(SeqCst))),
            _ => None,
        };
        let line = egui::RichText::new(transport_text(&self.marks, self.length())).monospace();
        ui.horizontal(|ui| {
            ui.set_min_height(ui.spacing().interact_size.y);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some((what, per_mille)) = progress {
                    ui.label(format!("{what} {:.0} %", per_mille as f32 / 10.0));
                    let size = ui.text_style_height(&egui::TextStyle::Body);
                    ui.add(egui::Spinner::new().size(size));
                    ui.separator();
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(line).truncate());
                });
            });
        });
    }

    /// One line whatever it says (see [`super::one_line`]): the frame below must not move.
    fn info(&self, ui: &mut egui::Ui) {
        let m = &self.media;
        ui.horizontal(|ui| {
            match m.video {
                Some((w, h)) => ui.monospace(format!(
                    "{w}×{h}{} · {:.2} fps · {}{}",
                    if m.rotation == 0 {
                        String::new()
                    } else {
                        format!(" (rotated {}°)", m.rotation)
                    },
                    m.fps,
                    clock_text(m.duration),
                    if m.has_audio {
                        " · audio"
                    } else {
                        " · no audio"
                    }
                )),
                None => ui.monospace(format!(
                    "audio · {}{} · {}",
                    match m.channels {
                        1 => "mono".to_string(),
                        2 => "stereo".to_string(),
                        n => format!("{n} channels"),
                    },
                    m.sample_rate
                        .map(|r| format!(" · {r} Hz"))
                        .unwrap_or_default(),
                    clock_text(m.duration)
                )),
            };
            if let Some([x, y, w, h]) = self.crop() {
                ui.separator();
                let even = if w % 2 + h % 2 > 0 {
                    format!(" (exports {}×{})", w & !1, h & !1)
                } else {
                    String::new()
                };
                // Cut, not wrapped, on a narrow window: this line changes during a drag.
                let text = egui::RichText::new(format!("crop {x},{y} {w}×{h}{even}")).monospace();
                ui.add(egui::Label::new(text).truncate());
            }
            if self.unsaved() {
                ui.separator();
                let pieces = self.timeline.clips().len();
                let text = format!(
                    "unsaved: {pieces} piece{}{}",
                    if pieces == 1 { "" } else { "s" },
                    if self.timeline.stereo() {
                        ", stereo"
                    } else {
                        ""
                    }
                );
                ui.add(egui::Label::new(egui::RichText::new(text)).truncate());
            }
            ui.separator();
            self.status.show(ui);
        });
    }

    fn picture(&mut self, ui: &mut egui::Ui) {
        let Some(src) = self.media.source_size() else {
            return;
        };
        // The window's frame; the frame on black inside it.
        let outer = ui.max_rect();
        let p = super::chrome::PALETTE;
        let panel = super::chrome::frame(&ui.painter_at(outer), outer, VIDEO_TITLE, true);
        let painter = ui.painter_at(panel);
        painter.rect_filled(panel, 0.0, p.picture_back);
        let Some(tex) = &self.tex else {
            painter.text(
                panel.center(),
                egui::Align2::CENTER_CENTER,
                "decoding…",
                super::chrome::font(),
                p.dim,
            );
            return;
        };
        let disp = super::fit(panel.shrink(4.0), src);
        let map = Map { disp, src };
        let resp = ui.allocate_rect(disp, Sense::click_and_drag());
        painter.image(
            tex.id(),
            disp,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        if !self.prompt {
            self.sel.handle(ui, &resp, map);
        }
        self.sel.draw(&painter, map);
        self.map = Some(map);
        self.painted = true;
    }

    fn close_prompt(&mut self, ctx: &egui::Context) {
        if !self.prompt {
            return;
        }
        let id = egui::Id::unique("unsaved-media");
        let can_save = self.can_replace && self.media.save_block().is_none() && !self.stale;
        let exporting = self.job.is_some();
        let modal = super::chrome::dialog(ctx, id, "Unsaved changes", |ui| {
            ui.set_width(360.0);
            ui.label(format!(
                "Save the changes to {} before closing?",
                self.title
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if can_save
                    && ui
                        .add_enabled(!exporting, egui::Button::new("Save"))
                        .on_disabled_hover_text("an export is running")
                        .clicked()
                {
                    self.prompt = false;
                    self.start_save(ctx, true);
                }
                if ui.button("Discard").clicked() {
                    self.prompt = false;
                    self.close_ok = !exporting;
                    // With an export running the window still waits for it (closing again
                    // closes it now): that rule is unchanged by the edits.
                    self.timeline = Timeline::new(self.media.rate(), self.media.ticks());
                    super::request_close(ctx);
                }
                if ui.button("Cancel").clicked() {
                    self.prompt = false;
                }
            });
        });
        if modal.should_close() {
            self.prompt = false;
        }
    }

    /// The decoders the window keeps for its file: the paused-frame scrubber (video) and the
    /// waveform (audio).
    fn start_decoders(&mut self, ctx: &egui::Context) {
        self.scrub = Scrubber::start(&self.media, ctx.clone());
        if self.audio_only() {
            let waker = ctx.clone();
            self.wave = Some(wave::start(
                &self.media.tools,
                &self.media.input,
                self.media.lanes().max(1),
                self.media.duration,
                move || waker.request_repaint(),
            ));
        }
    }
}

impl Drop for MediaWindow {
    fn drop(&mut self) {
        let left = match (self.job.take(), self.save.take()) {
            (Some(job), _) => Some(Leftover::Export(job)),
            (None, Some(save)) => Some(Leftover::Save(save)),
            _ => None,
        };
        if left.is_some() {
            *self.leftover.lock().unwrap() = left;
        }
    }
}

impl super::Window for MediaWindow {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !self.started {
            self.started = true;
            self.start_decoders(&ctx);
            self.seek(&ctx, self.marks.t);
            if self.export_hold.is_some() {
                self.start_export(&ctx);
            }
        }
        if std::mem::take(&mut self.ask_close) {
            // `SOPEN_STATE` `close=ask`: as if closing were asked for (the prompt's receipt).
            super::Window::close_requested(self);
        }
        self.poll_export(&ctx);
        self.poll_save(&ctx);
        self.keys(&ctx);
        self.clipboard(&ctx);
        self.pump(&ctx);
        if self.player.is_none() {
            let scrubbed = self
                .scrub
                .as_ref()
                .and_then(|s| s.result.lock().unwrap().take());
            match scrubbed {
                Some(Ok(frame)) => {
                    let rgba = frame.rgba.clone();
                    self.show_frame(&ctx, &rgba, frame.t, Some(frame.key));
                }
                Some(Err(e)) => self.status = Status::error(e),
                None => {}
            }
        }
        // A report that lands while the button is down (a drag, or a press that may become one)
        // waits until it is let go: its row would move the frame under the pointer.
        let held = self.sel.dragging() || ctx.input(|i| i.pointer.primary_down());
        if !held {
            if let Some(next) = self.pending_notices.take() {
                self.notices = next;
            }
        }
        egui::Panel::top("info").show(ui, |ui| {
            ui.add_space(3.0);
            self.info(ui);
            ui.add_space(1.0);
        });
        let warn = ui.visuals().warn_fg_color;
        let error = ui.visuals().error_fg_color;
        let lines: Vec<(String, Color32)> = (!self.media.notes.is_empty())
            .then(|| {
                (
                    format!("Save / Export: {}", self.media.notes.join("; ")),
                    warn,
                )
            })
            .into_iter()
            .chain(
                self.opened
                    .iter()
                    .chain(&self.notices)
                    .map(|n| (n.clone(), error)),
            )
            .collect();
        if !lines.is_empty() {
            egui::Panel::top("disclosures").show(ui, |ui| {
                super::disclosures(ui, &lines);
                ui.add_space(2.0);
            });
        }
        let mut seek = None;
        let audio_only = self.audio_only();
        // The F-key row, the window's last (on its own: the browser around it has the row).
        super::own_keys(ui, super::chrome::MEDIA_KEYS);
        egui::Panel::bottom("transport").show(ui, |ui| {
            ui.add_space(4.0);
            if !audio_only {
                seek = self.timeline(ui, 34.0, false);
                ui.add_space(4.0);
            }
            self.transport(ui);
            ui.add_space(4.0);
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                if audio_only {
                    // The window's frame, the waveform's timeline inside it.
                    let outer = ui.max_rect();
                    let inner =
                        super::chrome::frame(&ui.painter_at(outer), outer, AUDIO_TITLE, true);
                    let room = inner.shrink(4.0);
                    ui.scope_builder(egui::UiBuilder::new().max_rect(room), |ui| {
                        let h = ui.available_height().max(60.0);
                        seek = self.timeline(ui, h, true).or(seek);
                    });
                    self.painted = self.wave.as_ref().is_some_and(|w| {
                        w.lock()
                            .unwrap()
                            .as_ref()
                            .is_some_and(|p| p.complete || p.error.is_some())
                    });
                } else {
                    self.picture(ui);
                }
            });
        if let Some((t, done)) = seek {
            self.scrub_to(&ctx, t, done);
        }
        self.close_prompt(&ctx);
    }

    fn ready(&self) -> bool {
        let frame_ok = self.audio_only() || self.frame_ready();
        // With the export hook, the shot is taken while the export is held mid-way.
        let export_ok = self.export_hold.is_none()
            || self
                .job
                .as_ref()
                .is_some_and(|j| j.progress.load(SeqCst) >= 300);
        self.painted && frame_ok && self.player.is_none() && export_ok && self.save.is_none()
    }

    fn keys(&self) -> &'static [super::chrome::FKey] {
        super::chrome::MEDIA_KEYS
    }

    fn close_requested(&mut self) -> bool {
        if self.close_ok {
            return true;
        }
        if self.save.is_some() {
            // Never close mid-write: the window closes once the save has reported. Said in the
            // rows (read whole on a narrow window), after any report still held.
            self.close_when_saved = true;
            self.status = Status::info("saving…");
            let mut next = self
                .pending_notices
                .take()
                .unwrap_or_else(|| self.notices.clone());
            if !next.iter().any(|n| n == CLOSES_AFTER_SAVE) {
                next.push(CLOSES_AFTER_SAVE.to_string());
            }
            self.pending_notices = Some(next);
            return false;
        }
        if self.unsaved() && !self.stale {
            self.prompt = true;
            return false;
        }
        if self.job.is_none() {
            return true;
        }
        if self.close_when_done {
            // Second request: the window goes now; `open` waits for the export and prints
            // its outcome in the terminal.
            return true;
        }
        self.close_when_done = true;
        self.status = Status::info("exporting…");
        // What a second close does goes to the rows (read whole), after any report still held.
        let mut next = self
            .pending_notices
            .take()
            .unwrap_or_else(|| self.notices.clone());
        next.push(CLOSE_AGAIN.to_string());
        self.pending_notices = Some(next);
        false
    }
}

mod file;

pub mod timeline;
pub mod wave;

#[cfg(test)]
mod edit_tests;

#[cfg(test)]
mod input_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{LocalFs, RemoteFs};

    /// Closing during an export: the first request keeps the window (it closes when the export
    /// reports), the second closes it, and the in-flight export is handed to `open`, which
    /// waits and prints its outcome — never an exit with the write unreported.
    #[test]
    fn close_during_export_hands_the_job_over() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.mp4");
        std::fs::write(&path, b"x").unwrap();
        let abs = path.to_str().unwrap().to_string();
        let mut fs = LocalFs::new();
        let at = fs.stat(&abs).unwrap();
        let origin = Origin::new(Box::new(fs), None, None, abs, at);
        let media = test_media(path, Some((64, 36)), 4.0);
        let leftover: Arc<Mutex<Option<Leftover>>> = Arc::default();
        let (tx, rx) = mpsc::channel();
        let file = MediaFile::new(origin, &tmp.path().join("cache"));
        let mut w = MediaWindow::new(
            media,
            file,
            tmp.path().join("work"),
            Marks::default(),
            Selection::default(),
            Vec::new(),
        );
        w.sink = Sink::Null;
        w.leftover = leftover.clone();
        w.job = Some(ExportJob {
            progress: Arc::default(),
            rx,
        });
        use super::super::Window;
        assert!(!w.close_requested(), "first close waits for the export");
        assert_eq!(w.pending_notices, Some(vec![CLOSE_AGAIN.to_string()]));
        assert!(w.close_requested(), "second close closes the window");
        drop(w);
        let Some(Leftover::Export(job)) = leftover.lock().unwrap().take() else {
            panic!("export handed over");
        };
        tx.send(Ok(("box:/v.cut-1-2.mp4".to_string(), Vec::new())))
            .unwrap();
        let mut outcome = Outcome::default();
        let (status, _) = report_export(job.rx.recv().unwrap(), &mut outcome);
        assert!(outcome.exported);
        assert!(status.text.contains("exported box:/v.cut-1-2.mp4"));
    }

    /// A media file as the probe would describe it, without running ffprobe.
    pub(super) fn test_media(local: PathBuf, video: Option<(u32, u32)>, duration: f64) -> Media {
        Media {
            tools: Tools {
                ffmpeg: "ffmpeg".into(),
                ffprobe: "ffprobe".into(),
            },
            input: Input::path(&local),
            duration,
            video,
            rotation: 0,
            fps: 25.0,
            has_audio: false,
            channels: 0,
            sample_rate: None,
            notes: Vec::new(),
            probe: ffmpeg::parse_probe(r#"{"streams":[]}"#).unwrap(),
            plan: None,
            name: "v.mp4".to_string(),
        }
    }

    #[test]
    fn transport_text_keeps_its_width_wherever_the_playhead_is() {
        let duration = 754.0;
        let width = |t: f64, in_s: Option<f64>, out_s: Option<f64>| {
            let marks = Marks { t, in_s, out_s };
            transport_text(&marks, duration).chars().count()
        };
        let full = width(0.0, None, None);
        assert_eq!(
            transport_text(&Marks::default(), duration),
            " 0:00.00 / 12:34.00  ·  In  0:00.00  Out 12:34.00  (754.00 s)"
        );
        for (t, i, o) in [
            (599.99, None, None),
            (600.0, Some(1.0), Some(2.5)),
            (754.0, Some(600.0), None),
        ] {
            assert_eq!(width(t, i, o), full, "t={t} in={i:?} out={o:?}");
        }
    }

    #[test]
    fn clock_text_shapes() {
        assert_eq!(clock_text(0.0), "0:00.00");
        assert_eq!(clock_text(3.5), "0:03.50");
        assert_eq!(clock_text(62.257), "1:02.26");
        assert_eq!(clock_text(3723.0), "1:02:03.00");
        assert_eq!(clock_text(-1.0), "0:00.00");
    }

    #[test]
    fn marks_default_to_the_whole_file() {
        let mut m = Marks::default();
        assert_eq!(m.range(9.0), (0.0, 9.0));
        m.in_s = Some(2.0);
        assert_eq!(m.range(9.0), (2.0, 9.0));
    }

    fn tools_or_skip() -> Option<Tools> {
        match ffmpeg::resolve() {
            Ok(t) => Some(t),
            Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
                panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
            }
            Err(e) => {
                eprintln!("skipping media export test: {e}");
                None
            }
        }
    }

    /// A phone video stored 640×360 with a 90° display matrix: the window works in the
    /// displayed 360×640 — the preview frames come out upright at that size and a crop drawn in
    /// those pixels exports as drawn. Skips where this ffmpeg cannot tag a rotation.
    #[test]
    fn rotated_video_previews_and_crops_in_display_pixels() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plain.mp4");
        let ok = std::process::Command::new(&tools.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=25:duration=2",
            ])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&plain)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let src = tmp.path().join("phone.mp4");
        // ffprobe decides whether the fixture is rotated; the app's probe must then agree.
        if !crate::ffmpeg::fixtures::rotated_copy(&tools, &plain, &src) {
            return; // this ffmpeg cannot tag a fixture as rotated (said by `rotated_copy`)
        }
        assert_eq!(Media::probe(&src).unwrap().rotation % 180, 90);
        let media = Media::probe(&src).unwrap();
        assert_eq!(media.video, Some((360, 640)), "display size");
        let src_spec = playback::Source::whole(
            src.clone(),
            media.duration,
            media.decode_size().map(|size| (size, media.fps)),
            false,
        );
        assert_eq!(media.decode_size(), Some((360, 640)));
        let mut player =
            Player::start(&tools, &src_spec, 0.0, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
        let begin = Instant::now();
        let frame = loop {
            if let Some(f) = player.pump(Instant::now()).frame {
                break f;
            }
            assert!(begin.elapsed().as_secs() < 10, "no frame");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(frame.rgba.len(), 360 * 640 * 4, "upright frame");
        let abs = src.to_str().unwrap().to_string();
        let mut fs = LocalFs::new();
        let at = fs.stat(&abs).unwrap();
        let mut origin = Origin::new(Box::new(fs), None, None, abs, at);
        let marks = Marks {
            t: 0.0,
            in_s: Some(0.5),
            out_s: Some(1.5),
        };
        let timeline = Timeline::new(media.rate(), media.ticks());
        let job = ExportWork::new(&media, &timeline, &marks, Some([10, 20, 300, 600]));
        let out = job
            .run(&mut origin, &media, &tmp.path().join("work"), &mut |_| {})
            .unwrap()
            .0;
        let probe = ffmpeg::probe(&tools, Path::new(&out)).unwrap();
        assert_eq!(probe.display, Some((300, 600)));
    }

    /// The window's export path (crop + cut) end to end on a local file: odd crop rounded to
    /// even, duration = Out − In (frame-exact), audio kept, original untouched, the result a new
    /// sibling.
    #[test]
    fn export_crops_and_cuts_into_a_sibling() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("clip.mp4");
        let ok = std::process::Command::new(&tools.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=30:duration=4",
            ])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=4"])
            .args([
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&src)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let before = std::fs::read(&src).unwrap();
        let media = Media::probe(&src).unwrap();
        assert_eq!(media.video, Some((640, 360)));
        let abs = src.to_str().unwrap().to_string();
        let mut fs = LocalFs::new();
        let at = fs.stat(&abs).unwrap();
        let mut origin = Origin::new(Box::new(fs), None, None, abs, at);
        let marks = Marks {
            t: 1.0,
            in_s: Some(0.5),
            out_s: Some(2.25),
        };
        let timeline = Timeline::new(media.rate(), media.ticks());
        let job = ExportWork::new(&media, &timeline, &marks, Some([101, 51, 401, 201]));
        let mut last = 0.0;
        let path = job
            .run(&mut origin, &media, &tmp.path().join("work"), &mut |f| {
                last = f
            })
            .unwrap()
            .0;
        // Cuts are at frame times: Out 2.25 s at 30 fps is the start of frame 68 (2.267 s) —
        // the frames before 2.25 s are all in, as `-to 2.25` had them.
        assert!(path.ends_with("/clip.cut-0.5-2.267.mp4"), "{path}");
        assert_eq!(last, 1.0);
        let probe = ffmpeg::probe(&tools, Path::new(&path)).unwrap();
        assert_eq!(probe.video, Some((400, 200)));
        assert!(probe.has_audio);
        let d = probe.duration.unwrap();
        assert!((d - 1.75).abs() <= 0.1, "duration {d}");
        // Frames 15..68, exactly.
        let frames = crate::cut::edit_tests::count_frames(&tools, Path::new(&path));
        assert_eq!(frames, 53, "frames");
        assert_eq!(std::fs::read(&src).unwrap(), before);
        // In after Out is refused before anything runs.
        let bad = Marks {
            t: 0.0,
            in_s: Some(3.0),
            out_s: Some(1.0),
        };
        let err = ExportWork::new(&media, &timeline, &bad, None)
            .run(&mut origin, &media, &tmp.path().join("work"), &mut |_| {})
            .unwrap_err();
        assert!(err.to_string().contains("must be after"), "{err}");
    }
}
