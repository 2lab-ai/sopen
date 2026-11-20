//! The decoder chains (spec §7, message 13): one grid over the whole play, so pictures and
//! sound meet at every boundary; a short stream padded in bounded blocks, a stop padding
//! nothing; a failing decoder an error, not silence; the boundary receipt taken at the first
//! output; and the pictures of every piece, in order, on a clock the test drives.

use super::*;
use crate::ffmpeg::Input;

fn tools_or_skip() -> Option<Tools> {
    match crate::ffmpeg::resolve() {
        Ok(t) => Some(t),
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping chain test: {e}");
            None
        }
    }
}

fn ff(tools: &Tools, args: &[&str]) {
    let ok = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "fixture {args:?}");
}

/// A frame counter (frame N's luma 16 + 2N) at `rate`, `frames` long, with a 440 Hz track.
fn counter(tools: &Tools, dst: &Path, rate: &str, frames: u32) {
    ff(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("nullsrc=s=64x64:r={rate}:d=10,geq=lum='16+2*N':cb=128:cr=128,format=yuv420p"),
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=10",
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

/// A frame's number from its gray (R = (Y − 16) · 255 / 219, Y = 16 + 2N).
fn number(rgba: &[u8]) -> i64 {
    (f64::from(rgba[0]) * 219.0 / 255.0 / 2.0).round() as i64
}

#[test]
fn parts_lay_one_grid_over_the_play() {
    let fps = 25.0;
    let pieces: Vec<Piece> = (0..6)
        .map(|k| Piece {
            start: f64::from(k) * 0.4,
            len: 1.5 / fps,
        })
        .collect();
    let counts: Vec<Option<u64>> = parts(&pieces, fps).iter().map(|p| p.count).collect();
    assert_eq!(counts, [2, 1, 2, 1, 2, 1].map(Some), "not 2, 2, 2 …");
    // A play position that leaves a third of a frame of its piece: none from it.
    let seek = [
        Piece {
            start: 1.0,
            len: 0.3 / fps,
        },
        Piece {
            start: 3.0,
            len: 1.0,
        },
    ];
    let got: Vec<Option<u64>> = parts(&seek, fps).iter().map(|p| p.count).collect();
    assert_eq!(got, [Some(0), Some(25)]);
    // To the end of the file: no count.
    let whole = [Piece {
        start: 2.0,
        len: f64::INFINITY,
    }];
    assert_eq!(parts(&whole, fps)[0].count, None);
}

/// Collects the PCM `decode_pcm` gives for `pieces`, at `rate` × 2; with `stop_after`, the
/// consumer stops once it has that many samples. Returns the samples, the chain's state and
/// the biggest block handed over.
fn decode_all(
    tools: &Tools,
    input: &Input,
    pieces: &[Piece],
    rate: u32,
    stop_after: Option<usize>,
) -> (Vec<f32>, Arc<ChainState>, usize) {
    let live: Live = Arc::default();
    let stop = AtomicBool::new(false);
    let state = Arc::new(ChainState::default());
    let mut got: Vec<f32> = Vec::new();
    let mut biggest = 0usize;
    let mut out = |s: &[f32]| -> bool {
        biggest = biggest.max(s.len());
        got.extend_from_slice(s);
        if stop_after.is_some_and(|n| got.len() >= n) {
            stop.store(true, SeqCst);
            return false;
        }
        true
    };
    decode_pcm(
        tools,
        input,
        pieces,
        false,
        (rate, 2),
        None,
        (&live, &stop, &state),
        &mut out,
    );
    (got, state, biggest)
}

/// A hundred one-frame pieces at 30000/1001 fps (1601.6 samples each at 48 kHz): the sound is
/// exactly as long as the hundred frames — not a sample per piece more (each rounded alone:
/// 160 200) — so picture and sound still meet at the last boundary.
#[test]
fn sound_on_fractional_frame_pieces_does_not_drift_from_the_picture() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("n.mp4");
    counter(&tools, &src, "30000/1001", 200);
    let fps = 30000.0 / 1001.0;
    let pieces: Vec<Piece> = (0..100)
        .map(|k| Piece {
            start: f64::from((k * 7) % 150) / fps,
            len: 1.0 / fps,
        })
        .collect();
    let (pcm, state, _) = decode_all(&tools, &Input::path(&src), &pieces, 48_000, None);
    assert_eq!(state.error(), None);
    assert_eq!(pcm.len() / 2, 160_160, "round(100 / fps × 48000)");
    let frames: u64 = parts(&pieces, fps).iter().map(|p| p.count.unwrap()).sum();
    assert_eq!(frames, 100);
}

/// A stand-in ffmpeg (a shell script) written to `dir`: `body` runs with its arguments.
fn stand_in(dir: &Path, body: &str) -> Tools {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("ffmpeg");
    std::fs::write(
        &path,
        format!("#!/bin/sh\n[ \"$1\" = -version ] && exit 0\n{body}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Just written, it can be busy for a moment (Linux): started once to its end first.
    crate::testutil::run_fresh(Command::new(&path).arg("-version"));
    Tools {
        ffmpeg: path.clone(),
        ffprobe: path,
    }
}

/// 1000 samples of 1.0 (f32le) on stdout.
const ONES: &str = "i=0; while [ $i -lt 250 ]; do \
    printf '\\000\\000\\200\\077\\000\\000\\200\\077\\000\\000\\200\\077\\000\\000\\200\\077'; \
    i=$((i+1)); done";

/// A decoder whose output ends early, by itself: the rest of its piece is silence, given in
/// blocks of at most `PAD_CHUNK` samples (an hour-long piece costs no hour of memory), and a
/// stop during the padding ends it there.
#[test]
fn a_short_stream_is_padded_in_bounded_blocks_until_a_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = stand_in(tmp.path(), &format!("{ONES}; exit 0"));
    let hour = [Piece {
        start: 0.0,
        len: 3600.0,
    }];
    let (pcm, state, biggest) = decode_all(
        &tools,
        &Input::path(tmp.path()),
        &hour,
        48_000,
        Some(200_000),
    );
    assert_eq!(state.error(), None, "a short stream is no failure");
    assert!(pcm[..1000].iter().all(|v| *v == 1.0), "what it decoded");
    assert!(pcm[1000..].iter().all(|v| *v == 0.0), "then silence");
    assert!(biggest <= PAD_CHUNK, "padded in blocks: {biggest}");
    assert!(
        pcm.len() < 200_000 + PAD_CHUNK,
        "stopped: {} samples",
        pcm.len()
    );
}

/// A stop that lands while a decoder is ending (playback closed: its decoders are killed and
/// their output ends) pads nothing at all.
#[test]
fn a_stop_pads_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = stand_in(tmp.path(), &format!("{ONES}; exit 0"));
    let piece = Part {
        start: 0.0,
        count: Some(48_000 * 3600),
    };
    let stop = AtomicBool::new(false);
    let live: Live = Arc::default();
    let state = ChainState::default();
    // The decoder's output ends (as when it is killed) after the stop is set: no padding.
    let mut calls = 0usize;
    let mut out = |s: &[f32]| {
        calls += 1;
        assert!(s.iter().all(|v| *v == 1.0), "padding after a stop");
        stop.store(true, SeqCst);
        true
    };
    let mut pcm = Pcm {
        channels: 2,
        stop: &stop,
        out: &mut out,
        bytes: vec![0u8; 1024],
        samples: Vec::new(),
    };
    let input = Input::path(tmp.path());
    let start = |p: &Part| audio_cmd(&tools, &input, p, false, 48_000, 2);
    chain(vec![piece], start, &live, &stop, &state, &mut pcm);
    drop(pcm);
    assert!(calls > 0, "it read the decoder's output");
    assert_eq!(state.error(), None);
    // And `pad` itself checks the stop first.
    let mut out = |_: &[f32]| -> bool { panic!("padded after a stop") };
    let mut pcm = Pcm {
        channels: 2,
        stop: &stop,
        out: &mut out,
        bytes: vec![0u8; 1024],
        samples: Vec::new(),
    };
    assert!(!pcm.pad(&piece, 1000));
}

/// A decoder that fails — exits with an error after some output, or cannot be started at all —
/// stops the chain with the reason, rather than ending in silence as a short stream does.
#[test]
fn a_failing_decoder_is_an_error_not_silence() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = stand_in(
        tmp.path(),
        &format!("{ONES}; echo 'boom: corrupt packet' >&2; exit 1"),
    );
    let pieces = [
        Piece {
            start: 0.0,
            len: 1.0,
        },
        Piece {
            start: 5.0,
            len: 1.0,
        },
    ];
    let (pcm, state, _) = decode_all(&tools, &Input::path(tmp.path()), &pieces, 48_000, None);
    let error = state.error().expect("a failure");
    assert!(error.contains("boom: corrupt packet"), "{error}");
    assert!(error.contains("piece at 0.00 s"), "{error}");
    assert_eq!(
        pcm.len(),
        1000,
        "no padding, no second piece: {}",
        pcm.len()
    );
    // An ffmpeg that cannot be started.
    let gone = Tools {
        ffmpeg: tmp.path().join("no-such-ffmpeg"),
        ffprobe: tmp.path().join("no-such-ffprobe"),
    };
    let (pcm, state, _) = decode_all(&gone, &Input::path(tmp.path()), &pieces, 48_000, None);
    let error = state.error().expect("a failure");
    assert!(error.contains("cannot start ffmpeg"), "{error}");
    assert!(pcm.is_empty());
}

/// A decoder that prints megabytes of errors — thousands of lines, then a 3 MB line, then
/// its last words — before failing is read to its end (it never blocks on a full pipe) and the
/// chain's error is that last line; what is kept is bounded ([`ErrorTail`]).
#[test]
fn a_noisy_decoder_s_error_is_its_last_words_bounded() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = stand_in(
        tmp.path(),
        &format!(
            "{ONES}; i=0; while [ $i -lt 2000 ]; do echo \"noise $i\" >&2; i=$((i+1)); done; \
             head -c 3000000 /dev/zero | tr '\\0' x >&2; printf '\\nthe end' >&2; exit 1"
        ),
    );
    let piece = [Piece {
        start: 0.0,
        len: 1.0,
    }];
    let (pcm, state, _) = decode_all(&tools, &Input::path(tmp.path()), &piece, 48_000, None);
    let error = state.error().expect("a failure");
    assert!(
        error.ends_with(": the end"),
        "{}",
        &error[..120.min(error.len())]
    );
    assert!(error.len() < 8192, "{} bytes", error.len());
    assert_eq!(pcm.len(), 1000);
}

/// `ps`'s state of process `pid` (`Z` for a zombie), or `None` once it is gone.
fn process_state(pid: &str) -> Option<String> {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!state.is_empty()).then_some(state)
}

/// However a piece's reading ends while its decoder still runs — the consumer stopped, reading
/// failed, or its output ended with the stop flag set (as a playback dropped mid-piece sets it)
/// — the chain kills and waits for that decoder itself: no ffmpeg left running, none left a
/// zombie. Sequential: a drop's own kill running at the same time is not exercised here. The
/// stand-in ignores SIGPIPE, as ffmpeg does while it decodes.
#[test]
fn the_chain_kills_and_reaps_a_decoder_however_its_piece_ends() {
    struct Ends<'a> {
        fed: Fed,
        stop: &'a AtomicBool,
        pids: &'a Path,
    }
    impl Consumer for Ends<'_> {
        fn read(&mut self, _: &mut dyn Read, _: &Part, _: &mut dyn FnMut()) -> Fed {
            // The decoder is up (it wrote its pid) before the read ends.
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.pids.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if matches!(self.fed, Fed::Stopped | Fed::Ended { .. }) {
                self.stop.store(true, SeqCst);
            }
            self.fed.clone()
        }
        fn pad(&mut self, _: &Part, _: u64) -> bool {
            false
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let pids = tmp.path().join("pid");
    let tools = stand_in(
        tmp.path(),
        &format!(
            "trap '' PIPE; echo $$ > '{}.tmp'; mv '{0}.tmp' '{0}'; while :; do sleep 0.05; done",
            pids.display()
        ),
    );
    for fed in [
        Fed::Stopped,
        Fed::Failed("the pipe broke".into()),
        Fed::Ended { given: 0 },
    ] {
        let _ = std::fs::remove_file(&pids);
        let live: Live = Arc::default();
        let stop = AtomicBool::new(false);
        let state = ChainState::default();
        let mut consumer = Ends {
            fed: fed.clone(),
            stop: &stop,
            pids: &pids,
        };
        let input = Input::path(tmp.path());
        let part = Part {
            start: 0.0,
            count: Some(48_000),
        };
        let start = |p: &Part| audio_cmd(&tools, &input, p, false, 48_000, 2);
        chain(vec![part], start, &live, &stop, &state, &mut consumer);
        let pid = std::fs::read_to_string(&pids).unwrap();
        assert_eq!(
            process_state(pid.trim()),
            None,
            "{fed:?}: the decoder is still there (running, or a zombie)"
        );
        assert!(live.lock().unwrap().is_empty(), "{fed:?}");
    }
}

/// The boundary receipt runs from the moment the piece before stopped giving output — its
/// padding too, for a stream that ended short — to the next piece's first output: a long
/// padding (300 ms here) is not counted as the next decoder's wait.
#[test]
fn the_boundary_wait_starts_after_the_padding() {
    struct SlowPad;
    impl Consumer for SlowPad {
        fn read(&mut self, out: &mut dyn Read, _: &Part, first: &mut dyn FnMut()) -> Fed {
            let mut buf = [0u8; 4096];
            let mut given = 0u64;
            loop {
                match out.read(&mut buf) {
                    Ok(0) => return Fed::Ended { given },
                    Ok(n) => {
                        if given == 0 {
                            first();
                        }
                        given += n as u64 / 8;
                    }
                    Err(e) => return Fed::Failed(e.to_string()),
                }
            }
        }
        fn pad(&mut self, _: &Part, _: u64) -> bool {
            std::thread::sleep(Duration::from_millis(300));
            true
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let tools = stand_in(tmp.path(), &format!("{ONES}; exit 0"));
    let part = Part {
        start: 0.0,
        count: Some(48_000),
    };
    let live: Live = Arc::default();
    let stop = AtomicBool::new(false);
    let state = ChainState::default();
    let input = Input::path(tmp.path());
    let start = |p: &Part| audio_cmd(&tools, &input, p, false, 48_000, 2);
    chain(vec![part, part], start, &live, &stop, &state, &mut SlowPad);
    assert_eq!(state.error(), None);
    let waits = state.waits.lock().unwrap().clone();
    assert_eq!(waits.len(), 1);
    assert!(
        waits[0] < Duration::from_millis(150),
        "the padding was counted: {waits:?}"
    );
}

/// The boundary receipt is taken at a piece's first output, not before it is read: `first`
/// is called only once the decoder's output has given its first samples (a read that marks
/// itself done shows the order, whatever the timing).
#[test]
fn the_boundary_wait_is_taken_at_the_first_output() {
    struct Marks<'a>(&'a std::cell::Cell<bool>, usize);
    impl Read for Marks<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.0.set(true);
            let n = buf.len().min(self.1);
            buf[..n].fill(0);
            self.1 -= n;
            Ok(n)
        }
    }
    let read = std::cell::Cell::new(false);
    let stop = AtomicBool::new(false);
    let mut out = |_: &[f32]| true;
    let mut pcm = Pcm {
        channels: 2,
        stop: &stop,
        out: &mut out,
        bytes: vec![0u8; 64],
        samples: Vec::new(),
    };
    let mut calls = 0;
    let mut first = || {
        assert!(read.get(), "the receipt came before the first output");
        calls += 1;
    };
    let part = Part {
        start: 0.0,
        count: Some(100),
    };
    let fed = pcm.read(&mut Marks(&read, 800), &part, &mut first);
    assert_eq!(fed, Fed::Done);
    assert_eq!(calls, 1, "once per piece");
}

/// Plays `pieces` (picture only) on a clock the test moves one frame at a time, only once the
/// next frame is decoded: every frame shown, none dropped, whatever the machine's speed.
/// Returns `(pts, number)` of each frame shown.
fn play_on_a_driven_clock(
    tools: &Tools,
    src: &Path,
    pieces: Vec<Piece>,
    fps: f64,
    duration: f64,
) -> Vec<(f64, i64)> {
    let source = Source {
        input: Input::path(src),
        duration,
        video: Some(((64, 64), fps)),
        audio: false,
        pieces,
        stereo: false,
    };
    let mut player = Player::start(tools, &source, 0.0, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut now = Instant::now();
    let mut seen = Vec::new();
    loop {
        let p = player.pump(now);
        assert_eq!(p.error, None);
        if let Some(f) = p.frame {
            seen.push((f.pts, number(&f.rgba)));
        }
        if p.ended {
            break;
        }
        assert!(Instant::now() < deadline, "stuck after {seen:?}");
        if player.started_at.is_some() && player.frame_ready() {
            now += Duration::from_secs_f64(1.0 / fps);
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(player.stats().1, 0, "no frame dropped");
    seen
}

/// Every piece's pictures, in the timeline's order, each at its place: the three pieces of a
/// cut-and-paste, frame for frame, with pts = index / fps.
#[test]
fn every_piece_s_frames_play_in_order_at_their_times() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("c.mp4");
    counter(&tools, &src, "25", 100);
    let piece = |a: i64, b: i64| Piece {
        start: a as f64 / 25.0,
        len: (b - a) as f64 / 25.0,
    };
    let seen = play_on_a_driven_clock(
        &tools,
        &src,
        vec![piece(50, 75), piece(0, 25), piece(75, 100)],
        25.0,
        3.0,
    );
    let want: Vec<i64> = (50..75).chain(0..25).chain(75..100).collect();
    assert_eq!(seen.iter().map(|s| s.1).collect::<Vec<_>>(), want);
    for (i, (pts, _)) in seen.iter().enumerate() {
        assert!((pts - i as f64 / 25.0).abs() < 1e-9, "frame {i} at {pts}");
    }
}

/// Pieces a frame and a half long, after a play position that leaves a third of a frame of
/// its piece: the grid gives 2, 1, 2, 1 frames (none for the third of a frame), each piece
/// from its own first frame, the times still index / fps.
#[test]
fn fractional_pieces_play_the_grid_s_frames() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("c.mp4");
    counter(&tools, &src, "25", 100);
    let fps = 25.0;
    let mut pieces = vec![Piece {
        start: 40.7 / fps,
        len: 0.3 / fps,
    }];
    pieces.extend((0..4).map(|k| Piece {
        start: f64::from(80 - 15 * k) / fps,
        len: 1.5 / fps,
    }));
    let total: f64 = pieces.iter().map(|p| p.len).sum();
    let seen = play_on_a_driven_clock(&tools, &src, pieces.clone(), fps, total);
    let mut want = Vec::new();
    for (p, part) in pieces.iter().zip(parts(&pieces, fps)) {
        let first = (p.start * fps).round() as i64;
        want.extend((0..part.count.unwrap() as i64).map(|k| first + k));
    }
    assert_eq!(want, [80, 81, 65, 50, 51, 35]);
    assert_eq!(seen.iter().map(|s| s.1).collect::<Vec<_>>(), want);
    for (i, (pts, _)) in seen.iter().enumerate() {
        assert!((pts - i as f64 / fps).abs() < 1e-9, "frame {i} at {pts}");
    }
}

/// Plays frame ranges of `src` at `fps` into the null sink to the end; returns the boundary
/// waits (video, audio), frames dropped, and the latest a frame was shown after its time (ms).
fn measure(
    tools: &Tools,
    src: &Path,
    size: (u32, u32),
    fps: f64,
    ranges: &[(i64, i64)],
) -> ((Vec<Duration>, Vec<Duration>), u64, f64) {
    let pieces: Vec<Piece> = ranges
        .iter()
        .map(|&(a, b)| Piece {
            start: a as f64 / fps,
            len: (b - a) as f64 / fps,
        })
        .collect();
    let duration: f64 = pieces.iter().map(|p| p.len).sum();
    let source = Source {
        input: Input::path(src),
        duration,
        video: Some((size, fps)),
        audio: true,
        pieces,
        stereo: false,
    };
    let mut player = Player::start(tools, &source, 0.0, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
    let begin = Instant::now();
    let mut late = 0f64;
    loop {
        let p = player.pump(Instant::now());
        assert_eq!(p.error, None);
        if let Some(f) = p.frame {
            late = late.max(player.clock(Instant::now()) - f.pts);
        }
        if p.ended || begin.elapsed() > Duration::from_secs(duration as u64 + 30) {
            break;
        }
        std::thread::sleep(p.wake_in.min(Duration::from_millis(5)));
    }
    (player.boundary_waits(), player.stats().1, late * 1000.0)
}

/// Picture and sound of an edited timeline on a real-time clock (the null sink): both reach
/// every boundary, and how long each decoder waited there is printed — a measurement, not a
/// bound (it depends on the machine).
#[test]
fn boundary_waits_are_recorded_for_every_boundary() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("c.mp4");
    counter(&tools, &src, "25", 100);
    let ranges = [(50, 75), (0, 25), (75, 100)];
    let (waits, dropped, late) = measure(&tools, &src, (64, 64), 25.0, &ranges);
    eprintln!(
        "64×64 25 fps: boundary waits video {:?} audio {:?}; {dropped} dropped; latest frame \
         {late:.1} ms",
        waits.0, waits.1
    );
    assert_eq!(
        (waits.0.len(), waits.1.len()),
        (2, 2),
        "two boundaries each"
    );
}

/// The receipt on a long-GOP 1080p file (keyframes 10 s apart, cuts mid-GOP): run with
/// `cargo test --release -- --ignored boundary_receipt_1080p --nocapture`.
#[test]
#[ignore]
fn boundary_receipt_1080p() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("v1080.mp4");
    ff(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1920x1080:rate=30:duration=24",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=24",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-g",
            "300",
            "-keyint_min",
            "300",
            "-sc_threshold",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
            src.to_str().unwrap(),
        ],
    );
    let size = decode_size(1920, 1080);
    // Each cut 8–9 s past a keyframe: the decoder decodes from the keyframe to get there.
    let ranges = [(250, 340), (520, 610), (40, 130), (590, 680), (270, 360)];
    let (waits, dropped, late) = measure(&tools, &src, size, 30.0, &ranges);
    eprintln!(
        "1080p long-GOP 30 fps (decoded {}×{}): boundary waits video {:?} audio {:?}; \
         {dropped} dropped; latest frame {late:.1} ms",
        size.0, size.1, waits.0, waits.1
    );
}

/// The preview keeps streams that start apart together, as Save does: the sound starting late
/// is silence first (its burst of second 1 at 1 s into a piece from 0, not 0.52 s), the picture
/// starting late holds its first frame from the cut on (frame 25 at index 25, not 20).
#[test]
fn the_preview_keeps_streams_that_start_apart_together() {
    let Some(tools) = tools_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let late_sound = tmp.path().join("a.mp4");
    crate::cut::edit_tests::offset_fixture(&tools, &late_sound, 0.0, 0.48);
    let piece = [Piece {
        start: 0.0,
        len: 1.6,
    }];
    let (pcm, state, _) = decode_all(&tools, &Input::path(&late_sound), &piece, 48_000, None);
    assert_eq!(state.error(), None);
    let left: Vec<f32> = pcm.iter().step_by(2).copied().collect();
    let first = left.iter().position(|v| v.abs() > 0.3).expect("a burst");
    let at = first as f64 / 48_000.0;
    assert!((at - 1.0).abs() < 0.003, "burst of second 1 at {at} s");
    let late_picture = tmp.path().join("v.mp4");
    crate::cut::edit_tests::offset_fixture(&tools, &late_picture, 0.2, 0.0);
    let seen = play_on_a_driven_clock(&tools, &late_picture, piece.to_vec(), 25.0, 1.6);
    let numbers: Vec<i64> = seen.iter().map(|s| s.1).collect();
    assert_eq!(numbers.len(), 40);
    assert_eq!(numbers[..6], [5; 6], "the first picture held: {numbers:?}");
    assert_eq!(numbers[25], 25, "{numbers:?}");
}
