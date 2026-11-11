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
    let media = Media::probe(&path).unwrap();
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
    };
    let mut fx = Fixture {
        tmp,
        win,
        drv: Driver::new(900.0, 700.0),
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
    assert!(fx.win.job.is_some(), "E started an export");
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
    assert!(fx.win.job.is_some(), "the button started an export");
    fx.drv.until(&mut fx.win, 60, |w| w.job.is_none());
    assert_eq!(fx.cuts().len(), 2, "{:?}", fx.cuts());
}
