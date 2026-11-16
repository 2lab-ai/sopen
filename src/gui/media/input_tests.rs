//! The media window driven by real input events (timeline drags, frame drags, keys, the
//! Export button) through its `ui`. Needs ffmpeg (the frames come from it); skips without it,
//! like the other media tests. Playback goes to the null sink — no sound from a test run.

use super::*;
use crate::gui::testkit::Driver;
use crate::remote::{LocalFs, RemoteFs};
use eframe::egui::{Key, Modifiers, PointerButton};

fn tools_or_skip() -> Option<Tools> {
    match ffmpeg::resolve() {
        Ok(t) => Some(t),
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping media input test: {e}");
            None
        }
    }
}

/// 4 s of 320×240 at 25 fps with a silent audio track.
fn clip(tools: &Tools, dst: &Path) {
    let ok = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25:duration=4",
        ])
        .args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo"])
        .args([
            "-t", "4", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac",
        ])
        .arg(dst)
        .status()
        .unwrap()
        .success();
    assert!(ok, "fixture");
}

struct Fixture {
    tmp: tempfile::TempDir,
    win: MediaWindow,
    drv: Driver,
}

fn open(path: PathBuf, tmp: tempfile::TempDir) -> Fixture {
    open_sized(path, tmp, 900.0, 700.0)
}

fn open_sized(path: PathBuf, tmp: tempfile::TempDir, w: f32, h: f32) -> Fixture {
    open_with(path, tmp, w, h, Vec::new())
}

/// `notes` are added to what the probe found (a long "not kept" note for the layout tests).
fn open_with(path: PathBuf, tmp: tempfile::TempDir, w: f32, h: f32, notes: Vec<String>) -> Fixture {
    let mut media = Media::probe(&path).unwrap();
    media.notes.extend(notes);
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let win = MediaWindow {
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
        job: None,
        status: Status::default(),
        outcome: Arc::default(),
        started: false,
        painted: false,
        close_when_done: false,
        close_ok: false,
        leftover: Arc::default(),
        export_hold: None,
        resume: false,
        sink: Sink::Null,
        map: None,
        timeline_rect: None,
        notices: Vec::new(),
        pending_notices: None,
    };
    let mut fx = Fixture {
        tmp,
        win,
        drv: Driver::new(w, h),
    };
    fx.settle();
    fx
}

impl Fixture {
    /// Passes until the frame asked for is on screen and the layout has stopped moving.
    fn settle(&mut self) {
        self.drv.until(&mut self.win, 20, |w| {
            w.tex.is_some() && w.want.is_some() && w.tex_pts == w.want && w.map.is_some()
        });
        let mut stable = 0;
        for _ in 0..30 {
            let before = (self.win.map, self.win.timeline_rect);
            self.drv.idle(&mut self.win, 1);
            let now = (self.win.map, self.win.timeline_rect);
            stable = if now == before { stable + 1 } else { 0 };
            if stable >= 3 {
                return;
            }
        }
        panic!("the layout never settled");
    }

    /// Screen point at `f` (0..1) along the timeline.
    fn timeline(&self, f: f32) -> Pos2 {
        let r = self.win.timeline_rect.expect("timeline drawn");
        Pos2::new(r.left() + f * r.width(), r.center().y)
    }

    fn at(&self, x: f32, y: f32) -> Pos2 {
        self.win.map.expect("frame drawn").to_disp(Pos2::new(x, y))
    }

    fn key(&mut self, key: Key) {
        self.drv.key(&mut self.win, key, Modifiers::NONE);
    }

    fn cuts(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(self.tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains(".cut-"))
            .collect();
        out.sort();
        out
    }
}

fn fixture() -> Option<Fixture> {
    let tools = tools_or_skip()?;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("clip.mp4");
    clip(&tools, &path);
    Some(open(path, tmp))
}

#[test]
fn timeline_drag_moves_the_playhead_and_i_o_set_the_markers() {
    let Some(mut fx) = fixture() else { return };
    let d = fx.win.media.duration;
    let (from, to) = (fx.timeline(0.1), fx.timeline(0.5));
    fx.drv.drag(&mut fx.win, PointerButton::Primary, from, to);
    assert!(
        (fx.win.marks.t - d * 0.5).abs() < 0.05,
        "t = {}",
        fx.win.marks.t
    );
    fx.settle();
    assert_eq!(
        fx.win.tex_pts,
        Some(fx.win.marks.t),
        "the frame at the playhead"
    );
    fx.key(Key::I);
    assert_eq!(fx.win.marks.in_s, Some(fx.win.marks.t));
    let (from, to) = (fx.timeline(0.5), fx.timeline(0.75));
    fx.drv.drag(&mut fx.win, PointerButton::Primary, from, to);
    fx.key(Key::O);
    let out = fx.win.marks.out_s.expect("Out set");
    assert!((out - d * 0.75).abs() < 0.05, "out = {out}");
    assert!(fx.win.marks.in_s.unwrap() < out);
}

#[test]
fn drag_on_the_frame_sets_the_crop_in_source_pixels() {
    let Some(mut fx) = fixture() else { return };
    let (a, b) = (fx.at(20.0, 30.0), fx.at(200.0, 150.0));
    fx.drv.drag(&mut fx.win, PointerButton::Primary, a, b);
    assert_eq!(fx.win.crop(), Some([20, 30, 180, 120]));
    fx.key(Key::Escape);
    assert_eq!(fx.win.crop(), None);
}

/// A 90°-rotated source shows 240×320; the crop is drawn and kept in those pixels.
#[test]
fn crop_on_a_rotated_source_is_in_displayed_pixels() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("plain.mp4");
    clip(&tools, &plain);
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
    let mut fx = open(src, tmp);
    assert_eq!(fx.win.media.video, Some((240, 320)));
    let (a, b) = (fx.at(10.0, 20.0), fx.at(200.0, 300.0));
    fx.drv.drag(&mut fx.win, PointerButton::Primary, a, b);
    assert_eq!(fx.win.crop(), Some([10, 20, 190, 280]));
}

#[test]
fn space_toggles_playback() {
    let Some(mut fx) = fixture() else { return };
    fx.key(Key::Space);
    assert!(fx.win.player.is_some(), "playing");
    let start = fx.win.marks.t;
    fx.drv.until(&mut fx.win, 10, |w| w.marks.t > start + 0.3);
    fx.key(Key::Space);
    assert!(fx.win.player.is_none(), "paused");
    assert!(fx.win.marks.t > start + 0.3);
}

#[test]
fn e_and_the_export_button_export_in_out_and_crop() {
    let Some(mut fx) = fixture() else { return };
    let (a, b) = (fx.timeline(0.0), fx.timeline(0.25));
    fx.drv.drag(&mut fx.win, PointerButton::Primary, a, b);
    fx.key(Key::I);
    let (a, b) = (fx.timeline(0.25), fx.timeline(0.75));
    fx.drv.drag(&mut fx.win, PointerButton::Primary, a, b);
    fx.key(Key::O);
    let (in_s, out_s) = (fx.win.marks.in_s.unwrap(), fx.win.marks.out_s.unwrap());
    fx.settle();
    let (p, q) = (fx.at(41.0, 21.0), fx.at(241.0, 141.0));
    fx.drv.drag(&mut fx.win, PointerButton::Primary, p, q);
    assert_eq!(fx.win.crop(), Some([41, 21, 200, 120]));

    fx.key(Key::E);
    assert!(
        fx.win.job.is_some() || fx.win.status.text.starts_with("exported "),
        "E started an export"
    );
    fx.drv.until(&mut fx.win, 60, |w| w.job.is_none());
    assert!(
        fx.win.status.text.starts_with("exported "),
        "{}",
        fx.win.status.text
    );
    assert!(fx.win.outcome.lock().unwrap().exported);
    let cuts = fx.cuts();
    assert_eq!(cuts.len(), 1, "{cuts:?}");
    let tools = ffmpeg::resolve().unwrap();
    let probe = ffmpeg::probe(&tools, &cuts[0]).unwrap();
    assert_eq!(probe.display, Some((200, 120)), "the crop");
    let d = probe.duration.unwrap();
    assert!(
        (d - (out_s - in_s)).abs() < 0.1,
        "duration {d} vs {}",
        out_s - in_s
    );

    // The Export button does the same, into a new sibling.
    fx.drv.idle(&mut fx.win, 2);
    fx.drv.click_text(&mut fx.win, "Export  E");
    assert!(
        fx.win.job.is_some() || fx.win.outcome.lock().unwrap().exported,
        "the button started an export"
    );
    fx.drv.until(&mut fx.win, 60, |w| w.job.is_none());
    assert_eq!(fx.cuts().len(), 2, "{:?}", fx.cuts());
}

/// The frame must not move under the pointer while a crop is drawn: the info bar keeps its
/// height when the "crop …" readout appears next to a long status (it used to wrap).
#[test]
fn frame_stays_put_while_a_crop_is_drawn_next_to_a_long_status() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("clip.mp4");
    clip(&tools, &path);
    let mut fx = open_sized(path, tmp, 560.0, 520.0);
    let quiet = fx.win.map;
    fx.win.status = Status::info(format!("exported {}", "/a/rather/long/path".repeat(5)));
    fx.settle();
    assert_eq!(fx.win.map, quiet, "the info bar changed height");
    let map = fx.win.map;
    let (a, b) = (fx.at(20.0, 30.0), fx.at(200.0, 150.0));
    fx.drv.drag(&mut fx.win, PointerButton::Primary, a, b);
    assert_eq!(fx.win.map, map, "the frame stayed put during the drag");
    assert_eq!(fx.win.crop(), Some([20, 30, 180, 120]));
}

/// What Export will not keep is never cut off: on a 420 px window the whole note is laid out
/// (its own row, wrapping) and the frame does not move while a crop is drawn.
#[test]
fn export_note_is_shown_whole_on_a_narrow_window_and_the_frame_stays_put() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("clip.mp4");
    clip(&tools, &path);
    let note =
        "not kept: 1 subtitle stream, 2 data streams, 1 attachment stream, cover art".to_string();
    let mut fx = open_with(path, tmp, 420.0, 560.0, vec![note.clone()]);
    let row = format!("Export: {}", fx.win.media.notes.join("; "));
    assert!(fx.drv.find_text(&row).is_some(), "shown whole: {row}");
    let quiet = fx.win.map;
    let (a, b) = (fx.at(20.0, 30.0), fx.at(200.0, 150.0));
    fx.drv.drag(&mut fx.win, PointerButton::Primary, a, b);
    assert_eq!(fx.win.map, quiet, "the frame moved");
    assert_eq!(fx.win.crop(), Some([20, 30, 180, 120]));
}

/// An export result's notes go to the always-visible row; the one-line status only says where
/// the file went.
#[test]
fn export_result_notes_leave_the_status_line() {
    let mut outcome = Outcome::default();
    let notes = vec!["not kept: 1 subtitle stream".to_string()];
    let (status, shown) = report_export(
        Ok(("box:/a.cut-1-2.mp4".to_string(), notes.clone())),
        &mut outcome,
    );
    assert_eq!(status.text, "exported box:/a.cut-1-2.mp4");
    assert_eq!(shown, notes);
    assert!(outcome.exported);
}
