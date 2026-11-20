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
    let t = &probe.tracks[track];
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

/// [`pcm`] of a track that should hold exactly `n` samples, held to a plain `n`-sample encode by
/// this ffmpeg in the same codec and container: it must decode exactly as long as that file
/// does, and comes back as the `n` samples the container declares. A decoder that leaves the
/// container's trims to the player gives more: ffmpeg 8 decodes Matroska AAC with its 1024
/// priming samples (CodecDelay) and MP4 AAC to the end of its last frame (the edit list's end);
/// ffmpeg 9 applies both, and the plain file decodes to exactly `n`. The trims are written into
/// the file the same way by either.
fn pcm_as_plain(tools: &Tools, file: &Path, track: usize, n: usize) -> (u32, Vec<Vec<f32>>) {
    let (rate, lanes) = pcm(tools, file, track);
    let stream = format!("a:{track}");
    let codec = probe_lines(tools, file, &stream, "stream=codec_name").remove(0);
    let encoder = match codec.as_str() {
        "mp3" => "libmp3lame",
        other => other,
    };
    let ext = file.extension().unwrap().to_str().unwrap();
    let plain = file.with_file_name(format!("plain-{n}-{track}.{ext}"));
    let channels = lanes.len().to_string();
    ff(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("sine=f=1000:r={rate}"),
            "-af",
            &format!("atrim=end_sample={n}"),
            "-ac",
            &channels,
            "-c:a",
            encoder,
            plain.to_str().unwrap(),
        ],
    );
    let (_, reference) = pcm(tools, &plain, 0);
    assert_eq!(
        lanes[0].len(),
        reference[0].len(),
        "{}: track {track} decodes as long as a plain {n}-sample {codec} .{ext} \
         (this ffmpeg decodes that to {} samples)",
        file.display(),
        reference[0].len()
    );
    // What the decoder gives before the stream's time zero: the codec's priming, where the
    // container's start trim is not applied.
    let lead: usize = probe_lines(tools, &plain, "a:0", "frame=pts_time,nb_samples")
        .iter()
        .filter_map(|l| {
            let mut f = l.split(',');
            let pts: f64 = f.next()?.parse().ok()?;
            let samples: usize = f.next()?.parse().ok()?;
            (pts < 0.0).then_some(samples)
        })
        .sum();
    let lanes = lanes
        .into_iter()
        .map(|l| l[lead..lead + n].to_vec())
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
            frames: video.then_some((b - a) as u64),
            ..Span::of(a as f64 / rate, (b - a) as f64 / rate)
        })
        .collect()
}

/// Renders `spec` of `src` into `<dir>/<name>` the way Save and Export do.
fn render(tools: &Tools, src: &Path, name: &str, spec: &EditSpec) -> PathBuf {
    render_max(tools, src, name, spec, ffmpeg::MAX_INPUTS)
}

/// [`render`] with at most `max` inputs per run (forcing passes).
fn render_max(tools: &Tools, src: &Path, name: &str, spec: &EditSpec, max: usize) -> PathBuf {
    let probe = ffmpeg::probe(tools, src).unwrap();
    let (media, _, plan) =
        super::plan(tools, &probe, src.file_name().unwrap().to_str().unwrap()).unwrap();
    let dir = src.parent().unwrap();
    let dst = dir.join(name);
    let work = dir.join(format!("work-{name}"));
    crate::cache::private_dir(&work).unwrap();
    let mut last = 0.0;
    render_edit_with(
        tools,
        &ffmpeg::Input::path(src),
        &dst,
        spec,
        (media, &plan),
        &work,
        &mut |f| {
            assert!(f >= last - 1e-9, "progress never goes back");
            last = f;
        },
        max,
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
        // The timeline's 4 s to the sample: as long as a plain 4 s encode decodes.
        let (r, lanes) = pcm_as_plain(&tools, &out, 0, s(4.0) as usize);
        assert_eq!(lanes.len(), 2, "{name}: still stereo");
        let want = [(880, 1.0), (440, 1.0), (1320, 1.0), (1760, 1.0)];
        assert_tones(&segments(r, &lanes[0]), &want, tolerance, name);
        assert_tones(&segments(r, &lanes[1]), &[(660, 4.0)], tolerance, name);
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

/// The frame numbers a file shows, read from each frame's luma — every frame it holds, once
/// (`passthrough`: raw video output would otherwise be put on a constant-rate grid from the
/// file's start, repeating the first frame of a picture that starts after the sound).
pub(crate) fn frame_numbers(tools: &Tools, file: &Path) -> Vec<i64> {
    let out = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(file)
        .args([
            "-map",
            "0:v:0",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
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

/// `frame_numbers` reads the frames a file holds: where its picture starts more than half a
/// frame after its sound (ffmpeg 8 decodes Matroska AAC that way: its CodecDelay moves the sound
/// 21 ms earlier), the frames are not put back on a constant-rate grid, which repeated the first.
#[test]
fn frame_numbers_reads_the_frames_a_file_holds() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("late.mkv");
    ff(
        &tools,
        &[
            "-itsoffset",
            "0.03",
            "-f",
            "lavfi",
            "-i",
            "nullsrc=s=64x64:r=25:d=1,geq=lum='16+2*N':cb=128:cr=128,format=yuv420p",
            "-f",
            "lavfi",
            "-i",
            "sine=f=1000:r=48000:d=1",
            "-c:v",
            "libx264",
            "-crf",
            "10",
            "-c:a",
            "pcm_s16le",
            src.to_str().unwrap(),
        ],
    );
    assert_eq!(count_frames(&tools, &src), 25);
    assert_eq!(frame_numbers(&tools, &src), (0..25).collect::<Vec<_>>());
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
        // The sound is as long as the picture, to the sample (48 kHz).
        let samples = (f64::from(n) / rate * 48_000.0).round() as usize;
        let (r, lanes) = pcm_as_plain(&tools, &out, 0, samples);
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
    for (track, source) in probe.tracks.iter().enumerate().take(2) {
        // Two seconds at the track's own rate. AAC in Matroska carries no end trim: a plain
        // 2 s file's last 1024-sample frame decodes whole too, and so must this one.
        let rate = source.rate.unwrap();
        let (r, lanes) = pcm_as_plain(&tools, &out, track, 2 * rate as usize);
        assert_eq!(r, rate, "track {track} rate");
        // The cut between the pieces is exact.
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
    assert!(
        ffmpeg::edit_passes(&spec, ffmpeg::MAX_INPUTS)
            .unwrap()
            .len()
            > 1,
        "passes"
    );
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

/// A render whose ffmpeg prints megabytes of errors — thousands of lines, then one 3 MB line
/// that never ends — is read to its end and fails saying the end of what it printed, a few
/// kilobytes at most.
#[test]
fn a_noisy_render_fails_with_its_last_words_bounded() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("ffmpeg");
    std::fs::write(
        &fake,
        "#!/bin/sh\n[ \"$1\" = -version ] && exit 0\n\
         i=0; while [ $i -lt 2000 ]; do echo \"noise $i\" >&2; i=$((i+1)); done\n\
         head -c 3000000 /dev/zero | tr '\\0' x >&2\nprintf 'the end' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let tools = Tools {
        ffmpeg: fake.clone(),
        ffprobe: fake,
    };
    crate::testutil::run_fresh(Command::new(&tools.ffmpeg).arg("-version"));
    let err = ffmpeg::run_export(&tools, &["out.mp4".into()], 4.0, &mut |_| {}).unwrap_err();
    let text = format!("{err:#}");
    assert!(
        text.ends_with("xxxthe end"),
        "{}",
        &text[..80.min(text.len())]
    );
    assert!(text.len() < 8192, "{} bytes", text.len());
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
/// the frame-rate grid from its own start, as the preview shows it — every frame of every piece
/// is the one the grid puts there (the latest source frame at or before it, a source frame
/// taking the grid step its time rounds to), the output as long as the timeline says.
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
    // The source's frames: (number, time).
    let source: Vec<(i64, f64)> = (0..100i64)
        .filter(|n| *n < 25 || n % 5 == 0)
        .map(|n| (n, n as f64 / 25.0))
        .collect();
    let probe = ffmpeg::probe(&tools, &src).unwrap();
    let fps = probe.fps.unwrap();
    assert!(fps < 20.0, "a variable rate averaging below 25: {fps}");
    let tick = |s: f64| (s * fps).round() as i64;
    // Timeline: [1.0, 1.4) s, [0, 0.4) s, [2.0, 2.8) s, in ticks.
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
    // What each piece shows: for output step k, the latest source frame from half a step
    // before the piece's start on whose step (its time from the start, rounded) is ≤ k; before
    // the first such frame, the first one (held).
    let mut want: Vec<i64> = Vec::new();
    for &(a, b) in &ranges {
        let start = a as f64 / fps;
        let kept: Vec<(i64, i64)> = source
            .iter()
            .filter(|(_, t)| *t >= start - 0.5 / fps - 1e-9)
            .map(|&(n, t)| (n, ((t - start) * fps).round() as i64))
            .collect();
        for k in 0..(b - a) {
            let n = kept
                .iter()
                .rfind(|(_, step)| *step <= k)
                .unwrap_or(&kept[0])
                .0;
            want.push(n);
        }
    }
    assert_eq!(frame_numbers(&tools, &out), want, "every piece's frames");
    let d = ffmpeg::probe(&tools, &out).unwrap().duration.unwrap();
    assert!((d - want.len() as f64 / fps).abs() < 0.01, "{d} s");
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

/// Times (seconds) where the first channel of `file`'s track `track` goes from silence to a
/// burst (|x| > 0.3 after 10 ms below it), timed from the track's first decoded frame (as
/// [`burst_offsets`] does).
fn bursts(tools: &Tools, file: &Path, track: usize) -> (Vec<f64>, f64) {
    let (rate, lanes) = pcm(tools, file, track);
    let start = probe_list(tools, file, &format!("a:{track}"), "frame=pts_time")[0];
    let x = &lanes[0];
    let quiet = (rate / 100) as usize;
    let on: Vec<f64> = (1..x.len())
        .filter(|&i| {
            x[i].abs() > 0.3 && x[i.saturating_sub(quiet)..i].iter().all(|v| v.abs() < 0.3)
        })
        .map(|i| start + i as f64 / f64::from(rate))
        .collect();
    (on, x.len() as f64 / f64::from(rate))
}

/// A track that starts later than the picture, and one that does not: every stream of a piece
/// is moved by the same amount, so a burst at source second 1 or 2 lands where its frames land
/// in both tracks, and the late track's missing start is silence, not sound pulled forward.
#[test]
fn a_late_starting_track_keeps_its_offset_from_the_picture_and_the_other_track() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("late.mov");
    // Bursts of 1 kHz at whole source seconds; track 0 starts at 0.5 s (its content time
    // u is source time u + 0.5), track 1 at 0.
    ff(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "nullsrc=s=64x64:r=25:d=4,geq=lum='16+2*N':cb=128:cr=128,format=yuv420p",
            "-itsoffset",
            "0.5",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc='if(lt(mod(t+0.5\\,1)\\,0.05)\\,0.8*sin(2*PI*1000*t)\\,0)':s=48000:d=3.5",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc='if(lt(mod(t\\,1)\\,0.05)\\,0.8*sin(2*PI*1000*t)\\,0)':s=48000:d=4",
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
            "pcm_s16le",
            src.to_str().unwrap(),
        ],
    );
    let probe = ffmpeg::probe(&tools, &src).unwrap();
    assert_eq!(probe.audio_streams, 2);
    // Output: source [2.0, 2.4) then [0.2, 1.4): bursts of source seconds 2 and 1 at output
    // 0.0 and 1.2; the late track is silent for output 0.4–0.7 (source 0.2–0.5).
    let ranges = [(50, 60), (5, 35)];
    let spec = EditSpec {
        spans: spans(25.0, &ranges, true),
        fps: Some(25.0),
        stereo: false,
        crop: None,
        tracks: probe.tracks.clone(),
    };
    let out = render(&tools, &src, "out.mov", &spec);
    let want: Vec<i64> = ranges.iter().flat_map(|&(a, b)| a..b).collect();
    assert_eq!(frame_numbers(&tools, &out), want);
    for track in [0, 1] {
        let (on, len) = bursts(&tools, &out, track);
        assert!((len - 1.6).abs() < 0.03, "track {track}: {len} s");
        assert_eq!(on.len(), 2, "track {track}: {on:?}");
        assert!(on[0].abs() < 0.005, "track {track}: {on:?}");
        assert!((on[1] - 1.2).abs() < 0.005, "track {track}: {on:?}");
    }
}

/// The same edit rendered in one run and in lossless passes (a frame at 30000/1001 fps is
/// 1601.6 samples at 48 kHz: every piece's sample count is a rounding) holds the same frames
/// and the same sound, sample for sample — and as many samples as the whole timeline's length.
#[test]
fn one_run_and_passes_render_the_same_frames_and_samples() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("ntsc.mp4");
    counter(&tools, &src, "30000/1001", 120);
    let probe = ffmpeg::probe(&tools, &src).unwrap();
    let fps = probe.fps.unwrap();
    // Out of order pieces of 1, 3, 5, 7 … frames.
    let ranges: Vec<(i64, i64)> = (0..8i64)
        .map(|k| {
            let a = 110 - 13 * k;
            (a, a + 1 + 2 * (k % 4))
        })
        .collect();
    let spec = EditSpec {
        spans: spans(fps, &ranges, true),
        fps: Some(fps),
        stereo: false,
        crop: None,
        tracks: probe.tracks.clone(),
    };
    assert_eq!(spec.groups().len(), 8);
    let one = render_max(&tools, &src, "one.mp4", &spec, 24);
    let passes = render_max(&tools, &src, "passes.mp4", &spec, 3);
    assert_eq!(frame_numbers(&tools, &one), frame_numbers(&tools, &passes));
    let want: Vec<i64> = ranges.iter().flat_map(|&(a, b)| a..b).collect();
    assert_eq!(frame_numbers(&tools, &one), want);
    // Both the timeline's samples (as long as a plain encode of that many decodes).
    let frames: i64 = ranges.iter().map(|(a, b)| b - a).sum();
    let samples = (frames as f64 / fps * 48_000.0).round() as usize;
    let (_, a) = pcm_as_plain(&tools, &one, 0, samples);
    let (_, b) = pcm_as_plain(&tools, &passes, 0, samples);
    assert!(a == b, "the same sound, sample for sample");
}

/// Raw samples written as a WAV of `codec` (`pcm_s32le`, `pcm_f64le`) from `bytes` of
/// `format` (`s32le`, `f64le`), mono at 48 kHz.
fn raw_wav(
    tools: &Tools,
    dir: &Path,
    name: &str,
    bytes: &[u8],
    format: &str,
    codec: &str,
) -> PathBuf {
    let raw = dir.join(format!("{name}.raw"));
    std::fs::write(&raw, bytes).unwrap();
    let wav = dir.join(name);
    ff(
        tools,
        &[
            "-f",
            format,
            "-ar",
            "48000",
            "-ac",
            "1",
            "-i",
            raw.to_str().unwrap(),
            "-c:a",
            codec,
            wav.to_str().unwrap(),
        ],
    );
    wav
}

fn raw_of(tools: &Tools, file: &Path, format: &str) -> Vec<u8> {
    let out = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(file)
        .args(["-f", format, "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
}

/// A multi-pass render keeps 32-bit integer and 64-bit float samples exactly: its intermediate
/// holds each track in its own sample format (a float intermediate would round both).
#[test]
fn passes_keep_32_bit_and_64_bit_samples_exactly() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let n = 48_000 * 2;
    let ints: Vec<u8> = (0..n)
        .flat_map(|_| ((next() >> 32) as i32 / 2).to_le_bytes())
        .collect();
    let floats: Vec<u8> = (0..n)
        .flat_map(|_| ((next() >> 11) as f64 / (1u64 << 53) as f64 - 0.5).to_le_bytes())
        .collect();
    for (name, bytes, format, codec, width) in [
        ("s32.wav", ints, "s32le", "pcm_s32le", 4),
        ("f64.wav", floats, "f64le", "pcm_f64le", 8),
    ] {
        let src = raw_wav(&tools, tmp.path(), name, &bytes, format, codec);
        let probe = ffmpeg::probe(&tools, &src).unwrap();
        // Five out-of-order pieces of 0.1 s: passes of at most two inputs.
        let ranges: Vec<(i64, i64)> = [9, 2, 7, 0, 5]
            .iter()
            .map(|k| (k * 9_600, k * 9_600 + 4_800))
            .collect();
        let spec = EditSpec {
            spans: spans(48_000.0, &ranges, false),
            fps: None,
            stereo: false,
            crop: None,
            tracks: probe.tracks.clone(),
        };
        let out = render_max(&tools, &src, &format!("out-{name}"), &spec, 2);
        let got = raw_of(&tools, &out, format);
        let want: Vec<u8> = ranges
            .iter()
            .flat_map(|&(a, b)| bytes[a as usize * width..b as usize * width].to_vec())
            .collect();
        assert_eq!(got.len(), want.len(), "{name}: length");
        assert!(got == want, "{name}: every sample exactly");
    }
}

/// Where `clip` best matches `full` around sample `at` (lag in samples, ±`reach`), and the
/// largest difference there (over every 97th sample).
fn best_lag(full: &[f32], clip: &[f32], at: usize, reach: i64) -> (i64, f32) {
    (-reach..=reach)
        .map(|lag| {
            let o = (at as i64 + lag) as usize;
            let worst = (2_000..clip.len().min(full.len() - o) - 1)
                .step_by(97)
                .fold(0f32, |m, i| m.max((clip[i] - full[o + i]).abs()));
            (lag, worst)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap()
}

/// `sopen cut` of AAC is Out − In long and its sound is where it was in the source: the old
/// input-side `-ss`/`-to` cut started 1002 samples (22.7 ms) early.
#[test]
fn an_aac_cut_is_where_it_was_in_the_source() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("t.m4a");
    // A chirp: no two stretches alike, so there is one place the cut matches.
    let chirp = "aevalsrc='0.5*sin(2*PI*(200*t+300*t*t))'";
    tones(
        &tools,
        &src,
        chirp,
        44_100,
        &["-c:a", "aac", "-b:a", "192k"],
    );
    let published = cut_and_publish(
        &mut crate::remote::LocalFs::new(),
        &tools,
        &src,
        tmp.path().to_str().unwrap(),
        "t.m4a",
        &CutRequest {
            in_s: 1.0,
            out_s: 3.0,
            crop: None,
        },
        &tmp.path().join("work"),
        &mut |_| {},
    )
    .unwrap();
    // 2 s and 4 s to the sample, as far as this ffmpeg decodes AAC in MP4.
    let (_, cut) = pcm_as_plain(&tools, &tmp.path().join(&published.name), 0, 88_200);
    let (_, full) = pcm_as_plain(&tools, &src, 0, 176_400);
    let (lag, worst) = best_lag(&full[0], &cut[0], 44_100, 2_500);
    assert_eq!(
        lag, 0,
        "the cut's sound is {lag} samples off (difference {worst})"
    );
    assert!(worst < 0.1, "re-encoded, still the source's sound: {worst}");
}

/// A 4 s, 25 fps frame counter with 1 kHz bursts at whole source seconds on one AAC track, the
/// picture starting `video_late` s and the sound `audio_late` s into the file (`-itsoffset`),
/// in a `.ext` container.
pub(crate) fn offset_fixture(tools: &Tools, dst: &Path, video_late: f64, audio_late: f64) {
    let burst = format!(
        "aevalsrc='if(lt(mod(t+{audio_late}\\,1)\\,0.05)\\,0.8*sin(2*PI*1000*t)\\,0)':s=48000:d={}",
        4.0 - audio_late
    );
    let video = format!(
        "nullsrc=s=64x64:r=25:d={},geq=lum='16+2*(N+{})':cb=128:cr=128,format=yuv420p",
        4.0 - video_late,
        (video_late * 25.0).round()
    );
    ff(
        tools,
        &[
            "-itsoffset",
            &video_late.to_string(),
            "-f",
            "lavfi",
            "-i",
            &video,
            "-itsoffset",
            &audio_late.to_string(),
            "-f",
            "lavfi",
            "-i",
            &burst,
            "-map",
            "0",
            "-map",
            "1",
            "-c:v",
            "libx264",
            "-crf",
            "10",
            "-g",
            "25",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            dst.to_str().unwrap(),
        ],
    );
}

/// `ffprobe`'s lines (comma-separated fields) of `entries` for `stream`.
fn probe_lines(tools: &Tools, file: &Path, stream: &str, entries: &str) -> Vec<String> {
    let out = Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_entries",
            entries,
        ])
        .args(["-of", "csv=p=0"])
        .arg(file)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().trim_end_matches(',').to_string())
        .collect()
}

/// `ffprobe` numbers, one per line of `entries` for `stream`.
fn probe_list(tools: &Tools, file: &Path, stream: &str, entries: &str) -> Vec<f64> {
    probe_lines(tools, file, stream, entries)
        .iter()
        .filter_map(|l| l.parse().ok())
        .collect()
}

/// Each burst's time against the picture, in the file's own clock (each stream's timestamps,
/// so a container that starts its streams apart is measured as it plays): `(source second,
/// ms the burst comes after the frame showing that second)`. The sound's samples are timed from
/// its first decoded frame, so priming a decoder leaves in (not trimmed) sits before the
/// stream's start, where the container puts it.
fn burst_offsets(tools: &Tools, file: &Path) -> Vec<(i64, f64)> {
    let pts = probe_list(tools, file, "v:0", "frame=pts_time");
    let numbers = frame_numbers(tools, file);
    assert_eq!(pts.len(), numbers.len(), "{}", file.display());
    let start = probe_list(tools, file, "a:0", "frame=pts_time")[0];
    let (rate, lanes) = pcm(tools, file, 0);
    let x = &lanes[0];
    let quiet = (rate / 5) as usize;
    let on: Vec<f64> = (1..x.len())
        .filter(|&i| {
            x[i].abs() > 0.3 && x[i.saturating_sub(quiet)..i].iter().all(|v| v.abs() < 0.3)
        })
        .map(|i| start + i as f64 / f64::from(rate))
        .collect();
    on.iter()
        .filter_map(|&t| {
            // The frame on screen at the burst, and the second it shows.
            let k = pts.iter().rposition(|p| *p <= t + 0.02)?;
            let second = (numbers[k] as f64 / 25.0).round() as i64;
            let at = pts[numbers.iter().position(|n| *n == second * 25)?];
            Some((second, (t - at) * 1000.0))
        })
        .collect()
}

/// Streams that start apart in the source — the sound late, or the picture late — keep their
/// offset through an edit in every container Save writes: every burst lands on the frame of
/// its second (the late stream's missing start is silence, or the first picture held).
#[test]
fn streams_that_start_apart_stay_in_sync_in_every_container() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let mut wrong = Vec::new();
    for ext in ["mp4", "mov", "mkv", "ts"] {
        for (video_late, audio_late) in [(0.0, 0.48), (0.2, 0.0)] {
            let name = format!("v{video_late}-a{audio_late}.{ext}");
            let src = tmp.path().join(&name);
            offset_fixture(&tools, &src, video_late, audio_late);
            let probe = ffmpeg::probe(&tools, &src).unwrap();
            // [2.8, 3.6) then [0, 1.6): the bursts of seconds 3 and 1 (second 0 has no frame
            // when the picture starts late, no burst when the sound does). The frame slots are
            // where the window puts them: from the picture's offset within a frame period.
            let origin = probe.frame_origin();
            let pieces: Vec<Span> = spans(25.0, &[(70, 90), (0, 40)], true)
                .into_iter()
                .map(|s| Span {
                    start: s.start + origin,
                    ..s
                })
                .collect();
            let spec = EditSpec {
                spans: pieces,
                fps: Some(25.0),
                stereo: false,
                crop: None,
                tracks: probe.tracks.clone(),
            };
            let out = render(&tools, &src, &format!("out-{name}"), &spec);
            let got = burst_offsets(&tools, &out);
            let seconds: Vec<i64> = got.iter().map(|g| g.0).collect();
            let want: Vec<i64> = vec![3, 1];
            let off = got.iter().any(|g| g.1.abs() > 3.0);
            if seconds != want || off {
                wrong.push(format!("{name}: {got:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "out of sync: {wrong:#?}");
}

/// A render's ffmpeg dies with sopen: the test runs a render of a stand-in ffmpeg (which
/// prints progress forever and, as ffmpeg does, survives its progress pipe closing) in a child
/// process, kills that process with SIGKILL — nothing can be caught — and the stand-in is gone
/// within a few seconds, not rendering on as an orphan.
#[test]
fn a_render_s_ffmpeg_dies_with_its_sopen_even_on_sigkill() {
    const CHILD: &str = "SOPEN_TEST_ORPHAN_CHILD";
    if let Ok(dir) = std::env::var(CHILD) {
        // The child: run the stand-in until killed.
        let dir = PathBuf::from(dir);
        let tools = Tools {
            ffmpeg: dir.join("ffmpeg"),
            ffprobe: dir.join("ffmpeg"),
        };
        let _ = ffmpeg::run_watched(
            &tools,
            &["out.mp4".into()],
            4.0,
            &mut |_| {},
            std::time::Duration::from_secs(600),
        );
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("ffmpeg");
    let pid_file = tmp.path().join("pid");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n[ \"$1\" = -version ] && exit 0\ntrap '' PIPE\necho $$ > '{}'\n\
             i=0; while :; do i=$((i+1)); printf 'out_time_us=%d\\nprogress=continue\\n' $i; \
             sleep 0.05; done\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::testutil::run_fresh(Command::new(&fake).arg("-version"));
    let test = "cut::edit_tests::a_render_s_ffmpeg_dies_with_its_sopen_even_on_sigkill";
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, tmp.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let waited = std::time::Instant::now();
    let pid = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|t| t.trim().parse::<i32>().ok())
        {
            break pid;
        }
        assert!(
            waited.elapsed().as_secs() < 30,
            "the stand-in never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;
    assert!(alive(pid));
    child.kill().unwrap(); // SIGKILL
    let _ = child.wait();
    let killed = std::time::Instant::now();
    while alive(pid) {
        if killed.elapsed().as_secs() > 10 {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            panic!("the stand-in ffmpeg outlived its sopen");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
