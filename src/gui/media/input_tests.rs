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
    open_noticed(path, tmp, w, h, notes, Vec::new())
}

/// As `open` does it, with `notices` (where earlier copies' unsaved edits went).
fn open_noticed(
    path: PathBuf,
    tmp: tempfile::TempDir,
    w: f32,
    h: f32,
    notes: Vec<String>,
    notices: Vec<String>,
) -> Fixture {
    let mut media = Media::probe(&path).unwrap();
    media.notes.extend(notes);
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
        notices,
    );
    win.sink = Sink::Null;
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
            w.tex.is_some() && w.frame_ready() && w.map.is_some()
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
        fx.win.tex_pts(),
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

/// F-click-slop: a handle on the picture's edge takes a press on its outer half, painted in the
/// frame's margin outside the picture. With the crop drawn past the right edge, a press two
/// points right of the edge, on the E handle, and a drag 40 points left narrow the crop (egui's
/// 5-point reach used to carry that press to a response over the picture alone).
#[test]
fn a_press_on_a_handles_outer_half_outside_the_picture_resizes_the_crop() {
    let Some(mut fx) = fixture() else { return };
    let (a, b) = (
        fx.at(100.0, 60.0),
        fx.at(320.0, 180.0) + Vec2::new(30.0, 0.0),
    );
    fx.drv.drag(&mut fx.win, PointerButton::Primary, a, b);
    assert_eq!(
        fx.win.crop(),
        Some([100, 60, 220, 120]),
        "to the right edge"
    );
    let from = fx.at(320.0, 120.0) + Vec2::new(2.0, 0.0);
    let disp = fx.win.map.expect("frame drawn").disp;
    assert!(
        !disp.contains(from),
        "outside the picture: {from:?} {disp:?}"
    );
    let to = from - Vec2::new(40.0, 0.0);
    fx.drv.drag(&mut fx.win, PointerButton::Primary, from, to);
    let [x, y, w, h] = fx.win.crop().expect("the crop kept");
    assert_eq!(
        (x, y, h),
        (100, 60, 120),
        "the E handle moves the right edge alone"
    );
    assert!(w < 220, "narrower: {w}");
}

/// The frame's black margin beside the picture is the crop's outside, as the image window's
/// (both take the frame's whole inside, F-click-slop): with a crop up, a drag from the margin
/// left of the picture draws a new crop from the edge, the press clamped onto the picture; a
/// click on the margin right of it, past the handles' reach, clears the crop.
#[test]
fn the_margin_beside_the_picture_starts_a_crop_from_its_edge_and_a_click_there_clears_it() {
    let Some(mut fx) = fixture() else { return };
    let (a, b) = (fx.at(100.0, 60.0), fx.at(200.0, 150.0));
    fx.drv.drag(&mut fx.win, PointerButton::Primary, a, b);
    assert_eq!(fx.win.crop(), Some([100, 60, 100, 90]));
    let disp = fx.win.map.expect("frame drawn").disp;
    // Three points into the frame's four-point margin (`panel.shrink(4.0)`).
    let left = Pos2::new(disp.left() - 3.0, fx.at(0.0, 100.0).y);
    let to = fx.at(80.0, 160.0);
    fx.drv.drag(&mut fx.win, PointerButton::Primary, left, to);
    assert_eq!(fx.win.crop(), Some([0, 100, 80, 60]), "from the left edge");
    let right = Pos2::new(disp.right() + 3.0, fx.at(0.0, 120.0).y);
    fx.drv.click(&mut fx.win, right);
    assert_eq!(fx.win.crop(), None, "a click on the margin clears it");
}

/// A 90°-rotated source shows 240×320; the crop is drawn and kept in those pixels.
#[test]
fn crop_on_a_rotated_source_is_in_displayed_pixels() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("plain.mp4");
    clip(&tools, &plain);
    let src = tmp.path().join("phone.mp4");
    // Whether the fixture is rotated is ffprobe's word, not the app's: from here on the app's
    // probe must agree.
    if !crate::ffmpeg::fixtures::rotated_copy(&tools, &plain, &src) {
        return; // this ffmpeg cannot tag a fixture as rotated (said by `rotated_copy`)
    }
    let media = Media::probe(&src).unwrap();
    assert_eq!(
        media.rotation % 180,
        90,
        "the app's probe sees the rotation"
    );
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
    fx.drv
        .drag_watching(&mut fx.win, PointerButton::Primary, a, b, |w| {
            assert_eq!(w.map, map, "the frame moved under the held pointer");
        });
    assert_eq!(fx.win.map, map, "the frame stayed put through the release");
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
    let row = format!("Save / Export: {}", fx.win.media.notes.join("; "));
    assert!(fx.drv.find_text(&row).is_some(), "shown whole: {row}");
    let quiet = fx.win.map;
    let (a, b) = (fx.at(20.0, 30.0), fx.at(200.0, 150.0));
    fx.drv
        .drag_watching(&mut fx.win, PointerButton::Primary, a, b, |w| {
            assert_eq!(w.map, quiet, "the frame moved under the held pointer");
        });
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

/// An export never moves the frame — not when it starts, as it progresses, or when it reports —
/// at every width: its spinner (painted) and "exporting NN %" (whole) share the clock line's
/// row, and the clock line stays one line (a progress row of its own used to come and go with
/// the export, then sat empty at the bottom while none ran).
#[test]
fn a_running_export_never_moves_the_frame() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("clip.mp4");
    clip(&tools, &path);
    let (mut moved, mut unshown) = (Vec::new(), Vec::new());
    for w in [420, 560, 640, 760, 900, 1100, 1280] {
        let copy = tempfile::tempdir().unwrap();
        let mut fx = open_sized(path.clone(), copy, w as f32, 700.0);
        let idle = fx.win.map;
        let line = transport_text(&fx.win.marks, fx.win.media.duration);
        let progress = Arc::new(AtomicU32::new(0));
        let (tx, rx) = mpsc::channel();
        fx.win.job = Some(ExportJob {
            progress: progress.clone(),
            rx,
            stop: Arc::default(),
        });
        for per_mille in [0, 300, 1000] {
            progress.store(per_mille, SeqCst);
            fx.settle();
            if fx.win.map != idle {
                moved.push((w, format!("at {} %", per_mille / 10)));
            }
            let shown = format!("exporting {} %", per_mille / 10);
            // Its spinner: one arc painted just before the text, on its line (within a spacing
            // and a spinner's width: where on its circle the arc is depends on the time).
            let spins = fx.drv.find_text(&shown).is_some_and(|at| {
                let arcs = fx.drv.strokes().into_iter().filter(|r| {
                    r.right() <= at.left()
                        && r.left() >= at.left() - 30.0
                        && r.max.y >= at.min.y
                        && r.min.y <= at.max.y
                });
                arcs.count() == 1
            });
            if !spins || fx.drv.rows_of(&line) != Some(1) {
                unshown.push((w, shown));
            }
        }
        tx.send(Ok(("box:/clip.cut-0-4.mp4".to_string(), Vec::new())))
            .unwrap();
        fx.settle();
        assert!(fx.win.job.is_none(), "the export reported");
        if fx.win.map != idle {
            moved.push((w, "after it reported".to_string()));
        }
    }
    assert!(moved.is_empty(), "the frame moved: {moved:?}");
    assert!(
        unshown.is_empty(),
        "no spinner, the progress not whole, or the clock line not one line: {unshown:?}"
    );
}

/// A running Export holds the window as a Save does (08 §5, D2): a switch of views waits for
/// it, so its publish never meets the save session given back.
#[test]
fn a_running_export_holds_the_window_as_a_save() {
    let Some(mut fx) = fixture() else { return };
    use crate::gui::Window as _;
    assert_eq!(fx.win.hold(), crate::views::Hold::Clean);
    let (_tx, rx) = mpsc::channel();
    fx.win.job = Some(ExportJob {
        progress: Arc::new(AtomicU32::new(0)),
        rx,
        stop: Arc::default(),
    });
    assert_eq!(fx.win.hold(), crate::views::Hold::Saving, "while it runs");
    fx.win.job = None;
    assert_eq!(fx.win.hold(), crate::views::Hold::Clean);
}

/// The transport's clock line is never broken across rows: at every width it is one line drawn
/// inside its clip and the window, cut with "…" (the whole line on hover) where the window is
/// too narrow for it. It used to share
/// the buttons' wrapping row and break after "In …" at 640 px, the default width.
#[test]
fn the_transport_line_never_wraps_at_any_width() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("clip.mp4");
    clip(&tools, &path);
    let mut fx = open_sized(path, tmp, 1400.0, 560.0);
    let line = transport_text(&fx.win.marks, fx.win.media.duration);
    let (mut broken, mut whole) = (Vec::new(), Vec::new());
    for w in 420..=1400 {
        fx.drv.resize(w as f32, 560.0);
        fx.settle();
        // `rows_of` sees only a line drawn inside its clip and the window.
        if fx.drv.rows_of(&line) != Some(1) {
            broken.push(w);
        }
        if fx.drv.find_text(&line).is_some() {
            whole.push(w);
        }
    }
    assert!(
        broken.is_empty(),
        "the transport line is not one row in view at widths {broken:?}"
    );
    // Cut only where it must be: whole at the default width.
    assert!(whole.contains(&640), "whole only at widths {whole:?}");
}

/// An export that finishes while the button is down — before the pointer has moved far enough
/// to count as a drag, and during the drag — changes nothing under the pointer: the crop lands
/// where it was aimed, and the result's row appears after the release.
#[test]
fn an_export_finishing_under_a_held_button_waits_for_the_release() {
    let Some(mut fx) = fixture() else { return };
    let map = fx.win.map;
    let note = "not kept: 1 subtitle stream".to_string();
    let (tx, rx) = mpsc::channel();
    fx.win.job = Some(ExportJob {
        progress: Arc::default(),
        rx,
        stop: Arc::default(),
    });
    fx.settle();
    assert_eq!(fx.win.map, map);
    let (a, b) = (fx.at(20.0, 30.0), fx.at(200.0, 150.0));
    fx.drv.move_to(&mut fx.win, a);
    fx.drv.press(&mut fx.win, PointerButton::Primary);
    tx.send(Ok((
        "box:/clip.cut-0-4.mp4".to_string(),
        vec![note.clone()],
    )))
    .unwrap();
    for pass in 0..5 {
        fx.drv.idle(&mut fx.win, 1);
        assert_eq!(
            fx.win.map, map,
            "the frame moved under the pressed pointer ({pass})"
        );
    }
    assert!(
        fx.win.job.is_none(),
        "the result was taken while the button was down"
    );
    for k in 1..=8 {
        fx.drv.move_to(&mut fx.win, a + (b - a) * (k as f32 / 8.0));
        assert_eq!(fx.win.map, map, "the frame moved under the held pointer");
    }
    fx.drv.release(&mut fx.win, PointerButton::Primary);
    fx.drv.idle(&mut fx.win, 1);
    assert_eq!(fx.win.crop(), Some([20, 30, 180, 120]));
    fx.settle();
    assert!(
        fx.drv.find_text(&note).is_some(),
        "shown whole after the release"
    );
}

/// Where an earlier copy's unsaved edits went is read whole on open (640 px) and stays after an
/// export reports; the one-line status stays free for short news.
#[test]
fn the_kept_copy_notice_is_shown_whole_on_open_and_stays() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("clip.mp4");
    clip(&tools, &path);
    let notice = "the previous copy had unsaved edits; kept them at <cache>/example-host/srv/\
                  shared/archive/recordings/2026/november/clip.mp4.conflict-20261116T101010Z"
        .to_string();
    let mut fx = open_noticed(path, tmp, 640.0, 560.0, Vec::new(), vec![notice.clone()]);
    assert!(fx.drv.find_text(&notice).is_some(), "shown whole on open");
    assert!(fx.win.status.text.is_empty(), "{}", fx.win.status.text);
    let (tx, rx) = mpsc::channel();
    fx.win.job = Some(ExportJob {
        progress: Arc::default(),
        rx,
        stop: Arc::default(),
    });
    tx.send(Ok(("box:/clip.cut-0-4.mp4".to_string(), Vec::new())))
        .unwrap();
    fx.settle();
    assert!(
        fx.win.status.text.starts_with("exported "),
        "{}",
        fx.win.status.text
    );
    assert!(
        fx.drv.find_text(&notice).is_some(),
        "still shown after the export"
    );
}

/// Closing during an export on a narrow window: what a second close does is read whole.
#[test]
fn the_close_during_export_hint_is_shown_whole_on_a_narrow_window() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("clip.mp4");
    clip(&tools, &path);
    let mut fx = open_sized(path, tmp, 420.0, 560.0);
    let (_tx, rx) = mpsc::channel();
    fx.win.job = Some(ExportJob {
        progress: Arc::default(),
        rx,
        stop: Arc::default(),
    });
    fx.settle();
    use crate::gui::Window;
    assert!(!fx.win.close_requested());
    fx.settle();
    assert!(
        fx.drv.find_text(CLOSE_AGAIN).is_some(),
        "shown whole: {CLOSE_AGAIN}"
    );
}

/// On its own the window has PC Shell's F-key row, with F3 Exit, and F3 goes through the real
/// close: during an export the first close is refused (the rows say a second one closes), the
/// second one closes; without an export it closes at once.
#[test]
fn f3_closes_the_window() {
    let Some(mut fx) = fixture() else { return };
    fx.drv.as_shell();
    assert!(fx.drv.find_text("Exit   ").is_some(), "the F-key row");
    // An export that is still running (its worker never answers).
    let (_tx, rx) = mpsc::channel();
    fx.win.job = Some(ExportJob {
        progress: Arc::default(),
        rx,
        stop: Arc::default(),
    });
    fx.key(Key::F3);
    assert_eq!(
        fx.drv.refused_closes, 1,
        "the first close waits for the export"
    );
    assert!(!fx.drv.window_closed);
    assert!(fx.drv.find_text(CLOSE_AGAIN).is_some(), "said in the rows");
    fx.key(Key::F3);
    assert!(fx.drv.window_closed, "the second one closes");
    fx.win.job = None;
    let Some(mut clean) = fixture() else { return };
    clean.drv.as_shell();
    clean.key(Key::F3);
    assert_eq!(
        (clean.drv.refused_closes, clean.drv.window_closed),
        (0, true)
    );
}
