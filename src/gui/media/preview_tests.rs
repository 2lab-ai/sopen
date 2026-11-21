//! The media window as a preview (02 §4.2–§4.4, slice S1-M): what its preparation makes — the
//! probe, one frame, the waveform of the first minutes — within each step's time, with every
//! ffprobe and ffmpeg it starts killed and reaped on every way out; the previewed window, which
//! shows that and nothing else — no bars, no input, no sound, no process of its own; and the
//! hand-over, which decodes nothing again.
//!
//! - **Time** is a clock the test moves ([`Hand`]): a step is over its time when the test says
//!   so, never because a test machine was slow. The two tests of the real clock say so in their
//!   names.
//! - **Programs that stall** are stand-ins held at a named pipe ([`Hold`]): one process each,
//!   let go (or not) by the test — never a `sleep`.
//! - **Processes** are counted as the system lists them: the children the code started, by
//!   their ids, and anything that names the test's folder ([`none_left`]).
//! - **Sound** goes to a recording output ([`Recorder`]): what a test hears of a preview is
//!   that no output was opened and nothing was written to one.
//! - Anything that could block runs under [`bounded`].
//!
//! Real files need ffmpeg (skipped without it, unless `SOPEN_REQUIRE_FFMPEG` is set).

use super::fixtures::{
    bounded, clip, deep_clip, index_only, make, median, naming, none_left, quiet_then_loud,
    six_channels, stand_in, started_under, still_processes, still_running, supervised, timed, tone,
    tools as tools_or_skip, until, wide_pixels, Dir, Hand, Hold, Test, NEVER_ENDS,
};
use super::preview::{self, Children, Found, Step, Watch};
use super::*;
use crate::ffmpeg::spawns;
use crate::gui::playback::Recorder;
use crate::gui::testkit::{command, Driver};
use crate::gui::Window as _;
use crate::kind::Kind;
use crate::remote::{LocalFs, RemoteFs};
use eframe::egui::{Key, Modifiers, PointerButton};

/// A frame's time, generously: nothing a pass does may take longer.
const FRAME: Duration = Duration::from_millis(250);

/// What a test's preparation is held to: each step's time on the test's clock (or the real
/// one), how much sound, and its cancel. It notes when each step began (the real time).
struct Held {
    cancel: AtomicBool,
    /// ffprobe's, the frame's, the waveform's, the encoder question's.
    times: [Duration; 4],
    sound: Option<f64>,
    step: Mutex<Option<(Step, Instant)>>,
    /// `None`: the real clock.
    hand: Option<Hand>,
    began: Mutex<Vec<(Step, Instant)>>,
}

impl Held {
    /// 02 §4.3's: 5 s for ffprobe, for the frame and for the encoder question, the first 10
    /// minutes of sound in 10 s — on `hand`.
    fn budget(hand: &Hand) -> Self {
        Self {
            hand: Some(hand.clone()),
            ..Self::real([5, 5, 10, 5].map(Duration::from_secs))
        }
    }

    /// `times` on the real clock.
    fn real(times: [Duration; 4]) -> Self {
        Self {
            cancel: AtomicBool::new(false),
            times,
            sound: Some(600.0),
            step: Mutex::new(None),
            hand: None,
            began: Mutex::default(),
        }
    }

    fn now(&self) -> Instant {
        self.hand.as_ref().map_or_else(Instant::now, Hand::now)
    }

    /// How long `step` took on the real clock: from its beginning to the next step's (or now).
    fn took(&self, step: Step) -> Duration {
        let began = self.began.lock().unwrap();
        let at = began.iter().position(|(s, _)| *s == step).expect("begun");
        let end = began.get(at + 1).map_or_else(Instant::now, |(_, t)| *t);
        end - began[at].1
    }
}

impl Watch for Held {
    fn begin(&self, step: Step) {
        *self.step.lock().unwrap() = Some((step, self.now()));
        self.began.lock().unwrap().push((step, Instant::now()));
    }

    fn check(&self) -> Result<()> {
        if self.cancel.load(SeqCst) {
            bail!(crate::remote::Cancelled);
        }
        if let Some((step, since)) = *self.step.lock().unwrap() {
            let time = self.times[step as usize];
            if self.now().saturating_duration_since(since) >= time {
                bail!("{step:?} took over {} ms", time.as_millis());
            }
        }
        Ok(())
    }

    fn sound(&self) -> Option<f64> {
        self.sound
    }
}

fn kind_of(path: &Path) -> Kind {
    crate::kind::by_extension(&path.to_string_lossy()).unwrap_or(Kind::Video)
}

fn prepare_in(tools: &Tools, path: &Path, held: &Held, children: &Children) -> Result<Found> {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    preview::prepare(
        tools,
        &Input::path(path),
        &name,
        kind_of(path),
        children,
        held,
    )
}

fn prepare(tools: &Tools, path: &Path, held: &Held) -> Result<Found> {
    prepare_in(tools, path, held, &Children::default())
}

fn made(tools: &Tools, path: &Path, held: &Held) -> preview::Prepared {
    match prepare(tools, path, held) {
        Ok(Found::Media(made)) => *made,
        Ok(Found::NotMedia) => panic!("{} is not media", path.display()),
        Err(e) => panic!("{}: {e:#}", path.display()),
    }
}

/// A media window on a local file, and what its sound reached.
struct Fx {
    tmp: Dir,
    path: PathBuf,
    win: MediaWindow,
    drv: Driver,
    heard: &'static Recorder,
}

impl Fx {
    fn origin(path: &Path) -> Origin {
        let abs = path.to_str().unwrap().to_string();
        let mut fs = LocalFs::new();
        let at = fs.stat(&abs).unwrap();
        Origin::new(Box::new(fs), None, None, abs, at)
    }

    /// `win` in a side of the browser's size, its sound recorded (never the device).
    fn around(tmp: Dir, path: PathBuf, mut win: MediaWindow) -> Self {
        let heard = Recorder::new();
        win.sink = Sink::Recording(heard);
        Self {
            tmp,
            path,
            win,
            drv: Driver::new(500.0, 420.0),
            heard,
        }
    }

    /// What reached an audio output: outputs opened, buffers written.
    fn heard(&self) -> (u64, u64) {
        (
            self.heard.opens.load(SeqCst),
            self.heard.writes.load(SeqCst),
        )
    }

    /// One pass of the window, timed.
    fn pass(&mut self) -> Duration {
        timed(|| self.drv.idle(&mut self.win, 1)).1
    }

    /// The waveform as painted: each column's x and height, left to right.
    fn wave(&self) -> Vec<(f32, f32)> {
        let colour = super::super::chrome::PALETTE.wave;
        let mut lines: Vec<(f32, f32)> = self
            .drv
            .segments()
            .into_iter()
            .filter(|([a, b], c)| *c == colour && a.x == b.x)
            .map(|([a, b], _)| (a.x, (a.y - b.y).abs()))
            .collect();
        lines.sort_by(|a, b| a.0.total_cmp(&b.0));
        lines
    }
}

/// The preview of `path`, made by `made`, in its window.
fn shown(tmp: Dir, path: PathBuf, made: preview::Prepared) -> Fx {
    let cache = tmp.path().join("cache");
    let mut win = MediaWindow::previewing(Fx::origin(&path), made, &cache);
    win.set_preview(true);
    let mut fx = Fx::around(tmp, path, win);
    fx.drv.until(&mut fx.win, 10, |w| w.ready());
    fx.drv.idle(&mut fx.win, 2);
    fx
}

fn previewed(tmp: Dir, path: PathBuf, tools: &Tools, held: &Held) -> Fx {
    let made = made(tools, &path, held);
    shown(tmp, path, made)
}

fn video(t: &Test) -> Option<(Fx, Tools)> {
    let tools = tools_or_skip()?;
    let tmp = t.tmp();
    let path = tmp.path().join("clip.mp4");
    clip(&tools, &path, (320, 240));
    let held = Held::budget(&t.hand());
    Some((previewed(tmp, path, &tools, &held), tools))
}

/// A `secs`-second tone, its preview reading at most `sound` seconds of it.
fn sound(t: &Test, secs: u32, sound: Option<f64>) -> Option<(Fx, Tools)> {
    let tools = tools_or_skip()?;
    let tmp = t.tmp();
    let path = tmp.path().join("tone.wav");
    tone(&tools, &path, secs);
    let held = Held {
        sound,
        ..Held::budget(&t.hand())
    };
    Some((previewed(tmp, path, &tools, &held), tools))
}

/// What input could change in the media window, and what it runs.
fn state(win: &MediaWindow) -> String {
    format!(
        "t={} in={:?} out={:?} crop={:?} rev={} identity={} stereo={} playing={} job={} \
         save={} prompt={} status={:?} scrub={} decoding={}",
        win.marks.t,
        win.marks.in_s,
        win.marks.out_s,
        win.crop(),
        win.timeline.rev(),
        win.timeline.is_identity(),
        win.timeline.stereo(),
        win.player.is_some(),
        win.job.is_some(),
        win.save.is_some(),
        win.prompt,
        win.status.text,
        win.scrub.is_some(),
        win.wave.is_some(),
    )
}

/// Every key, click, drag and wheel a media window acts on, at the places its picture,
/// timeline and buttons are in a window of this size.
fn everything(fx: &mut Fx) {
    let (a, b) = (Pos2::new(120.0, 120.0), Pos2::new(380.0, 300.0));
    for button in [
        PointerButton::Primary,
        PointerButton::Secondary,
        PointerButton::Middle,
    ] {
        fx.drv.drag(&mut fx.win, button, a, b);
    }
    // Where the transport row and the timeline are in the viewer, and across the whole side.
    for y in [20.0, 60.0, 200.0, 330.0, 350.0, 372.0, 390.0, 408.0] {
        for x in [20.0, 45.0, 110.0, 180.0, 250.0, 330.0, 420.0, 480.0] {
            fx.drv.click(&mut fx.win, Pos2::new(x, y));
        }
    }
    fx.drv.wheel(&mut fx.win, Pos2::new(250.0, 200.0), 3.0);
    for (key, mods) in [
        (Key::Space, Modifiers::NONE),
        (Key::C, Modifiers::NONE),
        (Key::P, Modifiers::NONE),
        (Key::I, Modifiers::NONE),
        (Key::O, Modifiers::NONE),
        (Key::E, Modifiers::NONE),
        (Key::M, Modifiers::NONE),
        (Key::Delete, Modifiers::NONE),
        (Key::Backspace, Modifiers::NONE),
        (Key::ArrowRight, Modifiers::NONE),
        (Key::ArrowLeft, Modifiers::SHIFT),
        (Key::F5, Modifiers::NONE),
        (Key::Escape, Modifiers::NONE),
        (Key::Z, command(false)),
        (Key::Z, command(true)),
        (Key::S, command(false)),
    ] {
        fx.drv.key(&mut fx.win, key, mods);
    }
    fx.drv.copy(&mut fx.win);
    fx.drv.cut(&mut fx.win);
    fx.drv.paste_text(&mut fx.win, "typed over it");
    fx.drv.frame(
        &mut fx.win,
        vec![egui::Event::Text("typed".into())],
        Modifiers::NONE,
    );
    fx.drv.idle(&mut fx.win, 3);
}

fn assert_none_left(dir: &Path) {
    if let Err(left) = none_left(dir) {
        panic!("{left}");
    }
}

// ---------------------------------------------------------------------------------------------
// What a preview shows.

/// P1 (video): one frame — the frame at 0 s — fitted in a single-lined frame, with the video's
/// size and duration; none of the media window's bars.
#[test]
fn a_previewed_video_is_its_first_frame_with_its_size_and_duration() {
    bounded(120, |t| {
        let Some((fx, _tools)) = video(t) else { return };
        let pictures = fx.drv.pictures();
        assert_eq!(pictures.len(), 1, "the frame: {pictures:?}");
        assert!(
            pictures[0].width() > 320.0,
            "fitted to the side: {:?}",
            pictures[0]
        );
        let said = fx.drv.text_starting_with("320×240").expect("its size");
        assert!(said.contains("0:02.00"), "its duration: {said}");
        for bar in ["Play", "In  I", "Export  E", "Save", "Undo"] {
            assert!(fx.drv.find_text(bar).is_none(), "{bar} in a preview");
        }
        assert!(fx.drv.find_text("Exit   ").is_none(), "no F-key row");
        assert_eq!(
            fx.drv.colors_of(VIDEO_TITLE),
            Some(vec![super::super::chrome::PALETTE.frame_idle.color]),
            "a single-lined frame: the side does not have the keyboard"
        );
        assert_eq!(fx.win.tex_pts(), Some(0.0), "the frame at 0 s");
    });
}

/// P1 (audio): the waveform — painted, column by column across the side — with the whole
/// duration; no bars.
#[test]
fn a_previewed_sound_is_its_waveform_with_its_duration() {
    bounded(120, |t| {
        let Some((fx, _tools)) = sound(t, 3, Some(600.0)) else {
            return;
        };
        let said = fx.drv.text_starting_with("audio · mono").expect("the line");
        assert!(said.contains("0:03.00"), "the whole duration: {said}");
        assert!(!said.contains("preview of the first"), "all of it: {said}");
        let wave = fx.wave();
        assert!(wave.len() > 400, "a column a point: {}", wave.len());
        let (left, right) = (wave[0].0, wave[wave.len() - 1].0);
        assert!(
            left < 30.0 && right > 470.0,
            "over the side's width: {left}..{right}"
        );
        let tall = wave.iter().filter(|(_, h)| *h > 20.0).count();
        assert!(
            tall * 10 >= wave.len() * 9,
            "the tone, painted tall over the whole width: {tall} of {}",
            wave.len()
        );
        assert!(fx.drv.find_text("reading the waveform…").is_none());
        for bar in ["Play", "In  I", "Export  E", "Save"] {
            assert!(fx.drv.find_text(bar).is_none(), "{bar} in a preview");
        }
        assert_eq!(
            fx.drv.colors_of(AUDIO_TITLE),
            Some(vec![super::super::chrome::PALETTE.frame_idle.color])
        );
    });
}

/// P5: real clicks (where the Play button is, too), drags, the wheel, `C`, `P`, Space,
/// Cmd/Ctrl+V, typing — nothing changes in a previewed video or sound, what it paints stays the
/// same, no audio output is opened and nothing is written to one, no ffmpeg runs for it after
/// its preparation, and dropping it asks nothing.
#[test]
fn a_previewed_medium_takes_no_input_makes_no_sound_and_runs_nothing() {
    for which in ["video", "sound"] {
        bounded(120, move |t| {
            let Some((mut fx, _tools)) = (match which {
                "video" => video(t),
                _ => sound(t, 3, Some(600.0)),
            }) else {
                return;
            };
            let before = (state(&fx.win), fx.drv.pictures(), fx.wave());
            let runs = spawns::of(&fx.path);
            let pids = started_under(fx.tmp.path());
            assert_eq!(
                runs.preview + runs.probe,
                pids.len() as u64,
                "{which}: every run on the file was the preparation's own: {runs:?}"
            );
            assert_none_left(fx.tmp.path());
            everything(&mut fx);
            let after = (state(&fx.win), fx.drv.pictures(), fx.wave());
            assert_eq!(after, before, "{which}: the preview changed");
            assert_eq!(
                fx.heard(),
                (0, 0),
                "{which}: an audio output was opened, or written to"
            );
            assert_eq!(fx.win.plays, 0, "{which}: playback was started");
            assert!(
                fx.drv.take_commands().is_empty(),
                "{which}: nothing went to the clipboard"
            );
            assert_eq!(
                spawns::of(&fx.path),
                runs,
                "{which}: a previewed window runs no ffmpeg"
            );
            assert_eq!(started_under(fx.tmp.path()), pids, "{which}");
            assert!(
                fx.win.close_requested(),
                "{which}: dropping it asks nothing"
            );
            assert!(
                !fx.tmp.path().join("cache").exists(),
                "{which}: a preview leaves sopen's cache alone"
            );
        });
    }
}

// ---------------------------------------------------------------------------------------------
// The hand-over.

/// P6 (video): handed over, the window is the media window — its bars and the double frame back,
/// Space plays (now an output is opened and written to) — and the frame the preview decoded is
/// the one on screen: nothing ran again.
#[test]
fn a_handed_over_video_keeps_its_frame_and_plays() {
    bounded(120, |t| {
        let Some((mut fx, _tools)) = video(t) else {
            return;
        };
        let runs = spawns::of(&fx.path);
        // Only looked at so far: the file is not hashed for a Save.
        assert!(!fx.win.file.lock().unwrap().has_baseline());
        fx.win.set_preview(false);
        fx.drv
            .until(&mut fx.win, 10, |w| w.ready() && w.frame_ready());
        fx.drv.idle(&mut fx.win, 3);
        assert_eq!(
            spawns::of(&fx.path),
            runs,
            "the first frame was not decoded again, the file not probed again"
        );
        // The file as opened is hashed for Save's check at the hand-over; Save and Export are
        // planned as for a file opened from the list (the plan came with the preview).
        assert!(
            fx.win.file.lock().unwrap().has_baseline(),
            "hashed for Save"
        );
        let opened = Media::probe(&fx.path).unwrap();
        assert!(opened.plan.is_some());
        assert_eq!(
            format!("{:?}", fx.win.media.plan),
            format!("{:?}", opened.plan)
        );
        assert_eq!(fx.win.media.notes, opened.notes);
        assert_eq!(fx.win.media.save_block(), opened.save_block());
        for bar in ["Play", "In  I", "Export  E"] {
            assert!(fx.drv.find_text(bar).is_some(), "{bar}: the viewer's bars");
        }
        assert_eq!(
            fx.drv.colors_of(VIDEO_TITLE),
            Some(vec![super::super::chrome::PALETTE.frame_active.color])
        );
        assert_eq!(fx.win.tex_pts(), Some(0.0));
        assert_eq!(fx.heard(), (0, 0), "silent until it is played");
        fx.drv.key(&mut fx.win, Key::Space, Modifiers::NONE);
        assert!(fx.win.player.is_some(), "Space plays again");
        let heard = fx.heard;
        fx.drv
            .until(&mut fx.win, 10, |_| heard.writes.load(SeqCst) > 0);
        assert_eq!(fx.heard().0, 1, "one output, opened by Play");
        fx.drv.key(&mut fx.win, Key::Space, Modifiers::NONE);
        // A drag on the picture draws the crop rectangle again.
        fx.drv.until(&mut fx.win, 10, |w| w.frame_ready());
        let map = fx.win.map.expect("drawn");
        let (a, b) = (
            map.to_disp(Pos2::new(20.0, 30.0)),
            map.to_disp(Pos2::new(200.0, 150.0)),
        );
        fx.drv.drag(&mut fx.win, PointerButton::Primary, a, b);
        assert_eq!(fx.win.crop(), Some([20, 30, 180, 120]));
    });
}

/// P6 (audio): a sound the preview read whole is handed over with its waveform: no decode.
#[test]
fn a_handed_over_short_sound_keeps_its_waveform() {
    bounded(120, |t| {
        let Some((mut fx, _tools)) = sound(t, 3, Some(600.0)) else {
            return;
        };
        let runs = spawns::of(&fx.path);
        assert_eq!(wave::starts(&fx.path), [(0.0, None)], "read whole, once");
        fx.win.set_preview(false);
        fx.drv.until(&mut fx.win, 10, |w| w.ready());
        fx.drv.idle(&mut fx.win, 3);
        assert_eq!(spawns::of(&fx.path), runs, "nothing was decoded again");
        assert_eq!(wave::starts(&fx.path), [(0.0, None)]);
        assert!(fx.drv.find_text("Play").is_some(), "the viewer's bars");
        assert!(fx.drv.find_text("reading the waveform…").is_none());
        let tall = fx.wave().iter().filter(|(_, h)| *h > 20.0).count();
        assert!(tall > 300, "its waveform, painted: {tall} columns");
    });
}

/// P12 (audio): a sound longer than the preview reads shows the waveform of its first stretch
/// — painted over the side's whole width — and says so; handed over, those peaks stay as they
/// are and the rest is decoded from where the preview stopped — one more run, starting there —
/// and the viewer paints the whole sound.
#[test]
fn a_handed_over_long_sound_decodes_on_from_where_the_preview_stopped() {
    bounded(120, |t| {
        let Some(tools) = tools_or_skip() else { return };
        // Quiet for its first 2 s — all the preview reads — and loud from there.
        let tmp = t.tmp();
        let path = tmp.path().join("tone.wav");
        quiet_then_loud(&tools, &path, 6, 2);
        let held = Held {
            sound: Some(2.0),
            ..Held::budget(&t.hand())
        };
        let mut fx = previewed(tmp, path, &tools, &held);
        let said = fx.drv.text_starting_with("audio · mono").expect("the line");
        assert!(
            said.contains("0:06.00") && said.contains("preview of the first 2 s"),
            "{said}"
        );
        assert_eq!(wave::starts(&fx.path), [(0.0, Some(2.0))]);
        // Painted: the first 2 s over the whole width — the quiet part, edge to edge.
        let painted = fx.wave();
        assert!(painted.len() > 400, "a column a point: {}", painted.len());
        let quiet = median(painted.iter().map(|(_, h)| *h).collect());
        let loudest = painted.iter().map(|(_, h)| *h).fold(0.0, f32::max);
        assert!(quiet > 2.0, "the quiet part is painted: {quiet}");
        assert!(loudest < quiet * 3.0, "nothing loud in it: {loudest}");
        let (before, cut) = fx.win.peaks.clone().expect("the preview's peaks");
        assert_eq!(cut, Some(2.0));
        assert!(!before.complete);
        let at = |secs: f64| (secs / before.bin_secs) as usize;
        let empty = |bins: &[[f32; 2]]| bins.iter().all(|b| *b == [0.0, 0.0]);
        assert!(!empty(&before.lanes[0][..at(2.0) - 1]), "the first 2 s");
        assert!(
            empty(&before.lanes[0][at(2.0) + 1..]),
            "nothing past it yet"
        );
        let runs = spawns::of(&fx.path);
        fx.win.set_preview(false);
        fx.drv.until(&mut fx.win, 20, |w| w.ready());
        fx.drv.idle(&mut fx.win, 2);
        assert_eq!(
            wave::starts(&fx.path),
            [(0.0, Some(2.0)), (2.0, None)],
            "decoded on from 2 s: the first 2 s were not read again"
        );
        let now = spawns::of(&fx.path);
        assert_eq!(
            (now.preview, now.probe),
            (runs.preview + 1, runs.probe),
            "one more run"
        );
        let decode = fx.win.wave.as_ref().expect("the waveform");
        let after = decode.peaks.lock().unwrap().clone().expect("peaks");
        assert!(after.complete, "{:?}", after.error);
        assert_eq!(
            after.lanes[0][..at(2.0) - 1],
            before.lanes[0][..at(2.0) - 1],
            "the first 2 s stay as the preview read them"
        );
        // Painted after the hand-over: the whole sound — up to the 2 s mark (a third of the
        // width) the quiet the preview read, past it the loud part decoded since.
        let painted = fx.wave();
        let (left, right) = (painted[0].0, painted[painted.len() - 1].0);
        let mark = left + (right - left) / 3.0;
        let heights = |from: f32, to: f32| -> Vec<f32> {
            let within = painted.iter().filter(|(x, _)| *x >= from && *x <= to);
            within.map(|(_, h)| *h).collect()
        };
        let (kept, decoded_since) = (
            median(heights(left, mark - 8.0)),
            median(heights(mark + 8.0, right)),
        );
        assert!(kept > 1.0, "the first 2 s are painted: {kept}");
        assert!(
            decoded_since > kept * 5.0,
            "past the 2 s mark the loud part is painted: {decoded_since} against {kept}"
        );
        let low = heights(mark + 8.0, right)
            .iter()
            .filter(|h| **h < kept * 5.0)
            .count();
        assert_eq!(low, 0, "every column past the mark shows it");
    });
}

/// The waveform of a sound read in two runs — its first stretch, then from there on — is the
/// waveform of one run over the whole sound, within a column: the second run's samples land
/// where they are in the source. (Loudness that rises and falls, also in a compressed file: a
/// wrong place would show.)
#[test]
fn a_waveform_read_in_two_runs_is_the_waveform_of_one() {
    bounded(120, |t| {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = t.tmp();
        for (name, codec) in [
            ("sweep.wav", &[][..]),
            ("sweep.mp3", &["-c:a", "libmp3lame", "-b:a", "64k"][..]),
        ] {
            let path = tmp.path().join(name);
            let mut args = vec![
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=300:sample_rate=22050:duration=20",
                "-af",
                "volume='0.1+0.9*abs(sin(t/1.7*PI))':eval=frame",
                "-ac",
                "1",
            ];
            args.extend_from_slice(codec);
            make(&tools, &args, &path);
            let input = Input::path(&path);
            let duration = ffmpeg::probe(&tools, &input).unwrap().duration.unwrap();
            let read = |from: f64, len: Option<f64>, onto: Option<(&wave::Source, f64)>| {
                let out = wave::command(&tools, &input, 1, from, len)
                    .stderr(Stdio::null())
                    .output()
                    .unwrap();
                assert!(out.status.success());
                wave::read_onto(&mut &out.stdout[..], 1, duration, onto, &mut |_| {}).unwrap()
            };
            let one = read(0.0, None, None);
            let cut = 7.0;
            let first = read(0.0, Some(cut), None);
            let two = read(cut, None, Some((&first, cut)));
            let shown = Timeline::new(8000.0, (duration * 8000.0) as Tick);
            let (a, b) = (
                wave::columns(&one, &shown, 800, 1).remove(0),
                wave::columns(&two, &shown, 800, 1).remove(0),
            );
            let close =
                |x: [f32; 2], y: [f32; 2]| (x[0] - y[0]).abs() < 0.04 && (x[1] - y[1]).abs() < 0.04;
            let off: Vec<usize> = (1..799)
                .filter(|&k| !(close(b[k], a[k]) || close(b[k], a[k - 1]) || close(b[k], a[k + 1])))
                .collect();
            assert!(off.is_empty(), "{name}: columns {off:?} differ");
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Budgets (02 §4.3), on a clock the test moves.

/// Runs `prepare` on a thread, waits until its `nth` child runs, then gives the test the moment
/// (with a look at whether the preparation has ended): the preparation's result comes back.
fn while_child_runs(
    tools: Tools,
    path: PathBuf,
    held: Arc<Held>,
    children: Arc<Children>,
    nth: usize,
    moment: impl FnOnce(&dyn Fn() -> bool),
) -> Result<Found> {
    let dir = path.parent().unwrap().to_path_buf();
    let worker = std::thread::spawn(move || prepare_in(&tools, &path, &held, &children));
    until("the child starts", || started_under(&dir).len() >= nth);
    assert_eq!(still_running(&started_under(&dir)), 1, "it runs");
    moment(&|| worker.is_finished());
    until("the preparation ends", || worker.is_finished());
    worker.join().unwrap()
}

/// P3: a video whose probe hangs (a stand-in `ffprobe` that never answers) goes on for as long
/// as its time is not over — the real clock is not what decides — and is stopped when it is:
/// the error names the step, and the stand-in is killed and reaped.
#[test]
fn a_stalled_ffprobe_is_stopped_when_its_time_is_over() {
    bounded(60, |t| {
        let tmp = t.tmp();
        let hold = t.hold(tmp.path(), "hold");
        let stalls = stand_in(tmp.path(), "ffprobe", &hold.stalls());
        let tools = Tools {
            ffmpeg: stalls.clone(),
            ffprobe: stalls,
        };
        let path = tmp.path().join("hangs.mp4");
        std::fs::write(&path, b"whatever").unwrap();
        let hand = t.hand();
        let held = Arc::new(Held::budget(&hand));
        let ended = while_child_runs(tools, path, held, Arc::default(), 1, |finished| {
            hand.advance(Duration::from_millis(4900));
            std::thread::sleep(Duration::from_millis(100));
            assert!(!finished(), "4.9 s of its 5 s: it goes on");
            hand.advance(Duration::from_millis(100));
        });
        let error = ended.err().expect("stopped");
        assert_eq!(format!("{error:#}"), "Probe took over 5000 ms");
        assert_eq!(started_under(tmp.path()).len(), 1, "the probe");
        assert_none_left(tmp.path());
    });
}

/// P3: the first frame has its own time, counted from its own start: an ffmpeg that never gives
/// one is stopped when that is over.
#[test]
fn a_stalled_first_frame_is_stopped_when_its_own_time_is_over() {
    let Some(real) = tools_or_skip() else { return };
    bounded(60, move |t| {
        let tmp = t.tmp();
        let path = tmp.path().join("clip.mp4");
        clip(&real, &path, (320, 240));
        let hold = t.hold(tmp.path(), "hold");
        let tools = Tools {
            ffmpeg: stand_in(tmp.path(), "ffmpeg", &hold.stalls()),
            ffprobe: real.ffprobe,
        };
        let hand = t.hand();
        let held = Arc::new(Held::budget(&hand));
        let ended = while_child_runs(tools, path, held, Arc::default(), 2, |finished| {
            hand.advance(Duration::from_millis(4900));
            std::thread::sleep(Duration::from_millis(100));
            assert!(!finished(), "4.9 s of the frame's 5 s: it goes on");
            hand.advance(Duration::from_millis(100));
        });
        let error = ended.err().expect("stopped");
        assert_eq!(format!("{error:#}"), "Frame took over 5000 ms");
        assert_eq!(started_under(tmp.path()).len(), 2, "the probe, the frame");
        assert_none_left(tmp.path());
    });
}

/// P3: the waveform's time is 10 s, its own: a decode that gives sound for ever is stopped
/// when the clock says 10 s — not at ffprobe's 5.
#[test]
fn a_waveform_that_never_ends_is_stopped_when_its_time_is_over() {
    let Some(real) = tools_or_skip() else { return };
    bounded(60, move |t| {
        let tmp = t.tmp();
        let path = tmp.path().join("tone.wav");
        tone(&real, &path, 3);
        let tools = Tools {
            ffmpeg: stand_in(tmp.path(), "ffmpeg", NEVER_ENDS),
            ffprobe: real.ffprobe,
        };
        let hand = t.hand();
        let held = Arc::new(Held::budget(&hand));
        let ended = while_child_runs(tools, path, held, Arc::default(), 2, |finished| {
            hand.advance(Duration::from_millis(9900));
            std::thread::sleep(Duration::from_millis(100));
            assert!(!finished(), "9.9 s of the waveform's 10 s: it goes on");
            hand.advance(Duration::from_millis(100));
        });
        let error = ended.err().expect("stopped");
        assert_eq!(format!("{error:#}"), "Wave took over 10000 ms");
        assert_none_left(tmp.path());
    });
}

/// The real clock: a stalled probe held to 300 ms is stopped about then.
#[test]
fn on_the_real_clock_a_stalled_ffprobe_is_stopped_in_time() {
    bounded(60, |t| {
        let tmp = t.tmp();
        let hold = t.hold(tmp.path(), "hold");
        let stalls = stand_in(tmp.path(), "ffprobe", &hold.stalls());
        let tools = Tools {
            ffmpeg: stalls.clone(),
            ffprobe: stalls,
        };
        let path = tmp.path().join("hangs.mp4");
        std::fs::write(&path, b"whatever").unwrap();
        let held = Held::real([300, 300, 300, 300].map(Duration::from_millis));
        let (ended, took) = timed(|| prepare(&tools, &path, &held));
        assert_eq!(
            format!("{:#}", ended.err().expect("stopped")),
            "Probe took over 300 ms"
        );
        assert!(
            took >= Duration::from_millis(300) && took < Duration::from_secs(5),
            "{took:?}"
        );
        assert_none_left(tmp.path());
    });
}

/// P3, on the real clock: a two-hour sound shows the waveform of its first 10 minutes — said,
/// with the whole duration — its probe within ffprobe's 5 s and its waveform within the
/// waveform's 10 s, each measured by itself; nothing is left running.
#[test]
fn on_the_real_clock_a_two_hour_sound_previews_ten_minutes_within_each_budget() {
    bounded(120, |t| {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = t.tmp();
        let path = tmp.path().join("long.mp3");
        make(
            &tools,
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=8000:duration=7200",
                "-ac",
                "1",
                "-c:a",
                "libmp3lame",
                "-b:a",
                "8k",
            ],
            &path,
        );
        let held = Held::real([5, 5, 10, 5].map(Duration::from_secs));
        let made = made(&tools, &path, &held);
        let (probe, waveform) = (held.took(Step::Probe), held.took(Step::Wave));
        assert!(probe < Duration::from_secs(5), "ffprobe: {probe:?}");
        assert!(waveform < Duration::from_secs(10), "waveform: {waveform:?}");
        let fx = shown(tmp, path, made);
        assert_eq!(wave::starts(&fx.path), [(0.0, Some(600.0))]);
        let said = fx.drv.text_starting_with("audio · mono").expect("the line");
        assert!(
            said.contains("2:00:00.00") && said.contains("preview of the first 10 minutes"),
            "{said}"
        );
        let (peaks, cut) = fx.win.peaks.clone().expect("peaks");
        assert_eq!(cut, Some(600.0));
        let at = |secs: f64| (secs / peaks.bin_secs) as usize;
        assert!(
            peaks.lanes[0][at(1.0)..at(599.0)]
                .iter()
                .all(|[lo, hi]| hi - lo > 0.05),
            "the first 10 minutes"
        );
        assert!(
            peaks.lanes[0][at(601.0)..].iter().all(|b| *b == [0.0, 0.0]),
            "and no further"
        );
        // The preview paints those 10 minutes over its whole width.
        let painted = fx.wave();
        let tall = painted.iter().filter(|(_, h)| *h > 10.0).count();
        assert!(
            tall * 10 >= painted.len() * 9 && painted.len() > 400,
            "{tall} of {}",
            painted.len()
        );
        assert_eq!(started_under(fx.tmp.path()).len(), 2, "probe, waveform");
        assert_none_left(fx.tmp.path());
    });
}

// ---------------------------------------------------------------------------------------------
// Stops: no process outlives its preview.

/// P4: a waveform decode that is cancelled stops: its ffmpeg (here one that would give sound
/// for ever) is killed and reaped, and the preparation ends cancelled.
#[test]
fn a_cancelled_waveform_decode_stops() {
    let Some(real) = tools_or_skip() else { return };
    bounded(60, move |t| {
        let tmp = t.tmp();
        let path = tmp.path().join("tone.wav");
        tone(&real, &path, 3);
        let tools = Tools {
            ffmpeg: stand_in(tmp.path(), "ffmpeg", NEVER_ENDS),
            ffprobe: real.ffprobe,
        };
        let held = Arc::new(Held::budget(&t.hand()));
        let h = held.clone();
        let ended = while_child_runs(tools, path, held, Arc::default(), 2, |_| {
            h.cancel.store(true, SeqCst);
        });
        let error = ended.err().expect("cancelled");
        assert!(crate::remote::is_cancelled(&error), "{error:#}");
        assert_none_left(tmp.path());
    });
}

/// 02 §4.4, the window closing: whoever stops a preparation without waiting for its worker
/// stops its ffprobe at once — it no longer runs when the stop returns — and the stop never
/// waits: not for the process, and not for a worker that holds the slot (it is called on a
/// frame). The worker reaps, and a stop that came first lets nothing start.
#[test]
fn a_stop_from_outside_kills_the_running_child_and_never_waits() {
    bounded(60, |t| {
        let tmp = t.tmp();
        let hold = t.hold(tmp.path(), "hold");
        let stalls = stand_in(tmp.path(), "ffprobe", &hold.stalls());
        let tools = Tools {
            ffmpeg: stalls.clone(),
            ffprobe: stalls,
        };
        let path = tmp.path().join("hangs.mp4");
        std::fs::write(&path, b"whatever").unwrap();
        // No stop of its own: no cancel, a clock that stands still.
        let held = Arc::new(Held::budget(&t.hand()));
        let children = Arc::new(Children::default());
        let (c, dir) = (children.clone(), tmp.path().to_path_buf());
        let ended = while_child_runs(tools.clone(), path.clone(), held, children, 1, |_| {
            let ((), took) = timed(|| c.stop());
            assert!(
                took < Duration::from_millis(50),
                "the stop waited: {took:?}"
            );
            assert_eq!(
                still_running(&started_under(&dir)),
                0,
                "it no longer runs when the stop returns"
            );
        });
        let error = ended.err().expect("stopped");
        assert!(crate::remote::is_cancelled(&error), "{error:#}");
        assert_none_left(tmp.path());

        // A worker that holds the slot (it is starting a child): the stop does not wait for it.
        let children = Arc::new(Children::default());
        let held_slot = children.hold();
        let c = children.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(timed(|| c.stop()).1);
        });
        let took = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the stop waited for the slot's holder");
        assert!(took < Duration::from_millis(50), "{took:?}");
        drop(held_slot);
        // Stopped first: nothing starts at all.
        let held = Held::budget(&t.hand());
        let error = prepare_in(&tools, &path, &held, &children)
            .err()
            .expect("cancelled");
        assert!(crate::remote::is_cancelled(&error), "{error:#}");
        assert_eq!(started_under(tmp.path()).len(), 1, "no second child");
    });
}

/// A run that cannot start the thread that watches its child leaves no child: the one already
/// started is killed and reaped on the way out.
#[test]
fn a_run_that_cannot_watch_its_child_leaves_none() {
    bounded(60, |t| {
        let tmp = t.tmp();
        let hold = t.hold(tmp.path(), "hold");
        let stalls = stand_in(tmp.path(), "ffprobe", &hold.stalls());
        let tools = Tools {
            ffmpeg: stalls.clone(),
            ffprobe: stalls,
        };
        let path = tmp.path().join("hangs.mp4");
        std::fs::write(&path, b"whatever").unwrap();
        let children = Arc::new(Children::default());
        children.fail_watcher.store(true, SeqCst);
        let held = Arc::new(Held::budget(&t.hand()));
        let (c, h, p) = (children.clone(), held.clone(), path.clone());
        // Whether it returns an error or unwinds.
        let ended = std::thread::spawn(move || prepare_in(&tools, &p, &h, &c)).join();
        assert!(!matches!(ended, Ok(Ok(_))), "it cannot have worked");
        assert_eq!(started_under(tmp.path()).len(), 1, "the child was started");
        assert_none_left(tmp.path());
    });
}

/// The media window's own waveform decode stops with its window: dropped, its ffmpeg is killed
/// and reaped (it ran on to the end of the file before) — and the drop does not wait.
#[test]
fn a_dropped_waveform_decode_is_killed() {
    bounded(60, |t| {
        let tmp = t.tmp();
        let fake = stand_in(tmp.path(), "ffmpeg", NEVER_ENDS);
        let tools = Tools {
            ffmpeg: fake.clone(),
            ffprobe: fake,
        };
        let decode = wave::start(&tools, &Input::path(tmp.path()), 1, 3600.0, || {});
        until("the decode starts", || decode.pid().is_some());
        let pid = decode.pid().unwrap();
        assert_eq!(still_running(&[pid]), 1, "it runs");
        let ((), took) = timed(|| drop(decode));
        assert!(
            took < Duration::from_millis(50),
            "the drop waited: {took:?}"
        );
        assert_eq!(still_running(&[pid]), 0, "killed with its window");
        until("it is reaped", || still_processes(&[pid]) == 0);
        assert_none_left(tmp.path());
    });
}

/// An ffmpeg that has closed its output and lives on is still the decode's to stop: the decode
/// has read to the end and waits for the exit when its window goes — and the child is killed
/// and reaped.
#[test]
fn a_decoder_that_closed_its_output_and_lives_on_is_killed_with_its_window() {
    bounded(60, |t| {
        let tmp = t.tmp();
        let hold = t.hold(tmp.path(), "hold");
        let lingers = format!("exec >&- 2>&-\n{}\nexit 0", hold.line());
        let fake = stand_in(tmp.path(), "ffmpeg", &lingers);
        let tools = Tools {
            ffmpeg: fake.clone(),
            ffprobe: fake,
        };
        let decode = wave::start(&tools, &Input::path(tmp.path()), 1, 60.0, || {});
        until("the decode starts", || decode.pid().is_some());
        let pid = decode.pid().unwrap();
        until("its output is read to the end", || decode.waiting());
        assert_eq!(still_running(&[pid]), 1, "it lives on");
        drop(decode);
        until("it is killed and reaped", || still_processes(&[pid]) == 0);
        assert_none_left(tmp.path());
    });
}

/// A `.ts` ffprobe cannot read as media is not a medium (the browser shows its hex view, as
/// Enter does); any other file ffprobe cannot read says so (its info card's line).
#[test]
fn a_file_ffprobe_cannot_read_is_said_and_a_ts_is_not_media() {
    bounded(120, |t| {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = t.tmp();
        let held = Held::budget(&t.hand());
        let ts = tmp.path().join("stream.ts");
        std::fs::write(&ts, [0x47u8; 4096]).unwrap();
        assert!(matches!(
            preview::prepare(
                &tools,
                &Input::path(&ts),
                "stream.ts",
                Kind::Video,
                &Children::default(),
                &held
            ),
            Ok(Found::NotMedia)
        ));
        let mp4 = tmp.path().join("broken.mp4");
        std::fs::write(&mp4, b"not really a video").unwrap();
        let error = prepare(&tools, &mp4, &held).err().expect("unreadable");
        let said = format!("{error:#}");
        assert!(said.starts_with("ffprobe cannot read "), "{said}");
        assert_none_left(tmp.path());
    });
}

// ---------------------------------------------------------------------------------------------
// The encoder question (02 §6: nothing slow on a frame).

/// ffmpeg that answers `-h encoder=…` only once let go at `hold`, and is itself in everything
/// else.
fn held_encoder_answer(dir: &Path, real: &Tools, hold: &Hold) -> Tools {
    let body = format!(
        "case \"$*\" in *'-h encoder='*) {} ;; esac\nexec '{}' \"$@\"",
        hold.line(),
        real.ffmpeg.display()
    );
    Tools {
        ffmpeg: stand_in(dir, "ffmpeg", &body),
        ffprobe: real.ffprobe.clone(),
    }
}

/// A window on `path` as `open` makes it: probed, nothing asked of the encoder yet, its clock
/// the test's.
fn opened(tmp: Dir, path: PathBuf, tools: &Tools, hand: &Hand) -> Fx {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let media = Media::unplanned(tools, &Input::path(&path), &name).unwrap();
    let file = MediaFile::new(Fx::origin(&path), &tmp.path().join("cache"));
    let mut win = MediaWindow::new(
        media,
        file,
        tmp.path().join("work"),
        Marks::default(),
        Selection::default(),
        Vec::new(),
    );
    win.unplanned = true;
    win.clock = hand.clock();
    let mut fx = Fx::around(tmp, path, win);
    // Wide enough for the status row to say its line whole.
    fx.drv.resize(900.0, 520.0);
    fx
}

/// Neither Export nor Save starts, by key or by button, and the window says why.
fn save_and_export_wait(fx: &mut Fx, which: &str) {
    fx.drv.idle(&mut fx.win, 2);
    assert!(
        fx.drv.find_text("checking the encoder…").is_some(),
        "{which}: said while it is asked"
    );
    assert!(fx.win.media.plan.is_none(), "{which}");
    assert!(!fx.win.ready(), "{which}: not settled before the answer");
    fx.drv.key(&mut fx.win, Key::E, Modifiers::NONE);
    fx.drv.click_text(&mut fx.win, "Export  E");
    fx.drv.key(&mut fx.win, Key::S, command(false));
    assert!(fx.win.job.is_none() && fx.win.save.is_none(), "{which}");
    for _ in 0..5 {
        let took = fx.pass();
        assert!(took < FRAME, "{which}: a pass waited: {took:?}");
    }
}

/// 02 §6: for a video above 8 bits the encoder is asked on the preview's worker — the
/// preparation waits for the answer for as long as the test holds it — and its answer travels
/// with the hand-over: Enter finds Save and Export planned and runs nothing, no encoder
/// question and no ffmpeg at all.
#[test]
fn the_encoder_is_asked_while_the_preview_is_prepared() {
    let Some(real) = tools_or_skip() else { return };
    bounded(60, move |t| {
        let tmp = t.tmp();
        let path = tmp.path().join("deep.mov");
        deep_clip(&real, &path);
        let hold = t.hold(tmp.path(), "hold");
        let tools = held_encoder_answer(tmp.path(), &real, &hold);
        let held = Arc::new(Held::budget(&t.hand()));
        let found = while_child_runs(tools, path.clone(), held, Arc::default(), 3, |finished| {
            std::thread::sleep(Duration::from_millis(100));
            assert!(
                !finished(),
                "the preparation waits for the encoder's answer"
            );
            hold.release();
        });
        let Ok(Found::Media(made)) = found else {
            panic!("not prepared");
        };
        let mut fx = shown(tmp, path, *made);
        assert!(!fx.win.unplanned, "planned on the worker");
        let asked = started_under(fx.tmp.path());
        assert_eq!(asked.len(), 3, "the probe, the frame, the encoder");
        assert_none_left(fx.tmp.path());
        let runs = spawns::of(&fx.path);
        let ((), handed) = timed(|| fx.win.set_preview(false));
        let took = handed + fx.pass();
        assert!(took < FRAME, "the hand-over ran something slow: {took:?}");
        fx.drv.until(&mut fx.win, 10, |w| w.ready());
        fx.drv.idle(&mut fx.win, 3);
        assert!(fx.drv.find_text("checking the encoder…").is_none());
        assert_eq!(
            started_under(fx.tmp.path()),
            asked,
            "the hand-over asked the encoder nothing"
        );
        assert_eq!(spawns::of(&fx.path), runs, "and ran no ffmpeg on the file");
        let probe = ffmpeg::probe(&real, Input::path(&fx.path)).unwrap();
        let takes = ffmpeg::encoder_pix_fmts(&real, cut::PLAN_ENCODER).unwrap();
        let as_opened = cut::plan_with(&probe, "deep.mov", Some(&takes)).ok();
        assert!(as_opened.is_some());
        assert_eq!(
            format!("{:?}", fx.win.media.plan),
            format!("{as_opened:?}"),
            "as for a file opened from the list"
        );
        fx.drv.key(&mut fx.win, Key::E, Modifiers::NONE);
        assert!(fx.win.job.is_some(), "Export runs");
    });
}

/// 02 §6: an encoder that does not answer in its time on the preview's worker is stopped there
/// — the preview shows all the same — and a window opened without a preview has asked nothing
/// yet. In both, the window asks off its frames: no pass waits for the answer, Save and Export
/// are unavailable and say `checking the encoder…` until it lands, then they work.
#[test]
fn a_held_encoder_answer_never_holds_a_frame() {
    let Some(real) = tools_or_skip() else { return };
    for through_preview in [true, false] {
        let real = real.clone();
        bounded(60, move |t| {
            let which = if through_preview {
                "handed over"
            } else {
                "opened"
            };
            let tmp = t.tmp();
            let path = tmp.path().join("deep.mov");
            deep_clip(&real, &path);
            let hold = t.hold(tmp.path(), "hold");
            let tools = held_encoder_answer(tmp.path(), &real, &hold);
            let hand = t.hand();
            let mut fx = if through_preview {
                // The worker's question runs out of its 5 s (the test's clock).
                let worker_hand = t.hand();
                let held = Arc::new(Held::budget(&worker_hand));
                let found =
                    while_child_runs(tools.clone(), path.clone(), held, Arc::default(), 3, |_| {
                        worker_hand.advance(Duration::from_secs(5))
                    });
                let Ok(Found::Media(made)) = found else {
                    panic!("the preview shows all the same");
                };
                assert_none_left(tmp.path());
                let mut fx = shown(tmp, path, *made);
                assert!(fx.win.unplanned, "stopped at its time: not planned");
                assert_eq!(fx.drv.pictures().len(), 1, "its frame");
                fx.win.clock = hand.clock();
                fx.drv.resize(900.0, 520.0);
                let ((), handed) = timed(|| fx.win.set_preview(false));
                let took = handed + fx.pass();
                assert!(took < FRAME, "the hand-over waited: {took:?}");
                fx
            } else {
                let mut fx = opened(tmp, path, &tools, &hand);
                let took = fx.pass();
                assert!(took < FRAME, "the first pass waited: {took:?}");
                fx
            };
            save_and_export_wait(&mut fx, which);
            hold.release();
            fx.drv.until(&mut fx.win, 20, |w| w.media.plan.is_some());
            fx.drv.until(&mut fx.win, 10, |w| w.ready());
            fx.drv.idle(&mut fx.win, 2);
            assert!(
                fx.drv.find_text("checking the encoder…").is_none(),
                "{which}: the answer landed"
            );
            let probe = ffmpeg::probe(&real, Input::path(&fx.path)).unwrap();
            let takes = ffmpeg::encoder_pix_fmts(&real, cut::PLAN_ENCODER).unwrap();
            let as_opened = cut::plan_with(&probe, "deep.mov", Some(&takes)).ok();
            assert_eq!(
                format!("{:?}", fx.win.media.plan),
                format!("{as_opened:?}"),
                "{which}"
            );
            assert!(
                !fx.win.media.notes.iter().any(|n| n.contains("timed out")),
                "{which}: {:?}",
                fx.win.media.notes
            );
            fx.drv.key(&mut fx.win, Key::E, Modifiers::NONE);
            assert!(fx.win.job.is_some(), "{which}: Export is available now");
        });
    }
}

/// The window's own encoder question has 5 s: an ffmpeg that never answers is stopped — killed
/// and reaped — when the clock says so, and Save and Export become available all the same,
/// planned for 8-bit output (which every H.264 encoder takes), with a line that says so.
#[test]
fn an_encoder_that_never_answers_is_stopped_and_export_falls_back_to_8_bit() {
    let Some(real) = tools_or_skip() else { return };
    bounded(60, move |t| {
        let tmp = t.tmp();
        let path = tmp.path().join("deep.mov");
        deep_clip(&real, &path);
        let hold = t.hold(tmp.path(), "hold");
        let tools = held_encoder_answer(tmp.path(), &real, &hold);
        let hand = t.hand();
        let mut fx = opened(tmp, path, &tools, &hand);
        fx.pass();
        let dir = fx.tmp.path().to_path_buf();
        until("the encoder is asked", || !started_under(&dir).is_empty());
        save_and_export_wait(&mut fx, "opened");
        hand.advance(Duration::from_millis(4900));
        std::thread::sleep(Duration::from_millis(100));
        fx.drv.idle(&mut fx.win, 2);
        assert!(fx.win.planning.is_some(), "4.9 s of its 5 s: still asked");
        hand.advance(Duration::from_millis(100));
        fx.drv.until(&mut fx.win, 20, |w| w.planning.is_none());
        fx.drv.until(&mut fx.win, 10, |w| w.ready());
        fx.drv.idle(&mut fx.win, 2);
        assert_none_left(&dir);
        assert!(fx.drv.find_text("checking the encoder…").is_none());
        let note = "encoder check timed out; Export converts to 8-bit";
        assert!(
            fx.win.media.notes.iter().any(|n| n == note),
            "{:?}",
            fx.win.media.notes
        );
        // Nothing is said of what this ffmpeg's encoder can do: it never answered.
        let (_, _, planned) = fx.win.media.plan.as_ref().expect("planned");
        for said in fx.win.media.notes.iter().chain(&planned.notes) {
            assert!(
                !said.contains("libx264") && !said.contains("exported as 8-bit"),
                "a claim about the encoder: {said}"
            );
        }
        assert!(
            planned.notes.iter().any(|n| n == note),
            "{:?}",
            planned.notes
        );
        assert!(
            fx.drv
                .text_starting_with("Save / Export: ")
                .is_some_and(|t| t.contains(note)),
            "said in the window"
        );
        let (_, _, plan) = fx.win.media.plan.as_ref().expect("planned");
        assert_eq!(plan.pix_fmt, "yuv420p", "8 bits");
        fx.drv.key(&mut fx.win, Key::E, Modifiers::NONE);
        assert!(fx.win.job.is_some(), "Export is available");
    });
}

/// The window closes before the encoder has answered: its question is stopped with it — no
/// ffmpeg is left — and closing does not wait.
#[test]
fn closing_the_window_stops_its_encoder_question() {
    let Some(real) = tools_or_skip() else { return };
    bounded(60, move |t| {
        let tmp = t.tmp();
        let path = tmp.path().join("deep.mov");
        deep_clip(&real, &path);
        let hold = t.hold(tmp.path(), "hold");
        let tools = held_encoder_answer(tmp.path(), &real, &hold);
        let mut fx = opened(tmp, path, &tools, &t.hand());
        fx.pass();
        let dir = fx.tmp.path().to_path_buf();
        until("the encoder is asked", || !started_under(&dir).is_empty());
        let asked = started_under(&dir);
        assert_eq!(still_running(&asked), 1, "it waits for its answer");
        let Fx { tmp, win, .. } = fx;
        let ((), took) = timed(|| drop(win));
        assert!(took < Duration::from_millis(50), "closing waited: {took:?}");
        assert_eq!(still_running(&asked), 0, "stopped with the window");
        assert_none_left(tmp.path());
    });
}

// ---------------------------------------------------------------------------------------------
// A stop is never lost, and a runner that unwinds leaves no child (every owner of a child).

/// A stop that comes while the waveform's ffmpeg is being started — after the start's look at
/// the stop flag, the slot's lock held for the start — and gives up waiting for that lock is not
/// lost: the child the start then makes (one that keeps its output open and writes nothing) is
/// killed and reaped, with no second stop from outside.
#[test]
fn a_stop_during_a_slow_waveform_start_is_not_lost() {
    bounded(60, |t| {
        let tmp = t.tmp();
        let hold = t.hold(tmp.path(), "hold");
        let silent = stand_in(tmp.path(), "ffmpeg", &hold.stalls());
        let tools = Tools {
            ffmpeg: silent.clone(),
            ffprobe: silent,
        };
        let slot = Arc::new(Children::default());
        let (reached_tx, reached) = mpsc::channel();
        let (go, go_rx) = mpsc::channel::<()>();
        *slot.while_starting.lock().unwrap() = Some(Box::new(move || {
            let _ = reached_tx.send(());
            let _ = go_rx.recv_timeout(Duration::from_secs(30));
        }));
        let input = Input::path(tmp.path());
        let decode = wave::start_in(slot, &tools, &input, 1, 60.0, || {});
        reached
            .recv_timeout(Duration::from_secs(20))
            .expect("the start");
        // The owner goes while the start holds the lock: its stop waits its 20 ms, and returns.
        let ((), took) = timed(|| drop(decode));
        assert!(
            took >= Duration::from_millis(15) && took < Duration::from_millis(200),
            "the stop gave up on the lock: {took:?}"
        );
        assert!(started_under(tmp.path()).is_empty(), "nothing started yet");
        go.send(()).unwrap();
        until("the child is started", || {
            !started_under(tmp.path()).is_empty()
        });
        let pid = started_under(tmp.path());
        until("it is killed and reaped", || still_processes(&pid) == 0);
        assert_none_left(tmp.path());
    });
}

/// A waveform decode whose worker unwinds (here its wake-up call panics) leaves no child — also
/// one that closing its output does not end (it gave a little sound and waits): the ffmpeg is
/// killed and reaped — neither running nor waiting to be reaped — and the owner's drop after it
/// has nothing left to do.
#[test]
fn a_waveform_worker_that_unwinds_leaves_no_child() {
    bounded(60, |t| {
        let tmp = t.tmp();
        let hold = t.hold(tmp.path(), "hold");
        // Two samples, then nothing — and it does not write again, so no closed pipe ends it.
        let gives_a_little = format!(
            "printf '\\000\\000\\000\\000\\000\\000\\000\\000'\n{}",
            hold.stalls()
        );
        let fake = stand_in(tmp.path(), "ffmpeg", &gives_a_little);
        let tools = Tools {
            ffmpeg: fake.clone(),
            ffprobe: fake,
        };
        // A short sound: the first hand-over, and its wake-up, come after those two samples.
        let decode = wave::start(&tools, &Input::path(tmp.path()), 1, 0.01, || {
            panic!("the wake-up failed")
        });
        until("the decode starts", || {
            !started_under(tmp.path()).is_empty()
        });
        let pid = started_under(tmp.path());
        // With its owner still there: the worker's own way out stops and reaps.
        until("it is killed and reaped", || still_processes(&pid) == 0);
        drop(decode);
        assert_eq!(still_processes(&pid), 0, "neither running nor a zombie");
        assert_none_left(tmp.path());
    });
}

/// A waveform decode that cannot start the thread that watches its child leaves no child
/// either: the one already started is killed and reaped as the worker goes — its owner still
/// there, no stop from outside — and the failure is said where the waveform is.
#[test]
fn a_waveform_that_cannot_watch_its_child_leaves_none() {
    bounded(60, |t| {
        let tmp = t.tmp();
        let hold = t.hold(tmp.path(), "hold");
        let fake = stand_in(tmp.path(), "ffmpeg", &hold.stalls());
        let tools = Tools {
            ffmpeg: fake.clone(),
            ffprobe: fake,
        };
        let slot = Arc::new(Children::default());
        slot.fail_watcher.store(true, SeqCst);
        let input = Input::path(tmp.path());
        let decode = wave::start_in(slot, &tools, &input, 1, 60.0, || {});
        until("the decode starts", || {
            !started_under(tmp.path()).is_empty()
        });
        let pid = started_under(tmp.path());
        until("it is killed and reaped", || still_processes(&pid) == 0);
        until("the failure is said", || {
            let peaks = decode.peaks.lock().unwrap();
            peaks.as_ref().is_some_and(|p| p.error.is_some())
        });
        assert_none_left(tmp.path());
    });
}

/// The preview's runner (ffprobe, the frame, the waveform, the encoder question: one runner)
/// unwinding while its child runs — here whoever reads the output panics — does not hang and
/// leaves no child.
#[test]
fn a_preview_run_that_unwinds_leaves_no_child() {
    bounded(30, |t| {
        let tmp = t.tmp();
        let hold = t.hold(tmp.path(), "hold");
        let stalls = stand_in(tmp.path(), "ffprobe", &hold.stalls());
        let tools = Tools {
            ffmpeg: stalls.clone(),
            ffprobe: stalls,
        };
        let path = tmp.path().join("hangs.mp4");
        std::fs::write(&path, b"whatever").unwrap();
        let children = Arc::new(Children::default());
        children.panic_in_read.store(true, SeqCst);
        let held = Arc::new(Held::budget(&t.hand()));
        let worker = std::thread::spawn(move || prepare_in(&tools, &path, &held, &children));
        until("it unwinds", || worker.is_finished());
        assert!(worker.join().is_err(), "it unwound");
        assert_eq!(started_under(tmp.path()).len(), 1, "the child was started");
        assert_none_left(tmp.path());
    });
}

/// A stop that comes while a preview's child is being started and gives up on the lock is not
/// lost either: the runner stops the child it then makes, and ends cancelled.
#[test]
fn a_stop_during_a_slow_preview_start_is_not_lost() {
    bounded(60, |t| {
        let tmp = t.tmp();
        let hold = t.hold(tmp.path(), "hold");
        let stalls = stand_in(tmp.path(), "ffprobe", &hold.stalls());
        let tools = Tools {
            ffmpeg: stalls.clone(),
            ffprobe: stalls,
        };
        let path = tmp.path().join("hangs.mp4");
        std::fs::write(&path, b"whatever").unwrap();
        let children = Arc::new(Children::default());
        let (reached_tx, reached) = mpsc::channel();
        let (go, go_rx) = mpsc::channel::<()>();
        *children.while_starting.lock().unwrap() = Some(Box::new(move || {
            let _ = reached_tx.send(());
            let _ = go_rx.recv_timeout(Duration::from_secs(30));
        }));
        let held = Arc::new(Held::budget(&t.hand()));
        let (c, p) = (children.clone(), path.clone());
        let worker = std::thread::spawn(move || prepare_in(&tools, &p, &held, &c));
        reached
            .recv_timeout(Duration::from_secs(20))
            .expect("the start");
        children.stop();
        go.send(()).unwrap();
        until("the preparation ends", || worker.is_finished());
        let error = worker.join().unwrap().err().expect("cancelled");
        assert!(crate::remote::is_cancelled(&error), "{error:#}");
        assert_eq!(started_under(tmp.path()).len(), 1, "the child was started");
        assert_none_left(tmp.path());
    });
}

/// 02 §4.3: each step's time starts when the step does. A probe that took most of its 5 s
/// leaves the first frame its own whole 5 s: 4.9 s into the frame — 9.8 s after the preparation
/// began — it still runs, and it is stopped at its 5 s.
#[test]
fn a_steps_time_starts_when_the_step_does() {
    let Some(real) = tools_or_skip() else { return };
    bounded(60, move |t| {
        let tmp = t.tmp();
        let path = tmp.path().join("clip.mp4");
        clip(&real, &path, (320, 240));
        let (probe_hold, frame_hold) = (
            t.hold(tmp.path(), "probe-hold"),
            t.hold(tmp.path(), "frame-hold"),
        );
        let tools = Tools {
            ffmpeg: stand_in(tmp.path(), "ffmpeg", &frame_hold.stalls()),
            ffprobe: stand_in(tmp.path(), "ffprobe", &probe_hold.then(&real.ffprobe)),
        };
        let hand = t.hand();
        let held = Arc::new(Held::budget(&hand));
        let dir = tmp.path().to_path_buf();
        let ended = while_child_runs(tools, path, held, Arc::default(), 1, |finished| {
            // The probe, held for 4.9 s of its 5, then let go.
            hand.advance(Duration::from_millis(4900));
            probe_hold.release();
            until("the frame's decode starts", || {
                started_under(&dir).len() == 2
            });
            // The frame's own 5 s, from its own start.
            hand.advance(Duration::from_millis(4900));
            std::thread::sleep(Duration::from_millis(100));
            assert!(
                !finished(),
                "9.8 s in all, 4.9 s of the frame's 5: it goes on"
            );
            assert_eq!(still_running(&started_under(&dir)), 1);
            hand.advance(Duration::from_millis(100));
        });
        let error = ended.err().expect("stopped");
        assert_eq!(format!("{error:#}"), "Frame took over 5000 ms");
        assert_none_left(tmp.path());
    });
}

/// The tests' own supervisor: a test that does not end — here one whose children are never
/// stopped (as when cancellation is broken): a stand-in waiting at a hold on a clock that
/// stands still, and one that writes for ever — fails, and nothing of it is left: the
/// supervisor has killed and reaped what the test started, before it reports.
#[test]
fn a_test_that_does_not_end_is_failed_and_cleaned_up_after() {
    let dir = Arc::new(Mutex::new(None));
    let seen = dir.clone();
    let reached = Arc::new(AtomicBool::new(false));
    let there = reached.clone();
    let ended = supervised(15, move |t| {
        let tmp = t.tmp();
        *seen.lock().unwrap() = Some(tmp.path().to_path_buf());
        let hold = t.hold(tmp.path(), "hold");
        let (waits, writes) = (
            stand_in(tmp.path(), "waits", &hold.stalls()),
            stand_in(tmp.path(), "writes", NEVER_ENDS),
        );
        let file = tmp.path().join("some.mp4");
        // Two children nobody stops, their slots leaked: nothing of the code under test would
        // end them.
        for program in [waits, writes] {
            let slot: &'static Children = Box::leak(Box::default());
            let mut cmd = std::process::Command::new(program);
            cmd.arg(&file);
            // Its output stays open (a closed pipe would end the one that writes).
            std::mem::forget(slot.start(&mut cmd, Some(&file)).unwrap().unwrap());
        }
        assert_eq!(still_running(&started_under(tmp.path())), 2, "they run");
        there.store(true, SeqCst);
        // And a wait that nothing ends.
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    let why = ended.expect_err("it did not end");
    assert!(reached.load(SeqCst), "it got as far as its endless wait");
    // At least the two (a stand-in's own start-up check may still be going on a loaded machine).
    let killed: usize = why
        .strip_prefix("not over in 15 s; ")
        .and_then(|rest| rest.split(' ').next()?.parse().ok())
        .unwrap_or_else(|| panic!("{why}"));
    assert!(killed >= 2, "{why}");
    let dir = dir.lock().unwrap().clone().expect("its folder");
    let started = started_under(&dir);
    assert_eq!(started.len(), 2);
    assert_eq!(still_processes(&started), 0, "killed and reaped");
    assert_eq!(naming(&dir), 0, "nothing names its folder");
}

// ---------------------------------------------------------------------------------------------
// What the live probe found (round C).

/// What ffprobe and ffmpeg say is shown without what changes from run to run (`[component @
/// 0x…]` tags) and without the file's own path in front of it.
#[test]
fn what_the_tools_say_is_shown_plain() {
    let input = Input::path(Path::new("/tmp/a b/clip.mp4"));
    for (said, plain) in [
        (
            "[out#0/rawvideo @ 0xb72c40300] Nothing was written into output file",
            "Nothing was written into output file",
        ),
        (
            "/tmp/a b/clip.mp4: Invalid data found when processing input",
            "Invalid data found when processing input",
        ),
        (
            "[mov,mp4,m4a,3gp,3g2,mj2 @ 0x7f8] [h264 @ 0x12ab] no frame!",
            "no frame!",
        ),
        ("[not a tag] stays", "[not a tag] stays"),
        ("plain words", "plain words"),
    ] {
        assert_eq!(preview::plain(said, &input), plain, "{said}");
    }
}

/// A video whose index is whole and whose pictures are missing: its preview says there is no
/// frame — the same words every time, no address in them — and a window opened on it does not
/// sit there decoding for ever: it fails, saying why, and asks to be closed.
#[test]
fn a_video_with_no_frame_fails_its_window_and_says_so_plainly() {
    let Some(tools) = tools_or_skip() else { return };
    bounded(60, move |t| {
        let tmp = t.tmp();
        let path = tmp.path().join("cut.mp4");
        index_only(&tools, &path);
        let said: Vec<String> = (0..2)
            .map(|_| {
                let error = prepare(&tools, &path, &Held::budget(&t.hand()))
                    .err()
                    .expect("no frame");
                format!("{error:#}")
            })
            .collect();
        assert!(said[0].starts_with("no frame at 0:00.00"), "{}", said[0]);
        assert!(
            !said[0].contains("0x") && !said[0].contains('['),
            "an address or a tag in it: {}",
            said[0]
        );
        assert_eq!(said[0], said[1], "the same words every time");
        assert!(!said[0].contains("cut.mp4"), "the path again: {}", said[0]);

        // Opened: the window gives up within the paused frame's own time, and says why.
        let hand = t.hand();
        let mut fx = opened(tmp, path, &tools, &hand);
        fx.drv.until(&mut fx.win, 30, |w| w.failure().is_some());
        let why = fx.win.failure().unwrap();
        assert!(why.contains("no frame"), "{why}");
        assert!(fx.win.close_requested(), "it goes without a question");
        assert_eq!(
            fx.win.outcome.lock().unwrap().failed.as_deref(),
            Some(why.as_str()),
            "the open failed"
        );
        assert_none_left(fx.tmp.path());
    });
}

/// A video whose pixels are not square is shown in its own shape — previewed and opened: a
/// 720×576 picture of 16:11 pixels is 1047×576, not 5:4.
#[test]
fn a_video_of_wide_pixels_is_shown_in_its_shape() {
    let Some(tools) = tools_or_skip() else { return };
    bounded(120, move |t| {
        let tmp = t.tmp();
        let path = tmp.path().join("wide.mp4");
        wide_pixels(&tools, &path);
        let mut fx = previewed(tmp, path, &tools, &Held::budget(&t.hand()));
        assert_eq!(fx.win.media.probe.pixel_aspect, Some((16, 11)));
        let shape = |fx: &Fx| {
            let picture = fx.drv.pictures()[0];
            picture.width() / picture.height()
        };
        let wanted = 720.0 * 16.0 / 11.0 / 576.0;
        let previewed = shape(&fx);
        assert!(
            (previewed - wanted).abs() < 0.03,
            "previewed {previewed}, its shape is {wanted}"
        );
        fx.win.set_preview(false);
        fx.drv
            .until(&mut fx.win, 10, |w| w.ready() && w.frame_ready());
        fx.drv.idle(&mut fx.win, 2);
        let opened = shape(&fx);
        assert!((opened - wanted).abs() < 0.03, "opened {opened}");
        // A crop is still in the picture's own pixels.
        let map = fx.win.map.expect("drawn");
        let (a, b) = (
            map.to_disp(Pos2::new(100.0, 100.0)),
            map.to_disp(Pos2::new(500.0, 400.0)),
        );
        fx.drv.drag(&mut fx.win, PointerButton::Primary, a, b);
        assert_eq!(fx.win.crop(), Some([100, 100, 400, 300]));
    });
}

/// A sound in six channels: two lanes — its first two channels, by their own names, not a mix
/// called L and R — and a line that says so.
#[test]
fn a_six_channel_sound_shows_its_first_two_channels_by_name() {
    let Some(tools) = tools_or_skip() else { return };
    bounded(120, move |t| {
        let tmp = t.tmp();
        let path = tmp.path().join("surround.wav");
        six_channels(&tools, &path);
        let mut fx = previewed(tmp, path, &tools, &Held::budget(&t.hand()));
        let lanes = |fx: &Fx| {
            let said = fx.drv.text_starting_with("audio · ").expect("the line");
            assert!(said.contains("6 channels · first 2 shown"), "{said}");
            for (name, other) in [("FL", "L"), ("FR", "R")] {
                assert!(fx.drv.find_text(name).is_some(), "{name}: the lane's name");
                assert!(fx.drv.find_text(other).is_none(), "a lane called {other}");
            }
            // Painted: the first lane the tone, the second (front right, silent) flat.
            let colour = super::super::chrome::PALETTE.wave;
            let lines: Vec<(f32, f32)> = fx
                .drv
                .segments()
                .into_iter()
                .filter(|([a, b], c)| *c == colour && a.x == b.x)
                .map(|([a, b], _)| ((a.y + b.y) / 2.0, (a.y - b.y).abs()))
                .collect();
            let middle = median(lines.iter().map(|(y, _)| *y).collect());
            let lane = |upper: bool| -> Vec<f32> {
                let of_it = lines.iter().filter(|(y, _)| (*y < middle) == upper);
                of_it.map(|(_, h)| *h).collect()
            };
            (median(lane(true)), median(lane(false)))
        };
        let (front_left, front_right) = lanes(&fx);
        assert!(front_left > 20.0, "the first channel's tone: {front_left}");
        assert!(front_right < 2.0, "the second is silent: {front_right}");
        fx.win.set_preview(false);
        fx.drv.until(&mut fx.win, 10, |w| w.ready());
        fx.drv.resize(900.0, 520.0);
        fx.drv.idle(&mut fx.win, 3);
        let (front_left, front_right) = lanes(&fx);
        assert!(front_left > 20.0 && front_right < 2.0, "opened");
    });
}

/// The other end of the two tests below: run by them in a process of its own (never by the
/// suite), it starts a child that waits at a hold and then either waits to be signalled or
/// gives up as the screenshot hook's watchdog does.
#[test]
fn the_process_the_ending_tests_end() {
    let (Ok(dir), Ok(how)) = (
        std::env::var("SOPEN_TEST_ENDING_DIR"),
        std::env::var("SOPEN_TEST_ENDING"),
    ) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let tools = Tools {
        ffmpeg: dir.join("ffprobe"),
        ffprobe: dir.join("ffprobe"),
    };
    let path = dir.join("hangs.mp4");
    let held = Arc::new(Held::real([600, 600, 600, 600].map(Duration::from_secs)));
    let worker = std::thread::spawn(move || {
        let _ = prepare(&tools, &path, &held);
    });
    until("the probe starts", || !started_under(&dir).is_empty());
    println!("child started");
    if how == "watchdog" {
        crate::gui::give_up("the test's watchdog");
    }
    let _ = worker.join();
}

/// Runs this test binary's [`the_process_the_ending_tests_end`] in a process of its own, its
/// ffprobe a stand-in held in `dir`; returns once that child runs.
fn a_process_with_a_held_child(t: &Test, how: &str) -> (std::process::Child, PathBuf) {
    let tmp = t.tmp();
    let hold = t.hold(tmp.path(), "hold");
    stand_in(tmp.path(), "ffprobe", &hold.stalls());
    std::fs::write(tmp.path().join("hangs.mp4"), b"whatever").unwrap();
    let process = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "gui::media::preview_tests::the_process_the_ending_tests_end",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("SOPEN_TEST_ENDING_DIR", tmp.path())
        .env("SOPEN_TEST_ENDING", how)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    (process, tmp.path().to_path_buf())
}

/// sopen ended by a signal (SIGTERM, SIGHUP) leaves no ffprobe behind: the child a preview's
/// preparation runs is stopped before the process goes — which still dies of that signal.
#[test]
fn a_signal_that_ends_sopen_ends_its_children_first() {
    use std::os::unix::process::ExitStatusExt as _;
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        bounded(60, move |t| {
            let (mut process, dir) = a_process_with_a_held_child(t, "signal");
            until("its ffprobe runs", || naming(&dir) == 1);
            unsafe { libc::kill(process.id() as libc::pid_t, signal) };
            let status = process.wait().unwrap();
            assert_eq!(status.signal(), Some(signal), "it died of the signal");
            assert_eq!(naming(&dir), 0, "its ffprobe went first");
        });
    }
}

/// The screenshot hook's watchdog giving up ends the children the same way before it exits.
#[test]
fn the_watchdog_giving_up_ends_the_children_first() {
    bounded(60, |t| {
        let (mut process, dir) = a_process_with_a_held_child(t, "watchdog");
        let status = process.wait().unwrap();
        assert_eq!(status.code(), Some(1), "it gave up");
        assert_eq!(naming(&dir), 0, "its ffprobe went first");
    });
}
