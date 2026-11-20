//! Edit renders end to end (spec §7, message 13): fixtures whose stretches are told apart by
//! their content — a tone per second of audio, a frame counter in the picture's brightness —
//! are cut and pasted, rendered, decoded again, and the order and length of every stretch is
//! checked, sample- and frame-exact where the codec allows.

use super::*;
use crate::ffmpeg::{Span, Track};
use std::path::PathBuf;
use std::process::Command;

fn tools_or_skip() -> Option<Tools> {
    match ffmpeg::resolve() {
        Ok(t) => Some(t),
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping edit render test: {e}");
            None
        }
    }
}

fn ff(tools: &Tools, args: &[&str]) {
    let out = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Left: 440, 880, 1320, 1760 Hz, a second each; right: 660 Hz throughout (stereo), at `rate`.
const TONES: &str = "aevalsrc='0.5*sin(2*PI*(440+440*floor(t))*t)|0.5*sin(2*PI*660*t)'";
/// The same four seconds of the left channel alone.
const MONO: &str = "aevalsrc='0.5*sin(2*PI*(440+440*floor(t))*t)'";

fn tones(tools: &Tools, dst: &Path, src: &str, rate: u32, codec: &[&str]) {
    let input = format!("{src}:s={rate}:d=4");
    let mut args = vec!["-f", "lavfi", "-i", &input];
    args.extend(codec);
    args.push(dst.to_str().unwrap());
    ff(tools, &args);
}

/// The file's first audio track decoded as f32 at its own rate: one Vec per channel.
fn pcm(tools: &Tools, file: &Path, track: usize) -> (u32, Vec<Vec<f32>>) {
    let probe = ffmpeg::probe(tools, file).unwrap();
    let t = probe.tracks[track];
    let (rate, ch) = (t.rate.unwrap(), t.channels.unwrap() as usize);
    let out = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(file)
        .args(["-map", &format!("0:a:{track}"), "-f", "f32le", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let samples: Vec<f32> = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    let lanes = (0..ch)
        .map(|c| samples.iter().skip(c).step_by(ch).copied().collect())
        .collect();
    (rate, lanes)
}

/// The tones of one channel, in order, as `(Hz, seconds)`: the frequency from the spacing of
/// zero crossings (snapped to the nearest of the fixture's), each stretch measured between the
/// midpoints of the crossings where the frequency changes.
fn segments(rate: u32, x: &[f32]) -> Vec<(u32, f64)> {
    const HZ: [u32; 5] = [440, 660, 880, 1320, 1760];
    let cross: Vec<usize> = (1..x.len())
        .filter(|&i| (x[i - 1] < 0.0) != (x[i] < 0.0))
        .collect();
    let mut runs: Vec<(u32, usize, usize)> = Vec::new();
    for w in cross.windows(2) {
        let hz = f64::from(rate) / (2.0 * (w[1] - w[0]) as f64);
        let near = *HZ
            .iter()
            .min_by(|a, b| {
                (f64::from(**a) - hz)
                    .abs()
                    .total_cmp(&(f64::from(**b) - hz).abs())
            })
            .unwrap();
        match runs.last_mut() {
            Some(r) if r.0 == near => r.2 = w[1],
            _ => runs.push((near, w[0], w[1])),
        }
    }
    // Glitches at a boundary (a crossing pair straddling two tones) are dropped.
    runs.retain(|r| (r.2 - r.1) as f64 > f64::from(rate) * 0.02);
    let mut merged: Vec<(u32, usize, usize)> = Vec::new();
    for r in runs {
        match merged.last_mut() {
            Some(m) if m.0 == r.0 => m.2 = r.2,
            _ => merged.push(r),
        }
    }
    let n = merged.len();
    let edge = |k: usize| -> f64 {
        if k == 0 {
            0.0
        } else if k == n {
            x.len() as f64
        } else {
            (merged[k - 1].2 + merged[k].1) as f64 / 2.0
        }
    };
    (0..n)
        .map(|k| (merged[k].0, (edge(k + 1) - edge(k)) / f64::from(rate)))
        .collect()
}

fn assert_tones(got: &[(u32, f64)], want: &[(u32, f64)], tolerance: f64, what: &str) {
    let hz: Vec<u32> = got.iter().map(|g| g.0).collect();
    let want_hz: Vec<u32> = want.iter().map(|w| w.0).collect();
    assert_eq!(hz, want_hz, "{what}: tone order (got {got:?})");
    for (g, w) in got.iter().zip(want) {
        assert!(
            (g.1 - w.1).abs() <= tolerance,
            "{what}: {} Hz lasts {:.4} s, want {:.4} ± {tolerance} (all: {got:?})",
            g.0,
            g.1,
            w.1
        );
    }
}

fn spans(rate: f64, ranges: &[(i64, i64)], video: bool) -> Vec<Span> {
    ranges
        .iter()
        .map(|&(a, b)| Span {
            file: 0,
            start: a as f64 / rate,
            len: (b - a) as f64 / rate,
            frames: video.then_some((b - a) as u64),
        })
        .collect()
}

/// Renders `spec` of `src` into `<dir>/<name>` the way Save and Export do.
fn render(tools: &Tools, src: &Path, name: &str, spec: &EditSpec) -> PathBuf {
    let probe = ffmpeg::probe(tools, src).unwrap();
    let (media, _, plan) =
        super::plan(tools, &probe, src.file_name().unwrap().to_str().unwrap()).unwrap();
    let dir = src.parent().unwrap();
    let dst = dir.join(name);
    let work = dir.join(format!("work-{name}"));
    crate::cache::private_dir(&work).unwrap();
    let mut last = 0.0;
    render_edit(
        tools,
        &ffmpeg::Input::path(src),
        &dst,
        spec,
        media,
        &plan,
        &work,
        &mut |f| {
            assert!(f >= last - 1e-9, "progress never goes back");
            last = f;
        },
    )
    .unwrap();
    assert_eq!(last, 1.0, "progress reaches the end");
    dst
}

fn audio_spec(probe: &Probe, clips: Vec<Span>) -> EditSpec {
    EditSpec {
        spans: clips,
        fps: None,
        stereo: false,
        crop: None,
        tracks: probe.tracks.clone(),
    }
}

/// Second 1 cut out and pasted at the front, in WAV, FLAC, AAC (m4a) and MP3: the left
/// channel plays 880, 440, 1320, 1760 Hz — a second each, to a few milliseconds (to the sample
/// band for PCM) — and the right channel stays its own 660 Hz throughout: stereo kept apart.
#[test]
fn audio_cut_and_paste_keeps_order_length_and_both_channels() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    for (name, codec, tolerance) in [
        ("a.wav", &["-c:a", "pcm_s16le"][..], 0.002),
        ("a.flac", &["-c:a", "flac"][..], 0.002),
        ("a.m4a", &["-c:a", "aac", "-b:a", "192k"][..], 0.004),
        ("a.mp3", &["-c:a", "libmp3lame", "-q:a", "2"][..], 0.004),
    ] {
        let src = tmp.path().join(name);
        tones(&tools, &src, TONES, 44_100, codec);
        let probe = ffmpeg::probe(&tools, &src).unwrap();
        let rate = f64::from(probe.sample_rate.unwrap());
        let s = |secs: f64| (secs * rate) as i64;
        // [1, 2) then [0, 1) then [2, 4): what the timeline holds after the cut and paste.
        let spec = audio_spec(
            &probe,
            spans(
                rate,
                &[(s(1.0), s(2.0)), (0, s(1.0)), (s(2.0), s(4.0))],
                false,
            ),
        );
        let out = render(&tools, &src, &format!("out-{name}"), &spec);
        let (r, lanes) = pcm(&tools, &out, 0);
        assert_eq!(lanes.len(), 2, "{name}: still stereo");
        let want = [(880, 1.0), (440, 1.0), (1320, 1.0), (1760, 1.0)];
        assert_tones(&segments(r, &lanes[0]), &want, tolerance, name);
        assert_tones(&segments(r, &lanes[1]), &[(660, 4.0)], tolerance, name);
        let total = lanes[0].len() as f64 / f64::from(r);
        assert!((total - 4.0).abs() <= tolerance, "{name}: {total} s");
    }
}

/// Mono → Stereo: the output has two channels and both are the mono signal, sample for sample
/// and at its full level (a plain `-ac 2` would lower it 3 dB).
#[test]
fn mono_to_stereo_renders_identical_left_and_right_at_full_level() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("m.wav");
    tones(&tools, &src, MONO, 48_000, &["-c:a", "pcm_s16le"]);
    let probe = ffmpeg::probe(&tools, &src).unwrap();
    assert_eq!(probe.channels, Some(1));
    let mut spec = audio_spec(
        &probe,
        spans(48_000.0, &[(48_000, 96_000), (0, 48_000)], false),
    );
    spec.stereo = true;
    let out = render(&tools, &src, "stereo.wav", &spec);
    let (_, lanes) = pcm(&tools, &out, 0);
    assert_eq!(lanes.len(), 2, "two channels");
    assert_eq!(lanes[0], lanes[1], "left = right, every sample");
    let peak = lanes[0].iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!((peak - 0.5).abs() < 0.01, "full level: peak {peak}");
    assert_tones(
        &segments(48_000, &lanes[0]),
        &[(880, 1.0), (440, 1.0)],
        0.002,
        "stereo.wav",
    );
}

/// A frame counter: frame N's luma is 16 + 2N (so N survives H.264 at CRF 18), with the tones
/// as audio. `fps` like `25` or `30000/1001`.
fn counter(tools: &Tools, dst: &Path, fps: &str, frames: u32) {
    let video = format!(
        "nullsrc=s=64x64:r={fps}:d={},geq=lum='16+2*N':cb=128:cr=128,format=yuv420p",
        f64::from(frames) / eval(fps) + 0.001
    );
    ff(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            &video,
            "-f",
            "lavfi",
            "-i",
            &format!("{TONES}:s=48000:d=5"),
            "-frames:v",
            &frames.to_string(),
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

fn eval(fps: &str) -> f64 {
    match fps.split_once('/') {
        Some((a, b)) => a.parse::<f64>().unwrap() / b.parse::<f64>().unwrap(),
        None => fps.parse().unwrap(),
    }
}

/// The frame numbers a file shows, read from each frame's luma.
pub(crate) fn frame_numbers(tools: &Tools, file: &Path) -> Vec<i64> {
    let out = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(file)
        .args([
            "-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-",
        ])
        .output()
        .unwrap();
    let (w, h) = (64usize, 64usize);
    let frame = w * h * 3 / 2;
    out.stdout
        .chunks_exact(frame)
        .map(|f| {
            let y: f64 = f[..w * h].iter().map(|v| f64::from(*v)).sum::<f64>() / (w * h) as f64;
            ((y - 16.0) / 2.0).round() as i64
        })
        .collect()
}

/// A cut and paste on video, at 25 and at 30000/1001 fps: the output shows exactly the frames
/// the timeline does, in its order (frame-exact, no frame lost or repeated at a cut), and its
/// sound follows the pictures (the tone of each second moves with its frames).
#[test]
fn video_cut_and_paste_is_frame_exact_and_the_sound_follows() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    for (fps, n, cut) in [("25", 100, (25, 50)), ("30000/1001", 120, (30, 60))] {
        let name = format!("c{}.mp4", fps.replace('/', "_"));
        let src = tmp.path().join(&name);
        counter(&tools, &src, fps, n);
        let probe = ffmpeg::probe(&tools, &src).unwrap();
        let rate = probe.fps.unwrap();
        assert!((rate - eval(fps)).abs() < 1e-6, "{rate}");
        let ranges = [cut, (0, cut.0), (cut.1, n as i64)];
        let spec = EditSpec {
            spans: spans(rate, &ranges, true),
            fps: Some(rate),
            stereo: false,
            crop: None,
            tracks: probe.tracks.clone(),
        };
        let out = render(&tools, &src, &format!("out-{name}"), &spec);
        let want: Vec<i64> = ranges.iter().flat_map(|&(a, b)| a..b).collect();
        assert_eq!(frame_numbers(&tools, &out), want, "{fps}: frames in order");
        let (r, lanes) = pcm(&tools, &out, 0);
        let secs = |f: i64| f as f64 / rate;
        let total = lanes[0].len() as f64 / f64::from(r);
        assert!(
            (total - secs(n as i64)).abs() <= 1.0 / rate,
            "{fps}: sound {total} s for {} s of picture",
            secs(n as i64)
        );
        // Each second's tone moves with its frames: at 25 fps the pieces are whole seconds
        // (each tone lasts a second); at 29.97 they straddle the seconds by a frame or two,
        // so only the order is checked there.
        let tones = segments(r, &lanes[0]);
        let order = [880, 440, 1320, 1760];
        if fps == "25" {
            let want = order.map(|hz| (hz, 1.0));
            assert_tones(&tones, &want, 0.004, fps);
        } else {
            let hz: Vec<u32> = tones
                .iter()
                .map(|t| t.0)
                .filter(|h| order.contains(h))
                .collect();
            assert_eq!(hz, order, "{fps}: {tones:?}");
        }
    }
}

/// Every audio track is kept through an edit, each counted at its own rate (48 kHz mono and
/// 44.1 kHz stereo here) and as long as the picture; Mono → Stereo makes the mono track stereo
/// and leaves the stereo one as it was.
#[test]
fn every_audio_track_is_kept_each_at_its_rate() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("two.mkv");
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
            &format!("{MONO}:s=48000:d=4"),
            "-f",
            "lavfi",
            "-i",
            &format!("{TONES}:s=44100:d=4"),
            "-map",
            "0",
            "-map",
            "1",
            "-map",
            "2",
            "-c:v",
            "libx264",
            "-crf",
            "10",
            "-c:a",
            "flac",
            src.to_str().unwrap(),
        ],
    );
    let probe = ffmpeg::probe(&tools, &src).unwrap();
    assert_eq!(probe.audio_streams, 2);
    let mut spec = EditSpec {
        spans: spans(25.0, &[(50, 75), (0, 25)], true),
        fps: Some(25.0),
        stereo: true,
        crop: None,
        tracks: probe.tracks.clone(),
    };
    let out = render(&tools, &src, "out.mkv", &spec);
    let got = ffmpeg::probe(&tools, &out).unwrap();
    assert_eq!(got.audio_streams, 2, "both tracks");
    assert_eq!(
        got.tracks.iter().map(|t| t.channels).collect::<Vec<_>>(),
        [Some(2), Some(2)],
        "the mono track became stereo; the stereo one stayed"
    );
    for track in 0..2 {
        let (r, lanes) = pcm(&tools, &out, track);
        assert_eq!(r, probe.tracks[track].rate.unwrap(), "track {track} rate");
        // AAC in Matroska carries no end trim: the last 1024-sample frame may decode whole.
        let secs = lanes[0].len() as f64 / f64::from(r);
        assert!(
            (secs - 2.0).abs() < 1024.0 / f64::from(r),
            "track {track}: {secs} s"
        );
        // The cut between the pieces is exact; the end may run on by the AAC frame above.
        let tones = segments(r, &lanes[0]);
        let hz: Vec<u32> = tones.iter().map(|t| t.0).collect();
        assert_eq!(hz, [1320, 440], "track {track}: {tones:?}");
        assert!((tones[0].1 - 1.0).abs() < 0.004, "track {track}: {tones:?}");
        if track == 0 {
            // AAC is lossy: left and right agree to its quantisation (the PCM test above is
            // sample-exact).
            let worst = lanes[0]
                .iter()
                .zip(&lanes[1])
                .fold(0f32, |m, (l, r)| m.max((l - r).abs()));
            assert!(
                worst < 0.01,
                "panned: left = right (max difference {worst})"
            );
        } else {
            let right = segments(r, &lanes[1]);
            let hz: Vec<u32> = right.iter().map(|t| t.0).collect();
            assert_eq!(hz, [660], "track 1 right: {right:?}");
        }
    }
    assert_eq!(
        frame_numbers(&tools, &out),
        (50..75).chain(0..25).collect::<Vec<_>>()
    );
    spec.stereo = false;
    let plain = render(&tools, &src, "plain.mkv", &spec);
    assert_eq!(
        ffmpeg::probe(&tools, &plain).unwrap().tracks[0].channels,
        Some(1),
        "without Mono → Stereo the mono track stays mono"
    );
}

/// An edit that reads more separate stretches of the source than one run may open renders in
/// lossless passes and a final one — still frame-exact, in order.
#[test]
fn a_long_out_of_order_edit_renders_in_passes_frame_exact() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("c.mp4");
    counter(&tools, &src, "25", 100);
    let probe = ffmpeg::probe(&tools, &src).unwrap();
    // 2-frame pieces, every one of them before the last in the source (backwards).
    let ranges: Vec<(i64, i64)> = (0..ffmpeg::MAX_INPUTS as i64 + 6)
        .map(|k| {
            let a = 96 - 3 * k;
            (a, a + 2)
        })
        .collect();
    let spec = EditSpec {
        spans: spans(25.0, &ranges, true),
        fps: Some(25.0),
        stereo: false,
        crop: None,
        tracks: probe.tracks.clone(),
    };
    assert!(ffmpeg::edit_passes(&spec).unwrap().len() > 1, "passes");
    let out = render(&tools, &src, "out.mp4", &spec);
    let want: Vec<i64> = ranges.iter().flat_map(|&(a, b)| a..b).collect();
    assert_eq!(frame_numbers(&tools, &out), want);
}

/// A 10-bit source stays 10-bit through an edit where libx264 has it, and its colour tags
/// travel; the crop is applied once, to the whole output.
#[test]
fn an_edit_keeps_ten_bits_colour_tags_and_crops_the_whole_output() {
    let Some(tools) = tools_or_skip() else { return };
    if !ffmpeg::encoder_pix_fmts(&tools, "libx264")
        .unwrap()
        .contains("yuv420p10le")
    {
        eprintln!("skipping: this ffmpeg's libx264 has no 10-bit");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("deep.mp4");
    ff(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25:duration=4",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p10le",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            src.to_str().unwrap(),
        ],
    );
    let probe = ffmpeg::probe(&tools, &src).unwrap();
    let spec = EditSpec {
        spans: spans(25.0, &[(50, 75), (0, 25)], true),
        fps: Some(25.0),
        stereo: false,
        crop: Some(Rect {
            x: 10,
            y: 20,
            w: 101,
            h: 61,
        }),
        tracks: Vec::new(),
    };
    let out = render(&tools, &src, "out.mp4", &spec);
    let got = ffmpeg::probe(&tools, &out).unwrap();
    assert_eq!(got.pix_fmt.as_deref(), Some("yuv420p10le"));
    assert_eq!(got.color, probe.color);
    assert_eq!(got.video, Some((100, 60)));
    let d = got.duration.unwrap();
    assert!((d - 2.0).abs() < 0.05, "{d}");
}

/// The progress-stall watchdog: an ffmpeg that keeps printing the same progress (as ffmpeg 9
/// does while stuck) is stopped and the error says so — it does not hang the save.
#[test]
fn a_stuck_ffmpeg_is_stopped_by_the_watchdog() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("ffmpeg");
    std::fs::write(
        &fake,
        "#!/bin/sh\n[ \"$1\" = -version ] && exit 0\n\
         while :; do printf 'frame=3\\nout_time_us=120000\\nprogress=continue\\n'; \
         sleep 0.05; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let tools = Tools {
        ffmpeg: fake.clone(),
        ffprobe: fake,
    };
    // A stand-in just written can be busy for a moment (Linux): started once to its end first.
    crate::testutil::run_fresh(Command::new(&tools.ffmpeg).arg("-version"));
    let err = ffmpeg::run_watched(
        &tools,
        &["out.mp4".into()],
        4.0,
        &mut |_| {},
        std::time::Duration::from_millis(300),
    )
    .unwrap_err();
    // Where its output was depends on when the stall is seen (before or after the first
    // block); the message itself is `stall_messages`'s.
    let text = format!("{err:#}");
    assert!(text.contains("made no progress for 0.3 s"), "{text}");
    assert!(text.ends_with("and was stopped"), "{text}");
}

/// The window's Export with edits publishes a new sibling named by the timeline's In/Out, the
/// original untouched.
#[test]
fn an_edited_export_is_a_new_sibling() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("t.wav");
    tones(&tools, &src, TONES, 44_100, &["-c:a", "pcm_s16le"]);
    let before = std::fs::read(&src).unwrap();
    let probe = ffmpeg::probe(&tools, &src).unwrap();
    let spec = audio_spec(&probe, spans(44_100.0, &[(44_100, 88_200)], false));
    let mut fs = crate::remote::LocalFs::new();
    let published = render_and_publish(
        &mut fs,
        &tools,
        &ffmpeg::Input::path(&src),
        tmp.path().to_str().unwrap(),
        "t.wav",
        &spec,
        (0.0, 1.0),
        &tmp.path().join("work"),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(published.name, "t.cut-0-1.wav");
    assert_eq!(std::fs::read(&src).unwrap(), before);
    let (r, lanes) = pcm(&tools, &tmp.path().join(&published.name), 0);
    assert_tones(&segments(r, &lanes[0]), &[(880, 1.0)], 0.002, "export");
    let _ = Track::default();
}

/// How many video frames a file holds (decoded and counted).
pub(crate) fn count_frames(tools: &Tools, file: &Path) -> usize {
    let out = Command::new(&tools.ffprobe)
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(file)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

/// A variable-rate source (25 fps for a second, then a frame every 0.2 s): each piece is put on
/// the frame-rate grid from its first frame, as the preview shows it — the output holds exactly
/// the timeline's frames, as long as the timeline says, not frames past a piece's end.
#[test]
fn a_variable_rate_source_renders_the_timeline_s_frames_and_length() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("vfr.mp4");
    ff(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "nullsrc=s=64x64:r=25:d=4,geq=lum='16+2*N':cb=128:cr=128,format=yuv420p,\
             select='lt(n\\,25)+not(mod(n\\,5))'",
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-crf",
            "10",
            src.to_str().unwrap(),
        ],
    );
    let probe = ffmpeg::probe(&tools, &src).unwrap();
    let fps = probe.fps.unwrap();
    assert!(fps < 20.0, "a variable rate averaging below 25: {fps}");
    let tick = |s: f64| (s * fps).round() as i64;
    // Timeline: [1.0, 1.4) s, [0, 0.4) s, [2.0, 2.8) s.
    let ranges = [
        (tick(1.0), tick(1.4)),
        (0, tick(0.4)),
        (tick(2.0), tick(2.8)),
    ];
    let spec = EditSpec {
        spans: spans(fps, &ranges, true),
        fps: Some(fps),
        stereo: false,
        crop: None,
        tracks: Vec::new(),
    };
    let out = render(&tools, &src, "out.mp4", &spec);
    let want: i64 = ranges.iter().map(|(a, b)| b - a).sum();
    assert_eq!(
        count_frames(&tools, &out) as i64,
        want,
        "the timeline's frames"
    );
    let d = ffmpeg::probe(&tools, &out).unwrap().duration.unwrap();
    assert!((d - want as f64 / fps).abs() < 0.01, "{d} s");
    let numbers = frame_numbers(&tools, &out);
    // The first piece starts in the sparse part at source frame 25 (1.0 s) and holds no frame
    // from 1.4 s on (frame 35); the last starts at frame 50 (2.0 s) and ends before frame 70.
    assert_eq!(numbers[0], 25, "{numbers:?}");
    let first = (ranges[0].1 - ranges[0].0) as usize;
    assert!(
        numbers[..first].iter().all(|n| (25..35).contains(n)),
        "{numbers:?}"
    );
    let third = first + (ranges[1].1 - ranges[1].0) as usize;
    assert!(
        numbers[third..].iter().all(|n| (50..70).contains(n)),
        "{numbers:?}"
    );
}

/// Each audio track keeps its own tags (language, title) through an edit.
#[test]
fn audio_track_tags_travel_with_the_tracks() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("tagged.mkv");
    ff(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("{MONO}:s=48000:d=4"),
            "-f",
            "lavfi",
            "-i",
            &format!("{TONES}:s=48000:d=4"),
            "-map",
            "0",
            "-map",
            "1",
            "-c:a",
            "flac",
            "-metadata:s:a:0",
            "language=kor",
            "-metadata:s:a:0",
            "title=voice",
            "-metadata:s:a:1",
            "language=eng",
            src.to_str().unwrap(),
        ],
    );
    let probe = ffmpeg::probe(&tools, &src).unwrap();
    let spec = audio_spec(
        &probe,
        spans(48_000.0, &[(96_000, 144_000), (0, 48_000)], false),
    );
    let out = render(&tools, &src, "out.mkv", &spec);
    let tags = Command::new(&tools.ffprobe)
        .args(["-v", "error", "-select_streams", "a"])
        .args([
            "-show_entries",
            "stream_tags=language,title",
            "-of",
            "csv=p=0",
        ])
        .arg(&out)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&tags.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines, ["kor,voice", "eng"], "{text}");
}

/// A filter graph too long for one argument goes to a file — `-/filter_complex` from ffmpeg 7,
/// `-filter_complex_script` before — and the render reads it (forced here on a short edit).
#[test]
fn a_long_filter_graph_is_read_from_a_file() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("c.mp4");
    counter(&tools, &src, "25", 100);
    let probe = ffmpeg::probe(&tools, &src).unwrap();
    let (media, _, plan) = super::plan(&tools, &probe, "c.mp4").unwrap();
    let spec = EditSpec {
        spans: spans(25.0, &[(50, 60), (0, 10)], true),
        fps: Some(25.0),
        stereo: false,
        crop: None,
        tracks: probe.tracks.clone(),
    };
    let dst = tmp.path().join("out.mp4");
    let input = ffmpeg::Input::path(&src);
    let edit = ffmpeg::edit_args(&[&input], &dst, &spec, media, &plan, Encode::Final).unwrap();
    let args = ffmpeg::assemble_inline_up_to(&tools, edit, tmp.path(), "graph.txt", 0).unwrap();
    let joined: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        joined
            .iter()
            .any(|a| a == "-/filter_complex" || a == "-filter_complex_script"),
        "{joined:?}"
    );
    assert!(!joined.iter().any(|a| a == "-filter_complex"), "{joined:?}");
    assert!(tmp.path().join("graph.txt").is_file());
    ffmpeg::run_export(&tools, &args, spec.length(), &mut |_| {}).unwrap();
    let want: Vec<i64> = (50..60).chain(0..10).collect();
    assert_eq!(frame_numbers(&tools, &dst), want);
}
