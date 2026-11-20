//! The media window's edits (spec §7, message 13) driven by real input events through its `ui`:
//! a drag selects, Cmd/Ctrl+C / X / V, Delete, Cmd/Ctrl+Z / Shift+Cmd/Ctrl+Z, `M`, Save, the
//! unsaved-changes prompt, the F-key row — and no ffmpeg run while editing (counted per file).
//! Needs ffmpeg; playback goes to the null sink.

use super::*;
use crate::ffmpeg::spawns;
use crate::gui::testkit::{command, key_up, Driver};
use crate::remote::{LocalFs, RemoteFs};
use eframe::egui::{Event, Key, Modifiers, PointerButton};

fn tools_or_skip() -> Option<Tools> {
    match ffmpeg::resolve() {
        Ok(t) => Some(t),
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping media edit test: {e}");
            None
        }
    }
}

fn ff(tools: &Tools, args: &[&str]) {
    let ok = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "fixture {args:?}");
}

/// Four seconds at 44.1 kHz: left 440, 880, 1320, 1760 Hz a second each; right 660 Hz (stereo),
/// or the left alone (mono).
fn tones(tools: &Tools, dst: &Path, stereo: bool) {
    let left = "0.5*sin(2*PI*(440+440*floor(t))*t)";
    let expr = if stereo {
        format!("aevalsrc='{left}|0.5*sin(2*PI*660*t)':s=44100:d=4")
    } else {
        format!("aevalsrc='{left}':s=44100:d=4")
    };
    ff(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            &expr,
            "-c:a",
            "pcm_s16le",
            dst.to_str().unwrap(),
        ],
    );
}

/// 100 frames at 25 fps, frame N's luma 16 + 2N, with a tone track.
fn counter(tools: &Tools, dst: &Path) {
    ff(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "nullsrc=s=64x64:r=25:d=4,geq=lum='16+2*N':cb=128:cr=128,format=yuv420p",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=4",
            "-c:v",
            "libx264",
            "-crf",
            "10",
            "-g",
            "50",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
            dst.to_str().unwrap(),
        ],
    );
}

struct Fx {
    tmp: tempfile::TempDir,
    path: PathBuf,
    win: MediaWindow,
    drv: Driver,
}

fn open_file(tmp: tempfile::TempDir, path: PathBuf) -> Fx {
    let media = Media::probe(&path).unwrap();
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
    win.sink = Sink::Null;
    let mut fx = Fx {
        tmp,
        path,
        win,
        drv: Driver::new(900.0, 520.0),
    };
    fx.settle();
    fx
}

fn audio(stereo: bool) -> Option<Fx> {
    let tools = tools_or_skip()?;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(if stereo { "st.wav" } else { "mono.wav" });
    tones(&tools, &path, stereo);
    Some(open_file(tmp, path))
}

fn video() -> Option<Fx> {
    let tools = tools_or_skip()?;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("count.mp4");
    counter(&tools, &path);
    Some(open_file(tmp, path))
}

impl Fx {
    /// Passes until the waveform (audio) or the frame at the playhead (video) is on screen
    /// and nothing runs.
    fn settle(&mut self) {
        self.drv.until(&mut self.win, 30, |w| {
            w.timeline_rect.is_some()
                && w.save.is_none()
                && w.job.is_none()
                && if w.audio_only() {
                    w.wave
                        .as_ref()
                        .is_some_and(|s| s.lock().unwrap().as_ref().is_some_and(|p| p.complete))
                        && w.painted
                } else {
                    w.frame_ready()
                }
        });
        self.drv.idle(&mut self.win, 2);
    }

    fn at(&self, f: f32) -> Pos2 {
        let r = self.win.timeline_rect.expect("timeline drawn");
        Pos2::new(r.left() + f * r.width(), r.center().y)
    }

    fn drag(&mut self, from: f32, to: f32) {
        let (a, b) = (self.at(from), self.at(to));
        self.drv.drag(&mut self.win, PointerButton::Primary, a, b);
    }

    fn click(&mut self, f: f32) {
        let p = self.at(f);
        self.drv.click(&mut self.win, p);
    }

    fn key(&mut self, key: Key, mods: Modifiers) {
        self.drv.key(&mut self.win, key, mods);
    }

    /// Cmd/Ctrl+V with nothing on the system clipboard: egui-winit sends no event for the
    /// press, only V's key-up (Cmd/Ctrl still held).
    fn paste(&mut self) {
        self.drv.shortcut(&mut self.win, Key::V, None, 1);
    }

    /// The clips in seconds (for readable asserts).
    fn clips(&self) -> Vec<(f64, f64)> {
        let tl = &self.win.timeline;
        tl.clips()
            .iter()
            .map(|c| (round(tl.secs(c.start)), round(tl.secs(c.end))))
            .collect()
    }

    fn spawned(&self) -> spawns::Counts {
        spawns::of(&self.path)
    }
}

fn round(s: f64) -> f64 {
    (s * 100.0).round() / 100.0
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.03
}

/// A drag on the waveform selects (the playhead follows the pointer); a click moves the
/// playhead and clears the selection; a drag that starts on the playhead moves it and leaves
/// the selection alone; `I` / `O` set the selection's edges at the playhead.
#[test]
fn a_drag_selects_a_click_moves_the_playhead_and_i_o_set_the_edges() {
    let Some(mut fx) = audio(true) else { return };
    fx.drag(0.25, 0.5);
    let (i, o) = (fx.win.marks.in_s.unwrap(), fx.win.marks.out_s.unwrap());
    assert!(near(i, 1.0) && near(o, 2.0), "selected {i}..{o}");
    assert!(near(fx.win.marks.t, 2.0), "playhead {}", fx.win.marks.t);
    // A drag from where the last one ended selects again (no grab of the playhead).
    fx.drag(0.5, 0.625);
    let (i, o) = (fx.win.marks.in_s.unwrap(), fx.win.marks.out_s.unwrap());
    assert!(near(i, 2.0) && near(o, 2.5), "selected {i}..{o}");
    assert!(near(fx.win.marks.t, 2.5), "{}", fx.win.marks.t);
    // Backwards: the same kind of stretch.
    fx.drag(0.5, 0.25);
    let (i, o) = (fx.win.marks.in_s.unwrap(), fx.win.marks.out_s.unwrap());
    assert!(near(i, 1.0) && near(o, 2.0), "selected {i}..{o}");
    assert!(near(fx.win.marks.t, 1.0));
    // The arrows move the playhead (Alt: 10 ms); I sets the selection's start there.
    fx.key(Key::ArrowRight, Modifiers::NONE);
    assert!(near(fx.win.marks.t, 2.0));
    fx.key(Key::ArrowLeft, Modifiers::ALT);
    assert!(
        (fx.win.marks.t - 1.99).abs() < 1e-6,
        "Alt+← is 10 ms: {}",
        fx.win.marks.t
    );
    fx.key(Key::I, Modifiers::NONE);
    assert_eq!(fx.win.marks.in_s, Some(fx.win.marks.t));
    assert_eq!(fx.win.marks.out_s, Some(o));
    fx.click(0.75);
    assert!(!fx.win.marks.selected(), "a click clears the selection");
    assert!(near(fx.win.marks.t, 3.0));
    fx.key(Key::O, Modifiers::NONE);
    assert_eq!(fx.win.marks.range(fx.win.length()).0, 0.0);
    assert!(near(fx.win.marks.out_s.unwrap(), 3.0));
    // Esc clears it.
    fx.key(Key::Escape, Modifiers::NONE);
    assert!(!fx.win.marks.selected());
}

/// Cut, paste (with an empty system clipboard), copy, paste over a selection, Delete, undo and
/// redo — each by its real event — edit the list exactly; and none of it runs ffmpeg or
/// ffprobe on the file.
#[test]
fn edits_by_real_events_change_only_the_list_and_run_no_ffmpeg() {
    let Some(mut fx) = audio(true) else { return };
    let before = fx.spawned();
    // Select second 1 and cut it: the gap closes, the playhead is at the cut.
    fx.drag(0.25, 0.5);
    fx.drv.cut(&mut fx.win);
    assert_eq!(fx.clips(), [(0.0, 1.0), (2.0, 4.0)]);
    assert!(near(fx.win.length(), 3.0));
    assert!(near(fx.win.marks.t, 1.0));
    assert!(!fx.win.marks.selected());
    // To the start and paste: it goes in at the playhead.
    fx.click(0.0);
    fx.paste();
    assert_eq!(fx.clips(), [(1.0, 2.0), (0.0, 1.0), (2.0, 4.0)]);
    assert!(near(fx.win.length(), 4.0));
    assert!(near(fx.win.marks.t, 1.0), "after what was pasted");
    // Copy the last second, paste it over the first.
    fx.drag(0.75, 1.0);
    fx.drv.copy(&mut fx.win);
    assert_eq!(fx.clips().len(), 3, "copy changes nothing");
    fx.drag(0.0, 0.25);
    fx.paste();
    assert_eq!(fx.clips(), [(3.0, 4.0), (0.0, 1.0), (2.0, 4.0)]);
    // Delete: gone, not on the clipboard (a paste still pastes the copied second).
    fx.drag(0.25, 0.5);
    fx.key(Key::Delete, Modifiers::NONE);
    assert_eq!(fx.clips(), [(3.0, 4.0), (2.0, 4.0)]);
    fx.click(0.0);
    fx.paste();
    assert_eq!(fx.clips(), [(3.0, 4.0), (3.0, 4.0), (2.0, 4.0)]);
    // Undo three times, redo once.
    for _ in 0..3 {
        fx.key(Key::Z, command(false));
    }
    assert_eq!(fx.clips(), [(1.0, 2.0), (0.0, 1.0), (2.0, 4.0)]);
    fx.key(Key::Z, command(true));
    assert_eq!(fx.clips(), [(3.0, 4.0), (0.0, 1.0), (2.0, 4.0)]);
    assert_eq!(
        fx.spawned(),
        before,
        "editing started ffmpeg or ffprobe on the file"
    );
    assert!(fx.win.unsaved());
}

/// Cmd/Ctrl+V whose press sent nothing works however the keys come up — V before Cmd/Ctrl, or
/// Cmd/Ctrl first — and once per press; with text on the system clipboard (egui-winit sends
/// `Paste`) it pastes once too, not again at the key-up; a plain V does nothing.
#[test]
fn paste_does_not_depend_on_the_system_clipboard_or_the_release_order() {
    let Some(mut fx) = audio(false) else { return };
    fx.drag(0.0, 0.25);
    fx.drv.copy(&mut fx.win);
    fx.key(Key::Escape, Modifiers::NONE);
    fx.click(1.0);
    let len = |fx: &Fx| round(fx.win.length());
    assert_eq!(len(&fx), 4.0);
    // V up with Cmd held.
    fx.paste();
    assert_eq!(len(&fx), 5.0);
    // Cmd up first, then V up.
    fx.drv.frame(&mut fx.win, vec![], command(false));
    fx.drv.frame(&mut fx.win, vec![], Modifiers::NONE);
    let v_up = Event::Key {
        key: Key::V,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    fx.drv.frame(&mut fx.win, vec![v_up], Modifiers::NONE);
    fx.drv.idle(&mut fx.win, 1);
    assert_eq!(len(&fx), 6.0);
    // Text on the system clipboard: Paste, repeated while held, then V up — one paste.
    let paste = Event::Paste("whatever".into());
    fx.drv.shortcut(&mut fx.win, Key::V, Some(paste), 3);
    assert_eq!(len(&fx), 7.0);
    // A plain V: a key-down and a key-up, no paste.
    fx.key(Key::V, Modifiers::NONE);
    assert_eq!(len(&fx), 7.0);
    let _ = key_up(Key::V);
}

/// A stereo file shows two lanes, L and R; a mono file one. Mono → Stereo (`M`) shows two
/// lanes of the one channel and is an edit (unsaved, one undo step).
#[test]
fn stereo_shows_two_lanes_and_mono_to_stereo_is_an_undoable_edit() {
    let Some(st) = audio(true) else { return };
    assert!(st.drv.find_text("L").is_some() && st.drv.find_text("R").is_some());
    assert!(
        st.drv.find_text("Mono → Stereo  M").is_none(),
        "only for mono"
    );
    let Some(mut mono) = audio(false) else { return };
    assert!(mono.drv.find_text("L").is_none(), "one lane");
    let before = mono.spawned();
    mono.key(Key::M, Modifiers::NONE);
    assert!(mono.win.timeline.stereo() && mono.win.unsaved());
    mono.drv.idle(&mut mono.win, 2);
    assert!(mono.drv.find_text("L").is_some() && mono.drv.find_text("R").is_some());
    assert!(mono
        .drv
        .text_starting_with("unsaved: 1 piece, stereo")
        .is_some());
    mono.key(Key::Z, command(false));
    assert!(!mono.win.timeline.stereo() && !mono.win.unsaved());
    // The button does the same.
    mono.drv.click_text(&mut mono.win, "Mono → Stereo  M");
    assert!(mono.win.timeline.stereo());
    assert_eq!(mono.spawned(), before, "no ffmpeg for Mono → Stereo");
}

/// Edits make the window unsaved: F3 (the real close) asks first; Cancel keeps the window and
/// its edits; under the prompt no key edits; Discard closes it. Without an edit F3 closes at
/// once.
#[test]
fn closing_with_edits_asks_first() {
    let Some(mut fx) = audio(true) else { return };
    fx.drv.as_shell();
    assert!(fx.drv.find_text("Exit   ").is_some() && fx.drv.find_text("Save   ").is_some());
    assert_eq!(
        crate::gui::Window::keys(&fx.win),
        crate::gui::chrome::MEDIA_KEYS
    );
    fx.drag(0.25, 0.5);
    fx.drv.cut(&mut fx.win);
    fx.key(Key::F3, Modifiers::NONE);
    assert_eq!(fx.drv.refused_closes, 1, "refused");
    assert!(fx.drv.find_text("Unsaved changes").is_some(), "the prompt");
    // Under the prompt: no edit, no save.
    let clips = fx.clips();
    fx.key(Key::Z, command(false));
    fx.key(Key::F5, Modifiers::NONE);
    assert_eq!(fx.clips(), clips);
    assert!(fx.win.save.is_none());
    fx.drv.click_text(&mut fx.win, "Cancel");
    assert!(!fx.drv.window_closed && fx.win.unsaved());
    fx.key(Key::F3, Modifiers::NONE);
    fx.drv.click_text(&mut fx.win, "Discard");
    fx.drv.idle(&mut fx.win, 1);
    assert!(fx.drv.window_closed, "Discard closes it");
    let Some(mut clean) = audio(true) else { return };
    clean.drv.as_shell();
    clean.key(Key::F3, Modifiers::NONE);
    assert_eq!(
        (clean.drv.refused_closes, clean.drv.window_closed),
        (0, true)
    );
}

fn wait_save(fx: &mut Fx) {
    fx.drv.until(&mut fx.win, 60, |w| w.save.is_none());
    fx.settle();
}

/// The left channel's tones of a file, in order (from the cut tests' reader).
fn order(file: &Path) -> Vec<u32> {
    let tools = ffmpeg::resolve().unwrap();
    let out = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(file)
        .args(["-f", "f32le", "-ac", "1", "-"])
        .output()
        .unwrap();
    let x: Vec<f32> = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    // Zero crossings per 50 ms window → the nearest of the fixture's tones, runs merged.
    let w = 44_100 / 20;
    let mut tones: Vec<u32> = Vec::new();
    for chunk in x.chunks(w).filter(|c| c.len() == w) {
        let z = chunk
            .windows(2)
            .filter(|p| (p[0] < 0.0) != (p[1] < 0.0))
            .count();
        let hz = z as f64 * 10.0;
        let near = [440u32, 880, 1320, 1760]
            .into_iter()
            .min_by(|a, b| {
                (f64::from(*a) - hz)
                    .abs()
                    .total_cmp(&(f64::from(*b) - hz).abs())
            })
            .unwrap();
        if tones.last() != Some(&near) {
            tones.push(near);
        }
    }
    tones
}

/// Cmd/Ctrl+S renders the edit and writes it back over the file — after a backup of the
/// original — and the window plays the saved file, unedited: the file holds the new order,
/// the backup the old bytes; F5 is the same key. Save is the only thing that runs ffmpeg.
#[test]
fn save_renders_writes_back_after_a_backup_and_reloads() {
    let Some(mut fx) = audio(true) else { return };
    let original = std::fs::read(&fx.path).unwrap();
    let before = fx.spawned();
    fx.drag(0.25, 0.5);
    fx.drv.cut(&mut fx.win);
    fx.click(0.0);
    fx.paste();
    assert_eq!(fx.spawned(), before);
    fx.key(Key::S, command(false));
    assert!(fx.win.save.is_some(), "⌘S started a save");
    wait_save(&mut fx);
    assert!(
        fx.win.status.text.starts_with("saved "),
        "{}",
        fx.win.status.text
    );
    assert!(fx.win.outcome.lock().unwrap().saved);
    assert!(fx.spawned().render >= 1, "Save rendered with ffmpeg");
    assert_eq!(order(&fx.path), [880, 440, 1320, 1760], "the saved order");
    let backups: Vec<PathBuf> = walk(&fx.tmp.path().join("cache").join(".backup"));
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
    // Reloaded: the saved file, unedited.
    assert!(!fx.win.unsaved());
    assert!(near(fx.win.length(), 4.0));
    assert!(
        fx.win.clipboard.is_empty(),
        "the clipboard held the old file's ranges"
    );
    // F5 saves the next edit; the backup stays the first one.
    fx.drag(0.0, 0.25);
    fx.key(Key::Delete, Modifiers::NONE);
    fx.key(Key::F5, Modifiers::NONE);
    wait_save(&mut fx);
    assert_eq!(order(&fx.path), [440, 1320, 1760]);
    assert_eq!(walk(&fx.tmp.path().join("cache").join(".backup")).len(), 1);
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// A file changed since it was opened is never overwritten: Save is refused before anything is
/// backed up or rendered (a render can take minutes), the edits stay.
#[test]
fn save_refuses_a_file_changed_since_it_was_opened() {
    let Some(mut fx) = audio(true) else { return };
    fx.drag(0.25, 0.5);
    fx.key(Key::Delete, Modifiers::NONE);
    let other = b"someone else's bytes".to_vec();
    std::fs::write(&fx.path, &other).unwrap();
    fx.key(Key::S, command(false));
    wait_save_only(&mut fx);
    assert_eq!(fx.win.status.text, "not saved: the file changed");
    assert_eq!(std::fs::read(&fx.path).unwrap(), other, "untouched");
    assert!(fx.win.unsaved(), "the edits stay");
    assert_eq!(
        fx.spawned().render,
        0,
        "refused before anything was rendered"
    );
    assert!(
        walk(&fx.tmp.path().join("cache").join(".backup")).is_empty(),
        "nor backed up"
    );
    assert!(fx.win.outcome.lock().unwrap().refused.is_some());
}

fn wait_save_only(fx: &mut Fx) {
    fx.drv.until(&mut fx.win, 60, |w| w.save.is_none());
    fx.drv.idle(&mut fx.win, 2);
}

/// Video: the same edits on the timeline; the paused frame after an edit is the one the
/// timeline now shows there — from the frames already decoded, so the edits run nothing
/// (no render, no probe, no decode) — and playing an edited timeline shows its frames in the
/// timeline's order. Prints the edit-to-preview latency (paused: kept frame, and one decoded)
/// and the boundary gap while playing.
#[test]
fn video_edits_preview_at_once_without_running_ffmpeg() {
    let Some(mut fx) = video() else { return };
    let shown = |fx: &Fx| fx.win.frame_at(fx.win.marks.t);
    // Select frames [25, 50) by dragging: the frames at its edges are decoded on the way.
    fx.drag(0.25, 0.5);
    fx.settle();
    let before = fx.spawned();
    let t0 = Instant::now();
    fx.drv.cut(&mut fx.win);
    let passes = (0..)
        .find(|_| {
            let ready = fx.win.frame_ready();
            if !ready {
                fx.drv.idle(&mut fx.win, 1);
            }
            ready
        })
        .unwrap();
    let kept_ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(fx.clips(), [(0.0, 1.0), (2.0, 4.0)]);
    assert_eq!(shown(&fx), 50, "the frame now at the playhead");
    // Paste it back where it was: the file as it is again (not unsaved).
    fx.paste();
    fx.settle();
    assert!(!fx.win.unsaved());
    let after = fx.spawned();
    assert_eq!(
        after, before,
        "the edits ran ffmpeg: {before:?} → {after:?}"
    );
    // A frame not decoded yet: the edit asks for it once (a preview decode, never a render).
    fx.drag(0.6, 0.7);
    fx.settle();
    fx.win.scrub.as_ref().unwrap().cache.lock().unwrap().clear();
    let before = fx.spawned();
    let t1 = Instant::now();
    fx.key(Key::Delete, Modifiers::NONE);
    fx.drv
        .until(&mut fx.win, 20, |w| w.frame_ready() && w.tex_at.is_some());
    let decoded_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let after = fx.spawned();
    assert_eq!(
        (after.render, after.probe),
        (before.render, before.probe),
        "no render, no probe"
    );
    assert_eq!(after.preview - before.preview, 1, "one frame decoded");
    eprintln!(
        "edit-to-preview: kept frame {passes} pass(es) {kept_ms:.1} ms; decoded frame \
         {decoded_ms:.1} ms"
    );
}

/// Playing an edited timeline shows its frames in its order, across every boundary, and the
/// clock is the timeline's; prints how long each decoder waited at each boundary and how late
/// the frames there were.
#[test]
fn playback_follows_the_edit_list_across_boundaries() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("count.mp4");
    counter(&tools, &path);
    // Timeline: frames [50, 75) then [0, 25) then [75, 100).
    let pieces = [(50, 75), (0, 25), (75, 100)].map(|(a, b): (i64, i64)| Piece {
        start: a as f64 / 25.0,
        len: (b - a) as f64 / 25.0,
    });
    let src = Source {
        input: Input::path(&path),
        duration: 3.0,
        video: Some(((64, 64), 25.0)),
        audio: true,
        pieces: pieces.to_vec(),
        stereo: false,
    };
    let mut player = Player::start(&tools, &src, 0.0, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
    let begin = Instant::now();
    let mut seen: Vec<(f64, i64, f64)> = Vec::new();
    loop {
        let p = player.pump(Instant::now());
        if let Some(f) = p.frame {
            // Gray: R = (Y - 16) · 255 / 219, Y = 16 + 2N.
            let n = (f64::from(f.rgba[0]) * 219.0 / 255.0 / 2.0).round() as i64;
            seen.push((f.pts, n, player.clock(Instant::now()) - f.pts));
        }
        if p.ended || begin.elapsed().as_secs() > 15 {
            break;
        }
        std::thread::sleep(p.wake_in.min(std::time::Duration::from_millis(5)));
    }
    let numbers: Vec<i64> = seen.iter().map(|s| s.1).collect();
    let want: Vec<i64> = (50..75).chain(0..25).chain(75..100).collect();
    // Frames may be dropped when the machine is busy, never shown out of order.
    let mut pos = 0;
    for n in &numbers {
        let at = want[pos..].iter().position(|w| w == n);
        assert!(at.is_some(), "frame {n} out of order: {numbers:?}");
        pos += at.unwrap() + 1;
    }
    // How many are shown depends on the machine (a busy runner drops frames): the order is
    // the behaviour; a third of them shown is the house's loose bound.
    assert!(numbers.len() >= 25, "frames shown: {}", numbers.len());
    let (video_waits, audio_waits) = player.boundary_waits();
    let late_at = |k: usize| {
        seen.iter()
            .filter(|s| (s.0 - k as f64).abs() < 0.2)
            .map(|s| s.2)
            .fold(0f64, f64::max)
    };
    eprintln!(
        "boundary waits: video {:?} audio {:?}; frames dropped {}; latest frame near the \
         boundaries (1 s, 2 s): {:.1} ms, {:.1} ms",
        video_waits,
        audio_waits,
        player.stats().1,
        late_at(1) * 1000.0,
        late_at(2) * 1000.0
    );
    assert_eq!(video_waits.len(), 2, "two boundaries");
}

/// At 30000/1001 fps (a frame time that is no round number), the paused frame at tick `k` and
/// the first frame played from a piece that starts at `k` are frame `k` — the same frame Save
/// renders there (the cut tests check that side).
#[test]
fn paused_and_played_frames_are_the_frames_save_renders_at_ntsc_rates() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("ntsc.mp4");
    ff(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "nullsrc=s=64x64:r=30000/1001:d=4.01,geq=lum='16+2*N':cb=128:cr=128,format=yuv420p",
            "-frames:v",
            "100",
            "-c:v",
            "libx264",
            "-crf",
            "10",
            "-g",
            "30",
            "-pix_fmt",
            "yuv420p",
            path.to_str().unwrap(),
        ],
    );
    let mut fx = open_file(tmp, path.clone());
    let fps = fx.win.media.fps;
    assert!((fps - 30000.0 / 1001.0).abs() < 1e-6, "{fps}");
    let gray = |rgba: &[u8]| (f64::from(rgba[0]) * 219.0 / 255.0 / 2.0).round() as i64;
    for k in [1i64, 31, 47, 77] {
        let t = k as f64 / fps;
        let ctx = fx.drv.ctx.clone();
        fx.win.seek(&ctx, t);
        fx.settle();
        let shown = fx
            .win
            .scrub
            .as_ref()
            .unwrap()
            .cache
            .lock()
            .unwrap()
            .iter()
            .find(|(key, _)| *key == k)
            .map(|(_, f)| gray(f));
        assert_eq!(shown, Some(k), "paused frame at tick {k}");
        let src = Source {
            input: Input::path(&path),
            duration: 1.0,
            video: Some(((64, 64), fps)),
            audio: false,
            pieces: vec![Piece {
                start: t,
                len: 10.0 / fps,
            }],
            stereo: false,
        };
        let mut player =
            Player::start(&tools, &src, 0.0, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
        let begin = Instant::now();
        let first = loop {
            if let Some(f) = player.pump(Instant::now()).frame {
                break gray(&f.rgba);
            }
            assert!(begin.elapsed().as_secs() < 10, "no frame");
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        assert_eq!(first, k, "first played frame of a piece at tick {k}");
    }
}

/// `SOPEN_AUTOSAVE` with `clips=` saves the edit without a window (the Save receipt), after
/// the backup; `close=ask` brings the prompt up on the first pass of a window with edits.
#[test]
fn the_state_hook_edits_and_saves_without_a_window() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.wav");
    tones(&tools, &path, false);
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let hooks = Hooks {
        autosave: true,
        state: crate::gui::State::parse("clips=3-4,0-1;stereo=1").unwrap(),
        ..Hooks::default()
    };
    let cache = tmp.path().join("cache");
    let outcome = open(origin, &path, &cache, &hooks, Vec::new()).unwrap();
    assert!(outcome.saved);
    assert_eq!(order(&path), [1760, 440]);
    let probe = ffmpeg::probe(&tools, &path).unwrap();
    assert_eq!(probe.channels, Some(2), "Mono → Stereo saved");
    assert_eq!(walk(&cache.join(".backup")).len(), 1);
}

/// A remote file (here a local one posing as host `box`): Save writes it back, and the window
/// then plays the staged render (the cached copy still holds what was fetched) — from `.edits`,
/// removed when the window goes.
#[test]
fn a_saved_remote_file_plays_from_its_render_until_the_window_goes() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("r.wav");
    tones(&tools, &path, true);
    let media = Media::probe(&path).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, Some("box".into()), abs, at);
    let file = MediaFile::new(origin, &tmp.path().join("cache"));
    let mut win = MediaWindow::new(
        media,
        file,
        tmp.path().join("work"),
        Marks::default(),
        Selection::default(),
        Vec::new(),
    );
    win.sink = Sink::Null;
    let mut fx = Fx {
        tmp,
        path,
        win,
        drv: Driver::new(900.0, 520.0),
    };
    fx.settle();
    fx.drag(0.0, 0.25);
    fx.key(Key::Delete, Modifiers::NONE);
    fx.key(Key::S, command(false));
    wait_save(&mut fx);
    assert!(
        fx.win.status.text.starts_with("saved box:"),
        "{}",
        fx.win.status.text
    );
    assert_eq!(order(&fx.path), [880, 1320, 1760], "written back");
    let playing = fx.win.media.input.local().unwrap().to_path_buf();
    assert!(
        playing.starts_with(fx.tmp.path().join("cache").join(".edits")),
        "{}",
        playing.display()
    );
    assert_eq!(order(&playing), [880, 1320, 1760]);
    assert!(!fx.win.unsaved() && near(fx.win.length(), 3.0));
    drop(fx.win);
    assert!(!playing.exists(), "removed with the window");
}
