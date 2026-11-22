//! The media window's runs over a remote file read as a stream (06 §7.4; §12 V16, V18, V19,
//! V22): gates on `local_server()`'s `sftp-server`, ffmpeg reading through the real byte server,
//! `lavfi` fixtures. Nothing asserts wall-clock time; the 60 s rule runs on a clock the test
//! moves, and every wait only catches a hang.

use super::fixtures::{bounded, make, none_left, tools, until};
use super::timeline::{Clip, Timeline, View};
use super::*;
use crate::ffmpeg::{Served, Span};
use crate::remote::{LocalFs, RemoteFs, SftpRemote};
use crate::stream::http::{Config, Seams, Server};
use crate::stream::{limits, Class, Engine, Opening, Settings, BLOCK, CAP_MAX};
use std::sync::atomic::AtomicUsize;

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

/// 06 §12 V16's `refetched`: the bytes of blocks fetched more than once (`Stats` has none; read
/// from the engine's trace of what it sent).
fn refetched(source: &Source) -> u64 {
    let (prefetch, demand) = fetched_again(source);
    prefetch + demand
}

/// [`refetched`] by the class of the fetch that came again: `(prefetch, demand and bulk)`. A
/// demand fetched again is a block a reader waited for twice.
fn fetched_again(source: &Source) -> (u64, u64) {
    let mut seen = std::collections::HashSet::new();
    let (mut prefetch, mut other) = (0, 0);
    for t in &source.core().trace {
        if let (Some(class), Some(block)) = (t.class, t.block) {
            if !seen.insert(block) {
                match class {
                    Class::Prefetch => prefetch += BLOCK,
                    _ => other += BLOCK,
                }
            }
        }
    }
    (prefetch, other)
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
/// length, fetches at most `http_sent + inputs × 1.25 MiB + cap`, and fetches no block twice.
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
            idle(&source);
            let before = source.stats();
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
            idle(&source);
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
            let allowed = sent + 24 * (5 * MIB / 4) + CAP_MAX;
            eprintln!(
                "V16 render {name} ({} bytes): fetched {fetched}, http_sent {sent}, allowed \
                 {allowed}, refetched {}",
                source.size(),
                refetched(&source)
            );
            assert!(fetched <= allowed, "{name}: fetched {fetched} > {allowed}");
            assert_eq!(refetched(&source), 0, "{name}: a block fetched twice");
        }
        none_left(dir.path()).unwrap();
    });
}

/// 06 §12 V16's playback gate, on X1's fixture's shape (600 s at 4 Mbit/s, a key frame every
/// 8 s, its `moov` at the end; 320×240 here, which costs the same bytes): an edit of 10 pieces
/// of 10 s reordered, 12 s of play from timeline positions 5 s, 45 s and 85 s, one after another
/// on one Source. Each start, from `Player::start` to the drop, fetches at most `http_sent +
/// (play readers opened) × their ahead + cap` beyond what it fetched again. Neither the server
/// nor the Source counts the readers a start opened: they are bounded by the decoders it started
/// × 4, X1's most connections per input (a `moov` at the end).
///
/// What it fetches again is held to [`REFETCH_PER_START`], not to 06's one block: that is X1's
/// own fixture's threshold, and this one measures more (below).
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
        let url = PathBuf::from(media.input.show());
        idle(&source);
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
            let (stats, again, decoders) = (
                source.stats(),
                refetched(&source),
                ffmpeg::spawns::of(&url).preview,
            );
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
            idle(&source);
            let now = source.stats();
            let fetched = now.fetched.total - stats.fetched.total;
            let again = refetched(&source) - again;
            let sent = now.http_sent - stats.http_sent;
            let readers = (ffmpeg::spawns::of(&url).preview - decoders) * 4;
            let allowed = sent + readers * ahead + CAP_MAX;
            eprintln!(
                "V16 playback from {at} s: fetched {fetched} (again {again}), http_sent {sent}, \
                 readers ≤ {readers}, allowed {allowed}"
            );
            assert!(fetched - again <= allowed, "from {at} s: fetched {fetched}");
            // What comes again is readahead nobody read, never a block a decoder waited for.
            assert_eq!(
                fetched_again(&source).1,
                0,
                "from {at} s: a demand fetched again"
            );
            assert!(
                again <= REFETCH_PER_START,
                "from {at} s: {again} bytes fetched again"
            );
        }
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// What a playback start of [`v16_playback_of_an_edit_fetches_within_its_bound`] may fetch
/// again: a regression threshold for this fixture (06 §12 V16). Measured (ffmpeg 9.0.2, three
/// runs): 0, 14–24 and 9–15 blocks for the three starts, every one a prefetch that no decoder
/// read. Each decoder opens this `moov`-at-the-end file at byte 0, reads a few KB and seeks to
/// the `moov`; meanwhile the server fills that connection's socket from the cached head, each
/// read a sequential hit that grows its reader's readahead, so prefetch past the cached head
/// goes out before ffmpeg closes the connection. Never read, those blocks are the first the
/// cache evicts once it is full (outside every live window), and the next decoder's opening
/// prefetches them again. X1 measured 1 block in nine starts on its own 1080p fixture.
const REFETCH_PER_START: u64 = 2 * MIB;

/// 06 §12 V16: a 2-input render whose second input waits for its piece past 60 s (on the
/// server's clock, which the test moves) has no cut and the edit's length — the 60 s rule does
/// not run on a `render` body (06 §9).
#[test]
fn v16_a_render_input_that_waits_past_60_s_is_not_cut() {
    bounded(240, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let made = dir.path().join("made.mp4");
        long_video(&tools, &made, 120);
        let path = dir.path().join("v.mp4");
        faststart(&tools, &made, &path);
        let Some(source) = streamed(&path) else {
            return;
        };
        let hand = t.hand();
        let blocked = Arc::new(AtomicUsize::new(0));
        let count = blocked.clone();
        let config = Config {
            clock: hand.clock(),
            tick: Some(Duration::from_millis(20)),
            seams: Seams {
                blocking: Some(Arc::new(move || {
                    count.fetch_add(1, SeqCst);
                })),
                ..Seams::default()
            },
        };
        let server = Server::start_with(source.clone(), config).unwrap();
        let served = Served::new(server, source.clone(), "v.mp4", "v.mp4", false);
        let mut media = Media::served(&tools, served.clone(), "v.mp4", "v.mp4").unwrap();
        let (kind, plan) = planned(&mut media);
        // The second piece's input is opened with the first, and waits while the first plays.
        let spec = spec_of(&media, &[(70.0, 40.0), (10.0, 40.0)]);
        assert_eq!(spec.groups_from(&media.input).len(), 2);
        let out = dir.path().join("out.mp4");
        // The render's first progress report holds it — ffmpeg blocks on its progress pipe, and
        // its inputs' connections stop being read — until the clock has gone past the rule.
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
        // Both inputs are open; their connections fill and their writes wait.
        let held = blocked.load(SeqCst);
        until("the render's writes wait", || {
            blocked.load(SeqCst) >= held + 2
        });
        for _ in 0..2 {
            hand.advance(Duration::from_secs(61));
            // A few of the server's looks at its clock.
            std::thread::sleep(Duration::from_millis(200));
        }
        hold_tx.send(()).unwrap();
        render.join().unwrap().unwrap();
        for kind in Kind::ALL {
            assert_eq!(served.server().cuts(kind), 0, "a waiting input was cut");
        }
        assert_eq!(entry(&tools, &out, "v", "nb_frames"), "2000");
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// The file changes on the remote while an Export renders it: the render's stream is cut, and
/// though ffmpeg exits 0 on a body cut short (06 §7.4), nothing is published and nothing is left
/// under `<cache>/.exports` (06 §12 V22, V16's "a render whose `cuts(Render)` went up").
#[test]
fn v22_a_render_whose_stream_was_cut_publishes_nothing() {
    bounded(240, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("v.mp4");
        long_video(&tools, &path, 600);
        let size = std::fs::metadata(&path).unwrap().len();
        let Some((source, mut engine)) = stepped(&path) else {
            return;
        };
        let served = Served::start(source.clone(), "v.mp4", "v.mp4", false).unwrap();
        let media = stepping(&mut engine, || {
            Media::served(&tools, served.clone(), "v.mp4", "v.mp4").unwrap()
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
                    "v.mp4",
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
        assert!(served.server().cuts(Kind::Render) >= 1);
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

/// A `LocalFs` as an export's remote, whose upload or link a test watches: a Cancel arrives
/// during the upload (`cancel_in_upload`), or right after the link has landed.
struct Publishing {
    fs: LocalFs,
    stop: Arc<cut::Stop>,
    cancel_in_upload: bool,
    cancel_after_link: Arc<Mutex<Option<bool>>>,
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
        let up = self.fs.create_from(local, path, like);
        if self.cancel_in_upload {
            // The owner's Cancel, while the bytes went.
            assert!(self.stop.cancel(), "a Cancel during the upload is in time");
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
            *self.cancel_after_link.lock().unwrap() = Some(self.stop.cancel());
        }
        linked
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        self.fs.remove(path)
    }
}

/// 06 §12 V22: Cancel during the render kills ffmpeg and leaves no file under the exports and
/// nothing on the remote; during the upload it removes the remote temp and links nothing; after
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
        // During the upload.
        let stop = Arc::new(cut::Stop::default());
        let mut fs = Publishing {
            fs: LocalFs::new(),
            stop: stop.clone(),
            cancel_in_upload: true,
            cancel_after_link: Arc::default(),
        };
        let err = export(&mut fs, &stop, false).unwrap_err();
        assert!(crate::remote::is_cancelled(&err), "{err:#}");
        assert_eq!(listed(), before, "the temp is removed and nothing linked");
        assert_eq!(*fs.cancel_after_link.lock().unwrap(), None, "no link");
        nothing_under_exports();
        // After the link.
        let stop = Arc::new(cut::Stop::default());
        let late = Arc::new(Mutex::new(None));
        let mut fs = Publishing {
            fs: LocalFs::new(),
            stop: stop.clone(),
            cancel_in_upload: false,
            cancel_after_link: late.clone(),
        };
        let published = export(&mut fs, &stop, false).unwrap();
        assert_eq!(*late.lock().unwrap(), Some(false), "too late: refused");
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
/// that neither reads nor closes. A dropped player; an export stopped by its watchdog (its
/// limit shortened here); a probe past its time (shortened); a paused frame past its budget.
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
        // An export stopped by its watchdog.
        let args = media.input.of_kind(Kind::Render).args();
        let limit = Duration::from_secs(1);
        let err = ffmpeg::run_stoppable(&held, &args, 1.0, &mut |_| {}, limit, (&|| false, &|| 0))
            .unwrap_err();
        assert!(format!("{err:#}").contains("no progress"), "{err:#}");
        until("a stopped export's seat is free", || seats() == 0);
        // A probe past its time.
        let err = ffmpeg::probe_within(&held, &media.input, "v.mp4", limit).unwrap_err();
        assert!(format!("{err:#}").contains("longer than"), "{err:#}");
        until("a stopped probe's seat is free", || seats() == 0);
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
        idle(&source);
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
        let partial: Vec<f64> = reads
            .iter()
            .copied()
            .filter(|r| *r < media.duration)
            .collect();
        assert!(partial.len() >= 3, "handed over as it filled: {reads:?}");
        assert!(partial.windows(2).all(|w| w[0] <= w[1]), "{reads:?}");
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
            mp3::info_tag(&mut source.reader(ReaderKind::Media { bytes_per_s: 0 }))
        });
        assert!(fastseek);
        let served = Served::start(source.clone(), "m.mp3", "m.mp3", fastseek).unwrap();
        let media = stepping(&mut engine, || {
            Media::served(&tools, served.clone(), "m.mp3", "m.mp3").unwrap()
        });
        idle_stepped(&source, &mut engine);
        let from = source.core().trace.len();
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
        for t in &source.core().trace[from..n] {
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
    assert!(drv.find_text("cancelling the export…").is_some());
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
    assert!(drv
        .find_text("too late to cancel: the export is being published")
        .is_some());
}

/// [`idle`] for a stream whose engine the test steps.
fn idle_stepped(source: &Source, engine: &mut Engine) {
    let give_up = Instant::now() + Duration::from_secs(60);
    loop {
        {
            let mut core = source.core();
            if !core.queued() && core.pf.adm.in_flight() == 0 {
                return;
            }
        }
        assert!(Instant::now() < give_up, "the stream does not settle");
        engine.step(Some(Duration::from_millis(5)));
    }
}

/// 06 §9 and §12 V20's last item: every ffmpeg and ffprobe run on a remote input — the probe,
/// playback's picture and sound, the paused frame, the waveform, a render of two inputs — has
/// `-protocol_whitelist http,tcp` before each of its URLs, and reads the URL of its own reader
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
            let guarded = line.matches("-protocol_whitelist http,tcp http://").count()
                + line
                    .matches("-protocol_whitelist http,tcp -i http://")
                    .count()
                + line
                    .matches("-protocol_whitelist http,tcp -fflags +fastseek -i http://")
                    .count();
            assert_eq!(guarded, urls, "a URL without its whitelist: {line}");
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
        idle_stepped(&source, &mut engine);
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
/// and ends its wait (`ready`), with no read still said; once the file has changed on the
/// remote, a click elsewhere shows no frame, says the change, and ends its wait too.
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
        // The file changes: the next place asked for shows no frame, and says why.
        change_in_place(&path, &source);
        click(&mut drv, &mut win, 0.3);
        drv.until(&mut win, 60, |w| w.asked.is_none());
        drv.idle(&mut win, 2);
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
        drv.key(&mut win, egui::Key::ArrowRight, egui::Modifiers::NONE);
        let since = Instant::now();
        while since.elapsed() < Duration::from_millis(1500) {
            drv.idle(&mut win, 1);
            std::thread::sleep(Duration::from_millis(20));
        }
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

/// 06 §7.4, §12 V22: Cancel during the upload of a large Export stops sending — the temp on the
/// remote stops growing far short of the file — then the temp is removed and nothing is linked.
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
        let Some(mut fs) = SftpRemote::local_server() else {
            return;
        };
        // The export publishes into a folder of its own, watched from here.
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let spec = spec_of(&media, &[(0.0, 600.0)]);
        // 600 s of 16-bit stereo at 48 kHz.
        let size = 600 * 48_000 * 4;
        let stop = Arc::new(cut::Stop::default());
        let done = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (out, stop, done) = (out.clone(), stop.clone(), done.clone());
            std::thread::spawn(move || {
                let mut most = 0u64;
                while !done.load(SeqCst) {
                    for entry in std::fs::read_dir(&out).unwrap().flatten() {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.starts_with('.') && name.contains(".sopen-") {
                            let len = entry.metadata().map_or(0, |m| m.len());
                            most = most.max(len);
                            // The owner's Cancel, while the bytes go.
                            if len >= 8 * MIB {
                                stop.cancel();
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                most
            })
        };
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
        done.store(true, SeqCst);
        let most = watcher.join().unwrap();
        eprintln!("an upload of {size} bytes, cancelled: the temp reached {most} bytes");
        let err = result.unwrap_err();
        assert!(crate::remote::is_cancelled(&err), "{err:#}");
        assert!(most >= 8 * MIB, "the upload was under way: {most}");
        assert!(
            most < size / 4,
            "the upload stopped: the temp reached {most} of {size}"
        );
        let left: Vec<_> = std::fs::read_dir(&out).unwrap().flatten().collect();
        assert!(left.is_empty(), "nothing left on the remote: {left:?}");
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// A stand-in for ffmpeg and ffprobe that fails naming its input's URL, as ffmpeg does for a
/// server's refusal (it answers a render's encoder question from the real ffmpeg).
fn complainer(dir: &Path, real: &Path) -> Tools {
    let body = format!(
        "case \"$*\" in *-encoders*) exec '{}' \"$@\";; esac\n\
         prev=; url=; last=\n\
         for a in \"$@\"; do last=$a; [ \"$prev\" = -i ] && url=$a; prev=$a; done\n\
         [ -z \"$url\" ] && url=$last\n\
         echo \"$url: Server returned 404 Not Found\" >&2\n\
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
        let clean = |what: &str, said: &str| {
            assert!(said.contains(shown), "{what} names the file: {said}");
            assert!(
                !said.contains("127.0.0.1"),
                "{what} shows the address: {said}"
            );
            assert!(!said.contains(&token), "{what} shows the token: {said}");
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
        drop(wave);
        none_reading(&media);
        drop(media);
        none_left(dir.path()).unwrap();
    });
}

/// A stand-in for ffmpeg that reads its input slowly — 64 KiB every 100 ms for 3 s — and prints
/// nothing, then ends well: a render reading deep into a file to its first piece over a slow
/// link, with no output meanwhile.
fn slow_reader(dir: &Path) -> Tools {
    let path = dir.join("slow-reader");
    let script = "#!/bin/bash\n[ \"$1\" = -version ] && exit 0\n\
        url=\"\"; prev=\"\"\n\
        for a in \"$@\"; do [ \"$prev\" = -i ] && url=\"$a\"; prev=\"$a\"; done\n\
        rest=\"${url#http://127.0.0.1:}\"; port=\"${rest%%/*}\"; path=\"/${rest#*/}\"\n\
        exec 3<>\"/dev/tcp/127.0.0.1/$port\"\n\
        printf 'GET %s HTTP/1.1\\r\\nHost: 127.0.0.1:%s\\r\\n\\r\\n' \"$path\" \"$port\" >&3\n\
        for i in $(seq 1 30); do head -c 65536 <&3 >/dev/null; sleep 0.1; done\n\
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

/// 06 §7.4: a render's watchdog counts its input going on as progress. A run with no output for
/// longer than its limit while the byte server keeps sending it bytes is not stopped; one that
/// takes no bytes and gives no output is (the limit shortened here, as the owner gate does).
#[test]
fn a_render_fed_by_its_input_is_not_stopped_and_one_starved_is() {
    bounded(120, |t| {
        let Some(tools) = tools() else { return };
        let dir = t.tmp();
        let path = dir.path().join("v.mp4");
        long_video(&tools, &path, 60);
        let Some(source) = streamed(&path) else {
            return;
        };
        let media = Media::stream(source, "v.mp4", "v.mp4").unwrap();
        let render = media.input.of_kind(Kind::Render);
        let args = render.args();
        let limit = Duration::from_secs(1);
        let fed = || render.fed();
        let started = Instant::now();
        let reader = slow_reader(dir.path());
        ffmpeg::run_stoppable(&reader, &args, 1.0, &mut |_| {}, limit, (&|| false, &fed))
            .expect("fed, it is not stopped");
        let took = started.elapsed();
        assert!(
            took >= 2 * limit,
            "it ran {took:?}, past its limit, with no output"
        );
        let held = holder(dir.path());
        let err = ffmpeg::run_stoppable(&held, &args, 1.0, &mut |_| {}, limit, (&|| false, &fed))
            .unwrap_err();
        assert!(format!("{err:#}").contains("no progress"), "{err:#}");
        let server = media.input.served().unwrap().server();
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
/// a Cancel, the path the caller's to remove.
#[test]
fn an_upload_stops_between_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("big");
    std::fs::write(&src, vec![7u8; 32 << 20]).unwrap();
    let mut layers: Vec<(&str, Box<dyn RemoteFs>)> = vec![("local", Box::new(LocalFs::new()))];
    if let Some(sftp) = SftpRemote::local_server() {
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
        // Unstopped, the whole file goes.
        let whole = tmp.path().join(format!("whole-{what}"));
        let up = fs.create_from(&src, whole.to_str().unwrap(), None).unwrap();
        assert_eq!(up.size, 32 << 20, "{what}");
    }
}
