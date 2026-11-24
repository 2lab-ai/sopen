//! The media window's runs over a remote file read as a stream (06 §7.4; §12 V16, V18, V19,
//! V22): gates on `local_server()`'s `sftp-server`, ffmpeg reading through the real byte server,
//! `lavfi` fixtures. Nothing asserts wall-clock time; the 60 s rule runs on a clock the test
//! moves, and every wait only catches a hang.

use super::fixtures::{bounded, make, none_left, tools, until};
use super::timeline::{Clip, Timeline, View};
use super::*;
use crate::ffmpeg::{Served, Span};
use crate::remote::{LocalFs, RemoteFs, SftpRemote};
use crate::stream::http::{Config, Seams, Server, SEND_BUFFER};
use crate::stream::{limits, refetches, Class, Engine, Opening, Settings, BLOCK, CAP_MAX};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, AtomicUsize};

const MIB: u64 = 1 << 20;

fn no_reopen() -> crate::stream::Reopen {
    Box::new(|| anyhow::bail!("this test has no second connection"))
}

fn transport(path: &Path) -> Option<Box<dyn crate::remote::Ranged>> {
    let fs = SftpRemote::local_server()?;
    Some(Box::new(fs).into_ranged(path.to_str().unwrap()).unwrap())
}

/// `path` on the local `sftp-server` as a stream, its engine on a thread of its own.
fn streamed(path: &Path) -> Option<Source> {
    Some(Source::open(transport(path)?, no_reopen(), Arc::new(|| {})).unwrap())
}

/// [`streamed`] with the engine stepped by the test: nothing is fetched between steps.
fn stepped(path: &Path) -> Option<(Source, Engine)> {
    let settings = Settings {
        poll_every: Duration::from_secs(1_000_000),
        ..Settings::plain()
    };
    let opening = Opening {
        label: "local".into(),
        began: Instant::now(),
        sniff: 0,
    };
    let transport = transport(path)?;
    Some(Source::build(
        transport,
        no_reopen(),
        Arc::new(|| {}),
        opening,
        settings,
    ))
}

/// Runs `f` on a thread while the engine is stepped here, until it returns.
fn stepping<T: Send>(engine: &mut Engine, f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        let run = scope.spawn(f);
        while !run.is_finished() {
            engine.step(Some(Duration::from_millis(5)));
        }
        run.join().unwrap()
    })
}

/// Nothing of the stream is on its way or waits to go.
fn idle(source: &Source) {
    until("the stream is idle", || {
        let mut core = source.core();
        !core.queued() && core.pf.adm.in_flight() == 0
    });
}

/// [`idle`], and no reader of `source` open: what the test's setup opened (a probe's
/// connections, whose server threads go on after ffprobe has exited) is gone, so what is
/// measured from here is the test's own.
fn settled(source: &Source) {
    until("no reader is open and the stream is idle", || {
        let mut core = source.core();
        core.pf.open_ids().is_empty() && !core.queued() && core.pf.adm.in_flight() == 0
    });
}

/// 06 §12 V16's `refetched`: the bytes of blocks fetched more than once (`Stats` has none; read
/// from the engine's trace of what it sent).
fn refetched(source: &Source) -> u64 {
    let again = refetches(&source.core().trace);
    again.readahead + again.waited_twice
}

/// Where the engine's trace of what it sent is now: what a run sends from here is after it.
fn traced(source: &Source) -> usize {
    source.core().trace.len()
}

/// The cap the engine sent under since trace position `from`: the highest it was at a send.
fn cap_since(source: &Source, from: usize) -> u64 {
    let core = source.core();
    core.trace[from..].iter().map(|t| t.cap).max().unwrap_or(0)
}

/// Each block a reader waited for twice (two fetches other than a prefetch), with every fetch
/// of it: its place in the trace, its class, and the reader it was for (its kind and where it
/// reached).
fn demand_history(source: &Source) -> String {
    let core = source.core();
    let reaches = core.pf.reaches();
    let reader = |id: Option<u64>| {
        id.and_then(|id| reaches.iter().find(|r| r.id == id))
            .map_or("-".to_string(), |r| {
                format!("r{} {:?} {}..{}", r.id, r.kind, r.from, r.to)
            })
    };
    let mut waited = std::collections::HashMap::new();
    let mut again = Vec::new();
    for t in &core.trace {
        let (Some(class), Some(b)) = (t.class, t.block) else {
            continue;
        };
        let (before, wait) = (waited.get(&b).copied(), class != Class::Prefetch);
        if before == Some(true) && wait && !again.contains(&b) {
            again.push(b);
        }
        waited.insert(b, before.unwrap_or(false) || wait);
    }
    again
        .iter()
        .map(|b| {
            let fetches: Vec<String> = core
                .trace
                .iter()
                .enumerate()
                .filter(|(_, t)| t.block == Some(*b))
                .map(|(i, t)| format!("#{i} {:?} for {}", t.class, reader(t.reader)))
                .collect();
            format!("block {b}: {}", fetches.join("; "))
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// A 600 s video (1 Mbit/s, 320×240, a key frame every 8 s, AAC sound): its `moov` at the end,
/// as ffmpeg writes an mp4 (06 §12 V16's shape). 80 MB: more than the cache holds.
fn long_video(tools: &Tools, dst: &Path, secs: u32) {
    video_at(tools, dst, secs, "1M");
}

/// [`long_video`] at `rate` (bits a second, as ffmpeg takes it), padded to it.
fn video_at(tools: &Tools, dst: &Path, secs: u32, rate: &str) {
    let picture = format!("testsrc2=size=320x240:rate=25:duration={secs}");
    let sound = format!("sine=frequency=440:sample_rate=48000:duration={secs}");
    let video = [
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-g",
        "200",
        "-b:v",
        rate,
        "-maxrate",
        rate,
        "-bufsize",
        rate,
        "-x264-params",
        "nal-hrd=cbr",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "64k",
    ];
    let mut args = vec!["-f", "lavfi", "-i", &picture, "-f", "lavfi", "-i", &sound];
    args.extend(video);
    make(tools, &args, dst);
}

/// The same file with its `moov` in front.
fn faststart(tools: &Tools, src: &Path, dst: &Path) {
    let src = src.to_str().unwrap();
    make(
        tools,
        &["-i", src, "-c", "copy", "-movflags", "+faststart"],
        dst,
    );
}

/// 600 s of stereo noise as a 320 kbit/s mp3 (constant bitrate: a LAME `Info` tag): 24 MB.
fn long_mp3(tools: &Tools, dst: &Path) {
    let noise = "anoisesrc=seed=5:color=pink:amplitude=0.4:sample_rate=44100";
    make(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            noise,
            "-t",
            "600",
            "-ac",
            "2",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "320k",
        ],
        dst,
    );
}

/// No process reads `media`'s stream any more: none names its server's URLs (an ffmpeg on a URL
/// names no test folder, so [`none_left`] of the folder cannot see it).
fn none_reading(media: &Media) {
    let url = media.input.show();
    let served = url.rsplitn(3, '/').nth(2).expect("a served URL");
    none_left(Path::new(served)).unwrap();
}

/// `n` pieces of `len` seconds, spread over `secs` of the source and every one out of order:
/// each needs an input of its own.
fn reordered(n: usize, len: f64, secs: f64) -> Vec<(f64, f64)> {
    let gap = (secs - 2.0 * len) / n as f64;
    let starts: Vec<f64> = (0..n).map(|k| len + k as f64 * gap).collect();
    // 7 and 24 share no factor: every piece is somewhere else than after the one before.
    (0..n).map(|k| (starts[(k * 7) % n], len)).collect()
}

fn spec_of(media: &Media, pieces: &[(f64, f64)]) -> EditSpec {
    EditSpec {
        spans: pieces
            .iter()
            .map(|&(start, len)| Span::of(media.origin() + start, len))
            .collect(),
        fps: media.video.is_some().then_some(media.fps),
        stereo: false,
        crop: None,
        tracks: media.tracks(),
    }
}

fn planned(media: &mut Media) -> (MediaKind, ExportPlan) {
    media.plan();
    let (kind, _, plan) = media.plan.clone().expect("a plan");
    (kind, plan)
}

/// `ffprobe -show_entries stream=<entry>` of the first stream of `kind` (`v` or `a`).
fn entry(tools: &Tools, file: &Path, kind: &str, what: &str) -> String {
    let out = std::process::Command::new(&tools.ffprobe)
        .args(["-v", "error", "-select_streams", &format!("{kind}:0")])
        .args(["-show_entries", &format!("stream={what}"), "-of", "csv=p=0"])
        .arg(file)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// How many samples per channel `file`'s sound decodes to.
fn samples(tools: &Tools, file: &Path) -> u64 {
    let out = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(file)
        .args(["-f", "s16le", "-ac", "1", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout.len() as u64 / 2
}

/// 06 §12 V16's render gates: 24 reordered pieces (24 inputs) of a 600 s video with its `moov`
/// at the end, of the same file faststart, and of a wav. Each render has no cut and the edit's
/// length, fetches at most `http_sent + inputs × 1.25 MiB + cap` (the cap the highest the engine
/// sent under during the render), and fetches no block twice.
#[test]
fn v16_a_render_of_24_reordered_pieces_fetches_within_its_bound() {
    bounded(400, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let at_end = dir.path().join("end.mp4");
        long_video(&tools, &at_end, 600);
        let front = dir.path().join("front.mp4");
        faststart(&tools, &at_end, &front);
        let wav = dir.path().join("long.wav");
        let sound = "sine=frequency=330:sample_rate=48000:duration=600";
        make(&tools, &["-f", "lavfi", "-i", sound, "-ac", "2"], &wav);
        for (path, frames) in [(&at_end, true), (&front, true), (&wav, false)] {
            let name = path.file_name().unwrap().to_str().unwrap();
            let Some(source) = streamed(path) else { return };
            let mut media = Media::stream(source.clone(), name, name).unwrap();
            let (kind, plan) = planned(&mut media);
            let pieces = reordered(24, 1.0, 600.0);
            let spec = spec_of(&media, &pieces);
            assert_eq!(spec.groups_from(&media.input).len(), 24, "24 inputs");
            settled(&source);
            let before = source.stats();
            let from = traced(&source);
            let inputs = spec.groups_from(&media.input).len() as u64;
            let work = dir.path().join(format!("work-{name}"));
            std::fs::create_dir_all(&work).unwrap();
            let out = work.join(format!("out.{}", if frames { "mp4" } else { "wav" }));
            cut::render_edit(
                &tools,
                &media.input,
                &out,
                &spec,
                kind,
                &plan,
                &work,
                &mut |_| {},
            )
            .unwrap();
            // Every reader of the render closed: what they fetched is counted.
            settled(&source);
            let after = source.stats();
            let served = media.input.served().unwrap();
            assert_eq!(served.server().cuts(Kind::Render), 0, "{name}: no cut");
            if frames {
                assert_eq!(entry(&tools, &out, "v", "nb_frames"), "600", "{name}");
            } else {
                assert_eq!(entry(&tools, &out, "a", "duration_ts"), "1152000", "{name}");
            }
            let fetched = after.fetched.total - before.fetched.total;
            let sent = after.http_sent - before.http_sent;
            let cap = cap_since(&source, from);
            assert!(cap > 0, "{name}: the render sent nothing");
            let allowed = sent + inputs * (5 * MIB / 4) + cap;
            eprintln!(
                "V16 render {name} ({} bytes): fetched {fetched}, http_sent {sent}, {inputs} \
                 inputs, {} render readers, cap {cap}, allowed {allowed}, refetched {}",
                source.size(),
                served.server().opened(Kind::Render),
                refetched(&source)
            );
            assert!(fetched <= allowed, "{name}: fetched {fetched} > {allowed}");
            assert_eq!(refetched(&source), 0, "{name}: a block fetched twice");
            // Each targeting of every reader queued only what its read allowed.
            let reaches = source.core().pf.reaches();
            assert!(
                reaches.iter().any(|r| r.targeted > 0),
                "{name}: no readahead"
            );
            let strays: Vec<_> = reaches.iter().filter(|r| r.outside.is_some()).collect();
            assert!(
                strays.is_empty(),
                "{name}: readahead outside what its read allowed: {strays:?}"
            );
        }
        none_left(dir.path()).unwrap();
    });
}

/// 06 §12 V16's playback gate, on X1's fixture's shape (600 s at 4 Mbit/s, a key frame every
/// 8 s, its `moov` at the end; 320×240 here, which costs the same bytes): an edit of 10 pieces
/// of 10 s reordered, 12 s of play from timeline positions 5 s, 45 s and 85 s, one after another
/// on one Source. Each start, from `Player::start` to the drop, fetches at most `http_sent +
/// (play readers opened) × their ahead + cap` beyond what it fetched again: the readers as the
/// server counts the `play` responses it began, the cap the highest the engine sent under.
///
/// What it fetches again is readahead, never a block a decoder waited for: no block twice in
/// the start, each inside what one of the start's readers read or was allowed to read ahead,
/// every prefetch target inside what its read allowed, and no reader's readahead past its
/// allowance (`ahead`) — what the engine is allowed to do, from its own records
/// ([`crate::stream::Prefetcher::reaches`], the prefetcher's `legal_reach`). Not 06's one
/// block (X1's threshold for its own fixture), nor a fixed figure, for it grows with load.
/// Each decoder opens this `moov`-at-the-end file at byte 0 (and again at byte 48), reads a
/// few KB and seeks to the `moov`. Meanwhile the server fills
/// that connection's socket from the cached head; each read is a sequential hit that doubles
/// the reader's readahead, to MBs within a few blocks, and its prefetch past the cached head
/// drains for as long as ffmpeg keeps the connection open. Those blocks, evicted since an
/// earlier start read them, come again. Idle, ffmpeg closes such a connection 1–5 ms after it
/// opens it, and up to 1.19 MiB came again a start; loaded, it held them 190–320 ms, its first
/// frame 1.6 s late, and 1.69 MiB came again (macOS; 2.37 MiB on a loaded CI runner).
#[test]
fn v16_playback_of_an_edit_fetches_within_its_bound() {
    bounded(300, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("x1.mp4");
        video_at(&tools, &path, 600, "4M");
        let Some(source) = streamed(&path) else {
            return;
        };
        let media = Media::stream(source.clone(), "x1.mp4", "x1.mp4").unwrap();
        let mut tl = Timeline::new(media.rate(), media.ticks());
        let clips = reordered(10, 10.0, 600.0)
            .into_iter()
            .map(|(start, len)| Clip {
                start: tl.tick(start),
                end: tl.tick(start + len),
            })
            .collect();
        assert!(tl.set_clips(clips, View::default()));
        let rate = source.size() / media.duration as u64;
        let (ahead, _) = limits(ReaderKind::Media { bytes_per_s: rate });
        settled(&source);
        for at in [5.0, 45.0, 85.0] {
            let from = tl.tick(at);
            let src = playback::Source {
                input: media.input.clone(),
                duration: tl.secs(tl.len()),
                video: media.decode_size().map(|size| (size, media.fps)),
                audio: media.has_audio,
                pieces: tl
                    .from(from)
                    .iter()
                    .map(|c| Piece {
                        start: media.origin() + tl.secs(c.start),
                        len: tl.secs(c.len()),
                    })
                    .collect(),
                stereo: false,
            };
            let server = media.input.served().unwrap().server();
            let (stats, again, opened, traced_at) = (
                source.stats(),
                refetched(&source),
                server.opened(Kind::Play),
                traced(&source),
            );
            let last_reader = source.core().pf.last_id();
            let mut player = Player::start(
                &tools,
                &src,
                tl.secs(from),
                0.0,
                Sink::Null,
                Arc::new(|| {}),
            )
            .unwrap();
            let t0 = tl.secs(from);
            // Real time, on a loaded machine too: a bound of its own.
            let give_up = Instant::now() + Duration::from_secs(90);
            loop {
                let pump = player.pump(Instant::now());
                assert!(pump.error.is_none(), "{:?}", pump.error);
                if pump.position >= t0 + 12.0 {
                    break;
                }
                assert!(Instant::now() < give_up, "12 s of play: not in 90 s");
                std::thread::sleep(Duration::from_millis(5));
            }
            drop(player);
            // Every reader of this start closed: what they fetched is this start's, and the
            // next start begins with none of them.
            settled(&source);
            let now = source.stats();
            let fetched = now.fetched.total - stats.fetched.total;
            let again = refetched(&source) - again;
            let sent = now.http_sent - stats.http_sent;
            let readers = server.opened(Kind::Play) - opened;
            let cap = cap_since(&source, traced_at);
            assert!(
                readers >= 1 && cap > 0,
                "from {at} s: {readers} readers, cap {cap}"
            );
            let allowed = sent + readers * ahead + cap;
            eprintln!(
                "V16 playback from {at} s: fetched {fetched} (again {again}), http_sent {sent}, \
                 {readers} play readers, cap {cap}, allowed {allowed}"
            );
            assert!(fetched - again <= allowed, "from {at} s: fetched {fetched}");
            // What comes again is readahead — one of its two fetches: a reader that reads it
            // later waits for it —, never a block a decoder waited for twice. (The trace's lock
            // is let go before the message, which takes it again.)
            let waited_twice = refetches(&source.core().trace).waited_twice;
            assert_eq!(
                waited_twice,
                0,
                "from {at} s: a block waited for twice: {}",
                demand_history(&source)
            );
            // What the start's readers were let read: each one's readahead never past its
            // allowance, and every block that came again inside what one of them reached —
            // once in the start, a block fetched before it.
            let reaches: Vec<_> = source
                .core()
                .pf
                .reaches()
                .into_iter()
                .filter(|r| r.id > last_reader)
                .collect();
            let widest = reaches.iter().map(|r| r.widest).max().unwrap_or(0);
            assert!(
                widest <= ahead,
                "from {at} s: a readahead of {widest} > {ahead}"
            );
            // Each targeting queued only what its read allowed (`legal_reach`).
            assert!(
                reaches.iter().any(|r| r.targeted > 0),
                "from {at} s: no readahead was set"
            );
            let strays: Vec<_> = reaches.iter().filter(|r| r.outside.is_some()).collect();
            assert!(
                strays.is_empty(),
                "from {at} s: readahead outside what its read allowed: {strays:?}"
            );
            let reached = |b: u64| {
                reaches
                    .iter()
                    .any(|r| r.from <= b * BLOCK && b * BLOCK < r.to)
            };
            let (before, during) = {
                let core = source.core();
                let blocks = |t: &[crate::stream::Traced]| -> Vec<u64> {
                    t.iter().filter_map(|t| t.block).collect()
                };
                (
                    blocks(&core.trace[..traced_at]),
                    blocks(&core.trace[traced_at..]),
                )
            };
            let earlier: std::collections::HashSet<u64> = before.into_iter().collect();
            let mut once = std::collections::HashSet::new();
            for b in &during {
                assert!(
                    once.insert(*b),
                    "from {at} s: block {b} fetched twice in the start"
                );
            }
            let came_again: Vec<u64> = during
                .iter()
                .copied()
                .filter(|b| earlier.contains(b))
                .collect();
            assert_eq!(came_again.len() as u64 * BLOCK, again, "from {at} s");
            let outside: Vec<u64> = came_again
                .iter()
                .copied()
                .filter(|b| !reached(*b))
                .collect();
            assert!(
                outside.is_empty(),
                "from {at} s: fetched again outside every reader's reach: {outside:?}"
            );
            let could = earlier.iter().filter(|b| reached(**b)).count() as u64 * BLOCK;
            eprintln!(
                "V16 playback from {at} s: {} readers, readahead at most {widest}; {again} \
                 fetched again of the {could} fetched before inside their reach",
                reaches.len()
            );
        }
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// 06 §12 V16: a 2-input render whose inputs wait past 60 s (on the server's clock, which the
/// test moves) has no cut and the edit's length — the 60 s rule does not run on a `render`
/// body (06 §9). The waits are seen: each `render` body connection, by its id, has a write
/// blocked on its client; after each move of the clock, every one of them has looked at the
/// clock past the move and counted no time against its client.
#[test]
fn v16_a_render_input_that_waits_past_60_s_is_not_cut() {
    bounded(240, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        // 600 s (80 MB): what follows each piece is more than both sockets' buffers can take
        // on either platform, so each body's writes come to wait.
        let made = dir.path().join("made.mp4");
        long_video(&tools, &made, 600);
        let path = dir.path().join("v.mp4");
        faststart(&tools, &made, &path);
        let Some(source) = streamed(&path) else {
            return;
        };
        let hand = t.hand();
        // The render bodies blocked on their client, by connection; each look at the clock
        // after a wait ran out: the connection, the time counted against it, the time read.
        let blocked: Arc<Mutex<BTreeSet<u64>>> = Arc::default();
        let looked: Arc<Mutex<Vec<(u64, Duration, Instant)>>> = Arc::default();
        let (b, l) = (blocked.clone(), looked.clone());
        let config = Config {
            clock: hand.clock(),
            tick: Some(Duration::from_millis(20)),
            seams: Seams {
                body_blocked: Some(Arc::new(move |id, kind| {
                    if kind == Kind::Render {
                        b.lock().unwrap().insert(id);
                    }
                })),
                evaluated: Some(Arc::new(move |id, idle, now| {
                    l.lock().unwrap().push((id, idle, now));
                })),
                ..Seams::default()
            },
        };
        let server = Server::start_with(source.clone(), config).unwrap();
        let served = Served::new(server, source.clone(), "v.mp4", "v.mp4", false);
        let mut media = Media::served(&tools, served.clone(), "v.mp4", "v.mp4").unwrap();
        let (kind, plan) = planned(&mut media);
        // The second piece's input is opened with the first, and waits while the first plays.
        // 150 s pieces: the first is still being read well after the first progress report
        // (ffmpeg 9.1 renders 80 s of this in under a second, its AAC encoder 4× 9.0's).
        let spec = spec_of(&media, &[(200.0, 150.0), (10.0, 150.0)]);
        assert_eq!(spec.groups_from(&media.input).len(), 2);
        let out = dir.path().join("out.mp4");
        // The render's first progress report holds it, and its ffmpeg is stopped there (SIGSTOP)
        // until the clock has gone past the rule: its inputs' connections are not read. The
        // progress pipe alone would not stop it — the reader's queue and the pipe take far more
        // reports than a render makes, and ffmpeg renders on to its end unread.
        let (begun, hold) = (mpsc::channel(), mpsc::channel::<()>());
        let (begun_tx, begun_rx) = begun;
        let (hold_tx, hold_rx) = hold;
        let (input, tools2, out2) = (media.input.clone(), tools.clone(), out.clone());
        let work = dir.path().to_path_buf();
        let render = std::thread::spawn(move || {
            let mut first = true;
            cut::render_edit(
                &tools2,
                &input,
                &out2,
                &spec,
                kind,
                &plan,
                &work,
                &mut |f| {
                    if f > 0.0 && std::mem::take(&mut first) {
                        let _ = begun_tx.send(());
                        let _ = hold_rx.recv();
                    }
                },
            )
        });
        begun_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the render begins");
        // The one process that names the output: the render's ffmpeg, both of its inputs open.
        let ffmpeg = super::child::log::pids_naming(&out);
        assert_eq!(ffmpeg.len(), 1, "the render's ffmpeg: {ffmpeg:?}");
        let signal = |sig: libc::c_int| {
            // SAFETY: a signal to a pid this test just found as its render's ffmpeg.
            let sent = unsafe { libc::kill(ffmpeg[0] as libc::pid_t, sig) };
            assert_eq!(sent, 0, "signal {sig} to the render's ffmpeg");
        };
        signal(libc::SIGSTOP);
        // Both inputs are open; their connections fill and their writes wait.
        // The render bodies waiting now: those whose blocked writes look at the clock from
        // here on (a connection ffmpeg closed earlier looks no more).
        let from = looked.lock().unwrap().len();
        let waiting = || -> BTreeSet<u64> {
            let blocked = blocked.lock().unwrap();
            let looked = looked.lock().unwrap();
            looked[from..]
                .iter()
                .map(|l| l.0)
                .filter(|id| blocked.contains(id))
                .collect()
        };
        until("both render bodies wait on their client", || {
            waiting().len() >= 2
        });
        let ids = waiting();
        for _ in 0..2 {
            hand.advance(Duration::from_secs(61));
            let moved = hand.now();
            // Every waiting body looks at the clock past the move — or a cut ends the wait.
            let seen = |id: u64| {
                let looked = looked.lock().unwrap();
                looked
                    .iter()
                    .rev()
                    .find(|l| l.0 == id && l.2 >= moved)
                    .copied()
            };
            until("each waiting body looks at the moved clock", || {
                served.server().cuts(Kind::Render) > 0 || ids.iter().all(|id| seen(*id).is_some())
            });
            assert_eq!(
                served.server().cuts(Kind::Render),
                0,
                "a waiting input was cut"
            );
            for id in &ids {
                let (_, idle, _) = seen(*id).unwrap();
                assert_eq!(
                    idle,
                    Duration::ZERO,
                    "connection {id}: time counted against it"
                );
            }
        }
        signal(libc::SIGCONT);
        hold_tx.send(()).unwrap();
        render.join().unwrap().unwrap();
        for kind in Kind::ALL {
            assert_eq!(served.server().cuts(kind), 0, "a waiting input was cut");
        }
        assert_eq!(entry(&tools, &out, "v", "nb_frames"), "7500");
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// The file changes on the remote while an Export renders it: the render's stream is cut, and
/// ffmpeg exits 0 on that body cut short (06 §7.4: shown here, from its own exit status), yet
/// nothing is published and nothing is left under `<cache>/.exports` (06 §12 V22, V16's "a
/// render whose `cuts(Render)` went up"). A wav: its demuxer takes the end of its bytes for the
/// end of the sound, with no container index to miss (run on ffmpeg 9.0.2; not on 8).
#[test]
fn v22_a_render_whose_stream_was_cut_publishes_nothing() {
    bounded(240, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("v.wav");
        let sound = "sine=frequency=330:sample_rate=48000:duration=600";
        make(&tools, &["-f", "lavfi", "-i", sound, "-ac", "2"], &path);
        let size = std::fs::metadata(&path).unwrap().len();
        let Some((source, mut engine)) = stepped(&path) else {
            return;
        };
        let served = Served::start(source.clone(), "v.wav", "v.wav", false).unwrap();
        let media = stepping(&mut engine, || {
            Media::served(&tools, served.clone(), "v.wav", "v.wav").unwrap()
        });
        let spec = spec_of(&media, &[(300.0, 60.0)]);
        let exports = dir.path().join("exports");
        let listed = || {
            let mut names: Vec<String> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n != "exports")
                .collect();
            names.sort();
            names
        };
        let before = listed();
        let stop = cut::Stop::default();
        let (tools2, input) = (tools.clone(), media.input.clone());
        let target = dir.path().to_str().unwrap().to_string();
        let result = std::thread::scope(|scope| {
            let run = scope.spawn(|| {
                let mut fs = LocalFs::new();
                cut::render_and_publish(
                    &mut fs,
                    &tools2,
                    &input,
                    &target,
                    "v.wav",
                    &spec,
                    (300.0, 360.0),
                    &exports,
                    &mut |_| {},
                    &stop,
                )
            });
            // A good part of the render's input has gone to ffmpeg.
            let sent = source.stats().http_sent;
            while source.stats().http_sent < sent + 8 * MIB && !run.is_finished() {
                engine.step(Some(Duration::from_millis(20)));
            }
            assert!(!run.is_finished(), "the render ended before the change");
            until("ffmpeg's connection waits for a block", || {
                source.core().pf.waiting_readers() >= 1 || run.is_finished()
            });
            // A write in place, in another whole second (SFTP carries no more than that).
            use std::os::unix::fs::FileExt;
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.write_all_at(&[0xEE; 4096], size - 2 * MIB).unwrap();
            let opened = source.opened().mtime;
            file.set_modified(opened + Duration::from_secs(10)).unwrap();
            while !run.is_finished() {
                engine.step(Some(Duration::from_millis(20)));
            }
            run.join().unwrap()
        });
        let err = result.expect_err("a render whose stream was cut publishes nothing");
        let said = format!("{err:#}");
        assert!(said.contains("changed on the remote"), "{said}");
        assert!(
            served.server().cuts(Kind::Render) >= 1,
            "the cut is counted"
        );
        // ffmpeg itself took the cut body for the end of the file and ended well.
        let ended = ffmpeg::exits::naming(exports.to_str().unwrap());
        assert_eq!(ended.len(), 1, "one render run: {ended:?}");
        assert!(ended[0].success(), "ffmpeg exited {}", ended[0]);
        assert_eq!(listed(), before, "nothing was published");
        let left: Vec<_> = std::fs::read_dir(&exports)
            .map(|d| d.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "left under the exports: {left:?}");
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// When a test's Cancel comes to an export it watches through [`Publishing`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum CancelAt {
    /// Once this many bytes of the upload are in the remote file: the owner's Cancel, made on
    /// another thread while the upload holds at that checkpoint (its look at the stop before
    /// its next chunk) until the Cancel has landed.
    MidUpload(u64),
    /// Right after the upload has returned, before the link.
    AfterUpload,
    /// Right after the link has landed: too late.
    AfterLink,
}

/// What a [`Publishing`] saw.
#[derive(Default, Debug)]
struct Seen {
    /// The size of the file being uploaded (the rendered export).
    total: Option<u64>,
    /// The bytes in the remote temp when the Cancel landed, and when the temp was removed.
    at_cancel: Option<u64>,
    at_removal: Option<u64>,
    /// What a Cancel after the link said (`false`: too late).
    late: Option<bool>,
    linked: bool,
}

/// An export's remote (any file layer on this machine) that cancels the export at a moment of
/// its upload or link ([`CancelAt`]), and sees what the remote file held then.
struct Publishing {
    fs: Box<dyn RemoteFs + Send>,
    stop: Arc<cut::Stop>,
    at: CancelAt,
    seen: Arc<Mutex<Seen>>,
}

fn size_of(path: &str) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

impl RemoteFs for Publishing {
    fn stat_opt(&mut self, path: &str) -> Result<Option<crate::remote::Stat>> {
        self.fs.stat_opt(path)
    }
    fn lstat_opt(&mut self, path: &str) -> Result<Option<crate::remote::Stat>> {
        self.fs.lstat_opt(path)
    }
    fn realpath(&mut self, path: &str) -> Result<String> {
        self.fs.realpath(path)
    }
    fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>> {
        self.fs.read_head(path, max)
    }
    fn list_dir(&mut self, path: &str) -> Result<Vec<String>> {
        self.fs.list_dir(path)
    }
    fn list_entries(&mut self, path: &str) -> Result<Vec<crate::remote::Entry>> {
        self.fs.list_entries(path)
    }
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        self.fs.read_to(path, out, progress)
    }
    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<crate::remote::Like>,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        self.create_from_stoppable(local, path, like, &|| false)
    }
    fn create_from_stoppable(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<crate::remote::Like>,
        stop: &dyn Fn() -> bool,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        let (owner, seen) = (self.stop.clone(), self.seen.clone());
        seen.lock().unwrap().total = std::fs::metadata(local).ok().map(|m| m.len());
        let fired = std::cell::Cell::new(false);
        let checked = || {
            if let CancelAt::MidUpload(at) = self.at {
                let len = size_of(path);
                if !fired.get() && len >= at {
                    fired.set(true);
                    let owner = owner.clone();
                    let landed = std::thread::spawn(move || owner.cancel()).join().unwrap();
                    assert!(landed, "a Cancel during the upload is in time");
                    seen.lock().unwrap().at_cancel = Some(len);
                }
            }
            stop()
        };
        let up = self.fs.create_from_stoppable(local, path, like, &checked);
        if self.at == CancelAt::AfterUpload {
            assert!(
                self.stop.cancel(),
                "a Cancel after the upload, before the link, is in time"
            );
            self.seen.lock().unwrap().at_cancel = Some(size_of(path));
        }
        up
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        self.fs.set_mode(path, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.fs.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        let linked = self.fs.link_new(from, to);
        if linked.is_ok() {
            let mut seen = self.seen.lock().unwrap();
            seen.linked = true;
            if self.at == CancelAt::AfterLink {
                seen.late = Some(self.stop.cancel());
            }
        }
        linked
    }
    fn can_link(&self) -> bool {
        self.fs.can_link()
    }
    fn can_replace(&self) -> bool {
        self.fs.can_replace()
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        if path.contains(".sopen-") {
            self.seen.lock().unwrap().at_removal = Some(size_of(path));
        }
        self.fs.remove(path)
    }
}

/// 06 §12 V22: Cancel during the render kills ffmpeg and leaves no file under the exports and
/// nothing on the remote; during the upload it stops the upload, removes the remote temp and
/// links nothing (and so after the upload, before the link); after
/// the link it is too late and the result names the sibling; a name already taken is never
/// written over.
#[test]
fn v22_export_cancel_at_each_step() {
    bounded(240, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("v.mp4");
        long_video(&tools, &path, 60);
        let Some(source) = streamed(&path) else {
            return;
        };
        let media = Media::stream(source, "v.mp4", "v.mp4").unwrap();
        let spec = spec_of(&media, &[(10.0, 30.0)]);
        let target = dir.path().to_str().unwrap().to_string();
        let exports = dir.path().join("exports");
        let listed = || {
            let mut names: Vec<String> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n != "exports")
                .collect();
            names.sort();
            names
        };
        // How far the last render got.
        let reached = Mutex::new(0.0f64);
        let export = |fs: &mut dyn RemoteFs, stop: &cut::Stop, cancel_on_progress: bool| {
            *reached.lock().unwrap() = 0.0;
            cut::render_and_publish(
                fs,
                &tools,
                &media.input,
                &target,
                "v.mp4",
                &spec,
                (10.0, 40.0),
                &exports,
                &mut |f| {
                    let mut got = reached.lock().unwrap();
                    *got = got.max(f);
                    if cancel_on_progress && f > 0.0 {
                        stop.cancel();
                    }
                },
                stop,
            )
        };
        let nothing_under_exports = || {
            let left: Vec<_> = std::fs::read_dir(&exports)
                .map(|d| d.map(|e| e.unwrap().path()).collect())
                .unwrap_or_default();
            assert!(left.is_empty(), "left under the exports: {left:?}");
        };
        let before = listed();
        // During the render.
        let stop = cut::Stop::default();
        let err = export(&mut LocalFs::new(), &stop, true).unwrap_err();
        assert!(crate::remote::is_cancelled(&err), "{err:#}");
        let got = *reached.lock().unwrap();
        assert!(got < 0.9, "ffmpeg was stopped mid-way, not at {got}");
        assert_eq!(listed(), before, "nothing on the remote");
        nothing_under_exports();
        none_left(dir.path()).unwrap();
        // During the upload: the Cancel lands once 512 KiB are in the remote temp, and nothing
        // more is written.
        let publishing = |at: CancelAt| {
            let stop = Arc::new(cut::Stop::default());
            let fs = Publishing {
                fs: Box::new(LocalFs::new()),
                stop: stop.clone(),
                at,
                seen: Arc::default(),
            };
            (fs, stop)
        };
        let (mut fs, stop) = publishing(CancelAt::MidUpload(512 << 10));
        let err = export(&mut fs, &stop, false).unwrap_err();
        assert!(crate::remote::is_cancelled(&err), "{err:#}");
        let seen = fs.seen.lock().unwrap();
        let (at_cancel, at_removal) = (
            seen.at_cancel.expect("the Cancel landed mid-upload"),
            seen.at_removal.expect("the temp was removed"),
        );
        let total = seen.total.expect("an upload");
        // Mid-way: part of the file was in the temp when the Cancel landed, not all of it.
        assert!(0 < at_cancel && at_cancel < total, "{seen:?}");
        assert!(
            at_removal <= at_cancel + (256 << 10),
            "{seen:?}: written after the Cancel"
        );
        assert!(!seen.linked, "no link");
        drop(seen);
        assert_eq!(listed(), before, "the temp is removed and nothing linked");
        nothing_under_exports();
        // After the upload, before the link: the whole temp is there, and still goes.
        let (mut fs, stop) = publishing(CancelAt::AfterUpload);
        let err = export(&mut fs, &stop, false).unwrap_err();
        assert!(crate::remote::is_cancelled(&err), "{err:#}");
        let seen = fs.seen.lock().unwrap();
        assert!(
            seen.at_cancel.is_some_and(|n| n > 0) && !seen.linked,
            "{seen:?}"
        );
        drop(seen);
        assert_eq!(listed(), before, "the temp is removed and nothing linked");
        nothing_under_exports();
        // After the link.
        let (mut fs, stop) = publishing(CancelAt::AfterLink);
        let published = export(&mut fs, &stop, false).unwrap();
        assert_eq!(
            fs.seen.lock().unwrap().late,
            Some(false),
            "too late: refused"
        );
        assert_eq!(published.name, "v.cut-10-40.mp4");
        assert!(dir.path().join(&published.name).is_file());
        // Its name is taken now: the next export is published beside it, never over it.
        let first = std::fs::read(dir.path().join(&published.name)).unwrap();
        let again = export(&mut LocalFs::new(), &cut::Stop::default(), false).unwrap();
        assert_eq!(again.name, crate::remote::suffixed(&published.name, 1));
        assert_eq!(
            std::fs::read(dir.path().join(&published.name)).unwrap(),
            first,
            "the first export is as it was"
        );
        nothing_under_exports();
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// A stand-in for ffmpeg and ffprobe that asks the byte server for its input (the URL after
/// `-i`, else its last argument) and then neither reads nor closes: it holds its seat until it
/// is killed (`exec`: one process, its socket its own).
fn holder(dir: &Path) -> Tools {
    let path = dir.join("holder");
    let script = "#!/bin/bash\n[ \"$1\" = -version ] && exit 0\n\
        url=\"\"; prev=\"\"\n\
        for a in \"$@\"; do [ \"$prev\" = -i ] && url=\"$a\"; prev=\"$a\"; done\n\
        [ -z \"$url\" ] && url=\"${@: -1}\"\n\
        rest=\"${url#http://127.0.0.1:}\"; port=\"${rest%%/*}\"; path=\"/${rest#*/}\"\n\
        exec 3<>\"/dev/tcp/127.0.0.1/$port\"\n\
        printf 'GET %s HTTP/1.1\\r\\nHost: 127.0.0.1:%s\\r\\n\\r\\n' \"$path\" \"$port\" >&3\n\
        exec sleep 3600\n";
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::testutil::run_fresh(std::process::Command::new(&path).arg("-version"));
    Tools {
        ffmpeg: path.clone(),
        ffprobe: path,
    }
}

/// 06 §12 V16's owner gate (06 §7.4, W7's rule): every run on a `play` or `render` URL has an
/// owner that ends it, and once it has, the seats the run held are back to 0 — for a client
/// that neither reads nor closes. A dropped player; an export stopped by its watchdog and a
/// probe past its time, each timed out on a clock the test moves once the seat is held; a
/// paused frame past its budget.
#[test]
fn v16_every_run_on_a_stream_has_an_owner_that_frees_its_seats() {
    bounded(120, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("v.mp4");
        long_video(&tools, &path, 10);
        let Some(source) = streamed(&path) else {
            return;
        };
        let media = Media::stream(source, "v.mp4", "v.mp4").unwrap();
        let server = media.input.served().unwrap().server();
        let seats = || server.watch().open();
        until("no seat is held", || seats() == 0);
        let held = holder(dir.path());
        // A dropped player: its decoders are killed and reaped.
        let src = playback::Source {
            input: media.input.clone(),
            duration: media.duration,
            video: media.decode_size().map(|size| (size, media.fps)),
            audio: media.has_audio,
            pieces: Vec::new(),
            stereo: false,
        };
        let player = Player::start(&held, &src, 0.0, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
        until("the player's decoders hold their seats", || seats() >= 2);
        drop(player);
        until("a dropped player's seats are free", || seats() == 0);
        // An export stopped by its watchdog, and a probe past its time: each on a clock this
        // test moves, only once the run's seat is held, by the real limits.
        let args = media.input.of_kind(Kind::Render).args();
        let hand = t.hand();
        // The runs' clock, counting its reads: the first is the owner's start (the watchdog's,
        // the probe's deadline), and the clock moves only after it.
        let reads = AtomicUsize::new(0);
        let now = || {
            let at = hand.now();
            reads.fetch_add(1, SeqCst);
            at
        };
        let seated = || {
            until("the run holds its seat", || seats() >= 1);
            until("the run's owner has started its clock", || {
                reads.load(SeqCst) >= 1
            });
            assert!(seats() >= 1, "the seat is held");
        };
        reads.store(0, SeqCst);
        std::thread::scope(|scope| {
            let run = scope.spawn(|| {
                let watched = ffmpeg::Watched {
                    now: &now,
                    ..ffmpeg::Watched::plain()
                };
                ffmpeg::run_stoppable(&held, &args, 1.0, &mut |_| {}, ffmpeg::STALL_LIMIT, watched)
            });
            seated();
            assert!(!run.is_finished(), "nothing stops it before its time");
            hand.advance(ffmpeg::STALL_LIMIT + Duration::from_secs(1));
            let err = run.join().unwrap().unwrap_err();
            assert!(format!("{err:#}").contains("no progress"), "{err:#}");
        });
        until("a stopped export's seat is free", || seats() == 0);
        reads.store(0, SeqCst);
        std::thread::scope(|scope| {
            let run = scope.spawn(|| {
                let limit = ffmpeg::PROBE_LIMIT;
                ffmpeg::probe_timed(&held, &media.input, "v.mp4", limit, &now, &|| false)
            });
            seated();
            assert!(!run.is_finished(), "nothing stops it before its time");
            hand.advance(ffmpeg::PROBE_LIMIT + Duration::from_secs(1));
            let err = run.join().unwrap().unwrap_err();
            assert!(format!("{err:#}").contains("longer than"), "{err:#}");
        });
        until("a stopped probe's seat is free", || seats() == 0);
        let limit = Duration::from_secs(1);
        // A paused frame past its budget.
        let mut held_media = media.clone();
        held_media.tools = held.clone();
        let scrub = Scrubber::start_with(&held_media, egui::Context::default(), limit).unwrap();
        scrub.request(1.0, 25);
        until("the frame's decode holds its seat", || seats() >= 1);
        until("a frame past its budget frees its seat", || seats() == 0);
        drop(scrub);
        none_left(dir.path()).unwrap();
    });
}

/// 06 §12 V18: a 600 s mp3 with a LAME `Info` tag seeks with `+fastseek` on its `play` input: a
/// seek to 480 s adds at most `1 MiB + ahead + cap` to `fetched` between the seek and its first
/// sound (an exact seek reads the file up to it: 19 MB here). Its Export of 470–480 s seeks
/// exactly, through a `render` input without `+fastseek`, and is 10.000 s long.
#[test]
fn v18_an_mp3_seeks_fast_to_play_and_exactly_to_export() {
    bounded(240, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("m.mp3");
        long_mp3(&tools, &path);
        let Some(source) = streamed(&path) else {
            return;
        };
        let media = Media::stream(source.clone(), "m.mp3", "m.mp3").unwrap();
        let served = media.input.served().unwrap().clone();
        assert!(
            served.fastseek(),
            "a LAME Info tag: the play input seeks fast"
        );
        assert!(media.input.options().iter().any(|o| o == "+fastseek"));
        settled(&source);
        let before = source.fetched().total;
        let src = playback::Source {
            input: media.input.clone(),
            duration: media.duration,
            video: None,
            audio: true,
            pieces: Vec::new(),
            stereo: false,
        };
        let mut player =
            Player::start(&tools, &src, 480.0, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
        until("the seek's first sound", || {
            player.pump(Instant::now());
            player.sounded()
        });
        let fetched = source.fetched().total - before;
        drop(player);
        let rate = source.size() / media.duration as u64;
        let (ahead, _) = limits(ReaderKind::Media { bytes_per_s: rate });
        eprintln!("V18 seek to 480 s: fetched {fetched} (ahead {ahead}, cap {CAP_MAX})");
        assert!(
            fetched <= MIB + ahead + CAP_MAX,
            "the seek fetched {fetched} bytes"
        );
        // Export of 470–480 s: exact.
        let render = media.input.of_kind(Kind::Render);
        assert!(!render.options().iter().any(|o| o == "+fastseek"));
        let spec = spec_of(&media, &[(470.0, 10.0)]);
        let target = dir.path().to_str().unwrap().to_string();
        let published = cut::render_and_publish(
            &mut LocalFs::new(),
            &tools,
            &media.input,
            &target,
            "m.mp3",
            &spec,
            (470.0, 480.0),
            &dir.path().join("exports"),
            &mut |_| {},
            &cut::Stop::default(),
        )
        .unwrap();
        let out = dir.path().join(&published.name);
        // 10.000 s: as long as a plain 10 s encode by this ffmpeg decodes (441 000 samples with
        // ffmpeg 9).
        let plain = dir.path().join("plain.mp3");
        let tone = "sine=frequency=1000:sample_rate=44100";
        make(
            &tools,
            &[
                "-f",
                "lavfi",
                "-i",
                tone,
                "-t",
                "10",
                "-ac",
                "2",
                "-c:a",
                "libmp3lame",
                "-b:a",
                "320k",
            ],
            &plain,
        );
        assert_eq!(
            samples(&tools, &out),
            samples(&tools, &plain),
            "10.000 s long"
        );
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// 06 §12 V19: the waveform of a remote 24 MB mp3 completes having fetched at most 1.05 × the
/// file, filling as it reads (the peaks reach further at each hand-over before it is complete).
#[test]
fn v19_the_waveform_of_a_24_mb_mp3_fetches_it_about_once() {
    bounded(240, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("m.mp3");
        long_mp3(&tools, &path);
        let Some(source) = streamed(&path) else {
            return;
        };
        let media = Media::stream(source.clone(), "m.mp3", "m.mp3").unwrap();
        let reads: Arc<Mutex<Vec<f64>>> = Arc::default();
        let decode = {
            let reads = reads.clone();
            let peaks: Arc<Mutex<Option<wave::Shared>>> = Arc::default();
            let theirs = peaks.clone();
            let decode = wave::start(&tools, &media.input, 2, media.duration, move || {
                let shared = theirs.lock().unwrap().clone();
                if let Some(s) = shared.and_then(|s| s.lock().unwrap().clone()) {
                    reads.lock().unwrap().push(s.read);
                }
            });
            *peaks.lock().unwrap() = Some(decode.peaks.clone());
            decode
        };
        until("the waveform is complete", || {
            decode
                .peaks
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|p| p.complete || p.error.is_some())
        });
        let done = decode.peaks.lock().unwrap().clone().unwrap();
        assert!(done.complete, "{:?}", done.error);
        idle(&source);
        let fetched = source.fetched().total;
        let size = source.size();
        eprintln!(
            "V19 waveform of {size} bytes: fetched {fetched}; reach at each hand-over {:?}",
            reads.lock().unwrap()
        );
        assert!(fetched as f64 <= 1.05 * size as f64, "fetched {fetched}");
        let reads = reads.lock().unwrap().clone();
        // Each notice reads the peaks as they are by then: two may see the same hand-over.
        assert!(reads.windows(2).all(|w| w[0] <= w[1]), "{reads:?}");
        let mut partial: Vec<f64> = reads
            .iter()
            .copied()
            .filter(|r| *r < media.duration)
            .collect();
        partial.dedup();
        assert!(partial.iter().all(|r| *r > 0.0), "{reads:?}");
        assert!(partial.len() >= 3, "handed over as it filled: {reads:?}");
        drop(decode);
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// 06 §12 V19: the waveform reads through a `bulk` connection — every request it causes is a
/// Bulk one — and while it runs no bulk request is sent while playback's demand is queued: a
/// seek far into the file goes first.
#[test]
fn v19_the_waveform_yields_to_playback() {
    bounded(240, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("m.mp3");
        long_mp3(&tools, &path);
        let Some((source, mut engine)) = stepped(&path) else {
            return;
        };
        let fastseek = stepping(&mut engine, || {
            let never = AtomicBool::new(false);
            mp3::info_tag(
                &mut source.reader(ReaderKind::Media { bytes_per_s: 0 }),
                &never,
            )
            .unwrap()
        });
        assert!(fastseek);
        let served = Served::start(source.clone(), "m.mp3", "m.mp3", fastseek).unwrap();
        let media = stepping(&mut engine, || {
            Media::served(&tools, served.clone(), "m.mp3", "m.mp3").unwrap()
        });
        // No reader but the waveform's from here: the probe's connections are gone.
        settled_stepped(&source, &mut engine);
        let (from, before) = {
            let core = source.core();
            (core.trace.len(), core.pf.last_id())
        };
        let decode = wave::start(&tools, &media.input, 2, media.duration, || {});
        // The pass streams, then — no step — runs out of blocks and waits for its next one.
        let start = source.stats().fetched;
        while source.stats().fetched.bulk < 4 * MIB {
            let now = source.stats().fetched;
            assert!(
                now.total - start.total < 8 * MIB,
                "the waveform's reads are not Bulk ones: {now:?}"
            );
            engine.step(Some(Duration::from_millis(5)));
        }
        until("the waveform waits for a block", || {
            source.core().pf.waiting_readers() >= 1
        });
        engine.pump();
        let n = source.core().trace.len();
        // Its readers, and only theirs: every one a bulk reader, every request for one a Bulk
        // one, and no request sent meanwhile for a reader opened before it.
        let waveform: Vec<u64> = {
            let core = source.core();
            core.pf
                .reaches()
                .iter()
                .filter(|r| r.id > before)
                .map(|r| r.id)
                .collect()
        };
        assert!(!waveform.is_empty(), "the waveform opened no reader");
        for r in source.core().pf.reaches().iter().filter(|r| r.id > before) {
            assert_eq!(
                r.kind,
                ReaderKind::Bulk,
                "a waveform reader of another kind: {r:?}"
            );
        }
        for t in &source.core().trace[from..n] {
            assert!(
                t.reader.is_none_or(|r| waveform.contains(&r)),
                "a request for a reader that is not the waveform's: {t:?}"
            );
            assert!(
                matches!(t.class, Some(Class::Bulk) | None),
                "a waveform request in another class: {t:?}"
            );
        }
        // Playback seeks to 480 s: a new connection, far ahead of the waveform.
        let src = playback::Source {
            input: media.input.clone(),
            duration: media.duration,
            video: None,
            audio: true,
            pieces: Vec::new(),
            stereo: false,
        };
        let mut player =
            Player::start(&tools, &src, 480.0, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
        // With no step, nothing is fetched: the first connection reads the head the waveform
        // cached, and the seek's connection waits for its block, three quarters in.
        let (far, end) = (
            (source.size() as f64 * 0.75) as u64 / BLOCK,
            source.size() / BLOCK,
        );
        until("the seek's block is asked for", || {
            let core = source.core();
            (far..=end).any(|b| core.pf.waits(b))
        });
        assert_eq!(source.core().trace.len(), n, "nothing was sent meanwhile");
        while !player.sounded() {
            player.pump(Instant::now());
            engine.step(Some(Duration::from_millis(5)));
        }
        let trace = source.core().trace.clone();
        let demand = trace[n..]
            .iter()
            .position(|t| t.class == Some(Class::Demand) && t.block.is_some_and(|b| b >= far))
            .map(|i| n + i)
            .expect("the seek's demand was sent");
        for t in &trace[n..=demand] {
            assert!(
                !matches!(t.class, Some(Class::Bulk)),
                "a bulk request went while playback's demand was queued: {t:?}"
            );
        }
        drop(player);
        drop(decode);
        none_reading(&media);
        stepping(&mut engine, || drop(media));
        none_left(dir.path()).unwrap();
    });
}

/// The export's Cancel in the window (06 §7.4): the progress row's button, and Esc while it runs
/// — before the crop rectangle and the selection, which stay. Once the publishing has begun,
/// it is too late, and the window says so.
#[test]
fn esc_and_the_button_cancel_a_running_export() {
    let Some(tools) = tools() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("v.mp4");
    super::fixtures::clip(&tools, &path, (320, 240));
    let media = Media::probe(&path).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let mut sel = Selection::default();
    sel.set([10, 10, 100, 80]);
    let marks = Marks {
        t: 0.5,
        in_s: Some(0.2),
        out_s: Some(1.2),
    };
    let mut win = MediaWindow::new(
        media,
        MediaFile::new(origin, &tmp.path().join("cache")),
        tmp.path().join("work"),
        marks,
        sel,
        Vec::new(),
    );
    win.sink = Sink::Null;
    let mut drv = crate::gui::testkit::Driver::new(900.0, 560.0);
    drv.until(&mut win, 30, |w| w.frame_ready());
    let job = |win: &mut MediaWindow| {
        let stop = Arc::new(cut::Stop::default());
        let (tx, rx) = mpsc::channel();
        win.job = Some(ExportJob {
            progress: Arc::new(AtomicU32::new(400)),
            rx,
            stop: stop.clone(),
        });
        (stop, tx)
    };
    let (stop, _tx) = job(&mut win);
    drv.idle(&mut win, 2);
    drv.key(&mut win, egui::Key::Escape, egui::Modifiers::NONE);
    drv.idle(&mut win, 1);
    assert!(stop.cancelled(), "Esc cancels the export");
    assert!(
        win.sel.is_set() && win.marks.selected(),
        "Esc went to the export only"
    );
    // The status says so (the window's crop is an edit: the status row also names it, and may
    // cut the status short on this narrow window).
    assert_eq!(win.status.text, "cancelling the export…");
    let (stop, _tx) = job(&mut win);
    drv.idle(&mut win, 2);
    drv.click_text(&mut win, "Cancel");
    drv.idle(&mut win, 1);
    assert!(stop.cancelled(), "the button cancels the export");
    // Too late: the link was made.
    let (stop, _tx) = job(&mut win);
    assert!(stop.publish_for_tests());
    drv.idle(&mut win, 2);
    drv.key(&mut win, egui::Key::Escape, egui::Modifiers::NONE);
    drv.idle(&mut win, 1);
    assert!(!stop.cancelled());
    assert_eq!(
        win.status.text,
        "too late to cancel: the export is being published"
    );
}

/// [`idle`] for a stream whose engine the test steps.
/// [`settled`] and [`settled_stepped`] return only once every reader is closed — a reader left
/// open by a test's setup (here one a thread closes 300 ms on, after it asked for a block) is
/// waited for, though nothing is queued meanwhile.
#[test]
fn settling_waits_for_every_reader_to_close() {
    bounded(120, |t| {
        let dir = t.tmp();
        let path = dir.path().join("f");
        std::fs::write(&path, vec![1u8; 4 << 20]).unwrap();
        let lingers = |source: &Source| {
            let mut reader = source.reader(ReaderKind::Media { bytes_per_s: 0 });
            let closed = Arc::new(Mutex::new(None));
            let at = closed.clone();
            let thread = std::thread::spawn(move || {
                let mut buf = [0u8; 16];
                let _ = reader.read_at(0, &mut buf, &AtomicBool::new(false));
                std::thread::sleep(Duration::from_millis(300));
                // The time is taken before the close: a settle that waits for it returns after.
                let c = Instant::now();
                drop(reader);
                *at.lock().unwrap() = Some(c);
            });
            (thread, closed)
        };
        let Some(source) = streamed(&path) else {
            return;
        };
        let (thread, closed) = lingers(&source);
        until("the reader has read", || source.fetched().total > 0);
        settled(&source);
        let back = Instant::now();
        thread.join().unwrap();
        assert!(
            closed.lock().unwrap().is_some_and(|c| c <= back),
            "settled before the close"
        );
        let Some((source, mut engine)) = stepped(&path) else {
            return;
        };
        let (thread, closed) = lingers(&source);
        until("the reader waits", || {
            source.core().pf.waiting_readers() == 1
        });
        engine.step(Some(Duration::from_millis(5)));
        settled_stepped(&source, &mut engine);
        let back = Instant::now();
        thread.join().unwrap();
        assert!(
            closed.lock().unwrap().is_some_and(|c| c <= back),
            "settled before the close"
        );
    });
}

/// Steps `engine` until no reader of `source` is open and it is idle: what was opened for the
/// test's setup (a probe's connections, whose server threads go on after ffprobe has exited)
/// is gone, and nothing it asked for is still to be sent.
fn settled_stepped(source: &Source, engine: &mut Engine) {
    let give_up = Instant::now() + Duration::from_secs(60);
    loop {
        {
            let mut core = source.core();
            if core.pf.open_ids().is_empty() && !core.queued() && core.pf.adm.in_flight() == 0 {
                return;
            }
        }
        assert!(Instant::now() < give_up, "the stream does not settle");
        engine.step(Some(Duration::from_millis(5)));
    }
}

/// 06 §9 and §12 V20's last item: every ffmpeg and ffprobe run on a remote input — the probe,
/// playback's picture and sound, the paused frame, the waveform, a render of two inputs — has
/// a bounded receive buffer (`-recv_buffer_size`) and `-protocol_whitelist http,tcp` before
/// each of its URLs, and reads the URL of its own reader
/// kind (06 §7.4: `play`, `bulk`, `render`). A stand-in records each command line and fails
/// (it asks the real ffmpeg for its encoders, which a render looks up first).
#[test]
fn every_run_on_a_stream_reads_its_kind_behind_the_whitelist() {
    bounded(120, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("v.mp4");
        long_video(&tools, &path, 20);
        let Some(source) = streamed(&path) else {
            return;
        };
        let mut media = Media::stream(source, "v.mp4", "v.mp4").unwrap();
        let (kind, plan) = planned(&mut media);
        let calls = dir.path().join("calls");
        let recorder = super::fixtures::stand_in(
            dir.path(),
            "recorder",
            &format!(
                "case \"$*\" in *-encoders*) exec '{}' \"$@\";; esac\n\
                 printf '%s\\n' \"$*\" >> '{}'\nexit 1",
                tools.ffmpeg.display(),
                calls.display()
            ),
        );
        let rec = Tools {
            ffmpeg: recorder.clone(),
            ffprobe: recorder,
        };
        let _ = ffmpeg::probe_input(&rec, &media.input, "v.mp4");
        let src = playback::Source {
            input: media.input.clone(),
            duration: media.duration,
            video: media.decode_size().map(|size| (size, media.fps)),
            audio: media.has_audio,
            pieces: Vec::new(),
            stereo: false,
        };
        let player = Player::start(&rec, &src, 1.0, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
        // Both decoders have said their command line (a loaded machine starts them slowly).
        until("the player's decoders ran", || {
            let said = std::fs::read_to_string(&calls).unwrap_or_default();
            said.contains("rawvideo") && said.contains("f32le -ac")
        });
        drop(player);
        let mut recorded = media.clone();
        recorded.tools = rec.clone();
        let scrub = Scrubber::start(&recorded, egui::Context::default()).unwrap();
        scrub.request(2.0, 50);
        until("the frame's decode ran", || {
            scrub.result.lock().unwrap().take().is_some()
        });
        drop(scrub);
        let wave = wave::start(&rec, &media.input, 2, media.duration, || {});
        until("the waveform's decode ran", || {
            wave.peaks
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|p| p.error.is_some())
        });
        let spec = spec_of(&media, &[(12.0, 2.0), (3.0, 2.0)]);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let out = work.join("out.mp4");
        assert!(cut::render_edit(
            &rec,
            &media.input,
            &out,
            &spec,
            kind,
            &plan,
            &work,
            &mut |_| {}
        )
        .is_err());
        let lines = std::fs::read_to_string(&calls).unwrap();
        let mut kinds = std::collections::BTreeMap::new();
        for line in lines.lines().filter(|l| l.contains("http://")) {
            let urls = line.matches("http://").count();
            let guard = format!(
                "-recv_buffer_size {} -protocol_whitelist http,tcp",
                ffmpeg::RECV_BUFFER
            );
            let guarded = line.matches(&format!("{guard} http://")).count()
                + line.matches(&format!("{guard} -i http://")).count()
                + line
                    .matches(&format!("{guard} -fflags +fastseek -i http://"))
                    .count();
            assert_eq!(
                guarded, urls,
                "a URL without its receive buffer and whitelist: {line}"
            );
            let kind = ["/play/", "/bulk/", "/render/"]
                .into_iter()
                .find(|k| line.contains(k))
                .expect("a reader kind");
            let what = if line.contains("-show_streams") {
                "probe"
            } else if line.contains("-progress") {
                "render"
            } else if line.contains("f32le") && line.contains("-ar 8000") {
                "waveform"
            } else if line.contains("f32le") {
                "playback sound"
            } else if line.contains("format=rgba") && line.contains("-frames:v 1") {
                "paused frame"
            } else {
                "playback picture"
            };
            kinds.insert(what, (kind, urls));
        }
        eprintln!("runs on the stream: {kinds:?}");
        assert_eq!(kinds.get("probe"), Some(&("/play/", 1)));
        assert_eq!(kinds.get("playback picture"), Some(&("/play/", 1)));
        assert_eq!(kinds.get("playback sound"), Some(&("/play/", 1)));
        assert_eq!(kinds.get("paused frame"), Some(&("/play/", 1)));
        assert_eq!(kinds.get("waveform"), Some(&("/bulk/", 1)));
        assert_eq!(kinds.get("render"), Some(&("/render/", 2)));
        drop(wave);
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// Writes into the file in place, in another whole second (SFTP carries no more than that): its
/// stream turns `Changed` at its next look.
fn change_in_place(path: &Path, source: &Source) {
    use std::os::unix::fs::FileExt;
    let size = std::fs::metadata(path).unwrap().len();
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.write_all_at(&[0xEE; 4096], size / 2).unwrap();
    file.set_modified(source.opened().mtime + Duration::from_secs(10))
        .unwrap();
}

/// ffmpeg ends an input cut short as if the file ended there (06 §7.4), so every run on a stream
/// reads the server's cut account and says the Source's state: the waveform is not complete,
/// playback stops, the paused frame and a probe fail — each saying the file changed.
#[test]
fn a_stream_cut_short_fails_every_run_with_the_source_s_state() {
    bounded(240, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("v.mp4");
        long_video(&tools, &path, 600);
        let Some((source, mut engine)) = stepped(&path) else {
            return;
        };
        let served = Served::start(source.clone(), "v.mp4", "v.mp4", false).unwrap();
        let media = stepping(&mut engine, || {
            Media::served(&tools, served.clone(), "v.mp4", "v.mp4").unwrap()
        });
        // No reader open: the readers counted below are the runs'.
        settled_stepped(&source, &mut engine);
        // The waveform streams, then — no step — waits for its next block.
        let wave = wave::start(&tools, &media.input, 2, media.duration, || {});
        while source.stats().fetched.bulk < 4 * MIB {
            engine.step(Some(Duration::from_millis(5)));
        }
        until("the waveform waits", || {
            source.core().pf.waiting_readers() >= 1
        });
        // Playback at 100 s and the paused frame at 300 s: each waits for its first block.
        let src = playback::Source {
            input: media.input.clone(),
            duration: media.duration,
            video: media.decode_size().map(|size| (size, media.fps)),
            audio: media.has_audio,
            pieces: Vec::new(),
            stereo: false,
        };
        let mut player =
            Player::start(&tools, &src, 100.0, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
        let scrub =
            Scrubber::start_with(&media, egui::Context::default(), Duration::from_secs(120))
                .unwrap();
        scrub.request(300.0, 300 * 25);
        until("playback and the frame wait for their blocks", || {
            source.core().pf.waiting_readers() >= 4
        });
        change_in_place(&path, &source);
        let changed = |what: &str, said: &str| {
            assert!(said.contains("changed on the remote"), "{what}: {said}");
        };
        let (mut heard, mut framed) = (None, None);
        let give_up = Instant::now() + Duration::from_secs(60);
        let waved = loop {
            let peaks = wave.peaks.lock().unwrap().clone();
            assert!(
                Instant::now() < give_up,
                "not every run said the change: waveform {:?}, playback {heard:?} (its \
                 decoders say {:?}, frame ready {}), frame {:?}",
                peaks.map(|p| (p.complete, p.error)),
                player.error(),
                player.frame_ready(),
                framed
            );
            engine.step(Some(Duration::from_millis(5)));
            let pump = player.pump(Instant::now());
            if pump.error.is_some() {
                heard = pump.error;
            }
            if let Some(frame) = scrub.result.lock().unwrap().take() {
                framed = Some(frame.map(|_| ()));
            }
            let peaks = wave.peaks.lock().unwrap().clone();
            if let Some(p) = peaks.filter(|p| p.complete || p.error.is_some()) {
                if heard.is_some() && framed.is_some() {
                    break p;
                }
            }
        };
        assert!(!waved.complete, "a cut waveform is never complete");
        changed("the waveform", &waved.error.unwrap());
        changed("playback", &heard.unwrap());
        changed("the paused frame", &framed.unwrap().unwrap_err());
        let probed = stepping(&mut engine, || {
            ffmpeg::probe_input(&tools, &media.input, "v.mp4")
        });
        changed("a probe", &format!("{:#}", probed.unwrap_err()));
        drop((player, scrub, wave));
        none_reading(&media);
        stepping(&mut engine, || drop(media));
        none_left(dir.path()).unwrap();
    });
}

/// The window over a stream (06 §7.4, §11): a click far into the timeline shows the frame there
/// — its pixels nearer the frame decoded from the file at that place than either neighbour's —
/// and ends its wait (`ready`), with no read still said; once the file has changed on the
/// remote, a click elsewhere ends its wait too, and no picture stands for the place asked for:
/// the earlier frame goes, and the change is said where it was.
#[test]
fn a_far_scrub_in_the_window_shows_its_frame_and_ends_its_wait() {
    bounded(240, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("v.mp4");
        long_video(&tools, &path, 600);
        let Some(source) = streamed(&path) else {
            return;
        };
        let media = Media::stream(source.clone(), "v.mp4", "v.mp4").unwrap();
        let abs = path.to_str().unwrap().to_string();
        let mut fs = LocalFs::new();
        let at = fs.stat(&abs).unwrap();
        let origin = Origin::new(Box::new(fs), None, None, abs, at);
        let mut win = MediaWindow::new(
            media,
            MediaFile::new(origin, &dir.path().join("cache")),
            dir.path().join("work"),
            Marks::default(),
            Selection::default(),
            Vec::new(),
        );
        win.sink = Sink::Null;
        win.unplanned = true;
        let mut drv = crate::gui::testkit::Driver::new(900.0, 560.0);
        drv.until(&mut win, 60, |w| w.frame_ready() && w.planning.is_none());
        let click = |drv: &mut crate::gui::testkit::Driver, win: &mut MediaWindow, f: f32| {
            let r = win.timeline_rect.expect("the timeline is drawn");
            drv.click(win, egui::pos2(r.left() + f * r.width(), r.center().y));
        };
        let before = source.fetched().total;
        click(&mut drv, &mut win, 0.8);
        assert!(win.asked.is_some(), "the click is an input");
        drv.until(&mut win, 60, |w| w.frame_ready());
        drv.idle(&mut win, 2);
        eprintln!(
            "a far scrub to {:.1} s fetched {} bytes",
            win.marks.t,
            source.fetched().total - before
        );
        assert!(win.asked.is_none(), "its ready came with the frame");
        assert!(drv.text_starting_with(READING).is_none());
        // Which frame: the one at the place asked for, decoded from the file itself.
        let (_, key) = win.want.expect("a frame asked for");
        let (w, h) = win.media.decode_size().unwrap();
        let fps = win.media.fps;
        let decoded = |k: i64| {
            let at = win.media.origin() + (k as f64 - 0.5) / fps;
            let out = std::process::Command::new(&tools.ffmpeg)
                .args(["-nostdin", "-loglevel", "error", "-ss", &format!("{at:.6}")])
                .arg("-i")
                .arg(&path)
                .args(["-an", "-vf", &format!("scale={w}:{h},format=rgba")])
                .args(["-frames:v", "1", "-f", "rawvideo", "-"])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        };
        let apart = |a: &[u8], b: &[u8]| -> u64 {
            assert_eq!(a.len(), b.len());
            a.iter()
                .zip(b)
                .map(|(x, y)| u64::from(x.abs_diff(*y)))
                .sum()
        };
        let shown = win.shown_rgba.clone();
        let picture = drv.pictures();
        assert_eq!(picture.len(), 1, "the frame is on screen: {picture:?}");
        let (here, before, after) = (
            apart(&shown, &decoded(key)),
            apart(&shown, &decoded(key - 1)),
            apart(&shown, &decoded(key + 1)),
        );
        eprintln!(
            "frame {key}: {here} apart from its decode, {before} and {after} from its neighbours'"
        );
        assert!(
            here < before && here < after,
            "frame {key}: {here} / {before} / {after}"
        );
        // The file changes: the next place asked for puts no frame up, and says why.
        change_in_place(&path, &source);
        click(&mut drv, &mut win, 0.3);
        drv.until(&mut win, 60, |w| w.asked.is_none());
        drv.idle(&mut win, 2);
        assert!(!win.frame_ready(), "no frame for the place asked for");
        assert_eq!(win.tex_at, None, "no picture claims a place");
        assert!(win.shown_rgba == shown, "nothing new was put up");
        assert!(
            drv.pictures().is_empty(),
            "the earlier frame is gone: {:?}",
            drv.pictures()
        );
        // Said where the picture was (and in the status line).
        let area = picture[0].expand(8.0);
        let said: Vec<_> = drv
            .texts()
            .into_iter()
            .filter(|t| t.2.contains("changed on the remote"))
            .collect();
        assert!(
            said.iter().any(|t| area.contains_rect(t.0)),
            "{said:?} not in {area:?}"
        );
        assert!(
            win.status.text.contains("changed on the remote"),
            "{}",
            win.status.text
        );
        assert!(drv.text_starting_with(READING).is_none());
        drop(win);
        none_left(dir.path()).unwrap();
    });
}

/// A sound whose waveform is still being read (06 §7.4): the part of the timeline not read yet
/// lies under the dim band, and with `loading=1` (06 §11) the window is ready to be shot at its
/// first peaks; without it, only once the waveform is whole.
#[test]
fn a_waveform_still_reading_is_dimmed_and_loading_shoots_it_as_it_is() {
    let Some(tools) = tools() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.wav");
    super::fixtures::tone(&tools, &path, 10);
    let media = Media::probe(&path).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let mut win = MediaWindow::new(
        media,
        MediaFile::new(origin, &tmp.path().join("cache")),
        tmp.path().join("work"),
        Marks::default(),
        Selection::default(),
        Vec::new(),
    );
    win.sink = Sink::Null;
    let mut drv = crate::gui::testkit::Driver::new(900.0, 460.0);
    drv.until(&mut win, 30, |w| {
        w.wave.is_some() && w.timeline_rect.is_some()
    });
    // The first 4 s of 10 read.
    let bins = wave::bins_for(10.0);
    let partial = wave::Source {
        bin_secs: 10.0 / bins as f64,
        lanes: vec![(0..bins)
            .map(|k| {
                if k < bins * 4 / 10 {
                    [-0.5, 0.5]
                } else {
                    [0.0, 0.0]
                }
            })
            .collect()],
        version: 99,
        complete: false,
        read: 4.0,
        error: None,
    };
    win.wave = Some(wave::Decode::done(partial));
    win.columns = None;
    drv.idle(&mut win, 3);
    assert!(
        !super::super::Window::ready(&win),
        "not ready before it is whole"
    );
    let r = win.timeline_rect.unwrap();
    let dim = crate::gui::chrome::PALETTE.dim;
    // The band's hatch: full-height lines in the dim colour, from 4 s to the end.
    let hatch: Vec<f32> = drv
        .segments()
        .into_iter()
        .filter(|(p, c)| {
            *c == dim && p[0].x == p[1].x && (p[1].y - p[0].y).abs() >= r.height() - 1.0
        })
        .map(|(p, _)| p[0].x)
        .collect();
    let (from, to) = hatch
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
    let x4 = r.left() + 0.4 * r.width();
    assert!(hatch.len() > 10, "the unread part is dimmed: {hatch:?}");
    assert!(
        (from - x4).abs() <= 2.0,
        "from {from}; 4 s of 10 is at {x4} in {r:?}"
    );
    assert!(
        r.right() - to <= UNREAD_HATCH + 1.0,
        "to {to}; the end is at {}",
        r.right()
    );
    win.loading = true;
    drv.idle(&mut win, 3);
    assert!(
        super::super::Window::ready(&win),
        "loading: ready at its first peaks"
    );
}

/// A streamed sound that plays without sound (here: its ffmpeg is gone after the window opened,
/// so no decoder starts): a seek while it plays does not wait for sound that will not come —
/// the window says why it is silent, never that it is still reading.
#[test]
fn a_seek_in_a_sound_playing_silently_ends_its_wait() {
    bounded(120, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("m.mp3");
        let noise = "anoisesrc=seed=5:amplitude=0.3:sample_rate=44100";
        make(
            &tools,
            &["-f", "lavfi", "-i", noise, "-t", "60", "-c:a", "libmp3lame"],
            &path,
        );
        let Some(source) = streamed(&path) else {
            return;
        };
        let media = Media::stream(source, "m.mp3", "m.mp3").unwrap();
        let abs = path.to_str().unwrap().to_string();
        let mut fs = LocalFs::new();
        let at = fs.stat(&abs).unwrap();
        let origin = Origin::new(Box::new(fs), None, None, abs, at);
        let mut win = MediaWindow::new(
            media,
            MediaFile::new(origin, &dir.path().join("cache")),
            dir.path().join("work"),
            Marks::default(),
            Selection::default(),
            Vec::new(),
        );
        win.sink = Sink::Null;
        let mut drv = crate::gui::testkit::Driver::new(900.0, 460.0);
        drv.until(&mut win, 60, |w| w.painted);
        Arc::make_mut(&mut win.media).tools.ffmpeg = dir.path().join("missing");
        drv.key(&mut win, egui::Key::Space, egui::Modifiers::NONE);
        drv.idle(&mut win, 2);
        assert!(win.player.is_some(), "it plays, silently");
        let was = win.marks.t;
        drv.key(&mut win, egui::Key::ArrowRight, egui::Modifiers::NONE);
        assert!(win.marks.t > was, "the seek moved the playhead");
        drv.until(&mut win, 60, |w| {
            w.asked.is_none() && w.status.text.contains("playing without sound")
        });
        // And it stays so.
        drv.idle(&mut win, 5);
        assert!(win.asked.is_none(), "no sound is waited for");
        assert!(drv.text_starting_with(READING).is_none());
        assert!(
            win.status.text.contains("playing without sound"),
            "{}",
            win.status.text
        );
        drop(win);
        none_left(dir.path()).unwrap();
    });
}

/// 06 §7.4, §12 V22: Cancel during the upload of a large Export (115 MB over SFTP) stops
/// sending. The Cancel lands at the upload's own checkpoint once 8 MiB are in the remote temp;
/// what the temp holds when it is removed is at most one chunk more; nothing is linked and the
/// folder is left empty.
#[test]
fn v22_cancel_during_a_large_upload_stops_sending() {
    bounded(240, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("long.wav");
        let sound = "sine=frequency=330:sample_rate=48000:duration=600";
        make(&tools, &["-f", "lavfi", "-i", sound, "-ac", "2"], &path);
        let Some(source) = streamed(&path) else {
            return;
        };
        let media = Media::stream(source, "long.wav", "long.wav").unwrap();
        let Some(sftp) = SftpRemote::local_server() else {
            return;
        };
        let stop = Arc::new(cut::Stop::default());
        let mut fs = Publishing {
            fs: Box::new(sftp),
            stop: stop.clone(),
            at: CancelAt::MidUpload(8 * MIB),
            seen: Arc::default(),
        };
        // The export publishes into a folder of its own.
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let spec = spec_of(&media, &[(0.0, 600.0)]);
        // 600 s of 16-bit stereo at 48 kHz.
        let size = 600 * 48_000 * 4;
        let result = cut::render_and_publish(
            &mut fs,
            &tools,
            &media.input,
            out.to_str().unwrap(),
            "long.wav",
            &spec,
            (0.0, 600.0),
            &dir.path().join("exports"),
            &mut |_| {},
            &stop,
        );
        let err = result.unwrap_err();
        assert!(crate::remote::is_cancelled(&err), "{err:#}");
        let seen = fs.seen.lock().unwrap();
        eprintln!("an upload of {size} bytes, cancelled: {seen:?}");
        let at_cancel = seen.at_cancel.expect("the Cancel landed mid-upload");
        let at_removal = seen.at_removal.expect("the temp was removed");
        let total = seen.total.expect("an upload");
        assert!(total >= size, "{seen:?}: the whole export is uploaded");
        assert!(at_cancel >= 8 * MIB && at_cancel < total, "{seen:?}");
        assert!(
            at_removal <= at_cancel + (256 << 10),
            "{} bytes written after the Cancel",
            at_removal - at_cancel
        );
        assert!(!seen.linked, "nothing linked");
        let left: Vec<_> = std::fs::read_dir(&out).unwrap().flatten().collect();
        assert!(left.is_empty(), "nothing left on the remote: {left:?}");
        drop(seen);
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// A stand-in for ffmpeg and ffprobe that fails naming its input's URL, as ffmpeg does for a
/// server's refusal, the server's TCP endpoint and the URL's authority alone, as it does where
/// it cannot connect — and, last, addresses that are not this server's (it answers a render's
/// encoder question from the real ffmpeg).
fn complainer(dir: &Path, real: &Path) -> Tools {
    let body = format!(
        "case \"$*\" in *-encoders*) exec '{}' \"$@\";; esac\n\
         prev=; url=; last=\n\
         for a in \"$@\"; do last=$a; [ \"$prev\" = -i ] && url=$a; prev=$a; done\n\
         [ -z \"$url\" ] && url=$last\n\
         port=${{url#http://127.0.0.1:}}; port=${{port%%/*}}\n\
         echo \"Connection to tcp://127.0.0.1:$port failed: Connection refused; \
         http://127.0.0.1:$port: $url: Server returned 404 Not Found; not ours: \
         127.0.0.1:1 and http://127.0.0.1:${{port}}9/x\" >&2\n\
         exit 1",
        real.display()
    );
    let path = super::fixtures::stand_in(dir, "complainer", &body);
    Tools {
        ffmpeg: path.clone(),
        ffprobe: path,
    }
}

/// 06 §9: the byte server's address and token are never shown. ffmpeg names its input in its
/// errors; every one sopen shows — a render's, playback's, the waveform's, a probe's — says the
/// file as its owner knows it instead.
#[test]
fn ffmpeg_s_errors_name_the_file_never_the_stream_s_url() {
    bounded(120, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("v.mp4");
        long_video(&tools, &path, 20);
        let Some(source) = streamed(&path) else {
            return;
        };
        let shown = "remote-host:/media/v.mp4";
        let mut media = Media::stream(source, "v.mp4", shown).unwrap();
        let (kind, plan) = planned(&mut media);
        let url = media.input.show();
        let token = url.split('/').nth(3).unwrap().to_string();
        assert_eq!(token.len(), 32, "{url}");
        let bad = complainer(dir.path(), &tools.ffmpeg);
        let port = url
            .split('/')
            .nth(2)
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap()
            .to_string();
        let ours = format!("127.0.0.1:{port}");
        let theirs = format!("http://127.0.0.1:{port}9/x");
        // Nothing of this server's address or token; another address as it was printed.
        let unexposed = |what: &str, said: &str| {
            assert!(
                !said.replace(&theirs, "").contains(&ours),
                "{what} shows the server: {said}"
            );
            assert!(!said.contains(&token), "{what} shows the token: {said}");
        };
        let clean = |what: &str, said: &str| {
            unexposed(what, said);
            assert!(said.contains(shown), "{what} names the file: {said}");
            assert!(
                said.contains(&format!("Connection to {shown} failed")),
                "{what}: {said}"
            );
            assert!(said.contains(&theirs), "{what} keeps another port: {said}");
            assert!(
                said.contains("127.0.0.1:1 "),
                "{what} keeps another address: {said}"
            );
        };
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let spec = spec_of(&media, &[(2.0, 3.0)]);
        let out = work.join("out.mp4");
        let rendered = cut::render_edit(
            &bad,
            &media.input,
            &out,
            &spec,
            kind,
            &plan,
            &work,
            &mut |_| {},
        );
        clean("a render", &format!("{:#}", rendered.unwrap_err()));
        let src = playback::Source {
            input: media.input.clone(),
            duration: media.duration,
            video: media.decode_size().map(|size| (size, media.fps)),
            audio: media.has_audio,
            pieces: Vec::new(),
            stereo: false,
        };
        let player = Player::start(&bad, &src, 1.0, 0.0, Sink::Null, Arc::new(|| {})).unwrap();
        until("playback fails", || player.error().is_some());
        clean("playback", &player.error().unwrap());
        drop(player);
        let wave = wave::start(&bad, &media.input, 2, media.duration, || {});
        until("the waveform fails", || {
            wave.peaks
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|p| p.error.is_some())
        });
        let waved = wave.peaks.lock().unwrap().clone().unwrap().error.unwrap();
        clean("the waveform", &waved);
        let probed = ffmpeg::probe_input(&bad, &media.input, shown).unwrap_err();
        clean("a probe", &format!("{probed:#}"));
        // The paused frame shows none of ffmpeg's words (its errors are not read): only its
        // own.
        let mut failing = media.clone();
        failing.tools = bad.clone();
        let scrub = Scrubber::start(&failing, egui::Context::default()).unwrap();
        scrub.request(2.0, 50);
        until("the frame fails", || {
            scrub.result.lock().unwrap().frame.is_some()
        });
        let framed = scrub.result.lock().unwrap().take().unwrap();
        unexposed("the paused frame", &framed.map(|_| ()).unwrap_err());
        drop(scrub);
        drop(wave);
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// A stand-in for ffmpeg that reads its input — 64 KiB every 20 ms, 12.5 MiB in all, more than
/// the file or any socket's buffer — and prints nothing, then ends well: a render reading deep
/// into a file to its first piece, with no output meanwhile.
fn slow_reader(dir: &Path) -> Tools {
    let path = dir.join("slow-reader");
    let script = "#!/bin/bash\n[ \"$1\" = -version ] && exit 0\n\
        url=\"\"; prev=\"\"\n\
        for a in \"$@\"; do [ \"$prev\" = -i ] && url=\"$a\"; prev=\"$a\"; done\n\
        rest=\"${url#http://127.0.0.1:}\"; port=\"${rest%%/*}\"; path=\"/${rest#*/}\"\n\
        exec 3<>\"/dev/tcp/127.0.0.1/$port\"\n\
        printf 'GET %s HTTP/1.1\\r\\nHost: 127.0.0.1:%s\\r\\n\\r\\n' \"$path\" \"$port\" >&3\n\
        for i in $(seq 1 200); do head -c 65536 <&3 >/dev/null; sleep 0.02; done\n\
        exit 0\n";
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::testutil::run_fresh(std::process::Command::new(&path).arg("-version"));
    Tools {
        ffmpeg: path.clone(),
        ffprobe: path,
    }
}

/// 06 §7.4: a render's watchdog counts its input going on as progress — the bytes the byte
/// server got onto the run's socket. The clock moves inside the watchdog's own read of the
/// count, each time it is higher than at the last move, by 0.8 × the limit: so the watchdog
/// reads the moved time for the wait it resets, and never sees a move it has not reset for.
/// Several moves, far past the limit with no output, and a run being fed is not stopped. One
/// that takes nothing gives the server nothing to send: once the watchdog has read the same
/// count twice running (the reset for it done), the clock moves past the limit once, and it is
/// stopped, and its seat comes back. The file (24 MB) and what the reader takes (12.5 MiB) are
/// larger than the sockets' buffers on either platform.
#[test]
fn a_render_fed_by_its_input_is_not_stopped_and_one_starved_is() {
    bounded(180, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("v.mp4");
        long_video(&tools, &path, 180);
        let Some(source) = streamed(&path) else {
            return;
        };
        let media = Media::stream(source, "v.mp4", "v.mp4").unwrap();
        let render = media.input.of_kind(Kind::Render);
        let args = render.args();
        let limit = ffmpeg::STALL_LIMIT;
        let hand = t.hand();
        let now = hand.clock();
        // What the watchdog last read of the count, and how many of its reads running gave it.
        let (last, same) = (AtomicU64::new(0), AtomicUsize::new(0));
        // While `moving`, a higher count moves the clock, before the watchdog reads the time.
        let (moving, moved_at, moves) = (
            AtomicBool::new(true),
            AtomicU64::new(0),
            AtomicUsize::new(0),
        );
        let fed = || {
            let n = render.fed();
            if last.swap(n, SeqCst) == n {
                same.fetch_add(1, SeqCst);
            } else {
                same.store(0, SeqCst);
            }
            if moving.load(SeqCst) && n > moved_at.load(SeqCst) {
                moved_at.store(n, SeqCst);
                hand.advance(limit.mul_f64(0.8));
                moves.fetch_add(1, SeqCst);
            }
            n
        };
        // The run's watch, made on the run's own thread.
        let run_on = |tools: &Tools| {
            let watched = ffmpeg::Watched {
                fed: &fed,
                now: &*now,
                ..ffmpeg::Watched::plain()
            };
            ffmpeg::run_stoppable(tools, &args, 1.0, &mut |_| {}, limit, watched)
        };
        let reader = slow_reader(dir.path());
        run_on(&reader).expect("fed, it is not stopped");
        let moves = moves.load(SeqCst);
        assert!(
            moves >= 3,
            "{moves} moves of 0.8 × the limit: past it, with no output"
        );
        moving.store(false, SeqCst);
        let held = holder(dir.path());
        let server = media.input.served().unwrap().server();
        std::thread::scope(|scope| {
            let run = scope.spawn(|| run_on(&held));
            until("the starved run holds its seat", || {
                server.watch().open() >= 1
            });
            // What the sockets take is sent; then nothing more goes, and the watchdog has read
            // that count in a later read than the one that first gave it.
            let mut sent = render.fed();
            loop {
                std::thread::sleep(Duration::from_millis(300));
                if render.fed() == sent {
                    break;
                }
                sent = render.fed();
            }
            until("the watchdog has read the count it stands at twice", || {
                last.load(SeqCst) == sent && same.load(SeqCst) >= 1
            });
            assert_eq!(render.fed(), sent, "nothing more went");
            assert!(!run.is_finished(), "nothing stops it before its time");
            hand.advance(limit + Duration::from_secs(1));
            let err = run.join().unwrap().unwrap_err();
            assert!(format!("{err:#}").contains("no progress"), "{err:#}");
        });
        until("the stopped run's seat is free", || {
            server.watch().open() == 0
        });
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// [`ffmpeg::Watchdog::fed`]: more input than before is progress, the same is not.
#[test]
fn the_watchdog_counts_input_as_progress() {
    let t0 = Instant::now();
    let limit = Duration::from_secs(10);
    let mut dog = ffmpeg::Watchdog::new(limit, t0);
    dog.fed(100, t0 + Duration::from_secs(8));
    assert!(!dog.stalled(t0 + Duration::from_secs(17)), "fed at 8 s");
    dog.fed(100, t0 + Duration::from_secs(17));
    assert!(
        dog.stalled(t0 + Duration::from_secs(19)),
        "the same bytes are no progress"
    );
}

/// `RemoteFs::create_from_stoppable` (06 §7.4's upload Cancel), on SFTP and on a local disk: a
/// stop that comes after a few chunks ends the upload there — far short of the file, the error
/// a Cancel, the path the caller's to remove. On SFTP the handle's close is in the server's log
/// when the call returns.
#[test]
fn an_upload_stops_between_chunks_its_close_logged_by_the_return() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("big");
    std::fs::write(&src, vec![7u8; 32 << 20]).unwrap();
    // The SFTP session logs its requests (`sftp-server -l INFO`): a handle's close is there.
    let log = tmp.path().join("sftp-server.log");
    let mut layers: Vec<(&str, Box<dyn RemoteFs>)> = vec![("local", Box::new(LocalFs::new()))];
    if let Some(sftp) = SftpRemote::local_server_args(&[], Some(&log)) {
        layers.push(("sftp", Box::new(sftp)));
    }
    for (what, mut fs) in layers {
        let dst = tmp.path().join(format!("up-{what}"));
        let looks = std::cell::Cell::new(0);
        let stop = || {
            looks.set(looks.get() + 1);
            looks.get() > 4
        };
        let err = fs
            .create_from_stoppable(&src, dst.to_str().unwrap(), None, &stop)
            .unwrap_err();
        assert!(
            crate::remote::is_cancelled(&err.error),
            "{what}: {:#}",
            err.error
        );
        assert_eq!(err.created, crate::remote::Created::Yes, "{what}");
        let len = std::fs::metadata(&dst).unwrap().len();
        assert!(len <= 4 << 20, "{what}: {len} bytes went before the stop");
        if what == "sftp" {
            // The close is in the server's log when the call returns. That is all this shows:
            // not that its reply was waited for. With the handle dropped unclosed instead (the
            // code before), the drop's close was not in the log at that moment in 6 runs of 6
            // (macOS).
            let said = std::fs::read_to_string(&log).unwrap();
            let closed = format!("close \"{}\"", dst.display());
            assert!(
                said.contains(&closed),
                "{what}: no close in the log: {said}"
            );
        }
        // Unstopped, the whole file goes.
        let whole = tmp.path().join(format!("whole-{what}"));
        let up = fs.create_from(&src, whole.to_str().unwrap(), None).unwrap();
        assert_eq!(up.size, 32 << 20, "{what}");
    }
}

/// A sound's first peaks are its first content on screen (`first`), not the answer to a place
/// asked for: a seek waiting for its sound keeps waiting — its `ready` comes with sound, or its
/// end with a failure or the end of playback.
#[test]
fn a_sound_s_first_peaks_do_not_answer_a_seek() {
    bounded(120, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("m.mp3");
        let noise = "anoisesrc=seed=5:amplitude=0.3:sample_rate=44100";
        make(
            &tools,
            &["-f", "lavfi", "-i", noise, "-t", "30", "-c:a", "libmp3lame"],
            &path,
        );
        let Some(source) = streamed(&path) else {
            return;
        };
        let media = Media::stream(source, "m.mp3", "m.mp3").unwrap();
        let abs = path.to_str().unwrap().to_string();
        let mut fs = LocalFs::new();
        let at = fs.stat(&abs).unwrap();
        let origin = Origin::new(Box::new(fs), None, None, abs, at);
        let mut win = MediaWindow::new(
            media,
            MediaFile::new(origin, &dir.path().join("cache")),
            dir.path().join("work"),
            Marks::default(),
            Selection::default(),
            Vec::new(),
        );
        win.sink = Sink::Null;
        let mut drv = crate::gui::testkit::Driver::new(900.0, 460.0);
        drv.idle(&mut win, 1);
        // Nothing on screen yet, a seek waiting for its sound, and then the first peaks.
        let bins = wave::bins_for(win.media.duration);
        let first = wave::Source {
            bin_secs: win.media.duration / bins as f64,
            lanes: vec![vec![[-0.3, 0.3]; bins]; 2],
            version: 1,
            complete: false,
            read: 3.0,
            error: None,
        };
        win.wave = Some(wave::Decode::done(first));
        win.marked = false;
        win.asked = Some((Instant::now(), 0));
        drv.idle(&mut win, 2);
        assert!(win.marked, "the first peaks are the first content");
        assert!(win.asked.is_some(), "the seek still waits for its sound");
        drop(win);
        none_left(dir.path()).unwrap();
    });
}

/// A plain HTTP/1.1 GET of `url` on its own connection: its socket, with the request sent.
fn get(url: &str) -> std::net::TcpStream {
    use std::io::Write;
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    let mut socket = std::net::TcpStream::connect(host).unwrap();
    write!(
        socket,
        "GET /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    socket
}

/// Reads `socket` until `want` bytes have come, or its end: how many came.
fn read_up_to(socket: &mut std::net::TcpStream, want: u64) -> u64 {
    use std::io::Read;
    let (mut got, mut buf) = (0u64, vec![0u8; 256 << 10]);
    while got < want {
        let n = socket.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        got += n as u64;
    }
    got
}

/// The receive buffer the kernel keeps for `socket`.
fn receive_buffer_of(socket: &std::net::TcpStream) -> u64 {
    use std::os::fd::AsRawFd;
    let mut size: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: a valid descriptor, and an int of the length given to write into.
    unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&mut size as *mut libc::c_int).cast(),
            &mut len,
        );
    }
    size.max(0) as u64
}

/// 06 §7.4, V16: every connection's send buffer is bounded ([`SEND_BUFFER`]; Linux keeps twice
/// what is asked, macOS what is asked). A 48 MB body read as fast as the client can never has
/// it grown past that — left alone, Linux grows it to 2.5 MiB, macOS to 170–230 KB. And a client that stops reading, after 8 MiB, has been
/// sent no more than it read, what its own socket holds, the server's buffer and the block in
/// hand — counted once the server's write has found no room left and then waited a tick with
/// nothing taken (the `body_blocked` and `evaluated` seams).
#[test]
fn a_connection_is_read_ahead_of_its_client_by_one_send_buffer_at_most() {
    bounded(120, |t| {
        let dir = t.tmp();
        let path = dir.path().join("big");
        let size: u64 = 48 << 20;
        let body: Vec<u8> = (0..size)
            .map(|i| ((i * 2_654_435_761) >> 13) as u8)
            .collect();
        std::fs::write(&path, &body).unwrap();
        let Some(source) = streamed(&path) else {
            return;
        };
        let biggest = Arc::new(AtomicUsize::new(0));
        let seen = biggest.clone();
        // When a body's write found no room (its client takes none), and when a write's wait
        // ran out with nothing taken (every 50 ms while it waits).
        let blocked: Arc<Mutex<Vec<Instant>>> = Arc::default();
        let waited: Arc<Mutex<Vec<Instant>>> = Arc::default();
        let (b, w) = (blocked.clone(), waited.clone());
        let config = Config {
            clock: Arc::new(Instant::now),
            tick: Some(Duration::from_millis(50)),
            seams: Seams {
                send_buffer: Some(Arc::new(move |_, size| {
                    seen.fetch_max(size, SeqCst);
                })),
                body_blocked: Some(Arc::new(move |_, _| {
                    b.lock().unwrap().push(Instant::now());
                })),
                evaluated: Some(Arc::new(move |_, _, _| {
                    w.lock().unwrap().push(Instant::now());
                })),
                ..Seams::default()
            },
        };
        let server = Server::start_with(source.clone(), config).unwrap();
        let url = server.url(Kind::Bulk, "big");
        // What the kernel keeps of the size asked: Linux twice it (for its bookkeeping), macOS
        // just it — and left alone, more than that on either.
        let bound = if cfg!(target_os = "linux") {
            2 * SEND_BUFFER
        } else {
            SEND_BUFFER
        };
        let mut fast = get(&url);
        assert!(
            read_up_to(&mut fast, u64::MAX) > size,
            "the whole body came"
        );
        drop(fast);
        let most = biggest.load(SeqCst);
        eprintln!("a 48 MB body read fast: the send buffer at most {most} bytes");
        assert!(most > 0 && most <= bound, "the send buffer grew to {most}");
        // A client that reads 8 MiB fast, then stops.
        let before = server.sent(Kind::Bulk);
        let mut slow = get(&url);
        let read = read_up_to(&mut slow, 8 << 20);
        let stopped = Instant::now();
        // The server has filled what the sockets hold: its write found no room after the stop,
        // then waited a whole tick with nothing taken, and nothing went meanwhile. (A buffer
        // the kernel grows lets the write go on: then it is waited for again.)
        let after =
            |at: &Mutex<Vec<Instant>>, from: Instant| at.lock().unwrap().iter().any(|t| *t >= from);
        until("the body's write finds no room", || {
            after(&blocked, stopped)
        });
        let sent = loop {
            let (from, sent) = (Instant::now(), server.sent(Kind::Bulk) - before);
            until("the write waits a tick", || after(&waited, from));
            if server.sent(Kind::Bulk) - before == sent {
                break sent;
            }
        };
        let theirs = receive_buffer_of(&slow);
        let ahead = sent - read;
        eprintln!(
            "a client that stopped after {read} bytes: sent {ahead} more (its socket holds \
             {theirs}, the server's {bound}, a block {BLOCK})"
        );
        assert!(
            ahead <= theirs + bound as u64 + BLOCK,
            "{ahead} bytes ahead of the client"
        );
        drop(slow);
        drop(server);
        none_left(dir.path()).unwrap();
    });
}

/// An SFTP upload whose write fails part way (here the 3rd chunk's, as a test's seam has it):
/// the handle's close is in the server's log of the session's requests (`sftp-server -l INFO`)
/// when the call returns, and the error is one with the file made. That is all the log shows —
/// not that the close's reply was waited for. With the handle dropped unclosed instead (the
/// code before), the drop's close was not in the log at that moment in 4 runs of 4 (macOS).
/// Through the publishing of a sibling, the temp is removed and nothing is published.
#[test]
fn a_failed_upload_s_close_is_logged_by_the_return_and_its_temp_goes() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("big");
    std::fs::write(&src, vec![7u8; 4 << 20]).unwrap();
    let log = tmp.path().join("sftp-server.log");
    let Some(mut fs) = SftpRemote::local_server_args(&[], Some(&log)) else {
        return;
    };
    fs.fail_chunk(3);
    let out = tmp.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let up = out.join("up");
    let err = fs
        .create_from(&src, up.to_str().unwrap(), None)
        .unwrap_err();
    assert!(
        format!("{:#}", err.error).contains("the write failed"),
        "{:#}",
        err.error
    );
    assert_eq!(err.created, crate::remote::Created::Yes);
    let said = std::fs::read_to_string(&log).unwrap();
    let closed = format!("close \"{}\"", up.display());
    assert!(said.contains(&closed), "no close in the log: {said}");
    std::fs::remove_file(&up).unwrap();
    // Publishing a sibling: the temp it wrote goes, and nothing is named.
    let err = fs
        .write_new_sibling(&src, out.to_str().unwrap(), "up")
        .unwrap_err();
    assert!(format!("{err:#}").contains("the write failed"), "{err:#}");
    let said = std::fs::read_to_string(&log).unwrap();
    let temp_closed = said
        .lines()
        .any(|l| l.contains("close \"") && l.contains(".up.sopen-"));
    assert!(temp_closed, "no close of the temp in the log: {said}");
    let left: Vec<_> = std::fs::read_dir(&out).unwrap().flatten().collect();
    assert!(left.is_empty(), "left in the folder: {left:?}");
}

/// The stand-in `ffprobe` of the probe-stop tests: it waits at `hold`, never answering, until
/// it is killed. Whether one was started is counted as it is ([`ffmpeg::spawns::naming`]).
fn waiting_probe(dir: &Path, hold: &super::fixtures::Hold) -> Tools {
    let path = super::fixtures::stand_in(dir, "ffprobe", &hold.stalls());
    Tools {
        ffmpeg: path.clone(),
        ffprobe: path,
    }
}

/// How many `ffprobe` runs were started on a file named `name`.
fn probes_started(name: &str) -> u64 {
    ffmpeg::spawns::naming(name).probe
}

/// W8b review: a stop that comes during a remote medium's setup — after the open's first look
/// at it, before ffprobe is started (here an mp3's tag read, which waits on the stream: the
/// barrier) — ends the open there: once the read is let through it returns cancelled within a
/// bounded number of the engine's steps, no ffprobe was started, and none naming the file (a
/// name made for this run) is left.
#[test]
fn a_stop_during_a_mediums_setup_starts_no_probe() {
    bounded(120, |t| {
        let dir = t.tmp();
        let name = format!("stop-{}.mp3", &crate::remote::random_hex()[..12]);
        let file = dir.path().join(&name);
        std::fs::write(&file, vec![0x55u8; 256 * 1024]).unwrap();
        let Some((source, mut engine)) = stepped(&file) else {
            return;
        };
        let hold = super::fixtures::Hold::new(dir.path(), "hold");
        let tools = waiting_probe(dir.path(), &hold);
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let run =
                scope.spawn(|| Media::stream_with(&tools, source.clone(), &name, &name, &stop));
            until("the setup's read waits on the stream", || {
                source.core().pf.waiting_readers() >= 1
            });
            assert!(!run.is_finished(), "held in its setup");
            stop.store(true, SeqCst);
            let mut steps = 0;
            while !run.is_finished() {
                assert!(steps < 1000, "the open still runs after {steps} steps");
                engine.step(Some(Duration::from_millis(5)));
                std::thread::sleep(Duration::from_millis(5));
                steps += 1;
            }
            let Err(err) = run.join().unwrap() else {
                panic!("stopped, not opened");
            };
            assert!(err.is::<crate::remote::Cancelled>(), "{err:#}");
        });
        assert_eq!(probes_started(&name), 0, "no ffprobe was started");
        assert_eq!(super::fixtures::naming(Path::new(&name)), 0, "none left");
    });
}

/// W8b review: the same stop, with the stream never let through — the mp3's tag read waits on
/// a stream that gives nothing (a stalled remote) — ends the read itself: the open returns
/// cancelled within a bounded number of polls with the engine not stepped, no ffprobe was
/// started, and none naming the file is left.
#[test]
fn a_stop_during_a_mediums_setup_ends_its_tag_read_on_a_stalled_stream() {
    bounded(120, |t| {
        let dir = t.tmp();
        let name = format!("stop-{}.mp3", &crate::remote::random_hex()[..12]);
        let file = dir.path().join(&name);
        std::fs::write(&file, vec![0x55u8; 256 * 1024]).unwrap();
        let Some((source, mut engine)) = stepped(&file) else {
            return;
        };
        let hold = super::fixtures::Hold::new(dir.path(), "hold");
        let tools = waiting_probe(dir.path(), &hold);
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let run =
                scope.spawn(|| Media::stream_with(&tools, source.clone(), &name, &name, &stop));
            until("the setup's read waits on the stream", || {
                source.core().pf.waiting_readers() >= 1
            });
            assert!(!run.is_finished(), "held in its setup");
            stop.store(true, SeqCst);
            // The engine is not stepped: the stream gives nothing.
            let mut polls = 0;
            while !run.is_finished() && polls < 1000 {
                std::thread::sleep(Duration::from_millis(5));
                polls += 1;
            }
            let ended = run.is_finished();
            // Whatever it did, it is let end (a read that misses the stop waits on the stream).
            while !run.is_finished() {
                engine.step(Some(Duration::from_millis(5)));
                std::thread::sleep(Duration::from_millis(5));
            }
            let ran = run.join().unwrap();
            assert!(
                ended,
                "the open still waits on its tag read after {polls} polls"
            );
            let Err(err) = ran else {
                panic!("stopped, not opened");
            };
            assert!(err.is::<crate::remote::Cancelled>(), "{err:#}");
        });
        assert_eq!(probes_started(&name), 0, "no ffprobe was started");
        assert_eq!(super::fixtures::naming(Path::new(&name)), 0, "none left");
    });
}

/// W8b review: a `.ts` refine over its URL entered with the stop already said starts no
/// ffprobe: it returns within a bounded number of polls, and none naming the file is left.
#[test]
fn a_refine_entered_stopped_starts_no_probe() {
    bounded(120, |t| {
        let dir = t.tmp();
        let name = format!("stop-{}.ts", &crate::remote::random_hex()[..12]);
        let file = dir.path().join(&name);
        let mut body = vec![0u8; 188 * 64];
        for packet in body.chunks_mut(188) {
            packet[0] = 0x47;
        }
        std::fs::write(&file, &body).unwrap();
        // Never stepped: an ffprobe that read the URL would wait on it.
        let Some((source, _engine)) = stepped(&file) else {
            return;
        };
        let hold = super::fixtures::Hold::new(dir.path(), "hold");
        let tools = waiting_probe(dir.path(), &hold);
        let never = AtomicBool::new(false);
        let input = served_input(source.clone(), &name, &name, &never).unwrap();
        std::thread::scope(|scope| {
            let run = scope.spawn(|| {
                let video = crate::kind::Kind::Video;
                crate::kind::refine_input_with(Ok(tools.clone()), &input, &name, video, &|| true)
            });
            let mut polls = 0;
            while !run.is_finished() {
                assert!(polls < 1000, "the refine still runs after {polls} polls");
                std::thread::sleep(Duration::from_millis(5));
                polls += 1;
            }
            run.join().unwrap();
        });
        assert_eq!(probes_started(&name), 0, "no ffprobe was started");
        assert_eq!(super::fixtures::naming(Path::new(&name)), 0, "none left");
    });
}
