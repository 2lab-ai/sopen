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
                    w.wave.as_ref().is_some_and(|s| {
                        s.peaks.lock().unwrap().as_ref().is_some_and(|p| p.complete)
                    }) && w.painted
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
    // A bounded wait: a frame that never comes fails with what the window has.
    let mut passes = 0;
    while !fx.win.frame_ready() {
        assert!(
            passes < 200,
            "the frame never came: want {:?}, on screen {:?}, status {:?}",
            fx.win.want,
            fx.win.tex_at,
            fx.win.status.text
        );
        fx.drv.idle(&mut fx.win, 1);
        std::thread::sleep(std::time::Duration::from_millis(5));
        passes += 1;
    }
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

/// The window's paused-frame decoder with a stand-in ffmpeg whose `n`-th run (from 1) waits
/// while `gate-n` exists in the fixture's folder, then gives one picture (bytes `x`); `seed` is
/// kept from the start. Returns it and the file a line is written to as each run starts: where
/// it reads from (its `-ss`).
fn held_decoder(fx: &Fx, seed: Option<(Tick, Arc<Vec<u8>>)>) -> (Scrubber, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let dir = fx.tmp.path();
    let runs = dir.join("runs");
    let (w, h) = fx.win.media.decode_size().unwrap();
    let stub = dir.join("ffmpeg");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\n[ \"$1\" = -version ] && exit 0\necho \"$6\" >> '{runs}'\n\
             n=$(wc -l < '{runs}' | tr -d ' ')\n\
             while [ -e '{dir}/gate-'\"$n\" ]; do sleep 0.02; done\n\
             head -c {} /dev/zero | tr '\\0' x\n",
            w * h * 4,
            runs = runs.display(),
            dir = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::testutil::run_fresh(std::process::Command::new(&stub).arg("-version"));
    let mut media = (*fx.win.media).clone();
    media.tools = Tools {
        ffmpeg: stub.clone(),
        ffprobe: stub,
    };
    // A budget no held run reaches: each ends with its picture.
    let budget = std::time::Duration::from_secs(60);
    let scrub = Scrubber::start_seeded(&media, fx.drv.ctx.clone(), budget, seed).unwrap();
    (scrub, runs)
}

/// Until the stand-in's `n`-th run has started (no passes meanwhile). The decoder runs one
/// request at a time, so run `n` started means every earlier request's answer was given or
/// dropped.
fn wait_runs(runs: &Path, n: usize) {
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    while std::fs::read_to_string(runs).map_or(0, |s| s.lines().count()) < n {
        assert!(Instant::now() < deadline, "run {n} never started");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A cut while a decode for a time before it still runs (a drag passed that frame). The cut
/// shows the frame now at the playhead, kept, at once; the decode ending after it must not take
/// its place: on screen for a time the playhead has left, the frame would never be ready, and
/// nothing would ask again. (The order behind a rare failure of the test above, where the late
/// decode was frame 50's own — asked again by the drag's release — which now runs once:
/// `a_drag_ending_on_a_frame_decodes_it_once`.)
#[test]
fn a_decode_for_the_time_before_a_cut_ending_after_it_leaves_the_kept_frame() {
    let Some(mut fx) = video() else { return };
    let ctx = fx.drv.ctx.clone();
    let (w, h) = fx.win.media.decode_size().unwrap();
    let kept = Arc::new(vec![0u8; (w * h * 4) as usize]);
    let (scrub, runs) = held_decoder(&fx, Some((50, kept)));
    let gate = |n: usize| Sentinel::new(fx.tmp.path().join(format!("gate-{n}")));
    let (first, _later) = (gate(1), gate(2));
    fx.win.scrub = Some(scrub);
    fx.win.marks.in_s = Some(1.0);
    fx.win.marks.out_s = Some(2.0);
    // The drag passed 1.5 s (still decoding) and ended on 2 s: frame 50, kept.
    fx.win.seek(&ctx, 1.5);
    wait_runs(&runs, 1);
    fx.win.seek(&ctx, 2.0);
    fx.drv.idle(&mut fx.win, 1);
    assert_eq!(fx.win.tex_at, Some((2.0, Some(50))));
    fx.drv.cut(&mut fx.win);
    assert_eq!(fx.clips(), [(0.0, 1.0), (2.0, 4.0)]);
    assert!(fx.win.frame_ready(), "kept: {:?}", fx.win.tex_at);
    // A later ask, held: once its run starts, the passed frame's answer is given or dropped.
    fx.win.scrub.as_ref().unwrap().request(3.0, 75);
    drop(first);
    wait_runs(&runs, 2);
    fx.drv.idle(&mut fx.win, 2);
    assert!(
        fx.win.frame_ready(),
        "the frame never came: want {:?}, on screen {:?}, status {:?}",
        fx.win.want,
        fx.win.tex_at,
        fx.win.status.text
    );
}

/// A drag's last move and its release both ask for the frame under the pointer. When the
/// decoder took the last move's ask before the release, the release's ask is the frame already
/// decoding: it waits for that decode instead of starting a second one (one ffmpeg run, and no
/// decode still running once the frame is on screen).
#[test]
fn a_drag_ending_on_a_frame_decodes_it_once() {
    let Some(mut fx) = video() else { return };
    let (scrub, runs) = held_decoder(&fx, None);
    let gate = |n: usize| Sentinel::new(fx.tmp.path().join(format!("gate-{n}")));
    let (first, second) = (gate(1), gate(2));
    fx.win.scrub = Some(scrub);
    let (a, b) = (fx.at(0.25), fx.at(0.5));
    let mut first = Some(first);
    let mut calls = 0;
    fx.drv
        .drag_watching(&mut fx.win, PointerButton::Primary, a, b, |_| {
            calls += 1;
            // After the press and the last of 8 moves: the first move's decode ends and the
            // decoder takes the last move's ask, held, before the release asks again.
            if calls == 9 {
                wait_runs(&runs, 1);
                drop(first.take());
                wait_runs(&runs, 2);
            }
        });
    let end = fx.win.frame_at(fx.win.marks.t);
    // What the release left waiting to be decoded: once taken, it would run.
    let again = fx
        .win
        .scrub
        .as_ref()
        .unwrap()
        .req
        .0
        .lock()
        .unwrap()
        .is_some();
    drop(second);
    if again {
        wait_runs(&runs, 3);
    }
    fx.drv.until(&mut fx.win, 20, |w| w.frame_ready());
    let media = &fx.win.media;
    let at = format!("{:.6}", frame_slot(media.origin(), media.fps, end).0);
    let ran = std::fs::read_to_string(&runs).unwrap();
    let ran: Vec<&str> = ran.lines().collect();
    assert_eq!(
        ran.iter().filter(|l| **l == at).count(),
        1,
        "frame {end} (from {at}) decoded more than once: {ran:?}"
    );
}

/// The other order, and another frame. A drag passes a frame that is still decoding and ends on
/// a kept one, whose answer waits for the next pass (a seek from the timeline runs at the end of
/// one). The passed frame's decode ends before that pass: it must not take the kept frame's
/// place, or the playhead shows a frame it is not on, for good.
#[test]
fn a_frame_still_decoding_when_the_playhead_lands_on_a_kept_one_never_replaces_it() {
    let Some(mut fx) = video() else { return };
    let ctx = fx.drv.ctx.clone();
    let (w, h) = fx.win.media.decode_size().unwrap();
    let kept = Arc::new(vec![0u8; (w * h * 4) as usize]);
    let (scrub, runs) = held_decoder(&fx, Some((50, kept)));
    let gate = |n: usize| Sentinel::new(fx.tmp.path().join(format!("gate-{n}")));
    let (first, _later) = (gate(1), gate(2));
    fx.win.scrub = Some(scrub);
    let passed = fx.win.frame_at(1.2);
    assert_ne!(passed, 50);
    fx.win.seek(&ctx, 1.2);
    wait_runs(&runs, 1);
    fx.win.seek(&ctx, 2.0);
    // A later ask, held: once its run starts, the passed frame's answer is given or dropped.
    fx.win.scrub.as_ref().unwrap().request(3.0, 75);
    drop(first);
    wait_runs(&runs, 2);
    fx.drv.idle(&mut fx.win, 2);
    assert!(
        fx.win.frame_ready(),
        "want {:?} (frame {passed} passed), on screen {:?}",
        fx.win.want,
        fx.win.tex_at
    );
}

/// Playback started while a paused frame was still decoding (a drag, then Space at once). The
/// player draws the frames, so that decode's answer — the frame of where playback started — is
/// not taken while it plays; it must not be taken when playback ends either, whether the decode
/// ended while it played (`during`) or after. The end shows the frame at the end, asked for as a
/// pause asks.
fn playback_over_a_decode(during: bool) {
    let Some(mut fx) = video() else { return };
    let ctx = fx.drv.ctx.clone();
    let (scrub, runs) = held_decoder(&fx, None);
    let gate = |n: usize| Sentinel::new(fx.tmp.path().join(format!("gate-{n}")));
    let (mut first, second) = (Some(gate(1)), gate(2));
    fx.win.scrub = Some(scrub);
    let start = fx.win.frame_at(3.0);
    fx.win.seek(&ctx, 3.0);
    wait_runs(&runs, 1);
    // As Space does. No pass until the decode is dealt with: playback cannot end before.
    fx.win.play(&ctx);
    assert!(fx.win.player.is_some(), "playing");
    if during {
        drop(first.take());
        let deadline = Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let scrub = fx.win.scrub.as_ref().unwrap();
            let kept = scrub
                .cache
                .lock()
                .unwrap()
                .iter()
                .find(|(k, _)| *k == start)
                .map(|(_, f)| f.clone());
            // Its picture kept, and its answer given (waiting to be taken) or dropped (no one
            // but the kept frames and this check holds the picture).
            if kept.is_some_and(|f| {
                scrub.result.lock().unwrap().frame.is_some() || Arc::strong_count(&f) == 2
            }) {
                break;
            }
            assert!(Instant::now() < deadline, "frame {start} never decoded");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    fx.drv.until(&mut fx.win, 20, |w| w.player.is_none());
    let end = fx.win.marks.t;
    assert_eq!(
        fx.win.want,
        Some((end, fx.win.frame_at(end))),
        "the frame at the end is asked for"
    );
    // The end frame's decode starting (held) means the earlier one's answer was given or
    // dropped.
    drop(first);
    wait_runs(&runs, 2);
    fx.drv.idle(&mut fx.win, 2);
    assert_ne!(
        fx.win.tex_at.and_then(|(_, k)| k),
        Some(start),
        "the frame of where playback started on screen at its end ({end} s): {:?}",
        fx.win.tex_at
    );
    drop(second);
    fx.drv.until(&mut fx.win, 20, |w| w.frame_ready());
    assert_eq!(fx.win.tex_at, Some((end, Some(fx.win.frame_at(end)))));
}

/// [`playback_over_a_decode`], the decode ending while it plays.
#[test]
fn a_frame_decoded_while_playing_is_not_shown_when_playback_ends() {
    playback_over_a_decode(true);
}

/// [`playback_over_a_decode`], the decode ending after playback has.
#[test]
fn a_frame_decoded_after_playback_ends_is_not_shown_at_its_end() {
    playback_over_a_decode(false);
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
    // While it plays, its lock is held: a sweep (another window starting) leaves it.
    let lock = playing.with_file_name(format!(
        "{}.lock",
        playing.file_name().unwrap().to_string_lossy()
    ));
    assert!(lock.exists(), "locked while it plays");
    let cache = fx.tmp.path().join("cache");
    assert!(crate::cache::sweep_stale_jobs(&cache).is_empty());
    assert!(playing.exists());
    assert_eq!(
        fx.win.file.lock().unwrap().rendered_path(),
        Some(playing.as_path())
    );
    drop(fx.win);
    assert!(!playing.exists(), "removed with the window");
    assert!(!lock.exists(), "and its lock");
}

/// A save worker that panicked while holding the file (it poisons the lock) does not lock the
/// file out of the next Save: the window saves again.
#[test]
fn a_save_after_a_worker_panicked_still_saves() {
    let Some(mut fx) = audio(true) else { return };
    let file = fx.win.file.clone();
    let _ = std::thread::spawn(move || {
        let _held = file.lock().unwrap();
        panic!("a worker panics mid-save (on purpose)");
    })
    .join();
    assert!(fx.win.file.is_poisoned());
    fx.drag(0.25, 0.5);
    fx.key(Key::Delete, Modifiers::NONE);
    fx.key(Key::S, command(false));
    wait_save(&mut fx);
    assert!(
        fx.win.status.text.starts_with("saved "),
        "{}",
        fx.win.status.text
    );
    assert_eq!(order(&fx.path), [440, 1320, 1760]);
}

/// A waveform decode that failed is said in the window where the waveform is.
#[test]
fn a_failed_waveform_is_said_where_the_waveform_is() {
    let Some(mut fx) = audio(false) else { return };
    let failed = wave::Source {
        bin_secs: 1.0,
        lanes: Vec::new(),
        version: 99,
        complete: false,
        error: Some("ffmpeg exit status: 1: Invalid data".to_string()),
    };
    *fx.win.wave.as_ref().unwrap().peaks.lock().unwrap() = Some(failed);
    fx.drv.idle(&mut fx.win, 2);
    assert!(fx
        .drv
        .find_text("cannot read the waveform: ffmpeg exit status: 1: Invalid data")
        .is_some());
    assert!(fx.win.painted, "a finished state for the screenshot hook");
}

/// The frame a paused playhead shows at the picture's last frame is the last frame (it was the
/// one before it, cached under the last one's key), and past the picture's end (a longer sound
/// track) the last frame holds instead of "no frame".
#[test]
fn the_last_frame_is_the_last_frame_and_holds_past_the_picture() {
    let Some(tools) = tools_or_skip() else { return };
    let gray = |rgba: &[u8]| (f64::from(rgba[0]) * 219.0 / 255.0 / 2.0).round() as i64;
    for (name, sound) in [("plain.mp4", 4.0), ("longer-sound.mp4", 5.0)] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(name);
        ff(
            &tools,
            &[
                "-f",
                "lavfi",
                "-i",
                "nullsrc=s=64x64:r=25:d=4,geq=lum='16+2*N':cb=128:cr=128,format=yuv420p",
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency=440:duration={sound}"),
                "-c:v",
                "libx264",
                "-crf",
                "10",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                path.to_str().unwrap(),
            ],
        );
        let mut fx = open_file(tmp, path);
        for t in [fx.win.length(), 99.0 / 25.0, fx.win.length() - 0.3] {
            let ctx = fx.drv.ctx.clone();
            fx.win.seek(&ctx, t);
            fx.settle();
            let (_, key) = fx.win.want.unwrap();
            let shown = fx
                .win
                .scrub
                .as_ref()
                .unwrap()
                .cache
                .lock()
                .unwrap()
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, f)| gray(f));
            let want = (t * 25.0).round().min(99.0) as i64;
            assert_eq!(shown, Some(want), "{name} at {t} s (key {key})");
            assert!(!fx.win.status.error, "{name}: {}", fx.win.status.text);
        }
    }
}

/// A Save whose worker ends without a word (it panicked) is a visible failure: the window is
/// usable again with its edits, not busy for ever; an Export the same.
#[test]
fn a_worker_that_ends_without_a_result_is_a_failure_not_busy_forever() {
    let Some(mut fx) = audio(true) else { return };
    fx.drag(0.25, 0.5);
    fx.key(Key::Delete, Modifiers::NONE);
    let (tx, rx) = mpsc::channel();
    fx.win.save = Some(SaveJob {
        progress: Arc::default(),
        rx,
        close_after: false,
    });
    drop(tx);
    fx.drv.idle(&mut fx.win, 2);
    assert!(fx.win.save.is_none(), "no longer busy");
    assert_eq!(fx.win.status.text, "save failed");
    assert!(
        fx.win.unsaved() && fx.win.editable(),
        "the edits stay, usable"
    );
    assert!(fx
        .drv
        .find_text("the save stopped without a result")
        .is_some());
    let (tx, rx) = mpsc::channel();
    fx.win.job = Some(ExportJob {
        progress: Arc::default(),
        rx,
    });
    drop(tx);
    fx.drv.idle(&mut fx.win, 2);
    assert!(fx.win.job.is_none());
    assert_eq!(fx.win.status.text, "export failed");
}

/// The bytes of a file opened in place change but its size and modification time do not:
/// Save compares the content with the bytes as opened (hashed when the window opened) and
/// refuses before anything is backed up or rendered — it never writes over it.
#[test]
fn save_refuses_a_same_size_same_time_change_of_content() {
    let Some(mut fx) = audio(true) else { return };
    fx.drag(0.25, 0.5);
    fx.key(Key::Delete, Modifiers::NONE);
    // The bytes as opened are hashed on a worker from the window's start: changed after that.
    fx.drv
        .until(&mut fx.win, 20, |w| w.file.lock().unwrap().baseline_ready());
    let meta = std::fs::metadata(&fx.path).unwrap();
    let mut bytes = std::fs::read(&fx.path).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x55;
    bytes[mid + 1] ^= 0x55;
    std::fs::write(&fx.path, &bytes).unwrap();
    let file = std::fs::File::options().write(true).open(&fx.path).unwrap();
    file.set_modified(meta.modified().unwrap()).unwrap();
    drop(file);
    let now = std::fs::metadata(&fx.path).unwrap();
    assert_eq!(
        (now.len(), now.modified().unwrap()),
        (meta.len(), meta.modified().unwrap())
    );
    fx.key(Key::S, command(false));
    wait_save_only(&mut fx);
    assert_eq!(fx.win.status.text, "not saved: the file changed");
    assert_eq!(
        std::fs::read(&fx.path).unwrap(),
        bytes,
        "never written over"
    );
    assert_eq!(fx.spawned().render, 0, "nothing rendered");
    assert!(walk(&fx.tmp.path().join("cache").join(".backup")).is_empty());
    assert!(fx.win.unsaved());
}

/// The bytes as opened could not be hashed (a read error, the file's size and time as they
/// were): every Save is refused for it, not only the first — a second one did not fall back to
/// comparing size and time alone, and wrote over a change those do not show.
#[test]
fn a_failed_hash_of_the_file_as_opened_refuses_every_save() {
    let Some(mut fx) = audio(true) else { return };
    fx.drag(0.25, 0.5);
    fx.key(Key::Delete, Modifiers::NONE);
    fx.drv
        .until(&mut fx.win, 20, |w| w.file.lock().unwrap().baseline_ready());
    fx.win
        .file
        .lock()
        .unwrap()
        .fail_baseline("cannot read it (Input/output error)");
    // A change of content that leaves size and time as they were.
    let meta = std::fs::metadata(&fx.path).unwrap();
    let mut bytes = std::fs::read(&fx.path).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x55;
    std::fs::write(&fx.path, &bytes).unwrap();
    let file = std::fs::File::options().write(true).open(&fx.path).unwrap();
    file.set_modified(meta.modified().unwrap()).unwrap();
    drop(file);
    for attempt in ["first", "second"] {
        fx.key(Key::S, command(false));
        wait_save_only(&mut fx);
        assert_eq!(
            fx.win.status.text, "not saved: the file changed",
            "{attempt} Save"
        );
        assert!(
            fx.win
                .notices
                .iter()
                .any(|n| n.contains("cannot read it (Input/output error); not saved")),
            "{attempt} Save says why: {:?}",
            fx.win.notices
        );
    }
    assert_eq!(
        std::fs::read(&fx.path).unwrap(),
        bytes,
        "never written over"
    );
    assert_eq!(fx.spawned().render, 0, "nothing rendered");
    assert!(fx.win.unsaved());
}

/// In's and Out's labels never cover each other or leave the timeline, however narrow the
/// selection and wherever it is: in the middle, at the right end, at the left end — on the
/// video window's short timeline too.
#[test]
fn in_and_out_labels_never_meet() {
    let Some(mut fx) = video() else { return };
    let length = fx.win.length();
    for (a, b) in [
        (1.5, 1.6),
        (length - 0.12, length - 0.04),
        (0.0, 0.08),
        (2.0, 2.04),
    ] {
        fx.win.marks.in_s = Some(a);
        fx.win.marks.out_s = Some(b);
        fx.drv.idle(&mut fx.win, 2);
        let timeline = fx.win.timeline_rect.unwrap();
        // The labels on the timeline ("In 0:01.50"), not the buttons ("In  I").
        let find = |prefix: &str| {
            let all: Vec<Rect> = fx
                .drv
                .texts_starting_with(prefix)
                .into_iter()
                .filter(|(r, t)| t.contains(':') && timeline.expand(40.0).intersects(*r))
                .map(|(r, _)| r)
                .collect();
            assert_eq!(all.len(), 1, "{prefix}: {all:?}");
            all[0]
        };
        let (i, o) = (find("In "), find("Out "));
        let overlap = i.intersect(o);
        assert!(
            overlap.width() <= 0.0 || overlap.height() <= 0.0,
            "{a}..{b}: In {i:?} covers Out {o:?}"
        );
        for r in [i, o] {
            assert!(
                timeline.expand(0.5).contains_rect(r),
                "{a}..{b}: {r:?} out of {timeline:?}"
            );
        }
    }
}

/// A paused frame whose decode finds nothing inside the picture is a failure — not the file's
/// last picture kept under the frame asked for (the fallback for a last frame stamped a little
/// early once ran after any failure and read to the end of the file). The stand-in's one-frame
/// decode at the frame's slot gives nothing, one after it a picture (there is picture after the
/// slot); a decode without a frame limit gives pictures (bytes of `x`).
#[test]
fn a_failing_seek_inside_the_picture_is_no_frame_not_the_last_one() {
    use std::os::unix::fs::PermissionsExt;
    let Some(fx) = video() else { return };
    let (w, h) = fx.win.media.decode_size().unwrap();
    let key = 40;
    let at = fx.win.media.origin() + (key as f64 - 0.5) / fx.win.media.fps;
    let stub = fx.tmp.path().join("ffmpeg");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\n[ \"$1\" = -version ] && exit 0\n\
             [ \"$6\" = {at:.6} ] && exit 0\n\
             for a in \"$@\"; do [ \"$a\" = -frames:v ] && exec head -c {one} /dev/zero; done\n\
             head -c {} /dev/zero | tr '\\0' x\n",
            3 * w * h * 4,
            one = w * h * 4
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::testutil::run_fresh(std::process::Command::new(&stub).arg("-version"));
    let mut media = (*fx.win.media).clone();
    media.tools = Tools {
        ffmpeg: stub.clone(),
        ffprobe: stub,
    };
    let scrub = Scrubber::start(&media, fx.drv.ctx.clone()).unwrap();
    assert!(key < media.last_frame());
    scrub.request(1.6, key);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let got = loop {
        if let Some(r) = scrub.result.lock().unwrap().take() {
            break r;
        }
        assert!(std::time::Instant::now() < deadline, "no answer");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    match got {
        Ok(f) => panic!("frame {} shows a picture of {:#x}", f.key, f.rgba[0]),
        Err(e) => assert!(e.starts_with("no frame at "), "{e}"),
    }
    assert!(
        scrub.cache.lock().unwrap().iter().all(|(k, _)| *k != key),
        "nothing kept under frame {key}"
    );
}

/// The picture's last frame held for 1.5 s, so the timeline runs on past the last picture's
/// start: there, and at the very end, the one-frame decode at the slot finds nothing, and the
/// frame shown is the last picture, read from the two seconds before the slot's end only.
#[test]
fn a_last_frame_held_long_shows_the_last_picture() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("held.mp4");
    ff(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "nullsrc=s=64x64:r=25:d=4,geq=lum='16+2*N':cb=128:cr=128,format=yuv420p",
            "-c:v",
            "libx264",
            "-crf",
            "10",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-enc_time_base:v",
            "1:1000",
            "-video_track_timescale",
            "1000",
            "-bsf:v",
            "setts=duration='if(eq(N\\,99)\\,1500\\,DURATION)'",
            path.to_str().unwrap(),
        ],
    );
    let mut fx = open_file(tmp, path.clone());
    let (fps, origin) = (fx.win.media.fps, fx.win.media.origin());
    let gray = |rgba: &[u8]| (f64::from(rgba[0]) * 219.0 / 255.0 / 2.0).round() as i64;
    for t in [fx.win.length() - 1.0, fx.win.length()] {
        let ctx = fx.drv.ctx.clone();
        fx.win.seek(&ctx, t);
        fx.settle();
        let (_, key) = fx.win.want.unwrap();
        // The one-frame decode at this frame's slot finds nothing.
        let at = origin + (key as f64 - 0.5) / fps;
        let out = std::process::Command::new(&tools.ffmpeg)
            .args(["-nostdin", "-loglevel", "error", "-ss", &format!("{at:.6}")])
            .arg("-i")
            .arg(&path)
            .args(["-frames:v", "1", "-f", "rawvideo", "-"])
            .output()
            .unwrap();
        assert!(out.stdout.is_empty(), "frame {key} at {at} s: a picture");
        let shown = fx
            .win
            .scrub
            .as_ref()
            .unwrap()
            .cache
            .lock()
            .unwrap()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, f)| gray(f));
        assert_eq!(shown, Some(99), "at {t} s (frame {key})");
        assert!(!fx.win.status.error, "at {t} s: {}", fx.win.status.text);
    }
}

/// The window's own F-key, F5 (Save), is the key alone (04 §3): Shift, Alt or Cmd/Ctrl with it
/// does nothing, nor does F5 while a text field has the keyboard — no save starts, nothing is
/// rendered — and F5 by itself saves.
#[test]
fn f5_with_a_modifier_does_nothing() {
    let Some(mut fx) = audio(true) else { return };
    fx.drag(0.25, 0.5);
    fx.key(Key::Delete, Modifiers::NONE);
    for mods in [
        Modifiers::SHIFT,
        Modifiers::ALT,
        Modifiers::SHIFT | Modifiers::ALT,
        command(false),
        command(true),
    ] {
        fx.key(Key::F5, mods);
        fx.drv.idle(&mut fx.win, 2);
        assert!(fx.win.save.is_none(), "{mods:?}+F5 started a save");
    }
    // Nor F5 alone while a text field has the keyboard: the typing is the field's.
    let field = egui::Id::unique("a text field");
    let ctx = fx.drv.ctx.clone();
    egui::text_edit::TextEditState::default().store(&ctx, field);
    ctx.memory_mut(|m| m.request_focus(field));
    assert!(ctx.text_edit_focused());
    fx.key(Key::F5, Modifiers::NONE);
    assert!(fx.win.save.is_none(), "F5 into a text field started a save");
    ctx.memory_mut(|m| m.surrender_focus(field));
    fx.drv.idle(&mut fx.win, 2);
    assert!(fx.win.save.is_none());
    assert_eq!(fx.spawned().render, 0, "nothing rendered");
    assert!(fx.win.unsaved());
    fx.key(Key::F5, Modifiers::NONE);
    wait_save(&mut fx);
    assert!(
        fx.win.status.text.starts_with("saved "),
        "{}",
        fx.win.status.text
    );
}

/// `ps`'s state of process `pid` (`Z` for a zombie), or `None` once it is gone.
fn process_state(pid: &str) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!state.is_empty()).then_some(state)
}

/// A file whose removal ends the stand-ins that wait on it ("while it exists"): a failed test
/// ends them by deleting it, never by signalling a pid that may since be another process's.
struct Sentinel(PathBuf);

impl Sentinel {
    fn new(path: PathBuf) -> Self {
        std::fs::write(&path, b"").unwrap();
        Self(path)
    }
}

impl Drop for Sentinel {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// One paused frame has one work budget, the seek, the look past its slot and the read-back
/// together: an ffmpeg that never answers (a seek in an unindexed file, a scan to the end of a
/// long input) is stopped when it runs out — killed and reaped, no child left — and the frame is
/// "no frame here". Every decode reads a bounded stretch of its input (`-t` before `-i`). The
/// stand-in records its pid and arguments, then waits without a word for as long as the test's
/// sentinel file exists (the test ending removes it: nothing it started outlives it).
#[test]
fn a_paused_frame_that_never_comes_is_given_up_within_its_budget() {
    use std::os::unix::fs::PermissionsExt;
    let Some(fx) = video() else { return };
    let pids = fx.tmp.path().join("pids");
    let args = fx.tmp.path().join("args");
    let alive = Sentinel::new(fx.tmp.path().join("alive"));
    let stub = fx.tmp.path().join("ffmpeg");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\n[ \"$1\" = -version ] && exit 0\necho $$ >> '{}'\n\
             echo \"$*\" >> '{}'\nwhile [ -e '{}' ]; do sleep 0.1; done\n",
            pids.display(),
            args.display(),
            alive.0.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::testutil::run_fresh(std::process::Command::new(&stub).arg("-version"));
    let mut media = (*fx.win.media).clone();
    media.tools = Tools {
        ffmpeg: stub.clone(),
        ffprobe: stub,
    };
    let budget = std::time::Duration::from_secs(1);
    let scrub = Scrubber::start_with(&media, fx.drv.ctx.clone(), budget).unwrap();
    for key in [40, media.last_frame()] {
        let asked = std::time::Instant::now();
        scrub.request(key as f64 / 25.0, key);
        let got = loop {
            if let Some(r) = scrub.result.lock().unwrap().take() {
                break r;
            }
            assert!(
                asked.elapsed() < std::time::Duration::from_secs(20),
                "frame {key}: no answer"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let took = asked.elapsed();
        assert!(
            took < budget + std::time::Duration::from_secs(2),
            "{took:?}"
        );
        match got {
            Ok(_) => panic!("frame {key}: a picture from nowhere"),
            Err(e) => assert!(e.starts_with("no frame here"), "{e}"),
        }
        for pid in std::fs::read_to_string(&pids).unwrap().lines() {
            assert_eq!(process_state(pid), None, "ffmpeg {pid} left behind");
        }
    }
    for line in std::fs::read_to_string(&args).unwrap().lines() {
        let words: Vec<&str> = line.split(' ').collect();
        let input = words.iter().position(|w| *w == "-i").unwrap();
        assert!(words[..input].contains(&"-t"), "unbounded read: {line}");
    }
}

/// A 2 s picture with ten minutes of sound after it: the paused frame anywhere past the
/// picture's end is its last frame, found within the frame's budget, and no ffmpeg is left
/// reading the file afterwards.
#[test]
fn past_a_short_picture_s_end_the_last_frame_comes_within_the_budget() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("tail.mp4");
    ff(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "nullsrc=s=64x64:r=25:d=2,geq=lum='16+2*N':cb=128:cr=128,format=yuv420p",
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:r=8000:d=600",
            "-c:v",
            "libx264",
            "-crf",
            "10",
            "-c:a",
            "aac",
            "-b:a",
            "24k",
            path.to_str().unwrap(),
        ],
    );
    let mut fx = open_file(tmp, path.clone());
    let gray = |rgba: &[u8]| (f64::from(rgba[0]) * 219.0 / 255.0 / 2.0).round() as i64;
    for t in [2.5, 300.0, fx.win.length()] {
        let ctx = fx.drv.ctx.clone();
        let asked = std::time::Instant::now();
        fx.win.seek(&ctx, t);
        fx.settle();
        assert!(
            asked.elapsed() < SCRUB_BUDGET,
            "at {t} s: {:?}",
            asked.elapsed()
        );
        let (_, key) = fx.win.want.unwrap();
        let shown = fx
            .win
            .scrub
            .as_ref()
            .unwrap()
            .cache
            .lock()
            .unwrap()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, f)| gray(f));
        assert_eq!(shown, Some(49), "at {t} s (frame {key})");
        let ps = std::process::Command::new("ps")
            .args(["-axo", "command="])
            .output()
            .unwrap();
        let left = String::from_utf8_lossy(&ps.stdout)
            .lines()
            .filter(|l| l.contains(path.to_str().unwrap()))
            .count();
        assert_eq!(left, 0, "at {t} s: ffmpeg left reading the file");
    }
}
