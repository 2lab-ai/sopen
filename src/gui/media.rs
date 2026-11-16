//! The media window (spec §7): video plays in the window with sound, audio shows a waveform;
//! both have a scrubbable timeline, Space play/pause, `I`/`O` In/Out and Export (`E`), which
//! writes `<stem>.cut-<in>-<out>.<ext>` next to the original — never over anything. On video a
//! drag draws the crop rectangle in source pixels (rounded to even on export).
//!
//! Export is [`crate::cut::cut_and_publish`] unchanged (the same code as `sopen cut`), run on a
//! worker thread by the window and directly by the `SOPEN_AUTOEXPORT` hook.

use super::playback::{self, Frame, Player, Sink, Source};
use super::{Hooks, Map, Origin, Outcome, Selection, Status};
use crate::cut::{self, CutRequest};
use crate::ffmpeg::{self, Rect as CropRect, Tools};
use anyhow::{bail, Context as _, Result};
use eframe::egui::{self, Color32, Key, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::SeqCst};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

/// Min/max per waveform column, filled in by a decoder thread.
type Wave = Arc<Mutex<Option<Vec<[f32; 2]>>>>;

/// Waveform columns.
const WAVE_BINS: usize = 2000;
/// Waveform decode rate (mono).
const WAVE_RATE: f64 = 8000.0;

/// The probed file.
#[derive(Debug, Clone)]
pub struct Media {
    pub tools: Tools,
    pub local: PathBuf,
    pub duration: f64,
    /// Frame size as displayed (rotation applied), when there is a video stream. ffmpeg
    /// rotates frames before filters run, so the preview, the crop rectangle and the export's
    /// crop are all in these pixels.
    pub video: Option<(u32, u32)>,
    /// Display rotation in degrees (0, 90, 180, 270).
    pub rotation: u32,
    pub fps: f64,
    pub has_audio: bool,
    /// What an Export will not keep (or keeps differently), shown before Export.
    pub notes: Vec<String>,
}

impl Media {
    pub fn probe(local: &Path) -> Result<Self> {
        let tools = ffmpeg::resolve()?;
        let probe = ffmpeg::probe(&tools, local)?;
        let duration = probe
            .duration
            .filter(|d| *d > 0.0)
            .with_context(|| format!("ffprobe reports no duration for {}", local.display()))?;
        if probe.video.is_none() && !probe.has_audio {
            bail!("{} has no audio or video stream", local.display());
        }
        let name = local
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let notes = cut::plan(&tools, &probe, &name)
            .map(|(_, _, plan)| plan.notes)
            .unwrap_or_default();
        Ok(Self {
            tools,
            local: local.to_path_buf(),
            duration,
            video: probe.display,
            rotation: probe.rotation,
            fps: probe.fps.unwrap_or(30.0),
            has_audio: probe.has_audio,
            notes,
        })
    }

    fn source_size(&self) -> Option<Vec2> {
        self.video.map(|(w, h)| Vec2::new(w as f32, h as f32))
    }

    fn decode_size(&self) -> Option<(u32, u32)> {
        self.video.map(|(w, h)| playback::decode_size(w, h))
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

/// Running min/max per column over a stream of samples whose total is estimated up front.
pub struct Peaks {
    per_bin: usize,
    pub bins: Vec<[f32; 2]>,
    seen: usize,
}

impl Peaks {
    pub fn new(expected_samples: usize, bins: usize) -> Self {
        Self {
            per_bin: expected_samples.div_ceil(bins.max(1)).max(1),
            bins: vec![[0.0, 0.0]; bins.max(1)],
            seen: 0,
        }
    }

    pub fn push(&mut self, v: f32) {
        let k = (self.seen / self.per_bin).min(self.bins.len() - 1);
        let bin = &mut self.bins[k];
        bin[0] = bin[0].min(v);
        bin[1] = bin[1].max(v);
        self.seen += 1;
    }
}

fn start_waveform(media: &Media, ctx: egui::Context) -> Wave {
    let wave: Wave = Arc::default();
    let (out, tools, path, duration) = (
        wave.clone(),
        media.tools.clone(),
        media.local.clone(),
        media.duration,
    );
    std::thread::spawn(move || {
        let child = std::process::Command::new(&tools.ffmpeg)
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&path)
            .args(["-vn", "-ac", "1", "-ar"])
            .arg(format!("{WAVE_RATE}"))
            .args(["-f", "f32le", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else { return };
        let mut reader = BufReader::new(child.stdout.take().expect("piped"));
        let mut peaks = Peaks::new((duration * WAVE_RATE) as usize, WAVE_BINS);
        let mut b4 = [0u8; 4];
        while reader.read_exact(&mut b4).is_ok() {
            peaks.push(f32::from_le_bytes(b4));
        }
        let _ = child.wait();
        *out.lock().unwrap() = Some(peaks.bins);
        ctx.request_repaint();
    });
    wave
}

/// One-frame decodes for a paused playhead; the latest request wins.
struct Scrubber {
    req: Arc<(Mutex<Option<f64>>, Condvar)>,
    result: Arc<Mutex<Option<Result<Frame, String>>>>,
    quit: Arc<AtomicBool>,
}

impl Scrubber {
    fn start(media: &Media, ctx: egui::Context) -> Option<Self> {
        let (w, h) = media.decode_size()?;
        let req: Arc<(Mutex<Option<f64>>, Condvar)> = Arc::default();
        let result: Arc<Mutex<Option<Result<Frame, String>>>> = Arc::default();
        let quit = Arc::new(AtomicBool::new(false));
        let (rq, res, q) = (req.clone(), result.clone(), quit.clone());
        let (tools, path) = (media.tools.clone(), media.local.clone());
        // The last frame starts one frame before the end; asking past it decodes nothing.
        let last = (media.duration - 1.5 / media.fps.max(1.0)).max(0.0);
        std::thread::spawn(move || loop {
            let t = {
                let mut g = rq.0.lock().unwrap();
                while g.is_none() && !q.load(SeqCst) {
                    g = rq.1.wait(g).unwrap();
                }
                if q.load(SeqCst) {
                    return;
                }
                g.take().expect("checked")
            };
            let spawned = std::process::Command::new(&tools.ffmpeg)
                .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-ss"])
                .arg(format!("{:.3}", t.min(last)))
                .arg("-i")
                .arg(&path)
                .args(["-frames:v", "1", "-an", "-sn", "-vf"])
                .arg(format!("scale={w}:{h},format=rgba"))
                .args(["-f", "rawvideo", "-"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            let frame = match spawned {
                Err(e) => Err(format!("cannot run {}: {e}", tools.ffmpeg.display())),
                Ok(mut child) => {
                    let mut rgba = vec![0u8; (w * h * 4) as usize];
                    let read = child.stdout.take().expect("piped").read_exact(&mut rgba);
                    let _ = child.kill();
                    let _ = child.wait();
                    read.map(|()| Frame { pts: t, rgba })
                        .map_err(|_| format!("no frame at {}", clock_text(t)))
                }
            };
            *res.lock().unwrap() = Some(frame);
            ctx.request_repaint();
        });
        Some(Self { req, result, quit })
    }

    fn request(&self, t: f64) {
        *self.req.0.lock().unwrap() = Some(t);
        self.req.1.notify_one();
    }
}

impl Drop for Scrubber {
    fn drop(&mut self) {
        self.quit.store(true, SeqCst);
        self.req.1.notify_one();
    }
}

/// The edit state: playhead, In/Out, crop rectangle.
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
}

/// The export of `[in, out]` (cropped when `crop` is set) as a new sibling of the original.
/// Returns the new file's printed path (`host:/dir/name`) and what it did not keep.
pub fn export(
    origin: &mut Origin,
    media: &Media,
    req: &CutRequest,
    work_root: &Path,
    progress: &mut dyn FnMut(f64),
) -> Result<(String, Vec<String>)> {
    if req.out_s <= req.in_s {
        bail!(
            "Out ({}) must be after In ({})",
            clock_text(req.out_s),
            clock_text(req.in_s)
        );
    }
    let (dir, name) = origin.parent();
    let fs = origin.fs()?;
    let published = cut::cut_and_publish(
        fs,
        &media.tools,
        &media.local,
        &dir,
        &name,
        req,
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

fn request(media: &Media, marks: &Marks, crop: Option<[u32; 4]>) -> CutRequest {
    let (in_s, out_s) = marks.range(media.duration);
    CutRequest {
        in_s,
        out_s,
        crop: crop
            .filter(|_| media.video.is_some())
            .map(|[x, y, w, h]| CropRect { x, y, w, h }),
    }
}

/// Opens the media file: the window, or the windowless `SOPEN_AUTOEXPORT` run. `notice` is
/// shown in the status line on open.
pub fn open(
    mut origin: Origin,
    local: &Path,
    cache: &Path,
    hooks: &Hooks,
    notice: Option<String>,
) -> Result<Outcome> {
    let media = Media::probe(local)?;
    let state = &hooks.state;
    if !state.ops.is_empty() {
        bail!("SOPEN_STATE op applies to images; media takes rect, t, in and out");
    }
    let marks = Marks {
        t: state.t.unwrap_or(0.0).clamp(0.0, media.duration),
        in_s: state.in_s,
        out_s: state.out_s,
    };
    let mut sel = Selection::default();
    if let (Some(r), Some(_)) = (state.rect, media.video) {
        sel.set(r);
    }
    let work_root = cache.join("exports");
    if hooks.autoexport {
        let crop = media.source_size().and_then(|b| sel.pixels(b));
        let req = request(&media, &marks, crop);
        let started = Instant::now();
        for note in &media.notes {
            eprintln!("sopen: export: {note}");
        }
        let (path, notes) = export(&mut origin, &media, &req, &work_root, &mut |_| {})
            .map_err(|e| export_error(&e))?;
        let line = format!(
            "exported {path} ({} → {}{}) in {:.1} s",
            clock_text(req.in_s),
            clock_text(req.out_s),
            req.crop
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
    let title = origin.display();
    let outcome = Arc::new(Mutex::new(Outcome::default()));
    let leftover: Arc<Mutex<Option<ExportJob>>> = Arc::default();
    let size = match media.decode_size() {
        Some((w, h)) => [
            (w as f32).clamp(640.0, 1280.0),
            (h as f32 + 150.0).clamp(420.0, 900.0),
        ],
        None => [1000.0, 420.0],
    };
    let window = MediaWindow {
        media: Arc::new(media),
        origin: Arc::new(Mutex::new(origin)),
        work_root,
        marks,
        sel,
        tex: None,
        tex_pts: None,
        want: None,
        scrub: None,
        wave: None,
        player: None,
        job: None,
        status: notice.map(Status::error).unwrap_or_default(),
        outcome: outcome.clone(),
        started: false,
        painted: false,
        close_when_done: false,
        close_ok: false,
        leftover: leftover.clone(),
        export_hold: hooks.export_hold,
        resume: false,
        sink: Sink::Device,
        map: None,
        timeline_rect: None,
        notices: Vec::new(),
        pending_notices: None,
    };
    let run = super::run_window(&title, size, window, hooks);
    // Never exit with an export in flight without its outcome on the terminal.
    if let Some(job) = leftover.lock().unwrap().take() {
        eprintln!("sopen: the window closed during an export; waiting for it to finish");
        let result = job
            .rx
            .recv()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("the export stopped without a result")));
        let _ = report_export(result, &mut outcome.lock().unwrap());
    }
    run?;
    let outcome = outcome.lock().unwrap().clone();
    Ok(outcome)
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

struct ExportJob {
    /// 0..=1000 (per mille), written by the worker.
    progress: Arc<AtomicU32>,
    rx: Receiver<Result<(String, Vec<String>)>>,
}

struct MediaWindow {
    media: Arc<Media>,
    origin: Arc<Mutex<Origin>>,
    work_root: PathBuf,
    marks: Marks,
    sel: Selection,
    tex: Option<TextureHandle>,
    /// Time of the frame on screen, and of the frame asked for.
    tex_pts: Option<f64>,
    want: Option<f64>,
    scrub: Option<Scrubber>,
    wave: Option<Wave>,
    player: Option<Player>,
    job: Option<ExportJob>,
    status: Status,
    outcome: Arc<Mutex<Outcome>>,
    started: bool,
    painted: bool,
    /// Close was asked for during an export: close when it is done.
    close_when_done: bool,
    close_ok: bool,
    /// Where an export still in flight goes if the window is dropped; `open` waits for it.
    leftover: Arc<Mutex<Option<ExportJob>>>,
    /// Receipt hook: start an Export on open and hold it at 30 % (AGENTS.md "Receipts").
    export_hold: Option<std::time::Duration>,
    /// Playback paused by a timeline drag, to resume on release.
    resume: bool,
    /// Where playback sound goes: the device (tests: the null sink).
    sink: Sink,
    /// Where the last pass drew the frame (screen ↔ source pixels) and the timeline.
    map: Option<Map>,
    timeline_rect: Option<Rect>,
    /// What the last export reported that must be read whole, and the next such report, held
    /// while a crop is being drawn.
    notices: Vec<String>,
    pending_notices: Option<Vec<String>>,
}

impl MediaWindow {
    fn audio_only(&self) -> bool {
        self.media.video.is_none()
    }

    fn crop(&self) -> Option<[u32; 4]> {
        self.media.source_size().and_then(|b| self.sel.pixels(b))
    }

    fn show_frame(&mut self, ctx: &egui::Context, frame: Frame) {
        let Some((w, h)) = self.media.decode_size() else {
            return;
        };
        let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &frame.rgba);
        match &mut self.tex {
            Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("frame", image, egui::TextureOptions::LINEAR)),
        }
        self.tex_pts = Some(frame.pts);
    }

    fn seek(&mut self, ctx: &egui::Context, t: f64) {
        let t = t.clamp(0.0, self.media.duration);
        self.marks.t = t;
        if self.player.is_some() {
            self.player = None;
            self.play(ctx);
        } else if let Some(scrub) = &self.scrub {
            self.want = Some(t);
            scrub.request(t);
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
        let media = &self.media;
        let from = if self.marks.t >= media.duration - 0.05 {
            0.0
        } else {
            self.marks.t
        };
        let src = Source {
            path: media.local.clone(),
            duration: media.duration,
            video: media.decode_size().map(|size| (size, media.fps)),
            audio: media.has_audio,
        };
        let waker = ctx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || waker.request_repaint());
        match Player::start(&media.tools, &src, from, 1.0, self.sink, wake) {
            Ok(player) => {
                self.player = Some(player);
                self.status = Status::default();
            }
            Err(e) => self.status = Status::error(format!("cannot play: {e:#}")),
        }
    }

    fn pause(&mut self, ctx: &egui::Context) {
        if let Some(player) = self.player.take() {
            self.marks.t = player.clock(Instant::now()).min(self.media.duration);
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
            self.show_frame(ctx, frame);
        }
        if let Some(note) = note {
            self.status = Status::error(format!("{note} — playing without sound"));
        }
        if pump.ended {
            self.player = None;
            self.marks.t = pump.position;
        } else {
            ctx.request_repaint_after(pump.wake_in);
        }
    }

    fn set_in(&mut self) {
        let t = self.marks.t;
        if self.marks.out_s.is_some_and(|o| o <= t) {
            self.marks.out_s = None;
        }
        self.marks.in_s = Some(t);
    }

    fn set_out(&mut self) {
        let t = self.marks.t;
        if self.marks.in_s.is_some_and(|i| i >= t) {
            self.marks.in_s = None;
        }
        self.marks.out_s = Some(t);
    }

    fn start_export(&mut self, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        let req = request(&self.media, &self.marks, self.crop());
        if req.out_s <= req.in_s {
            self.status = Status::error("Out must be after In");
            return;
        }
        let progress = Arc::new(AtomicU32::new(0));
        let (tx, rx) = mpsc::channel();
        let (media, origin, work) = (
            self.media.clone(),
            self.origin.clone(),
            self.work_root.clone(),
        );
        let (shared, waker) = (progress.clone(), ctx.clone());
        let mut hold = self.export_hold;
        std::thread::spawn(move || {
            let mut origin = origin.lock().unwrap();
            let result = export(&mut origin, &media, &req, &work, &mut |f| {
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

    fn poll_export(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else {
            return;
        };
        let Ok(result) = job.rx.try_recv() else {
            return;
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
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
        use super::plain_key;
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
        if plain_key(ctx, Key::Escape) {
            self.sel.clear();
        }
        let step = |ctx: &egui::Context, key| {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, key)) {
                Some(5.0)
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

    /// The timeline: In–Out span, waveform (when built), markers, playhead. Returns a seek and
    /// whether the gesture ended (click, or drag released).
    fn timeline(&mut self, ui: &mut egui::Ui, height: f32) -> Option<(f64, bool)> {
        let (rect, resp) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), height),
            Sense::click_and_drag(),
        );
        self.timeline_rect = Some(rect);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, Color32::from_gray(40));
        let duration = self.media.duration.max(1e-6);
        let x_of = |t: f64| rect.left() + (t / duration) as f32 * rect.width();
        let (in_s, out_s) = self.marks.range(duration);
        if self.marks.in_s.is_some() || self.marks.out_s.is_some() {
            let span = Rect::from_x_y_ranges(x_of(in_s)..=x_of(out_s), rect.y_range());
            painter.rect_filled(span, 0.0, Color32::from_rgba_unmultiplied(80, 160, 255, 45));
        }
        if let Some(wave) = self.wave.as_ref().and_then(|w| w.lock().unwrap().clone()) {
            let mid = rect.center().y;
            let amp = rect.height() * 0.45;
            let n = wave.len() as f32;
            for (k, [lo, hi]) in wave.iter().enumerate() {
                let x = rect.left() + (k as f32 + 0.5) / n * rect.width();
                painter.line_segment(
                    [Pos2::new(x, mid - hi * amp), Pos2::new(x, mid - lo * amp)],
                    Stroke::new(1.0, Color32::from_rgb(90, 200, 140)),
                );
            }
        }
        let font = egui::FontId::monospace(12.0);
        let mark = |t: f64, color: Color32, label: String| {
            let x = x_of(t);
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(2.0, color),
            );
            // Labels near the right end go on the marker's left so they stay inside.
            let right = x + 3.0 + 9.0 * label.chars().count() as f32 > rect.right();
            let (pos, align) = if right {
                (
                    Pos2::new(x - 3.0, rect.top() + 2.0),
                    egui::Align2::RIGHT_TOP,
                )
            } else {
                (Pos2::new(x + 3.0, rect.top() + 2.0), egui::Align2::LEFT_TOP)
            };
            painter.text(pos, align, label, font.clone(), color);
        };
        if let Some(i) = self.marks.in_s {
            mark(
                i,
                Color32::from_rgb(70, 220, 90),
                format!("In {}", clock_text(i)),
            );
        }
        if let Some(o) = self.marks.out_s {
            mark(
                o,
                Color32::from_rgb(240, 90, 80),
                format!("Out {}", clock_text(o)),
            );
        }
        let x = x_of(self.marks.t);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(2.0, Color32::WHITE),
        );
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let at =
            |p: Pos2| f64::from(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0)) * duration;
        if resp.drag_stopped() {
            let t = resp.interact_pointer_pos().map_or(self.marks.t, at);
            return Some((t, true));
        }
        if resp.dragged() || resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                return Some((at(p), resp.clicked()));
            }
        }
        None
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
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
            if ui.button("In  I").clicked() {
                self.set_in();
            }
            if ui.button("Out  O").clicked() {
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
            let busy = self.job.is_some();
            if ui
                .add_enabled(!busy, egui::Button::new("Export  E"))
                .on_hover_text(
                    "re-encode In–Out (and the crop) into a new file next to the original",
                )
                .clicked()
            {
                self.start_export(&ctx);
            }
            ui.separator();
            let (in_s, out_s) = self.marks.range(self.media.duration);
            ui.monospace(format!(
                "{} / {}  ·  In {}  Out {}  ({:.2} s)",
                clock_text(self.marks.t),
                clock_text(self.media.duration),
                clock_text(in_s),
                clock_text(out_s),
                (out_s - in_s).max(0.0)
            ));
        });
        // Its own full-width row: a portrait (rotated) clip makes a narrow window.
        if let Some(job) = &self.job {
            let f = job.progress.load(SeqCst) as f32 / 1000.0;
            ui.add(
                egui::ProgressBar::new(f)
                    .text(format!("exporting {:.0} %", f * 100.0))
                    .desired_width(ui.available_width()),
            );
        }
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
                None => ui.monospace(format!("audio · {}", clock_text(m.duration))),
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
            ui.separator();
            self.status.show(ui);
        });
    }

    fn picture(&mut self, ui: &mut egui::Ui) {
        let Some(src) = self.media.source_size() else {
            return;
        };
        let panel = ui.max_rect();
        let painter = ui.painter_at(panel);
        painter.rect_filled(panel, 0.0, Color32::from_gray(22));
        let Some(tex) = &self.tex else {
            painter.text(
                panel.center(),
                egui::Align2::CENTER_CENTER,
                "decoding…",
                egui::FontId::proportional(16.0),
                Color32::GRAY,
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
        self.sel.handle(ui, &resp, map);
        self.sel.draw(&painter, map);
        self.map = Some(map);
        self.painted = true;
    }
}

impl Drop for MediaWindow {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            *self.leftover.lock().unwrap() = Some(job);
        }
    }
}

impl super::Window for MediaWindow {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !self.started {
            self.started = true;
            self.scrub = Scrubber::start(&self.media, ctx.clone());
            if self.audio_only() {
                self.wave = Some(start_waveform(&self.media, ctx.clone()));
            }
            self.seek(&ctx, self.marks.t);
            if self.export_hold.is_some() {
                self.start_export(&ctx);
            }
        }
        self.poll_export(&ctx);
        self.keys(&ctx);
        self.pump(&ctx);
        if self.player.is_none() {
            let scrubbed = self
                .scrub
                .as_ref()
                .and_then(|s| s.result.lock().unwrap().take());
            match scrubbed {
                Some(Ok(frame)) => self.show_frame(&ctx, frame),
                Some(Err(e)) => self.status = Status::error(e),
                None => {}
            }
        }
        if !self.sel.dragging() {
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
            .then(|| (format!("Export: {}", self.media.notes.join("; ")), warn))
            .into_iter()
            .chain(self.notices.iter().map(|n| (n.clone(), error)))
            .collect();
        if !lines.is_empty() {
            egui::Panel::top("disclosures").show(ui, |ui| {
                super::disclosures(ui, &lines);
                ui.add_space(2.0);
            });
        }
        let mut seek = None;
        let audio_only = self.audio_only();
        egui::Panel::bottom("transport").show(ui, |ui| {
            ui.add_space(4.0);
            if !audio_only {
                seek = self.timeline(ui, 34.0);
                ui.add_space(4.0);
            }
            self.transport(ui);
            ui.add_space(4.0);
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                if audio_only {
                    ui.painter()
                        .rect_filled(ui.max_rect(), 0.0, Color32::from_gray(22));
                    ui.add_space(12.0);
                    let h = (ui.available_height() - 12.0).max(60.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.set_width(ui.available_width() - 8.0);
                            seek = self.timeline(ui, h).or(seek);
                        });
                    });
                    self.painted = self
                        .wave
                        .as_ref()
                        .is_some_and(|w| w.lock().unwrap().is_some());
                } else {
                    self.picture(ui);
                }
            });
        if let Some((t, done)) = seek {
            self.scrub_to(&ctx, t, done);
        }
    }

    fn ready(&self) -> bool {
        let frame_ok = self.audio_only() || (self.want.is_some() && self.tex_pts == self.want);
        // With the export hook, the shot is taken while the export is held mid-way.
        let export_ok = self.export_hold.is_none()
            || self
                .job
                .as_ref()
                .is_some_and(|j| j.progress.load(SeqCst) >= 300);
        self.painted && frame_ok && self.player.is_none() && export_ok
    }

    fn close_requested(&mut self) -> bool {
        if self.close_ok || self.job.is_none() {
            return true;
        }
        if self.close_when_done {
            // Second request: the window goes now; `open` waits for the export and prints
            // its outcome in the terminal.
            return true;
        }
        self.close_when_done = true;
        self.status = Status::info(
            "exporting… the window closes when the export is done (close again to close it \
             now — sopen still waits for the export and prints the result in the terminal)",
        );
        false
    }
}

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
        let media = Media {
            tools: Tools {
                ffmpeg: "ffmpeg".into(),
                ffprobe: "ffprobe".into(),
            },
            local: path,
            duration: 4.0,
            video: Some((64, 36)),
            rotation: 0,
            fps: 25.0,
            has_audio: false,
            notes: Vec::new(),
        };
        let leftover: Arc<Mutex<Option<ExportJob>>> = Arc::default();
        let (tx, rx) = mpsc::channel();
        let mut w = MediaWindow {
            media: Arc::new(media),
            origin: Arc::new(Mutex::new(origin)),
            work_root: tmp.path().join("work"),
            marks: Marks::default(),
            sel: Selection::default(),
            tex: None,
            tex_pts: None,
            want: None,
            scrub: None,
            wave: None,
            player: None,
            job: Some(ExportJob {
                progress: Arc::default(),
                rx,
            }),
            status: Status::default(),
            outcome: Arc::default(),
            started: false,
            painted: false,
            close_when_done: false,
            close_ok: false,
            leftover: leftover.clone(),
            export_hold: None,
            resume: false,
            sink: Sink::Null,
            map: None,
            timeline_rect: None,
            notices: Vec::new(),
            pending_notices: None,
        };
        use super::super::Window;
        assert!(!w.close_requested(), "first close waits for the export");
        assert!(w.status.text.contains("closes when the export is done"));
        assert!(w.close_requested(), "second close closes the window");
        drop(w);
        let job = leftover.lock().unwrap().take().expect("export handed over");
        tx.send(Ok(("box:/v.cut-1-2.mp4".to_string(), Vec::new())))
            .unwrap();
        let mut outcome = Outcome::default();
        let (status, _) = report_export(job.rx.recv().unwrap(), &mut outcome);
        assert!(outcome.exported);
        assert!(status.text.contains("exported box:/v.cut-1-2.mp4"));
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
    fn peaks_bin_min_and_max() {
        let mut p = Peaks::new(10, 5);
        for v in [
            0.1, -0.2, 0.5, 0.3, -0.9, 0.0, 0.2, 0.2, 0.4, -0.1, 0.7, 0.8,
        ] {
            p.push(v);
        }
        assert_eq!(p.bins[0], [-0.2, 0.1]);
        assert_eq!(p.bins[2], [-0.9, 0.0]);
        // Samples past the estimate land in the last column.
        assert_eq!(p.bins[4], [-0.1, 0.8]);
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
        let tagged = [
            (vec!["-display_rotation", "90"], vec![]),
            (vec![], vec!["-metadata:s:v:0", "rotate=90"]),
        ]
        .into_iter()
        .any(|(input, output)| {
            std::process::Command::new(&tools.ffmpeg)
                .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
                .args(input)
                .arg("-i")
                .arg(&plain)
                .args(["-c", "copy"])
                .args(output)
                .arg(&src)
                .status()
                .is_ok_and(|s| s.success())
                && Media::probe(&src).is_ok_and(|m| m.rotation % 180 == 90)
        });
        if !tagged {
            eprintln!("skipping: this ffmpeg cannot tag a fixture as rotated");
            return;
        }
        let media = Media::probe(&src).unwrap();
        assert_eq!(media.video, Some((360, 640)), "display size");
        let src_spec = playback::Source {
            path: src.clone(),
            duration: media.duration,
            video: media.decode_size().map(|size| (size, media.fps)),
            audio: false,
        };
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
        let req = request(&media, &marks, Some([10, 20, 300, 600]));
        let out = export(
            &mut origin,
            &media,
            &req,
            &tmp.path().join("work"),
            &mut |_| {},
        )
        .unwrap()
        .0;
        let probe = ffmpeg::probe(&tools, Path::new(&out)).unwrap();
        assert_eq!(probe.display, Some((300, 600)));
    }

    /// The window's export path (crop + cut) end to end on a local file: odd crop rounded to
    /// even, duration = Out − In, audio kept, original untouched, the result a new sibling.
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
        let req = request(&media, &marks, Some([101, 51, 401, 201]));
        let mut last = 0.0;
        let path = export(
            &mut origin,
            &media,
            &req,
            &tmp.path().join("work"),
            &mut |f| last = f,
        )
        .unwrap()
        .0;
        assert!(path.ends_with("/clip.cut-0.5-2.25.mp4"), "{path}");
        assert_eq!(last, 1.0);
        let probe = ffmpeg::probe(&tools, Path::new(&path)).unwrap();
        assert_eq!(probe.video, Some((400, 200)));
        assert!(probe.has_audio);
        let d = probe.duration.unwrap();
        assert!((d - 1.75).abs() <= 0.1, "duration {d}");
        assert_eq!(std::fs::read(&src).unwrap(), before);
        // In after Out is refused before anything runs.
        let bad = Marks {
            t: 0.0,
            in_s: Some(3.0),
            out_s: Some(1.0),
        };
        let err = export(
            &mut origin,
            &media,
            &request(&media, &bad, None),
            &tmp.path().join("work"),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("must be after"), "{err}");
    }
}
