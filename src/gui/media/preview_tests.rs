//! The media window as a preview (02 §4.2–§4.4, slice S1-M): what its preparation makes — the
//! probe, one frame, the waveform of the first minutes — within each step's time, with every
//! ffprobe and ffmpeg it starts killed and reaped on every way out (counted with `ps`); the
//! previewed window, which shows that and nothing else — no bars, no input, no playback, no
//! process of its own; and the hand-over, which decodes nothing again. Real files need ffmpeg
//! (skipped without it, unless `SOPEN_REQUIRE_FFMPEG` is set); the stalls are stand-in programs.

use super::preview::{self, started_under, still_processes, Children, Found, Step, Watch};
use super::*;
use crate::ffmpeg::spawns;
use crate::gui::testkit::{command, Driver};
use crate::gui::Window as _;
use crate::kind::Kind;
use crate::remote::{LocalFs, RemoteFs};
use eframe::egui::{Key, Modifiers, PointerButton};
use std::os::unix::fs::PermissionsExt;

fn tools_or_skip() -> Option<Tools> {
    match ffmpeg::resolve() {
        Ok(t) => Some(t),
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping media preview test: {e}");
            None
        }
    }
}

fn make(tools: &Tools, args: &[&str], dst: &Path) {
    let ok = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .arg(dst)
        .status()
        .unwrap()
        .success();
    assert!(ok, "fixture {}", dst.display());
}

/// 2 s of 320×240 at 25 fps with a sound track.
fn clip(tools: &Tools, dst: &Path) {
    make(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ],
        dst,
    );
}

/// `secs` of a 440 Hz tone, mono, 8 kHz (a WAV: its samples are where its time says).
fn tone(tools: &Tools, dst: &Path, secs: u32) {
    let source = format!("sine=frequency=440:sample_rate=8000:duration={secs}");
    make(tools, &["-f", "lavfi", "-i", &source, "-ac", "1"], dst);
}

/// A program standing in for ffmpeg or ffprobe: `body` after the `-version` answer (asked once
/// here, so a start that finds the new file busy is retried before the test's own).
fn stand_in(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    let script = format!("#!/bin/sh\n[ \"$1\" = -version ] && exit 0\n{body}\n");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::testutil::run_fresh(std::process::Command::new(&path).arg("-version"));
    path
}

/// Never answers, never ends (one process: the shell becomes it).
const STALLS: &str = "exec sleep 600";
/// Sound without end.
const NEVER_ENDS: &str = "exec cat /dev/zero";

/// What a test's preparation is held to: each step's time, how much sound, and its cancel.
struct Held {
    cancel: AtomicBool,
    times: [Duration; 4],
    sound: Option<f64>,
    step: Mutex<Option<(Step, Instant)>>,
}

impl Held {
    /// 02 §4.3's: 5 s for ffprobe and for the frame, the first 10 minutes of sound in 10 s.
    fn budget() -> Self {
        Self::with(
            [
                Duration::from_secs(5),
                Duration::from_secs(5),
                Duration::from_secs(10),
            ],
            Some(600.0),
        )
    }

    /// `times`: ffprobe's, the frame's and the waveform's; the encoder check has ffprobe's.
    fn with(times: [Duration; 3], sound: Option<f64>) -> Self {
        Self {
            cancel: AtomicBool::new(false),
            times: [times[0], times[1], times[2], times[0]],
            sound,
            step: Mutex::new(None),
        }
    }
}

impl Watch for Held {
    fn begin(&self, step: Step) {
        *self.step.lock().unwrap() = Some((step, Instant::now()));
    }

    fn check(&self) -> Result<()> {
        if self.cancel.load(SeqCst) {
            bail!(crate::remote::Cancelled);
        }
        if let Some((step, since)) = *self.step.lock().unwrap() {
            let time = self.times[step as usize];
            if since.elapsed() >= time {
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

fn prepare(tools: &Tools, path: &Path, held: &Held) -> Result<Found> {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    preview::prepare(
        tools,
        &Input::path(path),
        &name,
        kind_of(path),
        &Children::default(),
        held,
    )
}

fn made(tools: &Tools, path: &Path, held: &Held) -> preview::Prepared {
    match prepare(tools, path, held) {
        Ok(Found::Media(made)) => *made,
        Ok(Found::NotMedia) => panic!("{} is not media", path.display()),
        Err(e) => panic!("{}: {e:#}", path.display()),
    }
}

/// A previewed media window on a local file, in a side of the browser's size.
struct Fx {
    tmp: tempfile::TempDir,
    path: PathBuf,
    win: MediaWindow,
    drv: Driver,
}

fn previewed(tmp: tempfile::TempDir, path: PathBuf, tools: &Tools, held: &Held) -> Fx {
    let made = made(tools, &path, held);
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let mut win = MediaWindow::previewing(origin, made, &tmp.path().join("cache"));
    win.set_preview(true);
    // Never the device: whatever a test finds, it is not heard.
    win.sink = Sink::Null;
    let mut fx = Fx {
        tmp,
        path,
        win,
        drv: Driver::new(500.0, 420.0),
    };
    fx.drv.until(&mut fx.win, 10, |w| w.ready());
    fx.drv.idle(&mut fx.win, 2);
    fx
}

fn video() -> Option<(Fx, Tools)> {
    let tools = tools_or_skip()?;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("clip.mp4");
    clip(&tools, &path);
    Some((previewed(tmp, path, &tools, &Held::budget()), tools))
}

/// A `secs`-second tone, its preview reading at most `sound` seconds of it.
fn sound(secs: u32, sound: Option<f64>) -> Option<(Fx, Tools)> {
    let tools = tools_or_skip()?;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("tone.wav");
    tone(&tools, &path, secs);
    let held = Held {
        sound,
        ..Held::budget()
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

/// P1 (video): one frame — the frame at 0 s — fitted in a single-lined frame, with the video's
/// size and duration; none of the media window's bars.
#[test]
fn a_previewed_video_is_its_first_frame_with_its_size_and_duration() {
    let Some((fx, _tools)) = video() else { return };
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
}

/// P1 (audio): the waveform, with the whole duration; no bars.
#[test]
fn a_previewed_sound_is_its_waveform_with_its_duration() {
    let Some((fx, _tools)) = sound(3, Some(600.0)) else {
        return;
    };
    let said = fx.drv.text_starting_with("audio · mono").expect("the line");
    assert!(said.contains("0:03.00"), "the whole duration: {said}");
    assert!(!said.contains("preview of the first"), "all of it: {said}");
    let (_, columns) = fx.win.columns.as_ref().expect("the waveform is drawn");
    let loud = columns[0].iter().filter(|[lo, hi]| hi - lo > 0.1).count();
    assert!(
        loud * 10 >= columns[0].len() * 9,
        "the tone, over the whole width: {loud} of {} columns",
        columns[0].len()
    );
    assert!(fx.drv.find_text("reading the waveform…").is_none());
    for bar in ["Play", "In  I", "Export  E", "Save"] {
        assert!(fx.drv.find_text(bar).is_none(), "{bar} in a preview");
    }
    assert_eq!(
        fx.drv.colors_of(AUDIO_TITLE),
        Some(vec![super::super::chrome::PALETTE.frame_idle.color])
    );
}

/// P5: real clicks (where the Play button is, too), drags, the wheel, `C`, `P`, Space,
/// Cmd/Ctrl+V, typing — nothing changes in a previewed video or sound, playback never starts
/// (no audio output is opened), no ffmpeg runs for it after its preparation, and dropping it
/// asks nothing.
#[test]
fn a_previewed_medium_takes_no_input_never_plays_and_runs_nothing() {
    for which in ["video", "sound"] {
        let Some((mut fx, _tools)) = (match which {
            "video" => video(),
            _ => sound(3, Some(600.0)),
        }) else {
            return;
        };
        let before = state(&fx.win);
        let runs = spawns::of(&fx.path);
        let pids = started_under(fx.tmp.path());
        assert_eq!(
            runs.preview + runs.probe,
            pids.len() as u64,
            "{which}: every run on the file was the preparation's own: {runs:?}"
        );
        assert_eq!(still_processes(&pids), 0, "{which}: none of them is left");
        everything(&mut fx);
        assert_eq!(state(&fx.win), before, "{which}: the preview changed");
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
        assert!(
            fx.win.close_requested(),
            "{which}: dropping it asks nothing"
        );
        assert!(
            !fx.tmp.path().join("cache").exists(),
            "{which}: a preview leaves sopen's cache alone"
        );
    }
}

/// P6 (video): handed over, the window is the media window — its bars and the double frame back,
/// Space plays — and the frame the preview decoded is the one on screen: nothing ran again.
#[test]
fn a_handed_over_video_keeps_its_frame_and_plays() {
    let Some((mut fx, _tools)) = video() else {
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
    fx.drv.key(&mut fx.win, Key::Space, Modifiers::NONE);
    assert_eq!(fx.win.plays, 1, "Space plays again");
    assert!(fx.win.player.is_some());
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
}

/// P6 (audio): a sound the preview read whole is handed over with its waveform: no decode.
#[test]
fn a_handed_over_short_sound_keeps_its_waveform() {
    let Some((mut fx, _tools)) = sound(3, Some(600.0)) else {
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
}

/// P12 (audio): a sound longer than the preview reads shows the waveform of its first stretch
/// and says so; handed over, those peaks stay as they are and the rest is decoded from where
/// the preview stopped — one more run, starting there.
#[test]
fn a_handed_over_long_sound_decodes_on_from_where_the_preview_stopped() {
    let Some((mut fx, _tools)) = sound(6, Some(2.0)) else {
        return;
    };
    let said = fx.drv.text_starting_with("audio · mono").expect("the line");
    assert!(
        said.contains("0:06.00") && said.contains("preview of the first 2 s"),
        "{said}"
    );
    assert_eq!(wave::starts(&fx.path), [(0.0, Some(2.0))]);
    let (before, cut) = fx.win.peaks.clone().expect("the preview's peaks");
    assert_eq!(cut, Some(2.0));
    assert!(!before.complete);
    let at = |secs: f64| (secs / before.bin_secs) as usize;
    let quiet = |bins: &[[f32; 2]]| bins.iter().all(|b| *b == [0.0, 0.0]);
    assert!(!quiet(&before.lanes[0][..at(2.0) - 1]), "the first 2 s");
    assert!(
        quiet(&before.lanes[0][at(2.0) + 1..]),
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
    let rest = &after.lanes[0][at(2.0) + 1..at(5.9)];
    assert!(
        rest.iter().all(|[lo, hi]| hi - lo > 0.1),
        "the rest is the tone"
    );
}

/// P3: a video whose probe hangs (a stand-in `ffprobe` that never answers) is stopped at the
/// probe's time — its card's line is the error — and the stand-in is killed and reaped.
#[test]
fn a_stalled_ffprobe_is_stopped_at_its_time_and_reaped() {
    let tmp = tempfile::tempdir().unwrap();
    let stalls = stand_in(tmp.path(), "ffprobe", STALLS);
    let tools = Tools {
        ffmpeg: stalls.clone(),
        ffprobe: stalls,
    };
    let path = tmp.path().join("hangs.mp4");
    std::fs::write(&path, b"whatever").unwrap();
    let ms = Duration::from_millis;
    let held = Held::with([ms(300), ms(300), ms(300)], Some(600.0));
    let started = Instant::now();
    let error = prepare(&tools, &path, &held).err().expect("stopped");
    let took = started.elapsed();
    assert_eq!(format!("{error:#}"), "Probe took over 300 ms");
    assert!(
        took >= ms(300) && took < Duration::from_secs(5),
        "within its time: {took:?}"
    );
    let pids = started_under(tmp.path());
    assert_eq!(pids.len(), 1, "the probe: {pids:?}");
    assert_eq!(still_processes(&pids), 0, "killed and reaped");
}

/// P3: the first frame has its own time: an ffmpeg that never gives one is stopped there.
#[test]
fn a_stalled_first_frame_is_stopped_at_its_time_and_reaped() {
    let Some(real) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("clip.mp4");
    clip(&real, &path);
    let tools = Tools {
        ffmpeg: stand_in(tmp.path(), "ffmpeg", STALLS),
        ffprobe: real.ffprobe,
    };
    let ms = Duration::from_millis;
    let held = Held::with([Duration::from_secs(20), ms(300), ms(300)], Some(600.0));
    let error = prepare(&tools, &path, &held).err().expect("stopped");
    assert_eq!(format!("{error:#}"), "Frame took over 300 ms");
    let pids = started_under(tmp.path());
    assert_eq!(pids.len(), 2, "the probe and the frame: {pids:?}");
    assert_eq!(still_processes(&pids), 0, "killed and reaped");
}

/// P3: a two-hour sound shows the waveform of its first 10 minutes — said, with the whole
/// duration — within the waveform's 10 s, and nothing is left running.
#[test]
fn a_two_hour_sound_previews_its_first_ten_minutes_in_time() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
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
    let started = Instant::now();
    let fx = previewed(tmp, path, &tools, &Held::budget());
    let took = started.elapsed();
    assert!(took < Duration::from_secs(10), "{took:?}");
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
    // The preview shows those 10 minutes over its whole width.
    let (_, columns) = fx.win.columns.as_ref().expect("drawn");
    let loud = columns[0].iter().filter(|[lo, hi]| hi - lo > 0.05).count();
    assert!(loud * 10 >= columns[0].len() * 9, "{loud}");
    let pids = started_under(fx.tmp.path());
    assert_eq!(pids.len(), 2, "the probe and the waveform: {pids:?}");
    assert_eq!(still_processes(&pids), 0, "no worker is left");
}

/// P4: a waveform decode that is cancelled stops: its ffmpeg (here one that would give sound
/// for ever) is killed and reaped, and the preparation ends cancelled.
#[test]
fn a_cancelled_waveform_decode_stops() {
    let Some(real) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("tone.wav");
    tone(&real, &path, 3);
    let tools = Tools {
        ffmpeg: stand_in(tmp.path(), "ffmpeg", NEVER_ENDS),
        ffprobe: real.ffprobe,
    };
    let long = Duration::from_secs(60);
    let held = Held::with([long, long, long], Some(600.0));
    let dir = tmp.path().to_path_buf();
    let ended = std::thread::scope(|scope| {
        let worker = scope.spawn(|| prepare(&tools, &path, &held));
        // The probe, then the decode: once it runs, the preview is left.
        let deadline = Instant::now() + Duration::from_secs(20);
        while started_under(&dir).len() < 2 {
            assert!(Instant::now() < deadline, "the decode never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(still_processes(&started_under(&dir)), 1, "it runs");
        held.cancel.store(true, SeqCst);
        let left = Instant::now();
        let ended = worker.join().unwrap();
        assert!(
            left.elapsed() < Duration::from_secs(5),
            "stopped at once: {:?}",
            left.elapsed()
        );
        ended
    });
    let error = ended.err().expect("cancelled");
    assert!(crate::remote::is_cancelled(&error), "{error:#}");
    assert_eq!(
        still_processes(&started_under(&dir)),
        0,
        "killed and reaped"
    );
}

/// 02 §4.4, the window closing: whoever stops a preparation without waiting for its worker
/// kills its ffprobe at once — the process is gone when the kill returns, not when the worker
/// next looks.
#[test]
fn a_stop_from_outside_kills_the_running_child_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let stalls = stand_in(tmp.path(), "ffprobe", STALLS);
    let tools = Tools {
        ffmpeg: stalls.clone(),
        ffprobe: stalls,
    };
    let path = tmp.path().join("hangs.mp4");
    std::fs::write(&path, b"whatever").unwrap();
    // No stop of its own: no cancel, a minute for each step.
    let long = Duration::from_secs(60);
    let held = Held::with([long, long, long], Some(600.0));
    let children = Children::default();
    let dir = tmp.path().to_path_buf();
    let ended = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let input = Input::path(&path);
            preview::prepare(&tools, &input, "hangs.mp4", Kind::Video, &children, &held)
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        while started_under(&dir).is_empty() {
            assert!(Instant::now() < deadline, "the probe never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(still_processes(&started_under(&dir)), 1, "it runs");
        children.kill();
        assert_eq!(
            still_processes(&started_under(&dir)),
            0,
            "gone when the kill returns"
        );
        worker.join().unwrap()
    });
    let error = ended.err().expect("stopped");
    assert!(crate::remote::is_cancelled(&error), "{error:#}");
    // Stopped before it starts: nothing runs at all.
    held.cancel.store(true, SeqCst);
    let error = prepare(&tools, &path, &held).err().expect("cancelled");
    assert!(crate::remote::is_cancelled(&error), "{error:#}");
    assert_eq!(started_under(&dir).len(), 1, "no second child was started");
}

/// The media window's own waveform decode stops with its window: dropped, its ffmpeg is killed
/// and reaped (it ran on to the end of the file before).
#[test]
fn a_dropped_waveform_decode_is_killed() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = stand_in(tmp.path(), "ffmpeg", NEVER_ENDS);
    let tools = Tools {
        ffmpeg: fake.clone(),
        ffprobe: fake,
    };
    let decode = wave::start(&tools, &Input::path(tmp.path()), 1, 3600.0, || {});
    let deadline = Instant::now() + Duration::from_secs(20);
    let pid = loop {
        if let Some(pid) = decode.pid() {
            break pid;
        }
        assert!(Instant::now() < deadline, "the decode never started");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(still_processes(&[pid]), 1, "it runs");
    drop(decode);
    assert_eq!(
        still_processes(&[pid]),
        0,
        "killed and reaped with its window"
    );
}

/// A `.ts` ffprobe cannot read as media is not a medium (the browser shows its hex view, as
/// Enter does); any other file ffprobe cannot read says so (its info card's line).
#[test]
fn a_file_ffprobe_cannot_read_is_said_and_a_ts_is_not_media() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let held = Held::budget();
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
    assert_eq!(still_processes(&started_under(tmp.path())), 0);
}

/// 3 s of 160×120 above 8 bits (10-bit ProRes, an encoder every ffmpeg has): a video whose
/// Save and Export plan asks ffmpeg what its H.264 encoder takes.
fn deep_clip(tools: &Tools, dst: &Path) {
    make(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=25:duration=3",
            "-c:v",
            "prores_ks",
            "-pix_fmt",
            "yuv422p10le",
        ],
        dst,
    );
}

/// ffmpeg, slow to say what an encoder takes (`-h encoder=…` answers after 2 s) and itself in
/// everything else.
fn slow_encoder_answer(dir: &Path, real: &Tools) -> Tools {
    let body = format!(
        "case \"$*\" in *'-h encoder='*) sleep 2 ;; esac\nexec '{}' \"$@\"",
        real.ffmpeg.display()
    );
    Tools {
        ffmpeg: stand_in(dir, "ffmpeg", &body),
        ffprobe: real.ffprobe.clone(),
    }
}

/// One pass of the window, timed.
fn pass(fx: &mut Fx) -> Duration {
    let started = Instant::now();
    fx.drv.idle(&mut fx.win, 1);
    started.elapsed()
}

/// A frame's time, generously: far under the encoder's 2 s.
const FRAME: Duration = Duration::from_millis(250);

/// 02 §6, nothing slow on a frame: for a video above 8 bits the encoder is asked on the
/// preview's worker, where it can be stopped, and its answer travels with the hand-over — Enter
/// finds Save and Export planned and runs nothing for it.
#[test]
fn the_encoder_is_asked_while_the_preview_is_prepared() {
    let Some(real) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("deep.mov");
    deep_clip(&real, &path);
    let tools = slow_encoder_answer(tmp.path(), &real);
    let started = Instant::now();
    let mut fx = previewed(tmp, path, &tools, &Held::budget());
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "the preparation waited for the encoder's answer"
    );
    assert!(!fx.win.unplanned, "planned on the worker");
    let pids = started_under(fx.tmp.path());
    assert_eq!(pids.len(), 3, "the probe, the frame, the encoder: {pids:?}");
    assert_eq!(still_processes(&pids), 0);
    let handed = Instant::now();
    fx.win.set_preview(false);
    let took = handed.elapsed() + pass(&mut fx);
    assert!(took < FRAME, "the hand-over ran something slow: {took:?}");
    fx.drv.until(&mut fx.win, 10, |w| w.ready());
    assert!(fx.drv.find_text("checking the encoder…").is_none());
    let opened = Media::probe(&fx.path).unwrap();
    assert!(opened.plan.is_some());
    assert_eq!(
        format!("{:?}", fx.win.media.plan),
        format!("{:?}", opened.plan),
        "as for a file opened from the list"
    );
    assert_eq!(fx.win.media.notes, opened.notes);
    fx.drv.key(&mut fx.win, Key::E, Modifiers::NONE);
    assert!(fx.win.job.is_some(), "Export runs");
}

/// 02 §6: an encoder that does not answer within its time on the preview's worker is stopped
/// there — the preview shows all the same — and a window opened without a preview has asked
/// nothing yet. In both, the window asks off its frames: no pass waits for the answer, Save and
/// Export are unavailable and say `checking the encoder…` until it lands, then they work.
#[test]
fn a_slow_encoder_answer_never_holds_a_frame() {
    let Some(real) = tools_or_skip() else { return };
    for through_preview in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("deep.mov");
        deep_clip(&real, &path);
        let tools = slow_encoder_answer(tmp.path(), &real);
        let mut fx = if through_preview {
            // The encoder check has ffprobe's time: here 300 ms.
            let ms = Duration::from_millis;
            let held = Held::with([ms(300), Duration::from_secs(5), ms(300)], Some(600.0));
            let mut fx = previewed(tmp, path, &tools, &held);
            assert!(fx.win.unplanned, "stopped at its time: not planned");
            assert_eq!(fx.drv.pictures().len(), 1, "the preview shows all the same");
            let pids = started_under(fx.tmp.path());
            assert_eq!(still_processes(&pids), 0, "the encoder check was reaped");
            let handed = Instant::now();
            fx.win.set_preview(false);
            let took = handed.elapsed() + pass(&mut fx);
            assert!(
                took < FRAME,
                "the hand-over waited for the encoder: {took:?}"
            );
            fx
        } else {
            // As `open` makes the window: probed, nothing asked of the encoder.
            let media = Media::unplanned(&tools, &Input::path(&path), "deep.mov").unwrap();
            let abs = path.to_str().unwrap().to_string();
            let mut fs = LocalFs::new();
            let at = fs.stat(&abs).unwrap();
            let origin = Origin::new(Box::new(fs), None, None, abs, at);
            let file = MediaFile::new(origin, &tmp.path().join("cache"));
            let mut win = MediaWindow::new(
                media,
                file,
                tmp.path().join("work"),
                Marks::default(),
                Selection::default(),
                Vec::new(),
            );
            win.unplanned = true;
            win.sink = Sink::Null;
            let mut fx = Fx {
                tmp,
                path,
                win,
                drv: Driver::new(700.0, 500.0),
            };
            let took = pass(&mut fx);
            assert!(
                took < FRAME,
                "the first pass waited for the encoder: {took:?}"
            );
            fx
        };
        let which = if through_preview {
            "handed over"
        } else {
            "opened"
        };
        // Wide enough for the status row to say it whole.
        fx.drv.resize(900.0, 520.0);
        fx.drv.idle(&mut fx.win, 2);
        assert!(
            fx.drv.find_text("checking the encoder…").is_some(),
            "{which}: said while it is asked"
        );
        assert!(fx.win.media.plan.is_none(), "{which}");
        assert!(!fx.win.ready(), "{which}: not settled before the answer");
        // Neither Export nor Save starts before the answer.
        fx.drv.key(&mut fx.win, Key::E, Modifiers::NONE);
        fx.drv.click_text(&mut fx.win, "Export  E");
        fx.drv.key(&mut fx.win, Key::S, command(false));
        assert!(fx.win.job.is_none() && fx.win.save.is_none(), "{which}");
        for _ in 0..5 {
            let took = pass(&mut fx);
            assert!(took < FRAME, "{which}: a pass waited: {took:?}");
        }
        fx.drv.until(&mut fx.win, 20, |w| w.media.plan.is_some());
        fx.drv.until(&mut fx.win, 10, |w| w.ready());
        fx.drv.idle(&mut fx.win, 2);
        assert!(
            fx.drv.find_text("checking the encoder…").is_none(),
            "{which}: the answer landed"
        );
        let opened = Media::probe(&fx.path).unwrap();
        assert_eq!(
            format!("{:?}", fx.win.media.plan),
            format!("{:?}", opened.plan),
            "{which}"
        );
        assert_eq!(fx.win.media.notes, opened.notes, "{which}");
        fx.drv.key(&mut fx.win, Key::E, Modifiers::NONE);
        assert!(fx.win.job.is_some(), "{which}: Export is available now");
    }
}
